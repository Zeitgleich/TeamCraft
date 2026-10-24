//! What the user sees after a panic (issue #322). The wasm instance is dead once the panic hook
//! returns (`RuntimeError: unreachable`): no event handler of the app runs again. So the hook builds
//! a plain DOM overlay over everything that only offers a reload, through a link that works without
//! any code of the app (and without inline script, which the CSP forbids). The desktop webview
//! reloads the same way.

use std::cell::Cell;
use std::panic::PanicHookInfo;

use crate::i18n::{Locale, translate};

/// Longest panic message copied into the details; the rest is cut.
const MAX_MESSAGE_CHARS: usize = 500;

thread_local! {
  static IS_SHOWN: Cell<bool> = const { Cell::new(false) };
}

/// Logs panics to the console (as before) and shows the overlay.
pub fn install() {
  std::panic::set_hook(Box::new(|info| {
    console_error_panic_hook::hook(info);
    show(info);
  }));
}

fn show(info: &PanicHookInfo<'_>) {
  if IS_SHOWN.with(|shown| shown.replace(true)) {
    return;
  }
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let locale = document
    .document_element()
    .and_then(|root| root.get_attribute("lang"))
    .and_then(|tag| Locale::from_tag(&tag))
    .unwrap_or(Locale::En);
  // Errors building the overlay are ignored: there is nothing left to report them to.
  let _ = build(&document, locale, &details(info), &reload_href());
}

fn build(document: &web_sys::Document, locale: Locale, details: &str, reload_href: &str) -> Option<()> {
  let text = |key| translate(locale, key, None);
  let element = |tag: &str, class: &str| {
    let element = document.create_element(tag).ok()?;
    element.set_class_name(class);
    Some(element)
  };
  let overlay = element("div", "panic-overlay")?;
  overlay.set_attribute("role", "alertdialog").ok()?;
  overlay.set_attribute("aria-modal", "true").ok()?;
  overlay.set_attribute("aria-labelledby", "panic-title").ok()?;
  let card = element("div", "panic-card")?;
  let title = element("h2", "")?;
  title.set_id("panic-title");
  title.set_text_content(Some(&text("panic-title")));
  let body = element("p", "")?;
  body.set_text_content(Some(&text("panic-body")));
  let reload = element("a", "panic-reload")?;
  reload.set_attribute("href", reload_href).ok()?;
  reload.set_text_content(Some(&text("panic-reload")));
  let label = element("label", "small")?;
  label.set_attribute("for", "panic-details").ok()?;
  label.set_text_content(Some(&text("panic-details-label")));
  let area = element("textarea", "panic-details input")?;
  area.set_id("panic-details");
  area.set_attribute("readonly", "").ok()?;
  area.set_attribute("translate", "no").ok()?;
  area.set_text_content(Some(details));
  for child in [&title, &body, &reload, &label, &area] {
    card.append_child(child).ok()?;
  }
  overlay.append_child(&card).ok()?;
  document.body()?.append_child(&overlay).ok()?;
  // Focus is trapped by the overlay covering everything; put it on the one thing to do.
  if let Some(link) = wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlElement>(&reload) {
    let _ = link.focus();
  }
  Some(())
}

/// Version, place and message of the panic. Panic messages come from the code (assertions, the
/// reactive runtime), not from user data; they are cut to a bound all the same.
fn details(info: &PanicHookInfo<'_>) -> String {
  let message = info
    .payload()
    .downcast_ref::<&str>()
    .map(|text| (*text).to_owned())
    .or_else(|| info.payload().downcast_ref::<String>().cloned())
    .unwrap_or_default();
  let message: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
  let location = info
    .location()
    .map(|location| format!("{}:{}:{}", location.file(), location.line(), location.column()))
    .unwrap_or_default();
  format!("TeamCraft {}\n{location}\n{message}", env!("CARGO_PKG_VERSION"))
}

/// The current page without its fragment: following a link to the same URL with a fragment would
/// only scroll, not reload.
fn reload_href() -> String {
  web_sys::window()
    .map(|window| window.location())
    .and_then(|location| {
      Some(format!(
        "{}{}",
        location.pathname().ok()?,
        location.search().ok()?
      ))
    })
    .unwrap_or_else(|| "/".to_owned())
}
