//! Finding people who share nothing with the user yet, and a DM with somebody without a device.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::time::Duration;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::{UserAddWait, pending_user_additions};

#[tokio::test]
async fn a_dm_with_somebody_without_a_device_adds_them_when_their_first_device_appears() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob_api = server.add_user("Bob");
  let bob_id = bob_api.user_id();

  // Bob hasn't set up the app: the DM is made anyway and waits for his device.
  let dm = alice.client.open_dm(bob_id).await.unwrap();
  assert_eq!(
    pending_user_additions(alice.store.as_ref(), dm).unwrap(),
    vec![(bob_id, UserAddWait::NoDevice)]
  );
  alice.client.send_text(dm, None, "welcome Bob").await.unwrap();
  // Waiting costs nothing: the queue keeps the addition without asking the server.
  alice.client.apply_pending_changes().await.unwrap();
  assert_eq!(pending_user_additions(alice.store.as_ref(), dm).unwrap().len(), 1);

  let mut bob = Peer::enroll_as(bob_api, "Bob").await;
  settle(&mut [&mut alice, &mut bob]).await;
  alice.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  assert!(
    pending_user_additions(alice.store.as_ref(), dm)
      .unwrap()
      .is_empty()
  );
  alice.client.send_text(dm, None, "now you are in").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(bob.texts(dm), vec!["welcome Bob", "now you are in"]);
}

#[tokio::test]
async fn a_first_device_missed_while_offline_is_found_by_the_next_full_sync() {
  let server = FakeServer::new();
  // Checked on every full sync (the test's clock stands still).
  let no_interval = teamcraft_core::ClientSettings {
    rejoin_retry: Duration::ZERO,
    ..common::settings()
  };
  let mut alice = Peer::enroll_with_settings(&server, "Alice", no_interval).await;
  let bob_api = server.add_user("Bob");
  let bob_id = bob_api.user_id();
  let dm = alice.client.open_dm(bob_id).await.unwrap();
  // Nothing to find yet: the check costs one device list and wakes nobody.
  assert_eq!(alice.client.recheck_waiting_additions().await.unwrap(), 0);

  // Bob sets up the app while Alice's gateway is gone beyond its resume window.
  server.drop_gateways(alice.client.user_id());
  let mut bob = Peer::enroll_as(bob_api, "Bob").await;

  // The next full sync finds Bob's device without the missed event and adds him.
  alice.client.full_sync().await.unwrap();
  assert!(
    pending_user_additions(alice.store.as_ref(), dm)
      .unwrap()
      .is_empty()
  );
  bob.client.full_sync().await.unwrap();
  assert!(bob.client.has_group(dm).unwrap());
}

#[tokio::test]
async fn waiting_people_are_checked_again_at_most_once_per_interval() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob_api = server.add_user("Bob");
  let dm = alice.client.open_dm(bob_api.user_id()).await.unwrap();
  assert_eq!(alice.client.recheck_waiting_additions().await.unwrap(), 0);
  server.drop_gateways(alice.client.user_id());
  let _bob = Peer::enroll_as(bob_api, "Bob").await;

  // Within `rejoin_retry` of the last check nothing is asked, so Bob still waits.
  assert_eq!(alice.client.recheck_waiting_additions().await.unwrap(), 0);
  assert_eq!(pending_user_additions(alice.store.as_ref(), dm).unwrap().len(), 1);
}

#[tokio::test]
async fn a_dm_with_somebody_who_has_devices_needs_no_waiting() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;

  let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
  assert!(
    pending_user_additions(alice.store.as_ref(), dm)
      .unwrap()
      .is_empty()
  );
}

