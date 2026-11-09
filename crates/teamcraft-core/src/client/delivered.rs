//! Delivered ticks (issue #598): after this device stored the messages of a DM or group DM, it
//! tells the server how far (gateway `delivered`, one per conversation when its newest stored
//! message moves forward). The runner sends the reports while connected; nothing is persisted: the
//! catch-up after a reconnect notes the newest stored message again.

use std::collections::{BTreeMap, HashMap};

use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationResponse, GatewayClientMessage, MessageId,
};

use super::Client;
use crate::api::ServerApi;
use crate::connection::ConnectionSupervisor;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageEdge, Table};

/// Reports not sent yet, and what the server was last told per conversation.
#[derive(Debug, Default)]
pub(crate) struct DeliveredReports {
  unsent: BTreeMap<ConversationId, MessageId>,
  sent: HashMap<ConversationId, MessageId>,
  /// Whether a conversation has ticks (DMs and group DMs), so a live message costs no record read.
  has_ticks: HashMap<ConversationId, bool>,
}

impl DeliveredReports {
  /// Queues `message` unless the server already has it or something newer.
  fn note(&mut self, conversation: ConversationId, message: MessageId) {
    if self.sent.get(&conversation).is_some_and(|sent| *sent >= message) {
      return;
    }
    let queued = self.unsent.entry(conversation).or_insert(message);
    *queued = (*queued).max(message);
  }

  /// The queued reports, by conversation ID; they leave the queue.
  fn take(&mut self) -> Vec<(ConversationId, MessageId)> {
    std::mem::take(&mut self.unsent).into_iter().collect()
  }

  fn mark_sent(&mut self, conversation: ConversationId, message: MessageId) {
    let sent = self.sent.entry(conversation).or_insert(message);
    *sent = (*sent).max(message);
  }

  /// Puts back a report that could not go out (the connection dropped); a newer one wins.
  fn requeue(&mut self, conversation: ConversationId, message: MessageId) {
    self.note(conversation, message);
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Notes that this device stored `conversation` up to its newest server message, for the runner
  /// to report. Channels have no ticks and report nothing.
  pub(crate) fn note_delivered(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if !self.has_delivery_ticks(conversation)? {
      return Ok(());
    }
    if let Some(newest) = self.store.edge_server_id(conversation, MessageEdge::Newest)? {
      self.delivered.note(conversation, newest);
    }
    Ok(())
  }

  /// Sends the queued reports on `connection` while it is connected; offline they wait, and a
  /// report that fails to go out goes with the next ones.
  pub async fn report_delivered(&mut self, connection: &mut ConnectionSupervisor<A::Gateway>, now_ms: i64) {
    if !connection.is_connected() {
      return;
    }
    let mut frames = self.take_delivered_frames().into_iter();
    for frame in frames.by_ref() {
      if !connection.send(now_ms, &frame).await {
        self.requeue_delivered(&frame);
        break;
      }
      self.mark_delivered_sent(&frame);
    }
    for frame in frames {
      self.requeue_delivered(&frame);
    }
  }

  /// The `delivered` frames due now; they leave the queue. Each one sent goes to
  /// [`Self::mark_delivered_sent`], each one that couldn't to [`Self::requeue_delivered`].
  pub fn take_delivered_frames(&mut self) -> Vec<GatewayClientMessage> {
    self
      .delivered
      .take()
      .into_iter()
      .map(|(conversation_id, message_id)| GatewayClientMessage::Delivered {
        conversation_id,
        message_id,
      })
      .collect()
  }

  /// The server has `frame`, a `delivered` frame from [`Self::take_delivered_frames`].
  pub fn mark_delivered_sent(&mut self, frame: &GatewayClientMessage) {
    if let GatewayClientMessage::Delivered {
      conversation_id,
      message_id,
    } = frame
    {
      self.delivered.mark_sent(*conversation_id, *message_id);
    }
  }

  /// `frame` could not go out; it goes with the next ones.
  pub fn requeue_delivered(&mut self, frame: &GatewayClientMessage) {
    if let GatewayClientMessage::Delivered {
      conversation_id,
      message_id,
    } = frame
    {
      self.delivered.requeue(*conversation_id, *message_id);
    }
  }

  fn has_delivery_ticks(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    if let Some(known) = self.delivered.has_ticks.get(&conversation) {
      return Ok(*known);
    }
    let maybe_conversation: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    // Not stored yet: don't remember, the conversation's record arrives with its catch-up.
    let Some(stored) = maybe_conversation else {
      return Ok(false);
    };
    let has_ticks = matches!(stored.kind, ConversationKind::Dm | ConversationKind::Group);
    self.delivered.has_ticks.insert(conversation, has_ticks);
    Ok(has_ticks)
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn a_report_goes_out_once_and_only_forward() {
    let mut reports = DeliveredReports::default();
    reports.note(conversation(), message(5));
    reports.note(conversation(), message(3));
    assert_eq!(reports.take(), vec![(conversation(), message(5))]);
    reports.mark_sent(conversation(), message(5));
    reports.note(conversation(), message(5));
    reports.note(conversation(), message(4));
    assert!(reports.take().is_empty());
    reports.note(conversation(), message(6));
    assert_eq!(reports.take(), vec![(conversation(), message(6))]);
  }

  #[test]
  fn a_report_that_failed_goes_with_the_next() {
    let mut reports = DeliveredReports::default();
    reports.note(conversation(), message(5));
    let taken = reports.take();
    reports.note(conversation(), message(7));
    for (conversation, message) in taken {
      reports.requeue(conversation, message);
    }
    assert_eq!(reports.take(), vec![(conversation(), message(7))]);
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(1).unwrap())
  }

  fn message(id: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
