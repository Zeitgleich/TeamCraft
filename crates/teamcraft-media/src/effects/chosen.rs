//! The UI's choice of background as the effects' [`BackgroundEffect`], for the browser's call
//! module (#439): the illustrations are drawn here, the user's own and the organization's pictures
//! decoded from the file the page's core hands over. The photos are only in the apps
//! (`photo-backgrounds`); the desktop maps its choices itself.

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

/// The effect for `chosen`; `maybe_picture` is the file of an own or organization picture.
/// Decoding and drawing take a moment (tens of milliseconds): keep the result while the choice
/// stays.
pub fn chosen_background(
  chosen: ChosenEffect,
  maybe_picture: Option<&[u8]>,
) -> Result<BackgroundEffect, EffectsError> {
  let image = match chosen {
    ChosenEffect::None => return Ok(BackgroundEffect::None),
    ChosenEffect::BlurLight => return Ok(BackgroundEffect::Blur(BlurStrength::Light)),
    ChosenEffect::BlurStrong => return Ok(BackgroundEffect::Blur(BlurStrength::Strong)),
    ChosenEffect::Preset(preset) => illustration(preset)?.render(PRESET_WIDTH, PRESET_HEIGHT)?,
    ChosenEffect::Custom(_) | ChosenEffect::Org(_) => {
      let picture = maybe_picture.ok_or_else(|| EffectsError::UnsupportedImage("no picture file".into()))?;
      BackgroundImage::decode(picture)?.fit_within(PRESET_WIDTH, PRESET_HEIGHT)
    },
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
        "the photo {} is only in the apps",
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
  fn every_illustration_is_drawn_and_photos_are_refused() {
    for preset in BackgroundPreset::ILLUSTRATIONS {
      let BackgroundEffect::Image(image) = chosen_background(ChosenEffect::Preset(preset), None).unwrap()
      else {
        panic!("{preset:?} is no picture");
      };
      assert_eq!((image.width(), image.height()), (PRESET_WIDTH, PRESET_HEIGHT));
    }
    for photo in BackgroundPreset::PHOTOS {
      assert!(
        matches!(
          chosen_background(ChosenEffect::Preset(photo), None),
          Err(EffectsError::UnsupportedImage(_))
        ),
        "{photo:?}"
      );
    }
  }

  #[test]
  fn own_and_organization_pictures_are_decoded_from_their_file() {
    let own = ChosenEffect::Custom("5".parse().unwrap());
    let org = ChosenEffect::Org("6".parse().unwrap());
    assert!(needs_picture(own) && needs_picture(org));
    let file = BackgroundImage::new(64, 48, vec![120; 64 * 48 * 4])
      .unwrap()
      .to_jpeg(80)
      .unwrap();
    for chosen in [own, org] {
      let BackgroundEffect::Image(image) = chosen_background(chosen, Some(&file)).unwrap() else {
        panic!("{chosen:?} is no picture");
      };
      assert_eq!((image.width(), image.height()), (64, 48));
      assert!(matches!(
        chosen_background(chosen, None),
        Err(EffectsError::UnsupportedImage(_))
      ));
      assert!(chosen_background(chosen, Some(b"not a picture")).is_err());
    }
  }
}
