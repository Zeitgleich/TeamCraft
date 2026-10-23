//! All TeamCraft cryptography: identity and device keys, conversation content keys, attachment
//! encryption, the key bag and its backup wrappers, and MLS groups for key distribution.
//!
//! The design is described in `docs/e2ee.md`; `README.md` in this crate maps every module to its
//! section. No other crate implements cryptography (`.claude/rules/security.md`).

pub mod account;
pub mod attachment;
pub mod attestation;
pub mod backup;
pub mod call_binding;
pub mod content_key;
pub mod device_list;
pub mod identity;
pub mod ids;
pub mod key_bag;
pub mod local_store;
pub mod membership;
pub mod message;
pub mod mls;
pub mod org_recovery;
pub mod push;
pub mod push_gateway;
pub mod rendezvous;
pub mod secret;
pub mod server_secret;
pub mod share_link;
pub mod workspace;

mod aead;
mod domain;
