//! The recovery kit in the browser (e2ee.md §6.5.3): the page itself is built in
//! `teamcraft_client_api::recovery_kit`; here it is saved through a download, and the app reads the
//! key back out of a saved kit ([`read_kit_file`]).

pub use teamcraft_client_api::recovery_kit::{KitTexts, is_group, kit_file_name, kit_html};
use teamcraft_crypto::recovery_key::RecoveryKey;
use zeroize::Zeroizing;

/// A file bigger than this is no kit (a kit is a few kilobytes); it isn't read.
const MAX_KIT_FILE_BYTES: f64 = 1024.0 * 1024.0;
/// How long the download's object URL stays valid; the browser has the file long before.
const OBJECT_URL_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// What reading a picked file found.
#[derive(Debug)]
pub enum KitFile {
  Key(RecoveryKey),
  /// The file holds no recovery key (another file, or one too big to be a kit).
  NoKey,
}

/// Offers `html` as a download named `file_name`, through a link the page never shows. In the
/// desktop app the webview saves it to the downloads folder and tells the page.
pub fn download(html: &str, file_name: &str) -> Result<(), String> {
  use wasm_bindgen::JsCast;

  let describe = |error: wasm_bindgen::JsValue| format!("{error:?}");
  let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(html));
  let options = web_sys::BlobPropertyBag::new();
  options.set_type("text/html;charset=utf-8");
  let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).map_err(describe)?;
  let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(describe)?;
  let document = web_sys::window()
    .and_then(|window| window.document())
    .ok_or("no document")?;
  let anchor: web_sys::HtmlAnchorElement = document
    .create_element("a")
    .map_err(describe)?
    .dyn_into()
    .map_err(|_| "not an anchor".to_owned())?;
  anchor.set_href(&url);
  anchor.set_download(file_name);
  anchor.set_rel("noopener");
  anchor.click();
  leptos::prelude::set_timeout(
    move || drop(web_sys::Url::revoke_object_url(&url)),
    OBJECT_URL_LIFETIME,
  );
  Ok(())
}

/// Reads the recovery key out of a file the user picked (a saved kit, or any text with the key's
/// machine form). Read in the page; the file goes nowhere.
pub async fn read_kit_file(file: web_sys::File) -> Result<KitFile, String> {
  if file.size() > MAX_KIT_FILE_BYTES {
    return Ok(KitFile::NoKey);
  }
  let text = wasm_bindgen_futures::JsFuture::from(file.text())
    .await
    .map_err(|error| format!("{error:?}"))?;
  let text = Zeroizing::new(text.as_string().unwrap_or_default());
  Ok(match RecoveryKey::find_in(&text) {
    Some(key) => KitFile::Key(key),
    None => KitFile::NoKey,
  })
}
