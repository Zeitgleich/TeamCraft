//! The few runtime services that differ between native targets (tokio) and the browser (the
//! JavaScript event loop driven by `wasm-bindgen-futures`): timers.
//!
//! Everything else in the engine is plain `async` without `Send` bounds, so the same code runs on a
//! single-threaded browser executor.

use std::time::Duration;

/// Completes after `duration`. Dropping the future cancels the timer.
pub async fn sleep(duration: Duration) {
  imp::sleep(duration).await;
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
  use std::time::Duration;

  pub(super) async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
  }
}

#[cfg(target_arch = "wasm32")]
mod imp {
  use std::future::Future;
  use std::pin::Pin;
  use std::task::{Context, Poll};
  use std::time::Duration;

  use futures_channel::oneshot;
  use wasm_bindgen::prelude::{Closure, wasm_bindgen};
  use wasm_bindgen::{JsCast, JsValue};

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
