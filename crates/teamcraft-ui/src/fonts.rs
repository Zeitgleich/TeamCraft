//! The interface font (issue #1494): applies the chosen [`UiFont`] as `--font-ui` and loads the
//! shipped font's files when it is first used.
//!
//! The files are in `fonts/` (copied by Trunk from `crates/teamcraft-ui/fonts`, built by
//! `scripts/build-fonts.py`) on the app's own origin: the CSP stays `font-src 'self'`. They are never
//! part of a first visit: nothing names them until someone picks a font or opens the font menu, and
//! then a CSSOM `FontFace` per file is added to `document.fonts`, which the browser downloads only
//! for text in its `unicode-range` (`font-display: swap`, the system font until it is there). The
//! signed release lists them as `on_demand`, so the service worker keeps them once fetched and never
//! fetches them in the background. The desktop app embeds the same files.

use std::cell::RefCell;
use std::collections::HashSet;

use leptos::prelude::*;
use teamcraft_client_api::font::{FontSource, UiFont};
use wasm_bindgen::JsCast;

use crate::settings::Settings;

/// Where Trunk puts the font files (index.html, `copy-dir`).
pub const FONT_DIRECTORY: &str = "/fonts";
/// The system's own interface font stack, behind every chosen font (000-tokens.css `--font-ui`).
const SYSTEM_STACK: &str =
  r#"system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans", "Helvetica Neue", sans-serif"#;
/// What our own font faces are called, so an installed font of the same name (another version,
/// other glyphs) never stands in for the shipped one.
const FAMILY_PREFIX: &str = "TeamCraft ";

/// Latin, Latin Extended, punctuation, currency, letterlike symbols and arrows; as
/// `scripts/build-fonts.py` `LATIN`.
const LATIN: &str = "U+0000-024F, U+0259, U+02B0-02FF, U+0300-036F, U+1E00-1EFF, U+2000-206F, U+20A0-20C0, \
                     U+2100-214F, U+2190-21FF, U+2212-2215, U+2C60-2C7F, U+A720-A7FF, U+FEFF, U+FFFD";
const LATIN_GREEK_CYRILLIC: &str = "U+0000-024F, U+0259, U+02B0-02FF, U+0300-036F, U+0370-03FF, \
  U+0400-052F, U+1C80-1C8F, U+1E00-1EFF, U+1F00-1FFF, U+2000-206F, U+20A0-20C0, U+2100-214F, \
  U+2190-21FF, U+2212-2215, U+2C60-2C7F, U+2DE0-2DFF, U+A640-A69F, U+A720-A7FF, U+FEFF, U+FFFD";
/// The weights the files hold: the subset variable fonts span the UI's (400 to 700).
const VARIABLE_WEIGHTS: &str = "400 700";

thread_local! {
  /// The fonts whose faces are in `document.fonts` already.
  static REGISTERED: RefCell<HashSet<UiFont>> = RefCell::new(HashSet::new());
}

/// One file of a shipped font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontFile {
  /// The file name in [`FONT_DIRECTORY`].
  pub name: &'static str,
  pub unicode_range: &'static str,
  /// A CSS `font-weight` value or range.
  pub weight: &'static str,
}

/// Applies the chosen font to the whole app, now and on every change.
pub fn apply_font(settings: Settings) {
  let font = Memo::new(move |_| settings.prefs.with(|prefs| prefs.display.font.clone()));
  Effect::new(move |_| {
    let font = font.get();
    register(&font);
    let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
      .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
    else {
      return;
    };
    let style = root.style();
    let applied = match font_stack(&font) {
      Some(stack) => style.set_property("--font-ui", &stack),
      // The stylesheet's own value (000-tokens.css).
      None => style.remove_property("--font-ui").map(drop),
    };
    if applied.is_err() {
      tracing::debug!("could not apply the font");
    }
  });
}

/// Adds the faces of every shipped font, for the font menu's previews: the browser fetches each
/// one's Latin file once the menu shows its name.
pub fn register_all() {
  for font in UiFont::ALL {
    register(&font);
  }
}

/// The `font-family` value that shows `font` whatever the app's font is: the system's stack for the
/// system font (a preview of it in the font menu).
pub fn preview_stack(font: &UiFont) -> String {
  font_stack(font).unwrap_or_else(|| SYSTEM_STACK.to_owned())
}

/// The `font-family` value of `font`, behind it the system's stack; `None` for the system font.
pub fn font_stack(font: &UiFont) -> Option<String> {
  let family = family_name(font)?;
  Some(format!("\"{family}\", {SYSTEM_STACK}"))
}

