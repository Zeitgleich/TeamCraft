//! Where the people go while a screen is shared (#1801): the shared screen is what everyone came
//! for, so it gets the space first. The people's strip sits beside it or below it, whichever
//! leaves the screen the larger picture, and scrolls rather than taking more room.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

/// The banner above the shared screen ("Sam is sharing their screen") and the gap below it.
const BANNER_PX: f64 = 40.0;
/// The gap between the screen and the strip (`.stage` gap).
const GAP_PX: f64 = 10.0;
/// The padding around the picture inside `.shared-screen`, on both sides.
const SCREEN_PADDING_PX: f64 = 32.0;
/// A side strip takes this share of the stage's width, within [`SIDE_MIN_PX`]..[`SIDE_MAX_PX`]:
/// tiles of 112 px still show a face and a name.
const SIDE_SHARE: f64 = 0.16;
const SIDE_MIN_PX: f64 = 112.0;
const SIDE_MAX_PX: f64 = 190.0;
/// A bottom strip takes this share of the stage's height, within
/// [`BOTTOM_MIN_PX`]..[`BOTTOM_MAX_PX`].
const BOTTOM_SHARE: f64 = 0.2;
const BOTTOM_MIN_PX: f64 = 64.0;
const BOTTOM_MAX_PX: f64 = 130.0;
/// Shared screens are mostly 16:9 (or 16:10) displays and windows.
const SCREEN_ASPECT: f64 = 16.0 / 9.0;

/// Where the people's strip goes beside a shared screen, and how much room it takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StripPlacement {
  /// A column to the right, `width_px` wide; it scrolls down.
  Side { width_px: f64 },
  /// A row below, `height_px` high; it scrolls sideways.
  Bottom { height_px: f64 },
}

/// The size of the shared screen's box (`.shared-screen`) in a stage of `width` × `height` CSS px
/// with the strip at `placement`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenBox {
  pub width: f64,
  pub height: f64,
}

impl StripPlacement {
  /// The strip's size as a CSS length, for `--strip-size`.
  pub fn css_size(self) -> String {
    let pixels = match self {
      Self::Side { width_px } => width_px,
      Self::Bottom { height_px } => height_px,
    };
    format!("{}px", pixels.round())
  }

  pub fn is_bottom(self) -> bool {
    matches!(self, Self::Bottom { .. })
  }
}

/// Where the strip goes in a stage of `width` × `height` CSS px: beside the screen or below it,
/// whichever leaves the larger 16:9 picture; beside it when both do equally (a wide window).
pub fn strip_placement(width: f64, height: f64) -> StripPlacement {
  let side = StripPlacement::Side {
    width_px: (width * SIDE_SHARE).clamp(SIDE_MIN_PX, SIDE_MAX_PX),
  };
  let bottom = StripPlacement::Bottom {
    height_px: (height * BOTTOM_SHARE).clamp(BOTTOM_MIN_PX, BOTTOM_MAX_PX),
  };
  let picture = |placement| picture_height(screen_box(width, height, placement));
  if picture(bottom) > picture(side) {
    bottom
  } else {
    side
  }
}

/// The shared screen's box with the strip at `placement`.
pub fn screen_box(width: f64, height: f64, placement: StripPlacement) -> ScreenBox {
  let below_banner = (height - BANNER_PX).max(0.0);
  match placement {
    StripPlacement::Side { width_px } => ScreenBox {
      width: (width - width_px - GAP_PX).max(0.0),
      height: below_banner,
    },
    StripPlacement::Bottom { height_px } => ScreenBox {
      width,
      height: (below_banner - height_px - GAP_PX).max(0.0),
    },
  }
}

/// How high a 16:9 picture comes out, fitted into `screen` within its padding.
fn picture_height(screen: ScreenBox) -> f64 {
  let width = (screen.width - SCREEN_PADDING_PX).max(0.0);
  let height = (screen.height - SCREEN_PADDING_PX).max(0.0);
  height.min(width / SCREEN_ASPECT)
}

