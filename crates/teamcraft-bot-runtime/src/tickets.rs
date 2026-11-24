//! Join tickets from the server, for rejoining a call after a lost connection or an SFU restart.
//! They go over a connection of their own: the worker's client is busy with the gateway.

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use teamcraft_core::api::{HttpServerApi, ServerApi, StaticToken};
use teamcraft_media::call_native::{JoinTicketSource, TicketFuture};
use teamcraft_media::call_resilience::{JoinTicket, JoinTicketError};
use teamcraft_proto::{ConversationId, CreateCallJoinTokenRequest, DeviceId};

/// Fresh join tickets for a conversation's call.
pub trait TicketFactory: Send + Sync {
  fn for_conversation(&self, conversation: ConversationId) -> Arc<dyn JoinTicketSource>;
}

/// Tickets from the server for this bot's device.
pub struct ServerTickets {
  api: Arc<HttpServerApi<StaticToken>>,
  device_id: DeviceId,
}

/// The tickets of one conversation's call.
struct ConversationTickets {
  api: Arc<HttpServerApi<StaticToken>>,
  conversation: ConversationId,
  device_id: DeviceId,
}

impl ServerTickets {
  pub fn new(server_url: &str, token: &SecretString, device_id: DeviceId) -> Self {
    Self {
      api: Arc::new(HttpServerApi::new(
        server_url,
        StaticToken(token.expose_secret().to_owned()),
      )),
      device_id,
    }
  }
}

impl TicketFactory for ServerTickets {
  fn for_conversation(&self, conversation: ConversationId) -> Arc<dyn JoinTicketSource> {
    Arc::new(ConversationTickets {
      api: Arc::clone(&self.api),
      conversation,
      device_id: self.device_id,
    })
  }
}

impl JoinTicketSource for ConversationTickets {
  fn fetch(&self) -> TicketFuture {
    let (api, conversation, device_id) = (Arc::clone(&self.api), self.conversation, self.device_id);
    Box::pin(async move {
      let join = api
        .create_call_join_token(conversation, &CreateCallJoinTokenRequest { device_id })
        .await
        .map_err(|error| JoinTicketError {
          reason: error.to_string(),
        })?;
      Ok(JoinTicket::from(join))
    })
  }
}
