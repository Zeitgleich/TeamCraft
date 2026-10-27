//! The persistent browser store (e2ee.md §9): encrypted records in IndexedDB.

mod browser_key;
pub(crate) mod idb;
mod indexed_db;
mod own_sound;
pub(crate) mod picked;
mod vault;

pub use browser_key::browser_database_key;
pub use indexed_db::{IndexedDbRecords, IndexedDbStore};
pub use own_sound::{own_sound_delete, own_sound_get, own_sound_put};
pub use vault::{vault_delete, vault_get, vault_put};
