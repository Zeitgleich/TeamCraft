//! History imported from another chat (e2ee.md §4.10, issue #402), against the fake server: the
//! import account posts encrypted history into a channel and a DM, members (also one who sets up
//! the app only afterwards) read it with its original author, and the import account leaves.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common;

use common::{Peer, settings, settle};
use teamcraft_core::ClientSettings;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::import::ImportDraft;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::imported::{ImportSource, ImportedAuthor, ImportedOrigin};
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter, MessageMeta};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{AdminSelfJoin, ConversationId, HistoryPolicy, ImportPolicy, UserId, WorkspacePolicy};

/// 2024-05-01.
const OLD_MS: i64 = 1_714_521_600_000;

#[tokio::test]
async fn a_channels_history_reaches_its_members_also_one_who_sets_up_the_app_later() {
  let server = FakeServer::new();
  let mut importer = Peer::enroll(&server, "Import").await;
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob_api = server.add_user("Bob");
  let bob_id = bob_api.user_id();
  let space = importer.space().await;
  let channel = importer.client.create_channel(space, "History").await.unwrap();

  let ids = importer
    .client
    .send_imported(
      channel,
      &[
        draft(
          "1",
          "first",
          ImportedAuthor::User {
            user_id: alice.client.user_id(),
          },
          OLD_MS,
        ),
        draft("2", "second", ImportedAuthor::Deleted, OLD_MS + 1),
      ],
    )
    .await
    .unwrap();
  assert!(ids[0] < ids[1]);
  // A second run sends the same IDs and stores nothing new.
  let again = importer
    .client
    .send_imported(channel, &[draft("1", "first", ImportedAuthor::Deleted, OLD_MS)])
    .await
    .unwrap();
  assert_eq!(again[0], ids[0]);

  for user in [alice.client.user_id(), bob_id] {
    importer
      .client
      .add_member_with_history(channel, user, Some(HistoryPolicy::Full))
      .await
      .unwrap();
  }
  settle(&mut [&mut importer, &mut alice]).await;
  alice.client.catch_up_messages(channel).await.unwrap();
  assert_eq!(alice.texts(channel), vec!["first", "second"]);
  assert_eq!(
    origin_of(&alice, channel, "first").author,
    ImportedAuthor::User {
      user_id: alice.client.user_id()
    }
  );

  // Bob sets up the app only now: the import device adds him with the history.
  let mut bob = Peer::enroll_as(bob_api, "Bob").await;
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  bob.client.full_sync().await.unwrap();
  bob.client.catch_up_messages(channel).await.unwrap();
  assert_eq!(bob.texts(channel), vec!["first", "second"]);
}

#[tokio::test]
async fn a_late_user_whose_device_also_asks_to_be_let_in_gets_the_history() {
  let server = FakeServer::new();
  let mut importer = Peer::enroll(&server, "Import").await;
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob_api = server.add_user("Bob");
  let bob_id = bob_api.user_id();
  let space = importer.space().await;
  let channel = importer.client.create_channel(space, "History").await.unwrap();
  importer
    .client
    .send_imported(
      channel,
      &[draft("1", "before Bob", ImportedAuthor::Deleted, OLD_MS)],
    )
    .await
    .unwrap();
  for user in [alice.client.user_id(), bob_id] {
    importer
      .client
      .add_member_with_history(channel, user, Some(HistoryPolicy::Full))
      .await
      .unwrap();
  }
  settle(&mut [&mut importer, &mut alice]).await;

  // Bob's first device syncs (and may ask to be let in) while Alice's device is online and the
  // import device is not.
  let mut bob = Peer::enroll_as(bob_api, "Bob").await;
  bob.client.full_sync().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  alice.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // The import device, which decided to share the history with him, adds him or, if somebody let
  // him in from the join on meanwhile, seals the history to his device.
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  bob.client.catch_up_messages(channel).await.unwrap();
  assert_eq!(bob.texts(channel), vec!["before Bob"]);
}

#[tokio::test]
async fn a_dm_is_founded_by_the_import_account_which_leaves_once_both_are_in() {
  let server = FakeServer::new();
  let mut importer = Peer::enroll(&server, "Import").await;
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let participants = vec![alice.client.user_id(), bob.client.user_id()];

  let response = importer
    .client
    .open_import_dm(participants.clone())
    .await
    .unwrap();
  let dm = response.id;
  // The tool records the participants as the DM's import grant before anything else.
  importer.client.grant_imported_history(dm, &participants).unwrap();
  assert!(
    importer
      .client
      .set_up_import_dm(response, &participants)
      .await
      .unwrap()
  );
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer
    .client
    .send_imported(
      dm,
      &[draft(
        "7",
        "hi from the old chat",
        ImportedAuthor::User {
          user_id: bob.client.user_id(),
        },
        OLD_MS,
      )],
    )
    .await
    .unwrap();
  for peer in [&mut alice, &mut bob] {
    peer.client.full_sync().await.unwrap();
    peer.client.catch_up_messages(dm).await.unwrap();
    assert_eq!(peer.texts(dm), vec!["hi from the old chat"], "{}", peer.name);
  }

  importer.client.leave_after_import(dm).await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  alice.client.apply_pending_changes().await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  let mut in_group = alice.client.group_users(dm).unwrap();
  in_group.sort();
  let mut expected = participants.clone();
  expected.sort();
  assert_eq!(in_group, expected, "only the two people are left in the group");
  // They still talk, and nobody wrote a goodbye.
  alice.client.send_text(dm, None, "still here").await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  assert_eq!(bob.texts(dm), vec!["hi from the old chat", "still here"]);
}

