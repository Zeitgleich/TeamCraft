//! `teamcraft://` links on every desktop OS (docs/app-links.md).
//!
//! The packages register the scheme (`plugins.deep-link` in tauri.conf.json: the installers on
//! Windows, `CFBundleURLTypes` on macOS, the `.desktop` entry on Linux). The system starts the app
//! with the link, or, when it runs, a second process hands the link to the running instance
//! (`single_instance.rs`) and exits; macOS tells the running app directly.
//!
//! Links wait here until the UI takes them: at a cold start the webview doesn't exist yet, and an
//! event sent then would be lost. The event only says "there is something": the link itself never
//! goes out in an event, which every window would receive. The UI decides what a link means.

use std::collections::VecDeque;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_deep_link::DeepLinkExt;
use tracing::{debug, warn};

use crate::tray;

pub use teamcraft_client_api::remote::DEEP_LINK_EVENT;

/// Links kept for the UI. A page firing links in a loop only costs the oldest ones.
const MAX_WAITING_LINKS: usize = 8;

/// Links the UI hasn't taken yet.
#[derive(Default)]
pub struct WaitingLinks(Mutex<VecDeque<String>>);

impl WaitingLinks {
  fn push(&self, link: String) {
    let mut links = lock(&self.0);
    if links.len() == MAX_WAITING_LINKS {
      links.pop_front();
    }
    links.push_back(link);
  }

  fn take(&self) -> Vec<String> {
    lock(&self.0).drain(..).collect()
  }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  // A plain list of strings: a panic elsewhere leaves it valid.
  mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The UI takes the links waiting for it. A link is handed over once.
#[tauri::command(rename_all = "snake_case")]
pub fn deep_link_take(links: State<'_, WaitingLinks>) -> Vec<String> {
  links.take()
}

/// Collects links, including the one the app was started with. Runs in `setup`.
pub fn collect(app: &AppHandle) {
  app.manage(WaitingLinks::default());
  register_appimage(app);
  let deep_links = app.deep_link();
  match deep_links.get_current() {
    Ok(Some(urls)) => receive(app, urls.iter().map(url::Url::as_str)),
    Ok(None) => {},
    Err(error) => warn!(%error, "could not read the link the app was started with"),
  }
  let handle = app.clone();
  deep_links.on_open_url(move |event| {
    receive(&handle, event.urls().iter().map(url::Url::as_str));
    tray::show_main_window(&handle);
  });
}

fn receive<'a>(app: &AppHandle, links: impl Iterator<Item = &'a str>) {
  let waiting = app.state::<WaitingLinks>();
  for link in links {
    // Links are never logged: they can carry key material (device linking).
    waiting.push(link.to_owned());
  }
  if let Err(error) = app.emit(DEEP_LINK_EVENT, "") {
    debug!(%error, "could not tell the webview about a link");
  }
}

/// An AppImage installs no `.desktop` entry, so nothing would know it handles the scheme: it
/// registers itself with the user's desktop. The deb, rpm and Flatpak ship theirs.
#[cfg(target_os = "linux")]
fn register_appimage(app: &AppHandle) {
  if app.env().appimage.is_some()
    && let Err(error) = app.deep_link().register_all()
  {
    warn!(%error, "could not register the AppImage as the handler of teamcraft links");
  }
}

#[cfg(not(target_os = "linux"))]
fn register_appimage(_app: &AppHandle) {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn links_wait_in_order_and_are_handed_over_once() {
    let links = WaitingLinks::default();
    links.push("teamcraft:/c/1/m/1".to_owned());
    links.push("teamcraft:/c/1/m/2".to_owned());
    assert_eq!(links.take(), ["teamcraft:/c/1/m/1", "teamcraft:/c/1/m/2"]);
    assert!(links.take().is_empty());
  }

  #[test]
  fn a_flood_of_links_keeps_the_newest() {
    let links = WaitingLinks::default();
    for number in 0..MAX_WAITING_LINKS + 3 {
      links.push(number.to_string());
    }
    let kept = links.take();
    assert_eq!(kept.len(), MAX_WAITING_LINKS);
    assert_eq!(kept[0], "3");
  }
}