/// The CSS family `font` goes by: our own name for a shipped font, the font's for an installed one.
pub fn family_name(font: &UiFont) -> Option<String> {
  let name = font.display_name()?;
  match font.source() {
    FontSource::System => None,
    FontSource::Shipped => Some(format!("{FAMILY_PREFIX}{name}")),
    FontSource::Installed => Some(name.to_owned()),
  }
}

/// The files of a shipped font; empty for the others.
pub fn font_files(font: &UiFont) -> &'static [FontFile] {
  match font {
    UiFont::Inter => INTER,
    UiFont::GoogleSansFlex => GOOGLE_SANS_FLEX,
    UiFont::AtkinsonHyperlegibleNext => ATKINSON_HYPERLEGIBLE_NEXT,
    UiFont::Lexend => LEXEND,
    UiFont::NotoSans => NOTO_SANS,
    UiFont::JetBrainsMono => JETBRAINS_MONO,
    UiFont::SourceSans3 => SOURCE_SANS_3,
    UiFont::IbmPlexSans => IBM_PLEX_SANS,
    UiFont::OpenDyslexic => OPENDYSLEXIC,
    UiFont::System | UiFont::Arial | UiFont::Georgia | UiFont::Verdana | UiFont::Unknown(_) => &[],
  }
}

const INTER: &[FontFile] = &[variable("inter.woff2", LATIN_GREEK_CYRILLIC)];
const GOOGLE_SANS_FLEX: &[FontFile] = &[variable("google-sans-flex.woff2", LATIN)];
const ATKINSON_HYPERLEGIBLE_NEXT: &[FontFile] = &[variable("atkinson-hyperlegible-next.woff2", LATIN)];
const LEXEND: &[FontFile] = &[variable("lexend.woff2", LATIN)];
const NOTO_SANS: &[FontFile] = &[variable("noto-sans.woff2", LATIN_GREEK_CYRILLIC)];
const JETBRAINS_MONO: &[FontFile] = &[variable("jetbrains-mono.woff2", LATIN_GREEK_CYRILLIC)];
/// Adobe's own variable file, unmodified (Reserved Font Name "Source"); weights 200 to 900.
const SOURCE_SANS_3: &[FontFile] = &[FontFile {
  name: "source-sans-3.woff2",
  unicode_range: LATIN_GREEK_CYRILLIC,
  weight: "200 900",
}];
/// IBM's own files by script, unmodified (Reserved Font Name "Plex"), with IBM's ranges.
const IBM_PLEX_SANS: &[FontFile] = &[
  plex(
    "ibm-plex-sans-latin1.woff2",
    "U+0000, U+000D, U+0020-007E, U+00A0-00FF, U+0131, U+0152-0153, U+02C6, U+02DA, U+02DC, \
     U+2013-2014, U+2018-201A, U+201C-201E, U+2020-2022, U+2026, U+2030, U+2039-203A, U+2044, \
     U+20AC, U+2122, U+2212, U+FB01-FB02",
  ),
  plex(
    "ibm-plex-sans-latin2.woff2",
    "U+0100-0101, U+0104-0130, U+0132-0151, U+0154-017F, U+018F, U+0192, U+01A0-01A1, \
     U+01AF-01B0, U+01FA-01FF, U+0218-021B, U+0237, U+0259, U+1E80-1E85, U+1E9E, U+20A1, U+20A4, \
     U+20A6, U+20A8-20AA, U+20AD-20AE, U+20B1-20B2, U+20B4-20B5, U+20B8-20BA, U+20BD, U+20BF",
  ),
  plex(
    "ibm-plex-sans-latin3.woff2",
    "U+0102-0103, U+01CD-01DC, U+1EA0-1EF9, U+20AB",
  ),
  plex(
    "ibm-plex-sans-pi.woff2",
    "U+0E3F, U+2000-200D, U+2015, U+2028-2029, U+202F, U+2032-2033, U+2070, U+2074-2079, \
     U+2080-2089, U+2113, U+2116, U+2126, U+212E, U+2150-2151, U+2153-215E, U+2190-2199, \
     U+21A9-21AA, U+21B0-21B3, U+21B6-21B7, U+21BA-21BB, U+21C4, U+21C6, U+2202, U+2206, U+220F, \
     U+2211, U+2215, U+221A, U+221E, U+222B, U+2248, U+2260, U+2264-2265, U+25CA, U+2713, U+274C, \
     U+2B0E-2B11, U+FEFF, U+FFFD",
  ),
  plex(
    "ibm-plex-sans-greek.woff2",
    "U+037E, U+0386-038A, U+038C, U+038E-03A1, U+03A3-03CE",
  ),
  plex(
    "ibm-plex-sans-cyrillic.woff2",
    "U+0400-045F, U+0462-0463, U+046A-046B, U+0472-0475, U+0490-04C2, U+04CF-04D9, U+04DC-04E9, \
     U+04EE-04F9, U+0524-0525",
  ),
];
/// Static, unmodified (Reserved Font Name "OpenDyslexic"): a file per weight.
const OPENDYSLEXIC: &[FontFile] = &[
  FontFile {
    name: "opendyslexic.woff2",
    unicode_range: LATIN_GREEK_CYRILLIC,
    weight: "400",
  },
  FontFile {
    name: "opendyslexic-bold.woff2",
    unicode_range: LATIN_GREEK_CYRILLIC,
    weight: "700",
  },
];