#[tokio::test]
async fn a_device_that_held_the_channel_gets_the_history_merged_into_it() {
  let server = FakeServer::new();
  let mut importer = Peer::enroll(&server, "Import").await;
  let mut alice = Peer::enroll(&server, "Alice").await;
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "Daily").await.unwrap();
  alice.client.send_text(channel, None, "live").await.unwrap();
  alice
    .client
    .add_member_with_history(channel, importer.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [&mut importer, &mut alice]).await;
  alice.client.catch_up_messages(channel).await.unwrap();
  assert_eq!(alice.texts(channel), vec!["live"]);

  // The mapping names Alice as a member of the channel in the old system.
  importer
    .client
    .name_import_readers(channel, &[alice.client.user_id()])
    .unwrap();
  importer
    .client
    .send_imported(channel, &[draft("1", "old", ImportedAuthor::Deleted, OLD_MS)])
    .await
    .unwrap();
  importer.client.finish_import(channel).await.unwrap();
  // The import sends no message events; the update that it finished brings the history.
  settle(&mut [&mut importer, &mut alice]).await;
  assert_eq!(alice.texts(channel), vec!["old", "live"]);
}

#[tokio::test]
async fn a_dm_its_participant_holds_alone_is_taken_over_by_the_import_accounts_group() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut importer = Peer::enroll(&server, "Import").await;
  let bob_api = server.add_user("Bob");
  let bob_id = bob_api.user_id();
  let (alice_id, importer_id) = (alice.client.user_id(), importer.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (importer_id, RosterRole::Admin)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(WorkspacePolicy {
      import: ImportPolicy {
        maybe_importer: Some(importer_id),
        maybe_joins_until_ms: Some(i64::MAX),
      },
      ..WorkspacePolicy::default()
    })
    .await
    .unwrap();
  // Alice wrote to Bob, who has no device yet: her device holds the DM's group alone.
  let dm = alice.client.open_dm(bob_id).await.unwrap();
  alice.client.send_text(dm, None, "live").await.unwrap();
  settle(&mut [&mut alice, &mut importer]).await;

  let participants = vec![alice_id, bob_id];
  let response = importer
    .client
    .open_import_dm(participants.clone())
    .await
    .unwrap();
  assert_eq!(response.id, dm);
  importer
    .client
    .set_up_import_dm(response, &participants)
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut importer]).await;
  importer
    .client
    .send_imported(dm, &[draft("1", "old", ImportedAuthor::Deleted, OLD_MS)])
    .await
    .unwrap();
  importer.client.finish_import(dm).await.unwrap();
  settle(&mut [&mut alice, &mut importer]).await;
  // Her device took the import account's group, so it reads what was imported.
  assert_eq!(alice.texts(dm), vec!["old", "live"]);

  // Bob sets up the app: he is added with all of it, also what Alice wrote before.
  let mut bob = Peer::enroll_as(bob_api, "Bob").await;
  for _ in 0..2 {
    settle(&mut [&mut alice, &mut importer, &mut bob]).await;
    importer.client.apply_pending_changes().await.unwrap();
    alice.client.apply_pending_changes().await.unwrap();
  }
  settle(&mut [&mut alice, &mut importer, &mut bob]).await;
  bob.client.full_sync().await.unwrap();
  bob.client.catch_up_messages(dm).await.unwrap();
  assert_eq!(bob.texts(dm), vec!["old", "live"]);
}

mod existing {
  //! A DM or notes that existed before the import: the import account, a member on the server
  //! already, asks to be let in, and a participant's device commits it (e2ee.md §4.10).
  use super::*;

  #[tokio::test]
  async fn a_participants_device_lets_the_import_account_into_an_existing_dm() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut importer = Peer::enroll(&server, "Import").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    sign_import_policy(&mut alice, &mut importer, AdminSelfJoin::Visible).await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    alice.client.send_text(dm, None, "live").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;

    let participants = vec![alice.client.user_id(), bob.client.user_id()];
    assert!(!open_existing(&mut importer, &participants, dm).await);
    importer.client.request_import_join(dm).await.unwrap();
    // Only Bob's device is online: either participant lets it in.
    settle(&mut [&mut bob, &mut importer]).await;
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    assert!(importer.client.has_group(dm).unwrap());

    import_one(&mut importer, dm).await;
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    for peer in [&mut alice, &mut bob] {
      peer.client.catch_up_messages(dm).await.unwrap();
      assert_eq!(peer.texts(dm), vec!["old", "live"], "{}", peer.name);
    }

