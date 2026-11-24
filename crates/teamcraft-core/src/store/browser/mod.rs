//! The persistent browser store (e2ee.md §9): encrypted records in IndexedDB, or on OPFS in a
//! dedicated worker (issue #2266).

mod browser_key;
mod browser_store;
mod dpop_key;
pub(crate) mod idb;
mod indexed_db;
mod opfs;
mod own_sound;
pub(crate) mod picked;
mod service_worker;
mod vault;

pub use browser_key::{browser_database_key, browser_database_key_without_upgrade};
pub use browser_store::{BrowserRecords, BrowserStore};
pub use dpop_key::{browser_dpop_prover, browser_dpop_prover_or_new};
pub use idb::{IdbFailureHook, set_failure_hook as set_idb_failure_hook};
pub use indexed_db::{IndexedDbRecords, IndexedDbStore};
pub use opfs::{OpfsOpening, OpfsRecords, OpfsStore};
pub use own_sound::{own_sound_delete, own_sound_get, own_sound_put};
pub use service_worker::{OpfsCopy, ServiceWorkerRecords, ServiceWorkerStore};
pub use vault::{vault_delete, vault_get, vault_put};