/// A subset variable font: the UI's weights.
const fn variable(name: &'static str, unicode_range: &'static str) -> FontFile {
  FontFile {
    name,
    unicode_range,
    weight: VARIABLE_WEIGHTS,
  }
}

/// IBM's variable files: weights 100 to 700.
const fn plex(name: &'static str, unicode_range: &'static str) -> FontFile {
  FontFile {
    name,
    unicode_range,
    weight: "100 700",
  }
}

/// Adds `font`'s faces to `document.fonts` once. Nothing is downloaded until text uses them.
fn register(font: &UiFont) {
  let files = font_files(font);
  let Some(family) = family_name(font).filter(|_| !files.is_empty()) else {
    return;
  };
  if REGISTERED.with_borrow(|registered| registered.contains(font)) {
    return;
  }
  let Some(set) = web_sys::window()
    .and_then(|window| window.document())
    .map(|doc| doc.fonts())
  else {
    return;
  };
  for file in files {
    let descriptors = web_sys::FontFaceDescriptors::new();
    descriptors.set_weight(file.weight);
    descriptors.set_unicode_range(file.unicode_range);
    descriptors.set_display("swap");
    let source = format!("url(\"{FONT_DIRECTORY}/{}\") format(\"woff2\")", file.name);
    match web_sys::FontFace::new_with_str_and_descriptors(&family, &source, &descriptors) {
      Ok(face) => {
        if set.add(&face).is_err() {
          tracing::debug!(font = %font, "could not add a font face");
        }
      },
      Err(_) => tracing::debug!(font = %font, "the browser refused a font face"),
    }
  }
  REGISTERED.with_borrow_mut(|registered| registered.insert(font.clone()));
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Trunk copies the files to where the faces point.
  #[test]
  fn index_html_copies_the_font_directory() {
    let index = include_str!("../index.html");
    assert!(
      index.contains(r#"<link data-trunk rel="copy-dir" href="fonts" data-target-path="fonts" />"#),
      "index.html must copy fonts/"
    );
    assert_eq!(FONT_DIRECTORY, "/fonts");
  }

  /// Every file a face names is in the repository, and every font file there is used.
  #[test]
  fn every_shipped_font_has_its_files() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts");
    let mut named = HashSet::new();
    for font in UiFont::ALL {
      let files = font_files(&font);
      assert_eq!(files.is_empty(), font.source() != FontSource::Shipped, "{font}");
      for file in files {
        assert!(directory.join(file.name).is_file(), "{}", file.name);
        named.insert(file.name.to_owned());
      }
    }
    let present: HashSet<String> = std::fs::read_dir(&directory)
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .filter(|name| name.ends_with(".woff2"))
      .collect();
    assert_eq!(present, named);
  }

  #[test]
  fn stacks_put_the_font_before_the_system_one() {
    assert_eq!(font_stack(&UiFont::System), None);
    assert_eq!(font_stack(&UiFont::Unknown("new".into())), None);
    assert!(preview_stack(&UiFont::System).starts_with("system-ui"));
    let inter = font_stack(&UiFont::Inter).unwrap();
    assert!(inter.starts_with(r#""TeamCraft Inter", system-ui"#), "{inter}");
    assert!(
      font_stack(&UiFont::Georgia)
        .unwrap()
        .starts_with(r#""Georgia", "#)
    );
  }

  /// Values go into CSS: no quote or bracket may end the family name or the URL early.
  #[test]
  fn names_are_safe_in_css() {
    for font in UiFont::ALL {
      if let Some(family) = family_name(&font) {
        assert!(
          family.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '),
          "{family}"
        );
      }
      for file in font_files(&font) {
        assert!(
          file
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.'))
        );
      }
    }
  }
}