    importer.client.leave_after_import(dm).await.unwrap();
    for _ in 0..2 {
      settle(&mut [&mut alice, &mut bob, &mut importer]).await;
      alice.client.apply_pending_changes().await.unwrap();
      importer.client.apply_pending_changes().await.unwrap();
    }
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    let mut in_group = alice.client.group_users(dm).unwrap();
    in_group.sort();
    let mut expected = participants.clone();
    expected.sort();
    assert_eq!(in_group, expected, "only the two people are left in the group");
  }

  #[tokio::test]
  async fn an_owners_device_lets_the_import_account_into_their_existing_notes() {
    let server = FakeServer::new();
    let mut phone = Peer::enroll_with_settings(&server, "Alice", settings().with_notes_to_self()).await;
    let mut importer = Peer::enroll(&server, "Import").await;
    sign_import_policy(&mut phone, &mut importer, AdminSelfJoin::Visible).await;
    let me = phone.client.user_id();
    let notes = phone.client.open_dm(me).await.unwrap();
    phone.client.send_text(notes, None, "buy milk").await.unwrap();
    // A second device of hers joins the notes, so their group is past epoch 0.
    let mut laptop = second_device(&mut phone).await;
    phone.client.full_sync().await.unwrap();
    phone.client.apply_pending_changes().await.unwrap();
    laptop.client.full_sync().await.unwrap();
    settle(&mut [&mut phone, &mut laptop, &mut importer]).await;

    assert!(!open_existing(&mut importer, &[me], notes).await);
    importer.client.request_import_join(notes).await.unwrap();
    settle(&mut [&mut laptop, &mut importer]).await;
    laptop.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut phone, &mut laptop, &mut importer]).await;
    assert!(importer.client.has_group(notes).unwrap());

    import_one(&mut importer, notes).await;
    settle(&mut [&mut phone, &mut laptop, &mut importer]).await;
    for peer in [&mut phone, &mut laptop] {
      peer.client.catch_up_messages(notes).await.unwrap();
      assert_eq!(peer.texts(notes), vec!["old", "buy milk"], "{}", peer.name);
    }
  }

  #[tokio::test]
  async fn without_visible_admin_self_joins_the_import_account_stays_out_and_waits() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut importer = Peer::enroll(&server, "Import").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    sign_import_policy(&mut alice, &mut importer, AdminSelfJoin::OwnerApproval).await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;

    let participants = vec![alice.client.user_id(), bob.client.user_id()];
    assert!(!open_existing(&mut importer, &participants, dm).await);
    importer.client.request_import_join(dm).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    alice.client.apply_pending_changes().await.unwrap();
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    assert!(!importer.client.has_group(dm).unwrap());
    let mut in_group = alice.client.group_users(dm).unwrap();
    in_group.sort();
    let mut expected = participants;
    expected.sort();
    assert_eq!(in_group, expected);
  }

  /// The feed shows only what the store holds. A device that had read the DM back to its
  /// beginning got one page of the import when it finished; the rest is read in the background
  /// until the oldest imported message is there. A store that already holds that one page (an
  /// older version marked the import as loaded) carries on the same way.
  #[tokio::test]
  async fn a_device_that_read_an_existing_dm_to_its_beginning_reads_all_of_its_import() {
    let server = FakeServer::new();
    let paged = ClientSettings {
      history_page_size: 2,
      imported_page_size: 2,
      ..settings()
    };
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut importer = Peer::enroll(&server, "Import").await;
    let mut bob = Peer::enroll_with_settings(&server, "Bob", paged).await;
    sign_import_policy(&mut alice, &mut importer, AdminSelfJoin::Visible).await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    alice.client.send_text(dm, None, "live").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    bob.client.catch_up_messages(dm).await.unwrap();
    assert_eq!(
      bob.client.load_older_messages(dm).await.unwrap(),
      0,
      "at the beginning"
    );

    let participants = vec![alice.client.user_id(), bob.client.user_id()];
    assert!(!open_existing(&mut importer, &participants, dm).await);
    importer.client.request_import_join(dm).await.unwrap();
    settle(&mut [&mut bob, &mut importer]).await;
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    let imported: Vec<String> = (0..7).map(|index| format!("old {index}")).collect();
    let drafts: Vec<ImportDraft> = imported
      .iter()
      .enumerate()
      .map(|(index, body)| {
        let offset = i64::try_from(index).unwrap();
        draft(&index.to_string(), body, ImportedAuthor::Deleted, OLD_MS + offset)
      })
      .collect();
    importer.client.send_imported(dm, &drafts).await.unwrap();
    importer.client.finish_import(dm).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut importer]).await;
    assert_eq!(bob.texts(dm), vec!["old 5", "old 6", "live"], "one page at once");

    let mut pages = 0;
    while bob.client.page_imported_history().await.unwrap() > 0 {
      pages += 1;
      assert!(pages < 10, "paging ends");
    }
    let mut expected = imported;
    expected.push("live".into());
    assert_eq!(bob.texts(dm), expected);
    // Done: nothing is read any more.
    assert_eq!(bob.client.page_imported_history().await.unwrap(), 0);
  }

  /// The import's DM or notes of `participants`, which is `existing`; `true` if the import device
  /// is in its group right away.
  async fn open_existing(importer: &mut Peer, participants: &[UserId], existing: ConversationId) -> bool {
    let response = importer
      .client
      .open_import_dm(participants.to_vec())
      .await
      .unwrap();
    assert_eq!(response.id, existing);
    importer
      .client
      .grant_imported_history(existing, participants)
      .unwrap();
    importer
      .client
      .set_up_import_dm(response, participants)
      .await
      .unwrap()
  }

  async fn import_one(importer: &mut Peer, conversation: ConversationId) {
    importer
      .client
      .send_imported(
        conversation,
        &[draft("1", "old", ImportedAuthor::Deleted, OLD_MS)],
      )
      .await
      .unwrap();
    importer.client.finish_import(conversation).await.unwrap();
  }

  /// The signed roster (`owner` owner, `importer` admin) and a policy naming `importer` as the
  /// import account, with admins' self-joins as `self_join` says.
  async fn sign_import_policy(owner: &mut Peer, importer: &mut Peer, self_join: AdminSelfJoin) {
    let (owner_id, importer_id) = (owner.client.user_id(), importer.client.user_id());
    owner
      .client
      .publish_roster(&[(owner_id, RosterRole::Owner), (importer_id, RosterRole::Admin)])
      .await
      .unwrap();
    owner
      .client
      .publish_policy(WorkspacePolicy {
        admin_self_join: self_join,
        import: ImportPolicy {
          maybe_importer: Some(importer_id),
          maybe_joins_until_ms: Some(i64::MAX),
        },
        ..WorkspacePolicy::default()
      })
      .await
      .unwrap();
    settle(&mut [&mut *owner, &mut *importer]).await;
    importer.client.full_sync().await.unwrap();
  }

  /// A further device of `owner`'s user, linked and connected.
  async fn second_device(owner: &mut Peer) -> Peer {
    let mut client = common::link_device(owner).await;
    let (gateway, _) = client.connect_gateway().await.unwrap();
    Peer {
      name: format!("{} laptop", owner.name),
      api: owner.api.clone(),
      store: client.store().clone(),
      client,
      gateway,
    }
  }
}

