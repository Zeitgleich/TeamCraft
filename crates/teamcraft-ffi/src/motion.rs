//! Live Photos and Motion Photos (issue #2181): the limits the phones hold a motion part to, and
//! cutting an Android Motion Photo into its picture and its video before both are sent.

use std::fs;
use std::io::Write;

use teamcraft_core::motion_photo::{MotionPhotoParts, locate};

use crate::error::TeamCraftError;

/// Largest picture file read to look for a video behind it: well above any camera's Motion Photo,
/// low enough that a phone holds it in memory once.
const MAX_MOTION_PHOTO_BYTES: u64 = 128 * 1024 * 1024;

/// Whether and how large a motion part may be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MotionLimitsItem {
  pub is_allowed: bool,
  pub max_bytes: u64,
}

/// What [`split_motion_photo`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MotionPhotoSplit {
  pub still_bytes: u64,
  pub video_bytes: u64,
}

/// Cuts the Motion Photo at `path` into its picture (written to `still_path`, the JPEG without the
/// video) and its MP4 (to `video_path`). `None` when the file is a plain picture, larger than a
/// Motion Photo gets, or anything the parser doesn't trust: the app sends the file as it is then.
/// Both outputs are created anew; nothing is written when there is no video.
#[uniffi::export]
pub fn split_motion_photo(
  path: String,
  still_path: String,
  video_path: String,
) -> Result<Option<MotionPhotoSplit>, TeamCraftError> {
  let size = fs::metadata(&path).map_err(io_error)?.len();
  if size > MAX_MOTION_PHOTO_BYTES {
    return Ok(None);
  }
  let file = fs::read(&path).map_err(io_error)?;
  let Some(MotionPhotoParts { still, video }) = locate(&file) else {
    return Ok(None);
  };
  write_part(&still_path, &file, still.clone())?;
  write_part(&video_path, &file, video.clone())?;
  Ok(Some(MotionPhotoSplit {
    still_bytes: still.end - still.start,
    video_bytes: video.end - video.start,
  }))
}

fn write_part(path: &str, file: &[u8], range: std::ops::Range<u64>) -> Result<(), TeamCraftError> {
  // Invariant: `locate` returns ranges inside the file it was given.
  let bytes = usize::try_from(range.start)
    .ok()
    .zip(usize::try_from(range.end).ok())
    .and_then(|(start, end)| file.get(start..end))
    .ok_or_else(|| TeamCraftError::InvalidInput("motion photo range outside the file".into()))?;
  let mut out = fs::File::create(path).map_err(io_error)?;
  out.write_all(bytes).map_err(io_error)?;
  out.sync_all().map_err(io_error)
}

fn io_error(error: std::io::Error) -> TeamCraftError {
  TeamCraftError::InvalidInput(format!("motion photo file: {error}"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_plain_picture_is_not_split_and_nothing_is_written() {
    let dir = Scratch::new("plain");
    let path = dir.path().join("plain.jpg");
    fs::write(&path, [0xFF, 0xD8, 0xFF, 0xD9]).unwrap();
    let still = dir.path().join("still.jpg");
    let video = dir.path().join("video.mp4");
    let split = split_motion_photo(
      path.display().to_string(),
      still.display().to_string(),
      video.display().to_string(),
    )
    .unwrap();
    assert_eq!(split, None);
    assert!(!still.exists() && !video.exists());
  }

  #[test]
  fn a_samsung_motion_photo_is_cut_in_two() {
    let dir = Scratch::new("samsung");
    let mut photo = vec![0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x04, 1, 2, 0xFF, 0xD9];
    let still_len = photo.len();
    photo.extend_from_slice(b"MotionPhoto_Data");
    let video = b"\x00\x00\x00\x10ftypisom\x00\x00\x02\x00\x00\x00\x00\x0Cmoov\x01\x02\x03\x04";
    photo.extend_from_slice(video);
    let path = dir.path().join("motion.jpg");
    fs::write(&path, &photo).unwrap();
    let (still, out) = (dir.path().join("still.jpg"), dir.path().join("video.mp4"));
    let split = split_motion_photo(
      path.display().to_string(),
      still.display().to_string(),
      out.display().to_string(),
    )
    .unwrap()
    .expect("split");
    assert_eq!(split.still_bytes as usize, still_len);
    assert_eq!(fs::read(&out).unwrap(), video);
    assert_eq!(fs::read(&still).unwrap(), &photo[..still_len]);
  }

  /// A directory of its own for one test, removed afterwards.
  struct Scratch(std::path::PathBuf);

  impl Scratch {
    fn new(name: &str) -> Self {
      let path = std::env::temp_dir().join(format!("teamcraft-motion-{name}-{}", std::process::id()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &std::path::Path {
      &self.0
    }
  }

  impl Drop for Scratch {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }
}
