//! The users this device knows, decoded once and kept in memory while the store's users don't
//! change (issue #1682). Every name on screen (the snapshot, a message's author, a mention, the
//! member pickers) looks the person up here: reading and decrypting every user record per lookup
//! made one name cost a pass over the whole workspace.
//!
//! What is kept is checked against [`LocalStore::users_revision`], which moves with every write
//! to the users that reached the reader's view of the store (and with a rollback or another
//! process's commit). A store that can't tell reads the users every time, as before.
//!
//! Memory: one entry per row of the users table, nothing more. A decoded user takes about 300
//! bytes (the summary, its names and the map's node), so a workspace of 10,000 people holds about
//! 3 MiB, once per process: the app's read threads share one directory
//! ([`crate::ui_backend::CoreChatBackend::with_user_directory`]).
//!
//! The names and e-mail addresses are overwritten when the last holder of a map lets go of it
//! (security.md, "Secrets"), and nothing here is logged.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_proto::{UserId, UserSummary};
use zeroize::Zeroize;

use crate::error::CoreError;
use crate::store::LocalStore;

/// The decoded users, shared by the read threads of one signed-in account.
#[derive(Default)]
pub struct UserDirectory {
  kept: Mutex<Option<KeptUsers>>,
}

/// The users by ID, as read at one revision. Overwritten when dropped.
#[derive(Default)]
pub struct UserMap(BTreeMap<UserId, UserSummary>);

/// The users read last and the revision they were read at.
struct KeptUsers {
  revision: u64,
  users: Arc<UserMap>,
}

impl UserDirectory {
  pub fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  /// The users of `store`: those kept when its revision didn't move, otherwise read with `read`
  /// and kept. A read that fails is not kept: it shows as nobody known until the next look.
  pub(crate) fn users<S: LocalStore + ?Sized>(
    &self,
    store: &S,
    read: impl FnOnce() -> Result<Vec<UserSummary>, CoreError>,
  ) -> Arc<UserMap> {
    let Some(revision) = store.users_revision() else {
      return Arc::new(UserMap::read(read).unwrap_or_default());
    };
    if let Some(kept) = self.lock().as_ref()
      && kept.revision == revision
    {
      return Arc::clone(&kept.users);
    }
    // Read without the lock: another read thread doesn't wait for this one's disk.
    let Some(users) = UserMap::read(read) else {
      return Arc::new(UserMap::default());
    };
    let users = Arc::new(users);
    let mut kept = self.lock();
    // A thread that read at a later revision meanwhile keeps its users.
    if kept.as_ref().is_none_or(|kept| kept.revision <= revision) {
      *kept = Some(KeptUsers {
        revision,
        users: Arc::clone(&users),
      });
    }
    users
  }

  /// Lets go of the users kept (the account signs out); they are overwritten once no read holds
  /// them any more.
  pub fn forget(&self) {
    self.lock().take();
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Option<KeptUsers>> {
    // Invariant-free data: at worst the users are read again.
    self.kept.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl UserMap {
  /// `None` when the read failed (logged): the UI has no error channel here.
  fn read(read: impl FnOnce() -> Result<Vec<UserSummary>, CoreError>) -> Option<Self> {
    match read() {
      Ok(list) => Some(Self(list.into_iter().map(|user| (user.id, user)).collect())),
      Err(error) => {
        tracing::error!(%error, "local store read failed");
        None
      },
    }
  }
}

impl Deref for UserMap {
  type Target = BTreeMap<UserId, UserSummary>;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl Drop for UserMap {
  fn drop(&mut self) {
    for user in self.0.values_mut() {
      user.display_name.zeroize();
      user.maybe_username.zeroize();
      user.maybe_email.zeroize();
      user.maybe_organization.zeroize();
    }
  }
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;

  use teamcraft_proto::{Id, Snowflake, UserKind};

  use super::*;
  use crate::store::{MemoryStore, StoreError, Table, TrackedStore};

  #[test]
  fn users_are_read_once_while_the_revision_stays() {
    let store = TrackedStore::new(MemoryStore::new());
    let directory = UserDirectory::default();
    let reads = Cell::new(0);
    let read = || {
      reads.set(reads.get() + 1);
      Ok(vec![summary(1, "Alex Example")])
    };

    let first = directory.users(&store, read);
    let second = directory.users(&store, read);

    assert_eq!(reads.get(), 1);
    assert!(Arc::ptr_eq(&first, &second));
  }

  #[test]
  fn a_write_to_the_users_reads_them_again() {
    let store = TrackedStore::new(MemoryStore::new());
    let directory = UserDirectory::default();
    directory.users(&store, || Ok(vec![summary(1, "Alex Example")]));

    store.put(Table::Users, "1", b"{}").unwrap();
    let users = directory.users(&store, || Ok(vec![summary(1, "Alex Renamed")]));

    assert_eq!(users[&id(1)].display_name, "Alex Renamed");
  }

  #[test]
  fn other_tables_keep_the_users() {
    let store = TrackedStore::new(MemoryStore::new());
    let directory = UserDirectory::default();
    let first = directory.users(&store, || Ok(vec![summary(1, "Alex Example")]));

    store.put(Table::Spaces, "1", b"{}").unwrap();
    let second = directory.users(&store, || panic!("read again"));

    assert!(Arc::ptr_eq(&first, &second));
  }

  #[test]
  fn a_failed_read_is_not_kept() {
    let store = TrackedStore::new(MemoryStore::new());
    let directory = UserDirectory::default();

    let failed = directory.users(&store, || {
      Err(CoreError::Store(StoreError::Corrupt("test".into())))
    });
    let read = directory.users(&store, || Ok(vec![summary(1, "Alex Example")]));

    assert!(failed.is_empty());
    assert_eq!(read.len(), 1);
  }

  #[test]
  fn a_store_without_a_revision_is_read_every_time() {
    let store = MemoryStore::new();
    let directory = UserDirectory::default();
    let reads = Cell::new(0);
    let read = || {
      reads.set(reads.get() + 1);
      Ok(Vec::new())
    };

    directory.users(&store, read);
    directory.users(&store, read);

    assert_eq!(reads.get(), 2);
  }

  #[test]
  fn forgetting_reads_the_users_again() {
    let store = TrackedStore::new(MemoryStore::new());
    let directory = UserDirectory::default();
    directory.users(&store, || Ok(vec![summary(1, "Alex Example")]));

    directory.forget();
    let users = directory.users(&store, || Ok(Vec::new()));

    assert!(users.is_empty());
  }

  #[test]
  fn users_read_at_an_older_revision_never_replace_newer_ones() {
    let store = TrackedStore::new(MemoryStore::new());
    let directory = UserDirectory::default();
    // A slow thread reads at the first revision; meanwhile the users change and another thread
    // reads and keeps the new ones. The slow thread's users must not replace them.
    let slow = directory.users(&store, || {
      store.put(Table::Users, "1", b"{}").unwrap();
      let fresh = directory.users(&store, || Ok(vec![summary(1, "Alex Renamed")]));
      assert_eq!(fresh[&id(1)].display_name, "Alex Renamed");
      Ok(vec![summary(1, "Alex Example")])
    });

    let kept = directory.users(&store, || panic!("read again"));

    assert_eq!(slow[&id(1)].display_name, "Alex Example");
    assert_eq!(kept[&id(1)].display_name, "Alex Renamed");
  }

  fn summary(raw: i64, name: &str) -> UserSummary {
    UserSummary {
      id: id(raw),
      maybe_username: None,
      display_name: name.to_owned(),
      kind: UserKind::Member,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
      is_deactivated: false,
      is_deleted: false,
    }
  }

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
