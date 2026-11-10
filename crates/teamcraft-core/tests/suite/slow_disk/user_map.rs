//! The decoded users kept in memory (issue #1682) over the store as the app opens it: the engine
//! writes through the writing connection while two read threads look people up through their
//! own connections and share one directory. A read thread must never show a name older than the
//! last commit it can see, and a group of writes the engine still holds open must not leave an
//! old name kept once it commits.

use std::sync::{Arc, Barrier};

use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::records;
use teamcraft_core::store::{LaneStore, LocalStore, Table, TrackedStore, write_group};
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_core::user_directory::UserDirectory;
use teamcraft_proto::{Id, Snowflake, UserId, UserKind, UserSummary};

use super::{PHONE_SYNC_DELAY, SlowStore};

/// A workspace of this many people: enough that a pass over them is real work.
const PEOPLE: i64 = 500;

#[test]
fn a_rename_reaches_both_read_threads_once_the_engine_commits_it() {
  let store = SlowStore::open(PHONE_SYNC_DELAY);
  let engine = TrackedStore::new(LaneStore::writer(Arc::clone(&store.writer)));
  write_group(&engine, || {
    for raw in 1..=PEOPLE {
      put_user(&engine, raw, &format!("Person {raw}"));
    }
    Ok::<_, teamcraft_core::CoreError>(())
  })
  .unwrap();
  let users = UserDirectory::new();
  let main = backend(engine.sharing(store.lane.clone()), &users);
  let query = backend(
    engine.sharing(LaneStore::reader(
      Arc::clone(&store.writer),
      Arc::clone(&store.second_reader),
    )),
    &users,
  );
  // The cold start: the snapshot on the main thread, a chat's authors on the query thread.
  assert_eq!(main.me().display_name, "Person 1");
  assert_eq!(name(&query, 2), "Person 2");

  // The engine holds a group open with the rename while the read threads look.
  let renamed = Barrier::new(2);
  let looked = Barrier::new(2);
  std::thread::scope(|scope| {
    scope.spawn(|| {
      write_group(&engine, || {
        put_user(&engine, 2, "Person Renamed");
        renamed.wait();
        looked.wait();
        Ok::<_, teamcraft_core::CoreError>(())
      })
      .unwrap();
    });
    renamed.wait();
    assert_eq!(name(&main, 2), "Person 2", "not committed yet");
    assert_eq!(name(&query, 2), "Person 2", "not committed yet");
    looked.wait();
  });

  assert_eq!(name(&main, 2), "Person Renamed");
  assert_eq!(name(&query, 2), "Person Renamed");
}

fn backend<S: LocalStore>(store: S, users: &Arc<UserDirectory>) -> CoreChatBackend<S> {
  CoreChatBackend::new(store, id(1), Box::new(ManualClock::new(0))).with_user_directory(Arc::clone(users))
}

fn name<S: LocalStore>(backend: &CoreChatBackend<S>, raw: i64) -> String {
  backend.user(id(raw)).unwrap().display_name
}

fn put_user(store: &impl LocalStore, raw: i64, name: &str) {
  let summary = UserSummary {
    id: id(raw),
    maybe_username: None,
    display_name: name.to_owned(),
    kind: UserKind::Member,
    maybe_email: None,
    maybe_organization: None,
    maybe_has_devices: None,
    is_deactivated: false,
    is_deleted: false,
  };
  records::put_json(store, Table::Users, &raw.to_string(), &summary).unwrap();
}

fn id(raw: i64) -> UserId {
  UserId::from_snowflake(Snowflake::new(raw).unwrap())
}
