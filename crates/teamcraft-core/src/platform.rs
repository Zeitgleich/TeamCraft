//! The few runtime services that differ between native targets (tokio) and the browser (the
//! JavaScript event loop driven by `wasm-bindgen-futures`): timers, and handing the thread back.
//!
//! Everything else in the engine is plain `async` without `Send` bounds, so the same code runs on a
//! single-threaded browser executor.

use std::time::Duration;

/// Completes after `duration`. Dropping the future cancels the timer.
pub async fn sleep(duration: Duration) {
  imp::sleep(duration).await;
}

/// Lets everything else that waits for the thread run before the caller goes on: in the browser
/// the event loop (the UI's input, its reads of the core, a paint, issue #1575), natively the other
/// tasks of the executor.
pub async fn yield_now() {
  imp::yield_now().await;
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
  use std::time::Duration;

  pub(super) async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
  }

  pub(super) async fn yield_now() {
    tokio::task::yield_now().await;
  }
}

#[cfg(target_arch = "wasm32")]
mod imp {
  use std::cell::RefCell;
  use std::collections::VecDeque;
  use std::future::Future;
  use std::pin::Pin;
  use std::rc::Rc;
  use std::task::{Context, Poll};
  use std::time::Duration;

  use futures_channel::oneshot;
  use wasm_bindgen::prelude::{Closure, wasm_bindgen};
  use wasm_bindgen::{JsCast, JsValue};
  use web_sys::{MessageChannel, MessageEvent, MessagePort};

  // The global functions exist in windows and in (service) workers alike; `web_sys::Window` doesn't.
  #[wasm_bindgen]
  extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(handler: &js_sys::Function, timeout_ms: i32) -> JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(handle: &JsValue);
  }

  /// A `setTimeout` as a future; the callback lives as long as the future.
  struct Sleep {
    fired: oneshot::Receiver<()>,
    handle: JsValue,
    _callback: Closure<dyn FnMut()>,
  }

  pub(super) async fn sleep(duration: Duration) {
    let (sender, fired) = oneshot::channel();
    let mut maybe_sender = Some(sender);
    let callback = Closure::<dyn FnMut()>::new(move || {
      if let Some(sender) = maybe_sender.take() {
        // The receiver is only gone if the sleep was dropped, and then the timer was cleared.
        let _ = sender.send(());
      }
    });
    let timeout_ms = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
    let handle = set_timeout(callback.as_ref().unchecked_ref(), timeout_ms);
    Sleep {
      fired,
      handle,
      _callback: callback,
    }
    .await;
  }

  /// A task of its own, not a microtask: the browser handles input and paints in between, which a
  /// resolved promise wouldn't let it. A message to the thread's own channel, not a zero timeout:
  /// a timer set from a timer's callback waits at least 4 ms from the fifth in a row (HTML's
  /// nesting clamp), so work that hands the thread back every few milliseconds spent as long
  /// waiting as working (issue #2397). Messages run in order with the other tasks, so neither
  /// IndexedDB's events nor the UI's timers wait behind them. Without a channel (it can't fail in
  /// a window or worker), the timeout.
  pub(super) async fn yield_now() {
    let (sender, turn) = oneshot::channel();
    let is_posted = YIELDS.with(|yields| {
      let mut yields = yields.borrow_mut();
      if yields.is_none() {
        *yields = ThreadYields::new();
      }
      yields.as_ref().is_some_and(|yields| yields.post(sender))
    });
    if is_posted {
      // A dropped sender (the channel went away) is a turn as well.
      let _ = turn.await;
    } else {
      sleep(Duration::ZERO).await;
    }
  }

  thread_local! {
    static YIELDS: RefCell<Option<ThreadYields>> = const { RefCell::new(None) };
  }

  /// The thread's channel for [`yield_now`]: each waiter posts one message and goes on when it
  /// arrives, first come first served.
  struct ThreadYields {
    sending: MessagePort,
    waiting: Rc<RefCell<VecDeque<oneshot::Sender<()>>>>,
    _receiving: MessagePort,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
  }

  impl ThreadYields {
    fn new() -> Option<Self> {
      let channel = MessageChannel::new().ok()?;
      let waiting: Rc<RefCell<VecDeque<oneshot::Sender<()>>>> = Rc::default();
      let woken = Rc::clone(&waiting);
      let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |_: MessageEvent| {
        let maybe_next = woken.borrow_mut().pop_front();
        if let Some(next) = maybe_next {
          // Its yield was dropped meanwhile: nobody to wake.
          let _ = next.send(());
        }
      });
      let receiving = channel.port1();
      receiving.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
      Some(Self {
        sending: channel.port2(),
        waiting,
        _receiving: receiving,
        _on_message: on_message,
      })
    }

    /// Queues `sender` to be woken by the next message; false if the message can't be posted.
    fn post(&self, sender: oneshot::Sender<()>) -> bool {
      if self.sending.post_message(&JsValue::UNDEFINED).is_err() {
        return false;
      }
      self.waiting.borrow_mut().push_back(sender);
      true
    }
  }

  impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
      // A cancelled sender can't happen while the closure is alive; treat it as fired anyway.
      Pin::new(&mut self.fired).poll(context).map(|_| ())
    }
  }

  impl Drop for Sleep {
    fn drop(&mut self) {
      clear_timeout(&self.handle);
    }
  }
}

/// A readable message for a JavaScript exception or rejection value.
#[cfg(target_arch = "wasm32")]
pub fn describe_js_error(error: &wasm_bindgen::JsValue) -> String {
  use wasm_bindgen::JsCast;
  error
    .dyn_ref::<js_sys::Error>()
    .map(|error| String::from(error.message()))
    .or_else(|| error.as_string())
    .unwrap_or_else(|| "JavaScript error".into())
}