/// The strip's placement for the element `stage_wrap`, following its size: `None` until it is laid
/// out. The observer goes with the component.
pub fn use_strip_placement(stage_wrap: NodeRef<leptos::html::Div>) -> Signal<Option<StripPlacement>> {
  let placement = RwSignal::new(None::<StripPlacement>);
  let observer = StoredValue::new_local(None::<(web_sys::ResizeObserver, Closure<dyn FnMut()>)>);
  Effect::new(move |_| {
    let Some(element) = stage_wrap.get() else {
      return;
    };
    let measured = element.clone();
    let on_resize = Closure::<dyn FnMut()>::new(move || {
      let width = f64::from(measured.client_width());
      let height = f64::from(measured.client_height());
      // Not laid out (hidden): keep what was placed.
      if width <= 0.0 || height <= 0.0 {
        return;
      }
      let next = strip_placement(width, height);
      if placement.try_get_untracked().flatten() != Some(next) {
        placement.try_set(Some(next));
      }
    });
    let Ok(resize) = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) else {
      return;
    };
    // The wrap's size comes from the call screen, not from the stage inside it: placing the strip
    // never resizes what is observed (no ResizeObserver loop, #1760).
    resize.observe(&element);
    if let Some(Some((previous, _))) = observer.try_update_value(|slot| slot.replace((resize, on_resize))) {
      previous.disconnect();
    }
  });
  on_cleanup(move || {
    if let Some(Some((resize, _))) = observer.try_update_value(Option::take) {
      resize.disconnect();
    }
  });
  placement.into()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Stages (the call area between the call's header and its controls) of the windows #1801 was
  /// found in and checked at: a browser window of 780×437, a small laptop window of 1024×600, a
  /// phone in landscape (844×390) and upright (390×844), and a large desktop window.
  const STAGES: [(&str, f64, f64); 6] = [
    ("780x437 window", 740.0, 268.0),
    ("1024x600 window", 984.0, 431.0),
    ("844x390 phone landscape", 804.0, 221.0),
    ("390x844 phone upright", 358.0, 640.0),
    ("1440x900 desktop", 1400.0, 700.0),
    ("2560x1440 desktop", 2520.0, 1240.0),
  ];

  /// The shared screen's box keeps at least 60 % of the call area's height at every size, and its
  /// picture nearly what it would be without the people.
  #[test]
  fn the_shared_screen_gets_most_of_the_height() {
    for (name, width, height) in STAGES {
      let placement = strip_placement(width, height);
      let screen = screen_box(width, height, placement);
      assert!(
        screen.height >= 0.6 * height,
        "{name}: {placement:?} leaves the screen {screen:?} of {height}"
      );
      // The picture loses at most a tenth to the strip: an upright phone's is as wide as the
      // phone and no higher, strip or not.
      let alone = picture_height(ScreenBox {
        width,
        height: height - BANNER_PX,
      });
      assert!(
        picture_height(screen) >= 0.9 * alone,
        "{name}: picture {} of {alone} without the strip",
        picture_height(screen)
      );
    }
  }

  /// Landscape windows put the people beside the screen, as before on a desktop; an upright phone
  /// below it.
  #[test]
  fn landscape_puts_the_strip_beside_upright_below() {
    assert!(
      !strip_placement(740.0, 268.0).is_bottom(),
      "the window #1801 was found in"
    );
    assert!(!strip_placement(804.0, 221.0).is_bottom());
    assert_eq!(
      strip_placement(1400.0, 700.0),
      StripPlacement::Side {
        width_px: SIDE_MAX_PX
      },
      "a desktop keeps its 190 px column"
    );
    assert!(strip_placement(358.0, 640.0).is_bottom());
  }

  /// The strip's size goes to CSS as whole pixels.
  #[test]
  fn the_strip_size_is_whole_pixels() {
    assert_eq!(StripPlacement::Side { width_px: 118.4 }.css_size(), "118px");
    assert_eq!(StripPlacement::Bottom { height_px: 64.0 }.css_size(), "64px");
  }
}
