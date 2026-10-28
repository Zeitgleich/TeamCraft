//! The recovery kit file (e2ee.md §6.5.3): one self-contained HTML page with the recovery key, its
//! QR code and what to do with it, built in the client and saved through a download. Nothing of it
//! goes to the server. The app reads the key back out of a saved kit ([`read_kit_file`]).

use teamcraft_crypto::recovery_key::RecoveryKey;
use zeroize::Zeroizing;

/// A file bigger than this is no kit (a kit is a few kilobytes); it isn't read.
const MAX_KIT_FILE_BYTES: f64 = 1024.0 * 1024.0;
/// How long the download's object URL stays valid; the browser has the file long before.
const OBJECT_URL_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// The kit's words, already translated; the account, server and date are in `account_line`.
pub struct KitTexts {
  pub language: String,
  pub title: String,
  pub account_line: String,
  pub intro: String,
  pub key_label: String,
  pub keep_title: String,
  pub keep: Vec<String>,
  pub use_title: String,
  pub use_text: String,
  pub lost_title: String,
  pub lost_text: String,
  pub never_asked: String,
}

/// What reading a picked file found.
#[derive(Debug)]
pub enum KitFile {
  Key(RecoveryKey),
  /// The file holds no recovery key (another file, or one too big to be a kit).
  NoKey,
}

