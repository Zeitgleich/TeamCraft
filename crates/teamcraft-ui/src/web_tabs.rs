//! One owner of an account's local store per browser (e2ee.md §9.4).
//!
//! Every tab of the same account would otherwise load the same IndexedDB into memory and run its own
//! core: own MLS state, own gateway session, own outbox. MLS state forks and outbox entries go out
//! twice. The tab holding the Web Lock `teamcraft-core-<user>` owns the store and runs the core;
//! every other tab of that account is a follower: its `core_*` calls go over the BroadcastChannel of
//! the same name to the owner, which answers them and tells every tab when data changed. When the
//! owner closes, the browser hands the lock to a waiting follower, which then opens the store.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use teamcraft_core::platform;
use teamcraft_proto::UserId;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};

/// Prefix of the lock and channel names; the user ID follows.
const NAME_PREFIX: &str = "teamcraft-core-";
/// How long a follower waits for the owner's answer. Enrollment (Argon2) and policy changes are the
/// slow calls; a tab that does not answer in this time is frozen or gone.
const CALL_TIMEOUT: Duration = Duration::from_secs(90);
/// A call is sent again this often until the owner confirms it: the owner's channel may not be open
/// yet (both tabs just started).
const RESEND_INTERVAL: Duration = Duration::from_secs(1);
/// Calls the owner remembers, so one sent twice runs once.
const SEEN_CALLS: usize = 256;

/// A follower's calls waiting for the owner, by call ID.
type PendingCalls = Rc<RefCell<Calls>>;

/// A held Web Lock: the store's lock while this tab owns the store, or [`exclusive`]'s. Dropping it
/// hands the lock to the next tab waiting for it.
pub struct OwnerLock {
  release: js_sys::Function,
}

/// What asking for the lock without waiting gave.
pub enum Ownership {
  Owned(OwnerLock),
  /// Another tab of this account owns the store.
  Taken,
  /// No Web Locks (an old browser): the tab runs its own core, as before.
  Unsupported,
}

/// The owner's side of the channel: answers followers' calls.
pub struct OwnerChannel {
  channel: web_sys::BroadcastChannel,
  _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
}

/// A follower's side of the channel: sends calls, waits for answers.
pub struct Follower {
  channel: web_sys::BroadcastChannel,
  tab: String,
  next_call: Cell<u64>,
  pending: PendingCalls,
  _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
}

/// What a follower waits for, per call.
#[derive(Default)]
struct Calls {
  acks: HashMap<u64, oneshot::Sender<()>>,
  replies: HashMap<u64, oneshot::Sender<Result<Value, String>>>,
}

/// One message on the channel, as JSON text.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TabMessage {
  /// A follower's `core_*` call.
  Call {
    id: u64,
    tab: String,
    command: String,
    args: Value,
  },
  /// The owner got the call and runs it.
  Ack { id: u64, tab: String },
  /// The owner's answer to the follower `tab`.
  Reply {
    id: u64,
    tab: String,
    result: Result<Value, String>,
  },
  /// The core's data changed: read again.
  Changed,
}

impl Drop for OwnerLock {
  fn drop(&mut self) {
    // Resolving the promise the lock callback returned releases the lock.
    if let Err(error) = self.release.call0(&JsValue::NULL) {
      tracing::warn!(error = %platform::describe_js_error(&error), "could not release the store lock");
    }
  }
}

/// Takes the store's lock if no other tab has it.
pub async fn try_own(user: UserId) -> Ownership {
  match request_lock(&name(user), true).await {
    Ok(Some(lock)) => Ownership::Owned(lock),
    Ok(None) => Ownership::Taken,
    Err(reason) => {
      tracing::warn!(%reason, "no Web Locks; this tab runs its own core");
      Ownership::Unsupported
    },
  }
}

/// Waits until the owning tab lets go of the store, then holds the lock.
pub async fn wait_to_own(user: UserId) -> Option<OwnerLock> {
  match request_lock(&name(user), false).await {
    Ok(maybe_lock) => maybe_lock,
    Err(reason) => {
      tracing::warn!(%reason, "could not wait for the store lock");
      None
    },
  }
}

