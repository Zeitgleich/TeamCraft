//! API, gateway and SFU signaling types shared by server and clients.
//!
//! This crate is wasm-compatible and contains no I/O. Wire enums carry an `Unknown` catch-all so
//! older clients survive new server variants (api-conventions.md).

mod api;
mod conversation;
mod error;
mod id;
mod role;
mod wire_enum;

pub use api::{GetMeResponse, HealthResponse, HealthStatus};
pub use conversation::{ConversationKind, ConversationVisibility, HistoryPolicy};
pub use error::{ErrorBody, ErrorCode};
pub use id::{
  ConversationId, DeviceId, Id, IdError, MessageId, SNOWFLAKE_EPOCH_MS, SNOWFLAKE_SEQUENCE_BITS,
  SNOWFLAKE_WORKER_BITS, Snowflake, SpaceId, ThreadId, UserId,
};
pub use role::Role;
