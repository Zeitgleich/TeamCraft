//! System notifications for admins' requests to join a channel the user owns (admin.md §2.4): one
//! per request, once per device, withdrawn when the request is gone (decided anywhere, withdrawn or
//! expired). The requests themselves come from the activity feed.

use std::collections::BTreeSet;

use teamcraft_client_api::activity::JoinRequest;
use teamcraft_client_api::os_notify::NotificationTag;

/// The requests this device notified about, by request (a new request of the same admin for the
/// same channel is a new one). Stored per user, so a restart doesn't notify again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinRequestAlerts {
  notified: BTreeSet<String>,
}

/// What to do after the list of requests changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlertChanges {
  /// Requests to notify about now.
  pub new: Vec<JoinRequest>,
  /// Notifications of requests that are gone.
  pub gone: Vec<NotificationTag>,
}

impl JoinRequestAlerts {
  pub fn from_json(text: &str) -> Self {
    Self {
      notified: serde_json::from_str(text).unwrap_or_default(),
    }
  }

  pub fn to_json(&self) -> String {
    serde_json::to_string(&self.notified).unwrap_or_default()
  }

  /// Takes the requests waiting now; forgets the ones that are gone.
  pub fn update(&mut self, requests: &[JoinRequest]) -> AlertChanges {
    let current: BTreeSet<String> = requests.iter().map(key).collect();
    let waiting: BTreeSet<NotificationTag> = requests.iter().map(tag).collect();
    // A newer request of the same admin and channel replaces its notification instead.
    let gone = self
      .notified
      .difference(&current)
      .filter_map(|known| tag_of(known))
      .filter(|known| !waiting.contains(known))
      .collect();
    let new = requests
      .iter()
      .filter(|request| !self.notified.contains(&key(request)))
      .cloned()
      .collect();
    self.notified = current;
    AlertChanges { new, gone }
  }
}

/// The notification's tag: one per admin and channel, so a newer request replaces an older one.
pub fn tag(request: &JoinRequest) -> NotificationTag {
  NotificationTag::join_request(request.conversation, request.user)
}

fn key(request: &JoinRequest) -> String {
  format!(
    "{}:{}:{}",
    request.conversation, request.user, request.requested_at
  )
}

fn tag_of(key: &str) -> Option<NotificationTag> {
  let mut parts = key.split(':');
  let (conversation, user) = (parts.next()?, parts.next()?);
  Some(NotificationTag::join_request(conversation, user))
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::activity::JoinRequestStanding;
  use teamcraft_client_api::model::id_from_raw;

  use super::*;

  #[test]
  fn a_request_notifies_once_and_is_withdrawn_when_it_is_gone() {
    let mut alerts = JoinRequestAlerts::default();
    let first = alerts.update(&[request(7, 2, 100)]);
    assert_eq!(first.new.len(), 1);
    assert!(first.gone.is_empty());
    // Read again (a reconnect, another change): nothing new.
    assert_eq!(alerts.update(&[request(7, 2, 100)]), AlertChanges::default());
    let decided = alerts.update(&[]);
    assert_eq!(decided.gone, vec![NotificationTag::join_request(7, 2)]);
  }

  #[test]
  fn a_restart_remembers_what_was_shown_and_a_new_request_notifies_again() {
    let mut alerts = JoinRequestAlerts::default();
    alerts.update(&[request(7, 2, 100)]);
    let mut restored = JoinRequestAlerts::from_json(&alerts.to_json());
    assert!(restored.update(&[request(7, 2, 100)]).new.is_empty());
    // Declined, asked again later: a new request.
    assert_eq!(restored.update(&[request(7, 2, 900)]).new.len(), 1);
    assert_eq!(
      JoinRequestAlerts::from_json("not json"),
      JoinRequestAlerts::default()
    );
  }

  fn request(conversation: u64, user: u64, requested_at: i64) -> JoinRequest {
    JoinRequest {
      conversation: id_from_raw(conversation),
      user: id_from_raw(user),
      user_name: "Admin A".into(),
      requested_at,
      expires_at: requested_at + 1_000,
      standing: JoinRequestStanding::Approvable,
    }
  }
}