/// Waits for the Web Lock `name`, which only one tab of the browser holds at a time. `None` without
/// Web Locks (an old browser): the caller goes ahead alone.
pub async fn exclusive(name: &str) -> Option<OwnerLock> {
  match request_lock(name, false).await {
    Ok(maybe_lock) => maybe_lock,
    Err(reason) => {
      tracing::debug!(%reason, lock = name, "no Web Lock");
      None
    },
  }
}

/// `navigator.locks.request(name, { ifAvailable }, callback)`. The callback returns a promise
/// that stays pending until [`OwnerLock`] is dropped. web-sys has the Web Locks API only behind its
/// unstable flag, so it is called through `Reflect`.
async fn request_lock(name: &str, if_available: bool) -> Result<Option<OwnerLock>, String> {
  let navigator = web_sys::window().ok_or("no window")?.navigator();
  let locks = js_sys::Reflect::get(&navigator, &"locks".into()).map_err(|_| "no navigator.locks")?;
  if locks.is_undefined() || locks.is_null() {
    return Err("no navigator.locks".to_owned());
  }
  let request: js_sys::Function = js_sys::Reflect::get(&locks, &"request".into())
    .ok()
    .and_then(|request| request.dyn_into().ok())
    .ok_or("no navigator.locks.request")?;
  let options = js_sys::Object::new();
  js_sys::Reflect::set(&options, &"ifAvailable".into(), &if_available.into()).map_err(|_| "lock options")?;
  let (granted, answer) = oneshot::channel::<Option<js_sys::Function>>();
  let callback = Closure::once_into_js(move |lock: JsValue| -> JsValue {
    if lock.is_null() {
      // The receiver only disappears if the waiting future was dropped; nobody is left to tell.
      let _ = granted.send(None);
      return JsValue::NULL;
    }
    let mut maybe_release = None;
    let held = js_sys::Promise::new(&mut |resolve, _reject| maybe_release = Some(resolve));
    // Nobody waits any more (the caller gave up): let go of the lock right away.
    if let Err(Some(release)) = granted.send(maybe_release)
      && release.call0(&JsValue::NULL).is_err()
    {
      tracing::debug!("could not release an unwanted lock");
    }
    held.into()
  });
  request
    .call3(&locks, &name.into(), &options, &callback)
    .map_err(|error| platform::describe_js_error(&error))?;
  match answer.await {
    Ok(Some(release)) => Ok(Some(OwnerLock { release })),
    Ok(None) => Ok(None),
    Err(_) => Err("the lock request ended without an answer".to_owned()),
  }
}

impl OwnerChannel {
  /// Answers followers' calls with `serve` until dropped.
  pub fn open<F, Fut>(user: UserId, serve: F) -> Result<Self, String>
  where
    F: Fn(String, Value) -> Fut + 'static,
    Fut: std::future::Future<Output = Result<Value, String>> + 'static,
  {
    let channel = open_channel(user)?;
    let serve = Rc::new(serve);
    let replies = channel.clone();
    let mut seen = VecDeque::<(String, u64)>::new();
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
      let Some(TabMessage::Call {
        id,
        tab,
        command,
        args,
      }) = read(&event)
      else {
        return;
      };
      post(&replies, &TabMessage::Ack { id, tab: tab.clone() });
      let call = (tab.clone(), id);
      if seen.contains(&call) {
        return;
      }
      if seen.len() == SEEN_CALLS {
        seen.pop_front();
      }
      seen.push_back(call);
      let serve = serve.clone();
      let replies = replies.clone();
      wasm_bindgen_futures::spawn_local(async move {
        let result = serve(command, args).await;
        post(&replies, &TabMessage::Reply { id, tab, result });
      });
    });
    channel.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    Ok(Self {
      channel,
      _on_message: on_message,
    })
  }

  /// Tells the followers to read again.
  pub fn announce_change(&self) {
    post(&self.channel, &TabMessage::Changed);
  }
}

impl Drop for OwnerChannel {
  fn drop(&mut self) {
    self.channel.set_onmessage(None);
    self.channel.close();
  }
}

