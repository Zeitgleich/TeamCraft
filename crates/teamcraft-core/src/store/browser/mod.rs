//! The persistent browser store (e2ee.md §9): encrypted records in IndexedDB.

mod browser_key;
mod idb;
mod indexed_db;

pub use browser_key::browser_database_key;
pub use indexed_db::IndexedDbStore;
