//! The seed users' devices kept online on the demo instance for store reviews (docs/operations.md,
//! "Demo instance"): a reviewer's new device restores the account from the key backup, which lets
//! it read, but only a device already in a conversation's encryption group lets it in, so that it
//! can write (docs/e2ee.md §4.6, "a further device"; a device that recovered with the passphrase
//! never joins on its own). Nobody real is ever online on the demo, so `demo-peers` runs the seed's
//! devices (owner, alex, sam, robin) from the stores the seeding run left ([`crate::demo::seed_store`]).
//!
//! Each device runs on the apps' own sync runner ([`SyncRunner`], as the desktop app does): one
//! gateway connection with heartbeats, reconnects with backoff, and the admissions, add-backs and
//! current-key hand-overs the core makes on `device_add` events and device requests. Nothing polls
//! the server; the gateway pushes events. On top, a device answers a direct message from somebody
//! outside the seed once, so a reviewer sees a message come back.
//!
//! The runner is not `Send`: everything here runs on a `tokio::task::LocalSet`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use teamcraft_core::api::HttpServerApi;
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::records::{self, Draft};
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table};
use teamcraft_core::{Client, ClientSettings};
use teamcraft_proto::{ConversationId, ConversationKind, ConversationResponse, MessageId, UserId};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::demo::{SeedStore, seed_store};
use crate::tokens::UserTokens;

/// What a seed device answers to a direct message from a reviewer.
pub const AUTO_REPLY: &str =
  "Thanks for your message! This is an automatic reply: the people in this demo workspace aren't real.";

/// One seed device online.
pub struct DemoPeer {
  handle: RunnerHandle,
  runner: JoinHandle<()>,
  replier: JoinHandle<()>,
}

/// A seed device opened from its store, not running yet.
struct Opened {
  store: SeedStore,
  client: Client<HttpServerApi<UserTokens>, SeedStore>,
}

/// Opens the seed devices of `logins` from `stores` and starts them against `base_url`. Panics if a
/// store is missing or not enrolled: the peers are only of use with the seed's devices.
pub fn start_all(base_url: &str, stores: &Path, logins: &[&str]) -> Vec<DemoPeer> {
  let opened: Vec<Opened> = logins.iter().map(|login| open(base_url, stores, login)).collect();
  // A device answers people outside the seed only, so two seed devices never answer each other.
  let seed_users: Rc<HashSet<UserId>> = Rc::new(opened.iter().map(|peer| peer.client.user_id()).collect());
  opened
    .into_iter()
    .map(|peer| DemoPeer::start(peer, seed_users.clone()))
    .collect()
}

fn open(base_url: &str, stores: &Path, login: &str) -> Opened {
  let store = seed_store(stores, login);
  let api = HttpServerApi::new(base_url, UserTokens::new(login));
  let client = Client::open(
    api,
    store.clone(),
    Box::new(SystemClock),
    // As every app has it (issue #1457).
    ClientSettings::default().with_capped_catch_up(),
  )
  .unwrap_or_else(|error| panic!("opening the store of {login}: {error}"))
  .unwrap_or_else(|| panic!("the store of {login} has no enrolled device: run demo-seed"));
  Opened { store, client }
}

impl DemoPeer {
  fn start(opened: Opened, seed_users: Rc<HashSet<UserId>>) -> Self {
    let Opened { store, client } = opened;
    let user_id = client.user_id();
    let wake = Rc::new(Notify::new());
    let on_change = {
      let wake = wake.clone();
      Box::new(move || wake.notify_one())
    };
    let (runner, handle) = SyncRunner::new(client, RunnerSettings::default(), on_change);
    let runner = tokio::task::spawn_local(async move {
      drop(runner.run().await);
    });
    let replier = Replier {
      store: store.clone(),
      me: user_id,
      seed_users,
      // Messages already there when the device starts are the seed's or were answered before.
      answered: newest_per_dm(&store),
      handle: handle.clone(),
    };
    let replier = tokio::task::spawn_local(replier.run(wake));
    Self {
      handle,
      runner,
      replier,
    }
  }

  /// Closes the gateway connection and the store.
  pub async fn stop(self) {
    self.replier.abort();
    self.handle.send(RunnerCommand::Stop);
    drop(self.runner.await);
  }
}

/// Answers new direct messages from people outside the seed.
struct Replier {
  store: SeedStore,
  me: UserId,
  seed_users: Rc<HashSet<UserId>>,
  /// Per direct message, the newest message not to answer.
  answered: HashMap<ConversationId, MessageId>,
  handle: RunnerHandle,
}

impl Replier {
  /// Looks again whenever the runner changed the store; `wake` collapses a burst into one look.
  async fn run(mut self, wake: Rc<Notify>) {
    loop {
      wake.notified().await;
      for (conversation, message) in self.due() {
        self.answered.insert(conversation, message);
        let draft = Draft {
          conversation_id: conversation,
          maybe_thread_id: None,
          content: MessageContent::text(AUTO_REPLY, Vec::new()),
        };
        match records::enqueue_message(&self.store, self.me, SystemClock.now_ms(), draft) {
          Ok(_) => {
            self.handle.send(RunnerCommand::Flush);
          },
          // Answering is a courtesy; the device's real job, admitting devices, goes on.
          Err(error) => eprintln!("demo-peers: answering in {conversation}: {error}"),
        }
      }
    }
  }

  /// The direct messages whose newest post is a new one from somebody outside the seed.
  fn due(&self) -> Vec<(ConversationId, MessageId)> {
    direct_messages(&self.store)
      .into_iter()
      .filter_map(|conversation| {
        let newest = newest_post(&self.store, conversation)?;
        let is_from_outside =
          newest.sender_user_id != self.me && !self.seed_users.contains(&newest.sender_user_id);
        let id = newest.maybe_server_id?;
        let is_new = self
          .answered
          .get(&conversation)
          .is_none_or(|answered| id > *answered);
        (is_from_outside && is_new).then_some((conversation, id))
      })
      .collect()
  }
}

/// The newest post of every direct message of the device, by server ID.
fn newest_per_dm(store: &SeedStore) -> HashMap<ConversationId, MessageId> {
  direct_messages(store)
    .into_iter()
    .filter_map(|conversation| {
      let id = newest_post(store, conversation)?.maybe_server_id?;
      Some((conversation, id))
    })
    .collect()
}

fn direct_messages(store: &SeedStore) -> Vec<ConversationId> {
  records::list_json::<ConversationResponse>(store, Table::Conversations)
    .unwrap_or_default()
    .into_iter()
    .filter(|conversation| conversation.kind == ConversationKind::Dm)
    .map(|conversation| conversation.id)
    .collect()
}

/// The last text or file of a conversation in display order (messages in the outbox last).
fn newest_post(store: &SeedStore, conversation: ConversationId) -> Option<StoredMessage> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .ok()?
    .into_iter()
    .filter(|message| {
      matches!(
        message.content,
        MessageContent::Text { .. } | MessageContent::File { .. }
      )
    })
    .max_by_key(StoredMessage::order)
}