impl Follower {
  /// Joins the owner's channel; `on_change` runs whenever the owner's data changed.
  pub fn join(user: UserId, on_change: impl Fn() + 'static) -> Result<Self, String> {
    let channel = open_channel(user)?;
    let tab = tab_id();
    let pending = PendingCalls::default();
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new({
      let pending = pending.clone();
      let me = tab.clone();
      move |event: web_sys::MessageEvent| match read(&event) {
        Some(TabMessage::Ack { id, tab }) if tab == me => {
          if let Some(ack) = pending.borrow_mut().acks.remove(&id) {
            // The caller may have timed out already.
            let _ = ack.send(());
          }
        },
        Some(TabMessage::Reply { id, tab, result }) if tab == me => {
          if let Some(reply) = pending.borrow_mut().replies.remove(&id) {
            // The caller may have timed out already.
            let _ = reply.send(result);
          }
        },
        Some(TabMessage::Changed) => on_change(),
        _ => {},
      }
    });
    channel.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    Ok(Self {
      channel,
      tab,
      next_call: Cell::new(0),
      pending,
      _on_message: on_message,
    })
  }

  /// Runs a `core_*` command in the owning tab.
  pub async fn call(&self, command: &str, args: Value) -> Result<Value, String> {
    let id = self.next_call.get().wrapping_add(1);
    self.next_call.set(id);
    let (reply, mut answer) = oneshot::channel();
    let (ack, mut acked) = oneshot::channel();
    {
      let mut calls = self.pending.borrow_mut();
      calls.replies.insert(id, reply);
      calls.acks.insert(id, ack);
    }
    let message = TabMessage::Call {
      id,
      tab: self.tab.clone(),
      command: command.to_owned(),
      args,
    };
    let mut timeout = Box::pin(platform::sleep(CALL_TIMEOUT));
    // Sent again until the owner confirms it; the owner runs it once.
    let mut is_acked = false;
    let outcome = loop {
      if !is_acked {
        post(&self.channel, &message);
      }
      let resend = Box::pin(platform::sleep(RESEND_INTERVAL));
      let waiting = select(&mut acked, resend);
      match select(&mut answer, select(waiting, &mut timeout)).await {
        Either::Left((result, _)) => break Some(result),
        Either::Right((Either::Left((Either::Left(_), _)), _)) => is_acked = true,
        Either::Right((Either::Left((Either::Right(((), _)), _)), _)) => {},
        Either::Right((Either::Right(((), _)), _)) => break None,
      }
      if is_acked {
        // Only the answer or the timeout is left to wait for.
        break match select(&mut answer, &mut timeout).await {
          Either::Left((result, _)) => Some(result),
          Either::Right(((), _)) => None,
        };
      }
    };
    match outcome {
      Some(Ok(result)) => result,
      // Dropped when this tab took over the store meanwhile: the caller asks again.
      Some(Err(_)) => Err("TeamCraft moved to this tab; try again".to_owned()),
      None => {
        let mut calls = self.pending.borrow_mut();
        calls.replies.remove(&id);
        calls.acks.remove(&id);
        Err("TeamCraft is open in another tab, which does not answer".to_owned())
      },
    }
  }
}

impl Drop for Follower {
  fn drop(&mut self) {
    self.channel.set_onmessage(None);
    self.channel.close();
  }
}

fn name(user: UserId) -> String {
  format!("{NAME_PREFIX}{user}")
}

fn open_channel(user: UserId) -> Result<web_sys::BroadcastChannel, String> {
  web_sys::BroadcastChannel::new(&name(user)).map_err(|error| platform::describe_js_error(&error))
}

fn post(channel: &web_sys::BroadcastChannel, message: &TabMessage) {
  let text = match serde_json::to_string(message) {
    Ok(text) => text,
    Err(error) => {
      tracing::warn!(%error, "could not encode a tab message");
      return;
    },
  };
  if let Err(error) = channel.post_message(&JsValue::from_str(&text)) {
    tracing::warn!(error = %platform::describe_js_error(&error), "could not reach the other tabs");
  }
}

fn read(event: &web_sys::MessageEvent) -> Option<TabMessage> {
  serde_json::from_str(&event.data().as_string()?).ok()
}

/// A name for this tab, only to address replies: random, not a secret.
fn tab_id() -> String {
  format!(
    "{:x}-{:x}",
    js_sys::Date::now() as u64,
    (js_sys::Math::random() * f64::from(u32::MAX)) as u64
  )
}
