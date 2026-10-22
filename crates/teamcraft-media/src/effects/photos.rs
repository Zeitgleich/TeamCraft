//! The built-in photo backgrounds: stock pictures that are public domain or CC0, cropped to
//! 1920x1080 and stripped of metadata by `scripts/process-backgrounds.py`. Their origin and
//! licences are in `assets/backgrounds/SOURCES.md`.
//!
//! The JPEG files are compiled in (about 3.5 MB), so only builds that show them (the desktop
//! app) pay for them; the picker's thumbnails are separate files the UI loads on demand.

use super::{BackgroundImage, EffectsError};

/// A built-in photo background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PhotoBackground {
  MeetingRoom,
  OpenPlanOffice,
  HomeOffice,
  MinimalOffice,
  LivingRoom,
  BrightRoom,
  LibraryShelves,
  Loft,
  SofaBeige,
  WhitePanels,
  Lounge,
  GreyLounge,
  OrangeWall,
  LibraryAisle,
  EmptyRoom,
  LakeMorning,
}

impl PhotoBackground {
  pub const ALL: [Self; 16] = [
    Self::MeetingRoom,
    Self::OpenPlanOffice,
    Self::HomeOffice,
    Self::MinimalOffice,
    Self::LivingRoom,
    Self::BrightRoom,
    Self::LibraryShelves,
    Self::Loft,
    Self::SofaBeige,
    Self::WhitePanels,
    Self::Lounge,
    Self::GreyLounge,
    Self::OrangeWall,
    Self::LibraryAisle,
    Self::EmptyRoom,
    Self::LakeMorning,
  ];

  /// File name (without `.jpg`) in `assets/backgrounds/`, the same identifier the UI uses.
  pub fn slug(self) -> &'static str {
    match self {
      Self::MeetingRoom => "meeting-room",
      Self::OpenPlanOffice => "open-plan-office",
      Self::HomeOffice => "home-office",
      Self::MinimalOffice => "minimal-office",
      Self::LivingRoom => "living-room",
      Self::BrightRoom => "bright-room",
      Self::LibraryShelves => "library-shelves",
      Self::Loft => "loft",
      Self::SofaBeige => "sofa-beige",
      Self::WhitePanels => "white-panels",
      Self::Lounge => "lounge",
      Self::GreyLounge => "grey-lounge",
      Self::OrangeWall => "orange-wall",
      Self::LibraryAisle => "library-aisle",
      Self::EmptyRoom => "empty-room",
      Self::LakeMorning => "lake-morning",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|photo| photo.slug() == slug)
  }

  /// The compiled-in JPEG file (1920x1080).
  pub fn jpeg(self) -> &'static [u8] {
    match self {
      Self::MeetingRoom => include_bytes!("../../../../assets/backgrounds/meeting-room.jpg"),
      Self::OpenPlanOffice => include_bytes!("../../../../assets/backgrounds/open-plan-office.jpg"),
      Self::HomeOffice => include_bytes!("../../../../assets/backgrounds/home-office.jpg"),
      Self::MinimalOffice => include_bytes!("../../../../assets/backgrounds/minimal-office.jpg"),
      Self::LivingRoom => include_bytes!("../../../../assets/backgrounds/living-room.jpg"),
      Self::BrightRoom => include_bytes!("../../../../assets/backgrounds/bright-room.jpg"),
      Self::LibraryShelves => include_bytes!("../../../../assets/backgrounds/library-shelves.jpg"),
      Self::Loft => include_bytes!("../../../../assets/backgrounds/loft.jpg"),
      Self::SofaBeige => include_bytes!("../../../../assets/backgrounds/sofa-beige.jpg"),
      Self::WhitePanels => include_bytes!("../../../../assets/backgrounds/white-panels.jpg"),
      Self::Lounge => include_bytes!("../../../../assets/backgrounds/lounge.jpg"),
      Self::GreyLounge => include_bytes!("../../../../assets/backgrounds/grey-lounge.jpg"),
      Self::OrangeWall => include_bytes!("../../../../assets/backgrounds/orange-wall.jpg"),
      Self::LibraryAisle => include_bytes!("../../../../assets/backgrounds/library-aisle.jpg"),
      Self::EmptyRoom => include_bytes!("../../../../assets/backgrounds/empty-room.jpg"),
      Self::LakeMorning => include_bytes!("../../../../assets/backgrounds/lake-morning.jpg"),
    }
  }

  /// The picture decoded and scaled down to fit `max_width` x `max_height` (a call sends at most
  /// 720p, so the compositor needs no more). Decoding takes a few milliseconds: do it once per
  /// selection, not per frame.
  pub fn decode(self, max_width: u32, max_height: u32) -> Result<BackgroundImage, EffectsError> {
    BackgroundImage::decode(self.jpeg()).map(|image| image.fit_within(max_width, max_height))
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use super::*;
  use crate::effects::{BACKGROUND_MAX_HEIGHT, BACKGROUND_MAX_WIDTH, PRESET_HEIGHT, PRESET_WIDTH};

  const MAX_FILE_BYTES: usize = 400 * 1024;

  #[test]
  fn every_photo_decodes_at_full_hd() {
    for photo in PhotoBackground::ALL {
      let image = BackgroundImage::decode(photo.jpeg()).unwrap();
      assert_eq!(
        (image.width(), image.height()),
        (BACKGROUND_MAX_WIDTH, BACKGROUND_MAX_HEIGHT),
        "{}",
        photo.slug()
      );
    }
  }

  #[test]
  fn photos_are_small_and_free_of_metadata() {
    for photo in PhotoBackground::ALL {
      let jpeg = photo.jpeg();
      assert!(
        jpeg.len() <= MAX_FILE_BYTES,
        "{} is {} bytes",
        photo.slug(),
        jpeg.len()
      );
      // EXIF and XMP live in APP1, ICC profiles in APP2, comments in COM (0xFF 0xE1/0xE2/0xFE).
      assert!(
        !jpeg
          .windows(2)
          .any(|marker| marker == [0xff, 0xe1] || marker == [0xff, 0xe2] || marker == [0xff, 0xfe]),
        "{} carries metadata",
        photo.slug()
      );
    }
  }

  #[test]
  fn slugs_are_unique_and_round_trip() {
    let slugs: HashSet<&str> = PhotoBackground::ALL.iter().map(|photo| photo.slug()).collect();
    assert_eq!(slugs.len(), PhotoBackground::ALL.len());
    for photo in PhotoBackground::ALL {
      assert_eq!(PhotoBackground::from_slug(photo.slug()), Some(photo));
    }
    assert_eq!(PhotoBackground::from_slug("forest"), None);
    assert_eq!(PhotoBackground::from_slug(""), None);
  }

  #[test]
  fn decode_scales_down_to_the_call_size_keeping_the_aspect_ratio() {
    let image = PhotoBackground::EmptyRoom
      .decode(PRESET_WIDTH, PRESET_HEIGHT)
      .unwrap();
    assert_eq!((image.width(), image.height()), (PRESET_WIDTH, PRESET_HEIGHT));
    let thumbnail = PhotoBackground::EmptyRoom.decode(160, 160).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (160, 90));
    assert!(image.rgba().chunks_exact(4).all(|pixel| pixel[3] == 255));
  }
}