/// The kit page for `key`. Everything that isn't the app's own markup is escaped.
pub fn kit_html(key: &RecoveryKey, texts: &KitTexts) -> Zeroizing<String> {
  let groups: String = key
    .groups()
    .iter()
    .map(|group| format!("<span>{group}</span>"))
    .collect();
  let groups = Zeroizing::new(groups);
  // Uppercase: the QR code's alphanumeric mode holds it in a smaller code.
  let machine = Zeroizing::new(key.to_machine().to_uppercase());
  let qr = match crate::qr::svg_path(&machine) {
    Some(drawn) => format!(
      r##"<svg viewBox="0 0 {size} {size}" role="img" aria-label="{label}" shape-rendering="crispEdges"><rect width="{size}" height="{size}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##,
      size = drawn.size,
      label = escape(&texts.key_label),
      path = drawn.path,
    ),
    None => String::new(),
  };
  let keep: String = texts
    .keep
    .iter()
    .map(|item| format!("<li>{}</li>", escape(item)))
    .collect();
  let html = format!(
    r#"<!doctype html>
<html lang="{language}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>{title}</title>
<style>
:root {{ color-scheme: light; }}
body {{ margin: 0; background: #fff; color: #1d1d1f; font: 16px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }}
main {{ max-width: 44rem; margin: 3rem auto; padding: 0 1.5rem; }}
h1 {{ margin: 0 0 0.25rem; font-size: 1.75rem; line-height: 1.15; letter-spacing: -0.01em; }}
h2 {{ margin: 1.5rem 0 0.25rem; font-size: 1.05rem; }}
.account {{ margin: 0 0 1.25rem; color: #6e6e73; }}
.key-box {{ display: flex; flex-wrap: wrap; gap: 1.5rem; align-items: center; margin: 1.5rem 0; padding: 1.25rem; border: 2px solid #1d1d1f; border-radius: 14px; }}
.key-box svg {{ flex: none; width: 9.5rem; height: 9.5rem; }}
.key-label {{ margin: 0 0 0.25rem; color: #6e6e73; font-size: 0.85rem; }}
.key {{ margin: 0; font: 600 1.4rem/1.7 ui-monospace, "SF Mono", Menlo, Consolas, monospace; letter-spacing: 0.06em; }}
.key span {{ display: inline-block; margin-right: 0.7em; }}
.machine {{ margin: 0.5rem 0 0; color: #6e6e73; font: 0.75rem ui-monospace, Menlo, Consolas, monospace; word-break: break-all; }}
ul {{ margin: 0.25rem 0; padding-left: 1.25rem; }}
p {{ margin: 0.25rem 0; }}
.never {{ margin-top: 1.5rem; font-weight: 600; }}
@media print {{ main {{ margin: 0 auto; }} @page {{ margin: 1.5cm; }} }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
<p class="account">{account_line}</p>
<p>{intro}</p>
<section class="key-box">
{qr}
<div>
<p class="key-label">{key_label}</p>
<p class="key">{groups}</p>
<p class="machine">{machine}</p>
</div>
</section>
<h2>{keep_title}</h2>
<ul>{keep}</ul>
<h2>{use_title}</h2>
<p>{use_text}</p>
<h2>{lost_title}</h2>
<p>{lost_text}</p>
<p class="never">{never_asked}</p>
</main>
</body>
</html>
"#,
    language = escape(&texts.language),
    title = escape(&texts.title),
    account_line = escape(&texts.account_line),
    intro = escape(&texts.intro),
    key_label = escape(&texts.key_label),
    groups = groups.as_str(),
    machine = machine.as_str(),
    keep_title = escape(&texts.keep_title),
    use_title = escape(&texts.use_title),
    use_text = escape(&texts.use_text),
    lost_title = escape(&texts.lost_title),
    lost_text = escape(&texts.lost_text),
    never_asked = escape(&texts.never_asked),
  );
  Zeroizing::new(html)
}

/// `teamcraft-recovery-kit-<host>.html`: names the server (a user may have several), never the key.
pub fn kit_file_name(server_host: &str) -> String {
  let host: String = server_host
    .chars()
    .map(|character| {
      if character.is_ascii_alphanumeric() || character == '.' || character == '-' {
        character.to_ascii_lowercase()
      } else {
        '-'
      }
    })
    .collect();
  let host = host.trim_matches(['-', '.']);
  if host.is_empty() {
    "teamcraft-recovery-kit.html".to_owned()
  } else {
    format!("teamcraft-recovery-kit-{host}.html")
  }
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

/// Whether `typed` is group `index` (from 0) of `key`, read as forgivingly as the key itself.
pub fn is_group(key: &RecoveryKey, index: usize, typed: &str) -> bool {
  let normalized: String = typed
    .chars()
    .filter(|character| !character.is_whitespace() && *character != '-')
    .map(|character| match character.to_ascii_uppercase() {
      'O' => '0',
      'I' | 'L' => '1',
      other => other,
    })
    .collect();
  let normalized = Zeroizing::new(normalized);
  key.groups().get(index).is_some_and(|group| *group == *normalized)
}

/// Text to put into HTML element content or a double-quoted attribute.
fn escape(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  for character in text.chars() {
    match character {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\'' => out.push_str("&#39;"),
      other => out.push(other),
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  #[test]
  fn the_kit_holds_the_key_twice_and_reads_back() {
    let key = RecoveryKey::generate(&mut OsRng);
    let html = kit_html(&key, &texts());
    for group in key.groups().iter() {
      assert!(html.contains(&format!("<span>{group}</span>")));
    }
    assert!(html.contains("<svg"));
    let read = RecoveryKey::find_in(&html).unwrap();
    assert_eq!(read.to_display(), key.to_display());
  }

  #[test]
  fn the_kit_is_static_and_escapes_what_it_did_not_write() {
    let key = RecoveryKey::generate(&mut OsRng);
    let mut texts = texts();
    texts.account_line = "Account: <img src=x onerror=alert(1)> & \"friends\"".into();
    let html = kit_html(&key, &texts);
    assert!(!html.contains("<img"));
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt; &amp; &quot;friends&quot;"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("http://") && !html.contains("https://"));
  }

  #[test]
  fn the_file_name_names_the_server_and_nothing_else() {
    assert_eq!(
      kit_file_name("Chat.Example.com"),
      "teamcraft-recovery-kit-chat.example.com.html"
    );
    assert_eq!(
      kit_file_name("chat.example.com:8443"),
      "teamcraft-recovery-kit-chat.example.com-8443.html"
    );
    assert_eq!(kit_file_name("../.."), "teamcraft-recovery-kit.html");
    assert_eq!(kit_file_name(""), "teamcraft-recovery-kit.html");
  }

  #[test]
  fn a_group_is_checked_as_forgivingly_as_the_key() {
    let key = RecoveryKey::generate(&mut OsRng);
    let groups = key.groups();
    let third = &groups[2];
    assert!(is_group(&key, 2, third));
    let sloppy = format!(" {} ", third.to_lowercase().replace('0', "o").replace('1', "l"));
    assert!(is_group(&key, 2, &sloppy));
    assert!(!is_group(&key, 2, ""));
    assert!(!is_group(&key, 8, third));
    let other = if groups[3] == *third {
      "ZZZZ"
    } else {
      groups[3].as_str()
    };
    assert!(!is_group(&key, 2, other));
  }

  fn texts() -> KitTexts {
    KitTexts {
      language: "en".into(),
      title: "TeamCraft recovery kit".into(),
      account_line: "Account: Alice · Server: chat.example.com".into(),
      intro: "Keep this.".into(),
      key_label: "Recovery key".into(),
      keep_title: "Keep it safe".into(),
      keep: vec!["Print it.".into()],
      use_title: "When".into(),
      use_text: "Setup.".into(),
      lost_title: "Lost".into(),
      lost_text: "New kit.".into(),
      never_asked: "Never asked.".into(),
    }
  }
}
