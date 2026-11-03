//! The microphone level of the pre-join check on the desktop (#442). The shell measures the
//! microphone and sends a reading about ten times a second over a Tauri channel, which is open
//! only while the check measures; the meter reads the newest one.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Reflect;
use leptos::task::spawn_local;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::auth::tauri::{call, close_channel, open_channel};

/// The newest level reading from the shell, and the channel it comes through.
#[derive(Clone, Default)]
pub struct ShellLevel {
  level: Rc<Cell<f32>>,
  maybe_channel: Rc<RefCell<Option<LevelChannel>>>,
}

struct LevelChannel {
  callback_id: u32,
  _callback: Closure<dyn FnMut(JsValue)>,
}

impl ShellLevel {
  /// The newest reading, from 0.0 to 1.0; 0.0 while nothing is measured.
  pub fn get(&self) -> f32 {
    self.level.get()
  }

  /// Opens the channel while the check measures the microphone, and closes it after.
  pub fn follow(&self, is_measuring: bool) {
    let is_open = self.maybe_channel.borrow().is_some();
    if is_measuring && !is_open {
      self.open();
    } else if !is_measuring && is_open {
      self.close();
    }
  }

  fn open(&self) {
    let level = Rc::clone(&self.level);
    let newest = Cell::new(-1.0_f64);
    // `{ message, index }` per reading; `{ end: true }` (no message) when the shell drops it.
    let callback = Closure::<dyn FnMut(JsValue)>::new(move |envelope: JsValue| {
      let field =
        |name: &str| Reflect::get(&envelope, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED);
      let (Some(index), Some(reading)) = (field("index").as_f64(), field("message").as_f64()) else {
        return;
      };
      // Messages can be delivered out of order; an older reading is stale.
      // async-ok: Cells, not signals (the meter reads `level` on its own tick).
      if index > newest.get() {
        // async-ok: a Cell, not a signal.
        newest.set(index);
        // async-ok: a Cell, not a signal.
        level.set((reading as f32).clamp(0.0, 1.0));
      }
    });
    let Some((callback_id, argument)) = open_channel(callback.as_ref().unchecked_ref()) else {
      tracing::warn!("the desktop bridge has no channels; the meter stays empty");
      return;
    };
    *self.maybe_channel.borrow_mut() = Some(LevelChannel {
      callback_id,
      _callback: callback,
    });
    spawn_local(async move {
      if let Err(error) = call::<()>("call_watch_level", &json!({ "levels": argument })).await {
        tracing::warn!(%error, "the shell does not send the microphone level");
      }
    });
  }

  fn close(&self) {
    let Some(channel) = self.maybe_channel.borrow_mut().take() else {
      return;
    };
    close_channel(channel.callback_id);
    self.level.set(0.0);
    let id = channel.callback_id;
    spawn_local(async move {
      // The shell forgets a channel it can't send to anyway; nothing to do when this fails.
      if let Err(error) = call::<()>("call_unwatch_level", &json!({ "channel": id })).await {
        tracing::debug!(%error, "the shell did not stop the microphone level");
      }
    });
  }
}
