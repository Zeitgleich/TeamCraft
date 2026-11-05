//! The built-in photo backgrounds for the browser's call module (#439). The apps compile the
//! pictures in (`photo-backgrounds`); the browser downloads one when it is picked. The web release
//! serves each as `backgrounds/<slug>-<hash>.jpg` (`crates/teamcraft-ui/scripts/add-background-photos.sh`,
//! the hash is the first 16 hex digits of the file's SHA-256), and the service worker fetches them
//! only when asked (`on_demand` in `release.json`). The call module checks every download against the
//! SHA-256 pinned here, so only the reviewed pictures go behind the camera, also where no signed
//! release checks the files.
//!
//! A picture `scripts/process-backgrounds.py` writes anew needs its pin updated here (the tests
//! compare the pins with `assets/backgrounds/`).

use sha2::{Digest, Sha256};
use teamcraft_client_api::calls::BackgroundPreset;

use super::EffectsError;

/// Hex digits of the hash in a photo's file name.
const NAME_HASH_DIGITS: usize = 16;

/// The SHA-256 of `assets/backgrounds/<slug>.jpg` for each photo.
fn pinned_sha256(photo: BackgroundPreset) -> Option<&'static str> {
  Some(match photo {
    BackgroundPreset::MeetingRoom => "1bae0cb40aa3d8bfb6e1964e1215febbecc6dea0f12e4ec2156c99199515489f",
    BackgroundPreset::OpenPlanOffice => "1fe1f654820255ba8994261d8de83ac9fdc5abf6d9aab67bcc44abef509bac3c",
    BackgroundPreset::HomeOffice => "1ac58ef4ec6df76990258fcda197435e4a1e460691c1a34dcaa8d7b4e8f330c3",
    BackgroundPreset::MinimalOffice => "1bc82b11dfb36ad966527337953c6550eb035eebefd34b13d7dc3ac523cabf01",
    BackgroundPreset::LivingRoom => "97d4ee2835800f5efd4c1d231b9ab8f064982b423fe7e7376b60a5809d5f9a51",
    BackgroundPreset::BrightRoom => "505d6b5849ae7181eea6bde4600eff24feddcf2ef9ceb2e6d7c7c7c58b0f34fe",
    BackgroundPreset::LibraryShelves => "18902a28620bd255e7c5874eddd3a50474f5482bc4d429752fb1ba6f8bc54276",
    BackgroundPreset::Loft => "128f96b9672a063647ae0d5a926f078c50efa36c4835d74dacf6e751ea8219db",
    BackgroundPreset::SofaBeige => "fec495424b79bb610ca1bb50ca0023d626c27ac754d2cdca6119ee8da82f4923",
    BackgroundPreset::WhitePanels => "461dc43f439db7c85648a50ba758b3cda4e735b5dc4e9491d81b0be5728800af",
    BackgroundPreset::Lounge => "a883b0d884ad19767cc4617ee299ee12d8f96e16506a38086148ab88bc3c2ee8",
    BackgroundPreset::GreyLounge => "2b11a959b42cdae148a02e6eda71d20c1cd391079650d52af3a9917f5ce21790",
    BackgroundPreset::OrangeWall => "1425f192e470f22f24ce29a1ef5124b95665b59e11468008365a31a21b2db161",
    BackgroundPreset::LibraryAisle => "81f4b88e2eb3f97ee822e0ddd339c13a3e09b5baf54ab7cd47f5c77781717580",
    BackgroundPreset::EmptyRoom => "16e7056ab68175db9820ee75eb4ee5ce14a92882376ba115bf8e31a361dbf493",
    BackgroundPreset::LakeMorning => "7655021ec7762fb515e919d7407ef81e24b66b83c72dda103c7b8d3290481ec7",
    BackgroundPreset::Office
    | BackgroundPreset::Bookshelf
    | BackgroundPreset::Sunrise
    | BackgroundPreset::Forest => {
      return None;
    },
  })
}

/// Where the web release serves `photo` (a path of the page's origin); none for the drawn
/// illustrations.
pub fn photo_path(photo: BackgroundPreset) -> Option<String> {
  let pinned = pinned_sha256(photo)?;
  Some(format!(
    "/backgrounds/{}-{}.jpg",
    photo.slug(),
    &pinned[..NAME_HASH_DIGITS]
  ))
}

/// Checks a downloaded `file` against the pinned SHA-256 of `photo`.
pub fn check_photo_file(photo: BackgroundPreset, file: &[u8]) -> Result<(), EffectsError> {
  let pinned = pinned_sha256(photo)
    .ok_or_else(|| EffectsError::UnsupportedImage(format!("{} is no photo", photo.slug())))?;
  if hex(&Sha256::digest(file)) == pinned {
    Ok(())
  } else {
    Err(EffectsError::UnsupportedImage(format!(
      "the photo {} is not the one this build pins",
      photo.slug()
    )))
  }
}

fn hex(bytes: &[u8]) -> String {
  use std::fmt::Write as _;
  bytes
    .iter()
    .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
      let _ = write!(text, "{byte:02x}");
      text
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn asset(photo: BackgroundPreset) -> Vec<u8> {
    let path = format!(
      "{}/../../assets/backgrounds/{}.jpg",
      env!("CARGO_MANIFEST_DIR"),
      photo.slug()
    );
    std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
  }

  /// The pins are the committed pictures: a picture processed anew needs its pin here.
  #[test]
  fn every_photo_is_pinned_to_its_file() {
    for photo in BackgroundPreset::PHOTOS {
      check_photo_file(photo, &asset(photo)).unwrap_or_else(|error| panic!("{photo:?}: {error}"));
    }
  }

  /// The name add-background-photos.sh gives the file: the slug and the hash's first 16 digits.
  #[test]
  fn photos_are_served_under_a_hashed_name_of_the_origin() {
    let path = photo_path(BackgroundPreset::LivingRoom).unwrap();
    assert_eq!(path, "/backgrounds/living-room-97d4ee2835800f5e.jpg");
    for photo in BackgroundPreset::PHOTOS {
      let path = photo_path(photo).unwrap();
      let digest = hex(&Sha256::digest(asset(photo)));
      assert_eq!(
        path,
        format!(
          "/backgrounds/{}-{}.jpg",
          photo.slug(),
          &digest[..NAME_HASH_DIGITS]
        )
      );
    }
    for illustration in BackgroundPreset::ILLUSTRATIONS {
      assert_eq!(photo_path(illustration), None);
    }
  }

  #[test]
  fn other_files_are_refused() {
    let mut file = asset(BackgroundPreset::Loft);
    assert!(check_photo_file(BackgroundPreset::Lounge, &file).is_err());
    file[100] ^= 1;
    assert!(check_photo_file(BackgroundPreset::Loft, &file).is_err());
    assert!(check_photo_file(BackgroundPreset::Loft, &[]).is_err());
    assert!(check_photo_file(BackgroundPreset::Forest, &file).is_err());
  }
}
