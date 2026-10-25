//! The persistent browser store (e2ee.md §9): encrypted records in IndexedDB.

mod browser_key;
mod idb;
mod indexed_db;
pub(crate) mod picked;
mod vault;

pub use browser_key::browser_database_key;
pub use indexed_db::{IndexedDbRecords, IndexedDbStore};
pub use vault::{vault_delete, vault_get, vault_put};