mod guests {
  //! Guests of the old system in what the import made (e2ee.md §4.10, "Guests in imported
  //! conversations", issue #1074), and who gets the import key (issue #1078).
  use super::*;
  use teamcraft_core::api::ServerApi;
  use teamcraft_core::imported::imported_message_id;
  use teamcraft_core::store::Table;
  use teamcraft_proto::{ListMessagesQuery, MessageResponse};

  #[tokio::test]
  async fn a_guest_the_import_adds_gets_the_history() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut guest = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
    let late_api = server.add_guest("Lea");
    let late_id = late_api.user_id();
    let channel = imported_channel(&mut importer, &[guest.client.user_id(), late_id]).await;

    for user in [guest.client.user_id(), late_id] {
      add_full(&mut importer, channel, user).await;
    }
    settle(&mut [&mut owner, &mut importer, &mut guest]).await;
    guest.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(guest.texts(channel), vec!["old"]);

    // Lea signs in only after the run: `serve` adds her with the history.
    let mut late = Peer::enroll_as(late_api, "Lea").await;
    join_late(&mut importer, &mut late).await;
    late.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(late.texts(channel), vec!["old"]);
  }

  #[tokio::test]
  async fn a_guest_somebody_else_adds_gets_no_history() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut guest = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
    let channel = imported_channel(&mut importer, &[owner.client.user_id(), guest.client.user_id()]).await;
    add_full(&mut importer, channel, owner.client.user_id()).await;
    settle(&mut [&mut owner, &mut importer]).await;

    // The owner's device adds the guest the grant names, asking for the full history.
    add_full(&mut owner, channel, guest.client.user_id()).await;
    settle(&mut [&mut owner, &mut importer, &mut guest]).await;
    guest.client.catch_up_messages(channel).await.unwrap();
    assert!(guest.texts(channel).is_empty(), "{:?}", guest.texts(channel));
  }

  #[tokio::test]
  async fn a_guest_the_mapping_does_not_name_gets_no_history_from_the_import() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut guest = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
    let channel = imported_channel(&mut importer, &[]).await;
    add_full(&mut importer, channel, guest.client.user_id()).await;
    settle(&mut [&mut owner, &mut importer, &mut guest]).await;
    guest.client.catch_up_messages(channel).await.unwrap();
    assert!(guest.texts(channel).is_empty(), "{:?}", guest.texts(channel));
  }

  #[tokio::test]
  async fn an_ended_or_missing_policy_shares_no_history_with_a_guest() {
    for maybe_until in [Some(1), None] {
      let server = FakeServer::new();
      let (mut owner, mut importer) = import_policy(&server, maybe_until).await;
      let mut guest = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
      let channel = imported_channel(&mut importer, &[guest.client.user_id()]).await;
      add_full(&mut importer, channel, guest.client.user_id()).await;
      settle(&mut [&mut owner, &mut importer, &mut guest]).await;
      guest.client.catch_up_messages(channel).await.unwrap();
      assert!(
        guest.texts(channel).is_empty(),
        "{maybe_until:?}: {:?}",
        guest.texts(channel)
      );
    }
  }

  #[tokio::test]
  async fn a_guests_addition_waiting_from_an_earlier_run_gets_the_history_once_granted() {
    let server = FakeServer::new();
    let (_owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let late_api = server.add_guest("Lea");
    let late_id = late_api.user_id();
    // A run before this version: no grant, so the addition waits with history from the join on.
    let channel = imported_channel(&mut importer, &[]).await;
    add_full(&mut importer, channel, late_id).await;

    // `serve` of this version records the grant before it applies the waiting additions.
    importer
      .client
      .grant_imported_history(channel, &[late_id])
      .unwrap();
    let mut late = Peer::enroll_as(late_api, "Lea").await;
    join_late(&mut importer, &mut late).await;
    late.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(late.texts(channel), vec!["old"]);
  }

  #[tokio::test]
  async fn a_member_switched_back_to_guest_keeps_the_history() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let channel = imported_channel(&mut importer, &[bob.client.user_id()]).await;
    add_full(&mut importer, channel, bob.client.user_id()).await;
    settle(&mut [&mut owner, &mut importer, &mut bob]).await;
    bob.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(bob.texts(channel), vec!["old"]);

    server.show_as_guest(bob.client.user_id());
    for _ in 0..2 {
      settle(&mut [&mut owner, &mut importer, &mut bob]).await;
      importer.client.apply_pending_changes().await.unwrap();
      bob.client.apply_pending_changes().await.unwrap();
    }
    bob.client.full_sync().await.unwrap();
    assert_eq!(bob.texts(channel), vec!["old"]);
    assert!(
      importer
        .client
        .group_users(channel)
        .unwrap()
        .contains(&bob.client.user_id())
    );
  }

  #[tokio::test]
  async fn a_guests_device_leaves_the_import_accounts_leave_to_the_members() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut guest = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
    let participants = vec![owner.client.user_id(), guest.client.user_id()];
    let response = importer
      .client
      .open_import_dm(participants.clone())
      .await
      .unwrap();
    let dm = response.id;
    importer.client.grant_imported_history(dm, &participants).unwrap();
    importer
      .client
      .set_up_import_dm(response, &participants)
      .await
      .unwrap();
    settle(&mut [&mut owner, &mut importer, &mut guest]).await;
    importer
      .client
      .send_imported(dm, &[draft("1", "old", ImportedAuthor::Deleted, OLD_MS)])
      .await
      .unwrap();
    importer.client.finish_import(dm).await.unwrap();

    // The guest's device sees the signed leave first; only the owner's device may commit it.
    importer.client.leave_after_import(dm).await.unwrap();
    for _ in 0..2 {
      settle(&mut [&mut importer, &mut guest]).await;
      guest.client.apply_pending_changes().await.unwrap();
      settle(&mut [&mut owner, &mut importer, &mut guest]).await;
      owner.client.apply_pending_changes().await.unwrap();
    }
    settle(&mut [&mut owner, &mut importer, &mut guest]).await;
    let mut in_group = owner.client.group_users(dm).unwrap();
    in_group.sort();
    let mut expected = participants.clone();
    expected.sort();
    assert_eq!(in_group, expected);
    guest.client.full_sync().await.unwrap();
    guest.client.catch_up_messages(dm).await.unwrap();
    guest.client.send_text(dm, None, "still one group").await.unwrap();
    settle(&mut [&mut owner, &mut guest]).await;
    assert_eq!(owner.texts(dm), vec!["old", "still one group"]);
    assert_eq!(guest.texts(dm), vec!["old", "still one group"]);
  }

  #[tokio::test]
  async fn the_import_account_leaves_a_guests_notes_and_a_dm_of_guests() {
    let server = FakeServer::new();
    let (_owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut gus = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
    let mut gil = Peer::enroll_as(server.add_guest("Gil"), "Gil").await;
    let notes = import_dm(&mut importer, &[gus.client.user_id()]).await;
    let dm = import_dm(&mut importer, &[gus.client.user_id(), gil.client.user_id()]).await;
    settle(&mut [&mut importer, &mut gus, &mut gil]).await;

    for conversation in [notes, dm] {
      importer.client.leave_after_import(conversation).await.unwrap();
    }
    for _ in 0..2 {
      settle(&mut [&mut importer, &mut gus, &mut gil]).await;
      gus.client.apply_pending_changes().await.unwrap();
      gil.client.apply_pending_changes().await.unwrap();
    }
    settle(&mut [&mut importer, &mut gus, &mut gil]).await;
    assert_eq!(gus.client.group_users(notes).unwrap(), vec![gus.client.user_id()]);
    let mut in_dm = gil.client.group_users(dm).unwrap();
    in_dm.sort();
    let mut expected = vec![gus.client.user_id(), gil.client.user_id()];
    expected.sort();
    assert_eq!(in_dm, expected, "the import account left the DM of guests");
    gus.client.catch_up_messages(dm).await.unwrap();
    gus.client.send_text(dm, None, "still one group").await.unwrap();
    settle(&mut [&mut gus, &mut gil]).await;
    gil.client.catch_up_messages(dm).await.unwrap();
    assert_eq!(gil.texts(dm), vec!["old", "still one group"]);
  }

  #[tokio::test]
  async fn a_late_guest_gets_the_import_but_not_what_members_wrote_before_their_join() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let late_api = server.add_guest("Lea");
    let late_id = late_api.user_id();
    let channel = imported_channel(&mut importer, &[owner.client.user_id(), late_id]).await;
    add_full(&mut importer, channel, owner.client.user_id()).await;
    add_full(&mut importer, channel, late_id).await;
    settle(&mut [&mut owner, &mut importer]).await;
    owner
      .client
      .send_text(channel, None, "live before Lea")
      .await
      .unwrap();
    settle(&mut [&mut owner, &mut importer]).await;

    let mut late = Peer::enroll_as(late_api, "Lea").await;
    join_late(&mut importer, &mut late).await;
    late.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(late.texts(channel), vec!["old"]);
    owner.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(owner.texts(channel), vec!["old", "live before Lea"]);
  }

  /// Issue #1078: a member in the group while the import runs writes under the group's key, never
  /// under the import key, so a guest the grant names gets the import and not that message.
  #[tokio::test]
  async fn a_late_guest_gets_no_message_members_wrote_during_the_import() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let late_api = server.add_guest("Lea");
    let late_id = late_api.user_id();
    let space = importer.space().await;
    let channel = importer.client.create_channel(space, "History").await.unwrap();
    importer
      .client
      .grant_imported_history(channel, &[owner.client.user_id(), late_id])
      .unwrap();
    importer
      .client
      .send_imported(channel, &[draft("1", "old 1", ImportedAuthor::Deleted, OLD_MS)])
      .await
      .unwrap();
    add_full(&mut importer, channel, owner.client.user_id()).await;
    settle(&mut [&mut owner, &mut importer]).await;
    owner
      .client
      .send_text(channel, None, "live during the import")
      .await
      .unwrap();
    settle(&mut [&mut owner, &mut importer]).await;
    importer
      .client
      .send_imported(
        channel,
        &[draft("2", "old 2", ImportedAuthor::Deleted, OLD_MS + 1)],
      )
      .await
      .unwrap();
    importer.client.finish_import(channel).await.unwrap();
    add_full(&mut importer, channel, late_id).await;
    settle(&mut [&mut owner, &mut importer]).await;

    let mut late = Peer::enroll_as(late_api, "Lea").await;
    join_late(&mut importer, &mut late).await;
    late.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(late.texts(channel), vec!["old 1", "old 2"]);
    owner.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(
      owner.texts(channel),
      vec!["old 1", "old 2", "live during the import"]
    );
  }

  #[tokio::test]
  async fn a_renewed_policy_needs_the_grant_again() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let late_api = server.add_guest("Lea");
    let late_id = late_api.user_id();
    let channel = imported_channel(&mut importer, &[late_id]).await;
    add_full(&mut importer, channel, late_id).await;
    // The owners sign the import period again, with another date.
    owner
      .client
      .publish_policy(WorkspacePolicy {
        import: ImportPolicy {
          maybe_importer: Some(importer.client.user_id()),
          maybe_joins_until_ms: Some(i64::MAX - 1),
        },
        ..WorkspacePolicy::default()
      })
      .await
      .unwrap();
    settle(&mut [&mut owner, &mut importer]).await;
    importer.client.full_sync().await.unwrap();

    let mut late = Peer::enroll_as(late_api, "Lea").await;
    join_late(&mut importer, &mut late).await;
    late.client.catch_up_messages(channel).await.unwrap();
    assert!(late.texts(channel).is_empty(), "{:?}", late.texts(channel));
  }

  #[tokio::test]
  async fn a_stale_workspace_state_shares_no_history_with_a_guest() {
    let server = FakeServer::new();
    let mut owner = Peer::enroll(&server, "Owner").await;
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut importer = Peer::enroll_on_clock(
      server.add_user("Import"),
      "Import",
      settings(),
      Box::new(clock.clone()),
    )
    .await;
    let mut guest = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
    sign_import_policy(&mut owner, &mut importer, Some(i64::MAX)).await;
    let channel = imported_channel(&mut importer, &[guest.client.user_id()]).await;
    // Nothing newer from the owners for longer than `workspace_max_age`: maybe withheld.
    clock.advance(31 * 24 * 60 * 60 * 1000);
    add_full(&mut importer, channel, guest.client.user_id()).await;
    settle(&mut [&mut owner, &mut importer, &mut guest]).await;
    guest.client.catch_up_messages(channel).await.unwrap();
    assert!(guest.texts(channel).is_empty(), "{:?}", guest.texts(channel));
  }

  /// A channel merged into: the import key goes to the members the mapping names, not to a member
  /// it doesn't name (the server's word that somebody is no guest is not enough) nor to a guest
  /// member, and members writing during the import write under the group's key.
  #[tokio::test]
  async fn a_merged_channel_seals_the_import_key_only_to_members_the_mapping_names() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut mallory = Peer::enroll(&server, "Mallory").await;
    let mut gus = Peer::enroll_as(server.add_guest("Gus"), "Gus").await;
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "Daily").await.unwrap();
    for (user, history) in [
      (importer.client.user_id(), HistoryPolicy::FromJoin),
      (mallory.client.user_id(), HistoryPolicy::Full),
      (gus.client.user_id(), HistoryPolicy::FromJoin),
    ] {
      add_with(&mut alice, channel, user, history).await;
    }
    settle(&mut [&mut alice, &mut importer, &mut mallory, &mut gus]).await;
    alice.client.send_text(channel, None, "before").await.unwrap();
    settle(&mut [&mut alice, &mut importer, &mut mallory, &mut gus]).await;

    importer
      .client
      .name_import_readers(channel, &[alice.client.user_id(), gus.client.user_id()])
      .unwrap();
    import_batch(&mut importer, channel, "1", "old 1").await;
    settle(&mut [&mut alice, &mut importer, &mut mallory, &mut gus]).await;
    alice.client.send_text(channel, None, "during").await.unwrap();
    settle(&mut [&mut alice, &mut importer, &mut mallory, &mut gus]).await;
    import_batch(&mut importer, channel, "2", "old 2").await;
    importer.client.finish_import(channel).await.unwrap();
    settle(&mut [&mut owner, &mut alice, &mut importer, &mut mallory, &mut gus]).await;

    let keys = import_keys_on_server(&alice, channel).await;
    assert_eq!(keys.len(), 1, "one import key");
    let live = live_keys_on_server(&alice, channel).await;
    assert!(
      live.iter().all(|key| !keys.contains(key)),
      "nobody wrote under the import key"
    );
    assert!(alice.client.has_content_key(channel, &keys[0]));
    for peer in [&mallory, &gus] {
      assert!(!peer.client.has_content_key(channel, &keys[0]), "{}", peer.name);
    }
    alice.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(alice.texts(channel), vec!["old 1", "old 2", "before", "during"]);
  }

  #[tokio::test]
  async fn a_member_removed_during_the_import_gets_no_later_import_key() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let space = importer.space().await;
    let channel = importer.client.create_channel(space, "History").await.unwrap();
    importer
      .client
      .grant_imported_history(channel, &[alice.client.user_id(), bob.client.user_id()])
      .unwrap();
    import_batch(&mut importer, channel, "1", "old 1").await;
    for user in [alice.client.user_id(), bob.client.user_id()] {
      add_full(&mut importer, channel, user).await;
    }
    settle(&mut [&mut importer, &mut alice, &mut bob]).await;
    importer
      .client
      .remove_member(channel, bob.client.user_id())
      .await
      .unwrap();
    for _ in 0..2 {
      settle(&mut [&mut importer, &mut alice, &mut bob]).await;
      importer.client.apply_pending_changes().await.unwrap();
    }
    assert!(
      !importer
        .client
        .group_users(channel)
        .unwrap()
        .contains(&bob.client.user_id())
    );
    import_batch(&mut importer, channel, "2", "old 2").await;
    importer.client.finish_import(channel).await.unwrap();
    settle(&mut [&mut owner, &mut importer, &mut alice, &mut bob]).await;

    let keys = import_keys_on_server(&alice, channel).await;
    assert_eq!(keys.len(), 2, "a new import key after the removal");
    assert!(alice.client.has_content_key(channel, &keys[1]));
    assert!(bob.client.has_content_key(channel, &keys[0]));
    assert!(!bob.client.has_content_key(channel, &keys[1]));
    alice.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(alice.texts(channel), vec!["old 1", "old 2"]);
  }

  #[tokio::test]
  async fn a_member_who_joins_after_the_last_batch_gets_the_import_key_on_the_next_pass() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "Daily").await.unwrap();
    add_with(
      &mut alice,
      channel,
      importer.client.user_id(),
      HistoryPolicy::FromJoin,
    )
    .await;
    settle(&mut [&mut alice, &mut importer]).await;
    importer
      .client
      .name_import_readers(channel, &[alice.client.user_id(), carol.client.user_id()])
      .unwrap();
    import_batch(&mut importer, channel, "1", "old").await;
    importer.client.finish_import(channel).await.unwrap();
    settle(&mut [&mut owner, &mut alice, &mut importer]).await;

    add_with(
      &mut alice,
      channel,
      carol.client.user_id(),
      HistoryPolicy::FromJoin,
    )
    .await;
    settle(&mut [&mut alice, &mut importer, &mut carol]).await;
    let keys = import_keys_on_server(&alice, channel).await;
    assert!(!carol.client.has_content_key(channel, &keys[0]));
    // `serve`'s next pass.
    importer.client.share_import_key(channel).await.unwrap();
    settle(&mut [&mut alice, &mut importer, &mut carol]).await;
    assert!(carol.client.has_content_key(channel, &keys[0]));
  }

  /// A record of an earlier version that noted every key of the ring, the live one included, is
  /// not read: a guest gets the import and nothing members wrote.
  #[tokio::test]
  async fn a_leftover_note_of_an_earlier_version_gives_a_guest_nothing_live() {
    let server = FakeServer::new();
    let (mut owner, mut importer) = import_policy(&server, Some(i64::MAX)).await;
    let late_api = server.add_guest("Lea");
    let late_id = late_api.user_id();
    let channel = imported_channel(&mut importer, &[owner.client.user_id(), late_id]).await;
    add_full(&mut importer, channel, owner.client.user_id()).await;
    settle(&mut [&mut owner, &mut importer]).await;
    owner.client.send_text(channel, None, "live").await.unwrap();
    settle(&mut [&mut owner, &mut importer]).await;
    let mut noted: Vec<String> = live_keys_on_server(&owner, channel)
      .await
      .iter()
      .chain(import_keys_on_server(&owner, channel).await.iter())
      .map(|key| key.iter().map(|byte| format!("{byte:02x}")).collect())
      .collect();
    noted.dedup();
    importer
      .store
      .put(
        Table::SyncState,
        &format!("import_keys/{channel}"),
        &serde_json::to_vec(&noted).unwrap(),
      )
      .unwrap();
    // `serve`'s next pass, then Lea's waiting addition.
    importer.client.share_import_key(channel).await.unwrap();
    add_full(&mut importer, channel, late_id).await;
    let mut late = Peer::enroll_as(late_api, "Lea").await;
    join_late(&mut importer, &mut late).await;
    late.client.catch_up_messages(channel).await.unwrap();
    assert_eq!(late.texts(channel), vec!["old"]);
  }

  /// An owner and the import account, with the signed roster and the import policy
  /// (`joins_until_ms`; `None`: no policy at all).
  async fn import_policy(server: &FakeServer, maybe_until: Option<i64>) -> (Peer, Peer) {
    let mut owner = Peer::enroll(server, "Owner").await;
    let mut importer = Peer::enroll(server, "Import").await;
    sign_import_policy(&mut owner, &mut importer, maybe_until).await;
    (owner, importer)
  }

  /// The signed roster (`owner` owner, `importer` admin) and the import policy naming `importer`.
  async fn sign_import_policy(owner: &mut Peer, importer: &mut Peer, maybe_until: Option<i64>) {
    let (owner_id, importer_id) = (owner.client.user_id(), importer.client.user_id());
    owner
      .client
      .publish_roster(&[(owner_id, RosterRole::Owner), (importer_id, RosterRole::Admin)])
      .await
      .unwrap();
    if maybe_until.is_some() {
      owner
        .client
        .publish_policy(WorkspacePolicy {
          import: ImportPolicy {
            maybe_importer: Some(importer_id),
            maybe_joins_until_ms: maybe_until,
          },
          ..WorkspacePolicy::default()
        })
        .await
        .unwrap();
    }
    settle(&mut [&mut *owner, &mut *importer]).await;
    importer.client.full_sync().await.unwrap();
  }

  /// A channel the import made, with one imported message, and the grant for `granted`.
  async fn imported_channel(importer: &mut Peer, granted: &[UserId]) -> ConversationId {
    let space = importer.space().await;
    let channel = importer.client.create_channel(space, "History").await.unwrap();
    if !granted.is_empty() {
      importer.client.grant_imported_history(channel, granted).unwrap();
    }
    importer
      .client
      .send_imported(channel, &[draft("1", "old", ImportedAuthor::Deleted, OLD_MS)])
      .await
      .unwrap();
    importer.client.finish_import(channel).await.unwrap();
    channel
  }

  /// A DM (or one person's notes) the import made for `participants`, with one imported message.
  async fn import_dm(importer: &mut Peer, participants: &[UserId]) -> ConversationId {
    let response = importer
      .client
      .open_import_dm(participants.to_vec())
      .await
      .unwrap();
    let dm = response.id;
    importer.client.grant_imported_history(dm, participants).unwrap();
    importer
      .client
      .set_up_import_dm(response, participants)
      .await
      .unwrap();
    importer
      .client
      .send_imported(dm, &[draft("1", "old", ImportedAuthor::Deleted, OLD_MS)])
      .await
      .unwrap();
    importer.client.finish_import(dm).await.unwrap();
    dm
  }

  async fn add_full(adder: &mut Peer, channel: ConversationId, user: UserId) {
    adder
      .client
      .add_member_with_history(channel, user, Some(HistoryPolicy::Full))
      .await
      .unwrap();
  }

  async fn add_with(adder: &mut Peer, channel: ConversationId, user: UserId, history: HistoryPolicy) {
    adder
      .client
      .add_member_with_history(channel, user, Some(history))
      .await
      .unwrap();
  }

  async fn import_batch(importer: &mut Peer, channel: ConversationId, source_id: &str, body: &str) {
    importer
      .client
      .send_imported(
        channel,
        &[draft(source_id, body, ImportedAuthor::Deleted, OLD_MS)],
      )
      .await
      .unwrap();
  }

  /// The distinct content keys of the imported messages (source IDs "1" and "2"), oldest first, as
  /// the server lists them to `reader`.
  async fn import_keys_on_server(reader: &Peer, channel: ConversationId) -> Vec<Vec<u8>> {
    keys_on_server(reader, channel, |message| is_imported(channel, message)).await
  }

  /// The distinct content keys of every other message, as the server lists them to `reader`.
  async fn live_keys_on_server(reader: &Peer, channel: ConversationId) -> Vec<Vec<u8>> {
    keys_on_server(reader, channel, |message| !is_imported(channel, message)).await
  }

  fn is_imported(channel: ConversationId, message: &MessageResponse) -> bool {
    ["1", "2"].iter().any(|source_id| {
      message.client_message_id == imported_message_id(channel, ImportSource::Synology, source_id)
    })
  }

  async fn keys_on_server(
    reader: &Peer,
    channel: ConversationId,
    filter: impl Fn(&MessageResponse) -> bool,
  ) -> Vec<Vec<u8>> {
    let mut page = reader
      .api
      .list_messages(channel, &ListMessagesQuery::default())
      .await
      .unwrap()
      .messages;
    page.sort_by_key(|message| message.id);
    let mut keys: Vec<Vec<u8>> = Vec::new();
    for message in page.iter().filter(|message| filter(message)) {
      let key = message.content_key_id.as_slice().to_vec();
      if !keys.contains(&key) {
        keys.push(key);
      }
    }
    keys
  }

  /// `late` set up the app after the run; `serve` applies the waiting additions.
  async fn join_late(importer: &mut Peer, late: &mut Peer) {
    for _ in 0..2 {
      settle(&mut [&mut *importer, &mut *late]).await;
      importer.client.apply_pending_changes().await.unwrap();
    }
    settle(&mut [&mut *importer, &mut *late]).await;
    late.client.full_sync().await.unwrap();
  }
}

fn draft(source_id: &str, body: &str, author: ImportedAuthor, sent_at_ms: i64) -> ImportDraft {
  ImportDraft {
    content: MessageContent::Text {
      body: body.into(),
      mentions: Vec::new(),
      meta: MessageMeta {
        maybe_imported: Some(Box::new(ImportedOrigin {
          source: ImportSource::Synology,
          source_id: source_id.into(),
          author,
          sent_at_ms,
          maybe_edited_at_ms: None,
          reactions: Vec::new(),
          missing_files: Vec::new(),
        })),
        ..MessageMeta::default()
      },
    },
    maybe_thread_root: None,
  }
}

fn origin_of(peer: &Peer, conversation: ConversationId, body: &str) -> ImportedOrigin {
  peer
    .store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .find_map(|message| match message.content {
      MessageContent::Text { body: text, meta, .. } if text == body => {
        meta.maybe_imported.map(|origin| *origin)
      },
      _ => None,
    })
    .unwrap()
}
