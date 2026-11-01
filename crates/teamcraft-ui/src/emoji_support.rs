//! Which emoji this device's font can draw. The catalog has all of Unicode, but an older system
//! font draws newer emoji as empty boxes (or as their parts, for sequences). The picker offers only
//! what the font draws; no emoji font is shipped (Noto Color Emoji alone is about 10 MB, twice the
//! web download budget).

use std::cell::OnceCell;

use crate::emoji_catalog::{self, CatalogEmoji, EmojiVersion};

thread_local! {
  static NEWEST_DRAWABLE: OnceCell<Option<EmojiVersion>> = const { OnceCell::new() };
}

/// Whether the device's font draws `entry`, given [`newest_drawable`].
pub fn can_draw(entry: &CatalogEmoji, maybe_newest: Option<EmojiVersion>) -> bool {
  maybe_newest.is_none_or(|newest| entry.version <= newest)
}

/// Whether `entry` has skin tones the device's font draws, given [`newest_drawable`].
pub fn can_draw_skin_tones(entry: &CatalogEmoji, maybe_newest: Option<EmojiVersion>) -> bool {
  match (entry.skin_tone_version(), maybe_newest) {
    (Some(version), Some(newest)) => version <= newest,
    (maybe_version, None) => maybe_version.is_some(),
    (None, Some(_)) => false,
  }
}

/// The newest emoji version the font draws in colour, measured once per page; `None` when it
/// can't be told (no colour emoji font at all, or no canvas): then everything is offered.
pub fn newest_drawable() -> Option<EmojiVersion> {
  NEWEST_DRAWABLE.with(|cell| *cell.get_or_init(measure))
}

/// Draws one emoji per version, newest first, that is a single code point (an unknown one becomes
/// a monochrome box) and returns the first version drawn in colour. Versions that only added
/// sequences count as drawable only up to the newest probed version that draws.
#[cfg(target_arch = "wasm32")]
fn measure() -> Option<EmojiVersion> {
  let probes = probes();
  let canvas = Probe::new()?;
  let (_, oldest) = probes.last()?;
  if !canvas.is_colored(oldest) {
    return None;
  }
  probes
    .iter()
    .find(|(_, emoji)| canvas.is_colored(emoji))
    .map(|(version, _)| *version)
}

#[cfg(not(target_arch = "wasm32"))]
fn measure() -> Option<EmojiVersion> {
  None
}

/// One single-code-point emoji per version, newest version first.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn probes() -> Vec<(EmojiVersion, &'static str)> {
  let mut probes: Vec<(EmojiVersion, &'static str)> = Vec::new();
  for entry in emoji_catalog::all() {
    let is_single = entry
      .emoji
      .chars()
      .filter(|character| *character != '\u{fe0f}')
      .count()
      == 1;
    if is_single && !probes.iter().any(|(version, _)| *version == entry.version) {
      probes.push((entry.version, entry.emoji));
    }
  }
  probes.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
  probes
}

#[cfg(target_arch = "wasm32")]
struct Probe {
  context: web_sys::CanvasRenderingContext2d,
}

#[cfg(target_arch = "wasm32")]
impl Probe {
  const SIDE: u32 = 24;

  fn new() -> Option<Self> {
    use wasm_bindgen::JsCast;
    let canvas = web_sys::window()?
      .document()?
      .create_element("canvas")
      .ok()?
      .dyn_into::<web_sys::HtmlCanvasElement>()
      .ok()?;
    canvas.set_width(Self::SIDE);
    canvas.set_height(Self::SIDE);
    let context = canvas
      .get_context("2d")
      .ok()??
      .dyn_into::<web_sys::CanvasRenderingContext2d>()
      .ok()?;
    context.set_font("20px sans-serif");
    context.set_text_baseline("top");
    context.set_fill_style_str("#000");
    Some(Self { context })
  }

  /// Whether `emoji` comes out with colour: a missing glyph is drawn in the black fill colour.
  fn is_colored(&self, emoji: &str) -> bool {
    let side = f64::from(Self::SIDE);
    self.context.clear_rect(0.0, 0.0, side, side);
    if self.context.fill_text(emoji, 0.0, 0.0).is_err() {
      return false;
    }
    let Ok(image) = self.context.get_image_data(0.0, 0.0, side, side) else {
      return false;
    };
    image
      .data()
      .as_chunks::<4>()
      .0
      .iter()
      .any(|&[red, green, blue, alpha]| {
        alpha > 0 && (red.abs_diff(green) > 24 || green.abs_diff(blue) > 24 || red.abs_diff(blue) > 24)
      })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn one_probe_per_version_newest_first_each_a_single_code_point() {
    let probes = probes();
    assert!(probes.len() > 10);
    assert!(probes.windows(2).all(|pair| pair[0].0 > pair[1].0));
    assert!(
      probes
        .iter()
        .all(|(_, emoji)| emoji.chars().filter(|c| *c != '\u{fe0f}').count() == 1)
    );
  }

  #[test]
  fn newer_emoji_than_the_font_are_left_out() {
    let melting = emoji_catalog::get("🫠").expect("melting face");
    assert!(can_draw(melting, None));
    assert!(can_draw(melting, Some(EmojiVersion::new(14, 0))));
    assert!(!can_draw(melting, Some(EmojiVersion::new(13, 1))));
    let wave = emoji_catalog::get("👋").expect("wave");
    assert!(can_draw_skin_tones(wave, Some(EmojiVersion::new(1, 0))));
    assert!(!can_draw_skin_tones(wave, Some(EmojiVersion::new(0, 6))));
  }
}