#[tokio::test]
async fn a_member_out_of_key_packages_waits_and_says_so() {
  let server = FakeServer::new();
  // Retried right away instead of after the stagger (the test's clock stands still).
  let no_stagger = teamcraft_core::ClientSettings {
    device_add_stagger: Duration::ZERO,
    ..common::settings()
  };
  let mut alice = Peer::enroll_with_settings(&server, "Alice", no_stagger).await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let bob_id = bob.client.user_id();
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  // Everything Bob uploaded at enrollment has expired, and his app hasn't run since.
  server.advance_clock(Duration::from_secs(100 * 24 * 60 * 60));

  alice.client.add_member(channel, bob_id).await.unwrap();
  assert_eq!(
    pending_user_additions(alice.store.as_ref(), channel).unwrap(),
    vec![(bob_id, UserAddWait::NoKeyPackage)]
  );

  // Bob's app runs again and renews them; the retry adds him.
  bob.client.top_up_key_packages().await.unwrap();
  alice.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(
    pending_user_additions(alice.store.as_ref(), channel)
      .unwrap()
      .is_empty()
  );
  assert!(bob.client.has_group(channel).unwrap());
}

#[tokio::test]
async fn a_dm_with_somebody_out_of_key_packages_is_left_to_the_dm_setup() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  server.advance_clock(Duration::from_secs(100 * 24 * 60 * 60));

  // The DM's own setup record retries it (with the founding-race rules); no second mechanism.
  let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
  assert!(
    pending_user_additions(alice.store.as_ref(), dm)
      .unwrap()
      .is_empty()
  );
}

/// The people page and the search find colleagues through the server's directory.
#[cfg(feature = "ui")]
mod directory_search {
  use teamcraft_client_api::backend::ChatBackend;
  use teamcraft_client_api::model::DeviceSetup;
  use teamcraft_core::ui_backend::CoreCommand;

  use super::*;
  use crate::common::{backend_of, run_commands};

  #[tokio::test]
  async fn a_colleague_who_shares_nothing_is_found_and_kept() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let jan_id = server.add_user("Jan").user_id();
    let ui = backend_of(&alice);

    // Before the server answered: nothing known yet, and the search is asked for.
    assert!(ui.search_people("jan").is_empty());
    run_commands(&mut alice, &ui).await;
    assert_eq!(server.directory_search_count(), 1);

    let found = ui.search_people("JAN");
    assert_eq!(found.iter().map(|user| user.id).collect::<Vec<_>>(), vec![jan_id]);
    assert_eq!(found[0].device_setup, DeviceSetup::NoDevices);
    // Cached for the people page, also offline; the lists of the user's own people don't grow.
    assert!(ui.search_people("").iter().any(|user| user.id == jan_id));
    assert!(ui.users().iter().all(|user| user.id != jan_id));
  }

  #[tokio::test]
  async fn the_same_query_is_not_asked_again_while_fresh() {
    let server = FakeServer::new();
    let alice = Peer::enroll(&server, "Alice").await;
    let ui = backend_of(&alice);

    ui.search_people("jan");
    ui.search_people(" Jan ");
    ui.search_people("ja");
    let searches: Vec<CoreCommand> = ui
      .drain_commands()
      .into_iter()
      .filter(|command| matches!(command, CoreCommand::SearchDirectory { .. }))
      .collect();
    assert_eq!(
      searches,
      vec![
        CoreCommand::SearchDirectory { query: "jan".into() },
        CoreCommand::SearchDirectory { query: "ja".into() },
      ]
    );
  }

  #[tokio::test]
  async fn a_member_list_keeps_what_the_directory_said_about_devices() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let bob = Peer::enroll(&server, "Bob").await;
    let ui = backend_of(&alice);
    ui.search_people("bob");
    run_commands(&mut alice, &ui).await;

    // The DM's member list doesn't say whether Bob has a device; the directory did.
    alice.client.open_dm(bob.client.user_id()).await.unwrap();
    alice.client.full_sync().await.unwrap();
    let found = ui.search_people("bob");
    assert_eq!(found.len(), 1, "Bob is listed once");
    assert_eq!(found[0].device_setup, DeviceSetup::HasDevices);
  }
}
