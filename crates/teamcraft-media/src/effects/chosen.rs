//! The UI's choice of background as the effects' [`BackgroundEffect`], for the browser's call
//! module (#439): the illustrations are drawn here; the user's own and the organization's pictures
//! come decoded from the file the page's core hands over, the photos from the file the call module
//! downloaded ([`photo_path`](super::photo_path), checked with
//! [`check_photo_file`](super::check_photo_file)). The browser decodes both
//! (`call_web::decode_picture`), so the call module carries no image decoder. The desktop maps its
//! choices itself.

use std::sync::Arc;

use teamcraft_client_api::calls::{BackgroundEffect as ChosenEffect, BackgroundPreset};

use super::{
  BackgroundEffect, BackgroundImage, BlurStrength, EffectsError, PRESET_HEIGHT, PRESET_WIDTH,
  PresetBackground,
};

/// Whether `effect` needs a picture file from the core (the user's own or the organization's).
pub fn needs_picture(effect: ChosenEffect) -> bool {
  matches!(effect, ChosenEffect::Custom(_) | ChosenEffect::Org(_))
}

/// Whether `effect` is a photo, whose picture file the call module downloads.
pub fn is_photo(effect: ChosenEffect) -> bool {
  matches!(effect, ChosenEffect::Preset(preset) if preset.is_photo())
}

/// The effect for `chosen`; `maybe_picture` is an own or organization picture or a photo, decoded
/// (it is scaled to fit the presets' size). Drawing takes a moment (tens of milliseconds): keep the result
/// while the choice stays.
pub fn chosen_background(
  chosen: ChosenEffect,
  maybe_picture: Option<BackgroundImage>,
) -> Result<BackgroundEffect, EffectsError> {
  let image = match chosen {
    ChosenEffect::None => return Ok(BackgroundEffect::None),
    ChosenEffect::BlurLight => return Ok(BackgroundEffect::Blur(BlurStrength::Light)),
    ChosenEffect::BlurStrong => return Ok(BackgroundEffect::Blur(BlurStrength::Strong)),
    ChosenEffect::Preset(preset) if !preset.is_photo() => {
      illustration(preset)?.render(PRESET_WIDTH, PRESET_HEIGHT)?
    },
    ChosenEffect::Preset(_) | ChosenEffect::Custom(_) | ChosenEffect::Org(_) => maybe_picture
      .ok_or_else(|| EffectsError::UnsupportedImage("no picture file".into()))?
      .fit_within(PRESET_WIDTH, PRESET_HEIGHT),
  };
  Ok(BackgroundEffect::Image(Arc::new(image)))
}

/// The drawn illustration behind a preset; the photos aren't drawn.
fn illustration(preset: BackgroundPreset) -> Result<PresetBackground, EffectsError> {
  Ok(match preset {
    BackgroundPreset::Office => PresetBackground::Office,
    BackgroundPreset::Bookshelf => PresetBackground::Bookshelf,
    BackgroundPreset::Sunrise => PresetBackground::Sunrise,
    BackgroundPreset::Forest => PresetBackground::Forest,
    photo => {
      return Err(EffectsError::UnsupportedImage(format!(
        "the photo {} is a file, not drawn",
        photo.slug()
      )));
    },
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn blur_and_none_need_nothing() {
    assert_eq!(
      chosen_background(ChosenEffect::None, None).unwrap(),
      BackgroundEffect::None
    );
    assert_eq!(
      chosen_background(ChosenEffect::BlurLight, None).unwrap(),
      BackgroundEffect::Blur(BlurStrength::Light)
    );
    assert_eq!(
      chosen_background(ChosenEffect::BlurStrong, None).unwrap(),
      BackgroundEffect::Blur(BlurStrength::Strong)
    );
    assert!(!needs_picture(ChosenEffect::BlurStrong));
  }

  #[test]
  fn every_illustration_is_drawn() {
    for preset in BackgroundPreset::ILLUSTRATIONS {
      assert!(!is_photo(ChosenEffect::Preset(preset)));
      let BackgroundEffect::Image(image) = chosen_background(ChosenEffect::Preset(preset), None).unwrap()
      else {
        panic!("{preset:?} is no picture");
      };
      assert_eq!((image.width(), image.height()), (PRESET_WIDTH, PRESET_HEIGHT));
    }
  }

  /// A photo goes behind as its downloaded picture; without one it can't.
  #[test]
  fn photos_need_their_picture() {
    for photo in BackgroundPreset::PHOTOS {
      let chosen = ChosenEffect::Preset(photo);
      assert!(is_photo(chosen) && !needs_picture(chosen));
      assert!(
        matches!(
          chosen_background(chosen, None),
          Err(EffectsError::UnsupportedImage(_))
        ),
        "{photo:?}"
      );
    }
    let picture = BackgroundImage::new(
      PRESET_WIDTH,
      PRESET_HEIGHT,
      vec![90; PRESET_WIDTH as usize * PRESET_HEIGHT as usize * 4],
    )
    .unwrap();
    let chosen = ChosenEffect::Preset(BackgroundPreset::Loft);
    assert_eq!(
      chosen_background(chosen, Some(picture.clone())).unwrap(),
      BackgroundEffect::Image(Arc::new(picture))
    );
  }

  #[test]
  fn own_and_organization_pictures_go_behind_scaled_to_fit() {
    let own = ChosenEffect::Custom("5".parse().unwrap());
    let org = ChosenEffect::Org("6".parse().unwrap());
    assert!(needs_picture(own) && needs_picture(org));
    let small = || BackgroundImage::new(64, 48, vec![120; 64 * 48 * 4]).unwrap();
    let large = || {
      BackgroundImage::new(
        PRESET_WIDTH * 2,
        PRESET_HEIGHT,
        vec![120; PRESET_WIDTH as usize * PRESET_HEIGHT as usize * 8],
      )
      .unwrap()
    };
    for chosen in [own, org] {
      let BackgroundEffect::Image(image) = chosen_background(chosen, Some(small())).unwrap() else {
        panic!("{chosen:?} is no picture");
      };
      assert_eq!((image.width(), image.height()), (64, 48));
      let BackgroundEffect::Image(image) = chosen_background(chosen, Some(large())).unwrap() else {
        panic!("{chosen:?} is no picture");
      };
      assert_eq!((image.width(), image.height()), (PRESET_WIDTH, PRESET_HEIGHT / 2));
      assert!(matches!(
        chosen_background(chosen, None),
        Err(EffectsError::UnsupportedImage(_))
      ));
    }
  }
}
