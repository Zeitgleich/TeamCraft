//! Picks the camera mode closest to what a call wants from the modes a device offers, so opening a
//! camera never fails just because the preferred pixel format or size is missing (#516: AVFoundation
//! cameras don't offer NV12 1280×720@30). Pure logic, shared by the platform backends.
//!
//! [`open_negotiated`] opens one camera through a [`CameraBackend`] and settles its mode: a mode the
//! camera rejects is not the end (#819: "Could not set device property CameraFormat with value
//! 640x480@15FPS"), the next best mode it offers is tried instead.

use tracing::{info, warn};

use crate::capture_convert::PixelFormat;
use crate::video_capture::{CameraInfo, CaptureError, CaptureFormat};

/// Pixel formats [`crate::capture_convert::RawPicture::to_i420`] converts, cheapest first. Planar
/// and semi-planar YUV need the least work, packed YUV a repack, RGB a colour conversion, MJPEG a
/// decode. (UYVY is not converted yet.)
const PIXEL_FORMAT_PREFERENCE: [PixelFormat; 6] = [
  PixelFormat::Nv12,
  PixelFormat::I420,
  PixelFormat::Yuyv,
  PixelFormat::Bgrx,
  PixelFormat::Rgbx,
  PixelFormat::Mjpeg,
];

/// Modes tried after the best one was rejected, before the camera keeps the mode it opened in.
const MODE_RETRIES: usize = 3;

/// The ways to get a camera open before its mode is chosen, in the order they are tried. Some
/// platforms set a mode while opening, and some modes they list can't be set (nokhwa's AVFoundation
/// lists both ends of a frame rate range but sets only a range's top, so "the first mode listed"
/// can be a range's bottom that it then rejects).
pub const START_MODES: [StartMode; 3] = [
  StartMode::HighestFrameRate,
  StartMode::HighestResolution,
  StartMode::FirstListed,
];

/// One mode a camera reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferedMode {
  pub pixel_format: PixelFormat,
  pub width: u32,
  pub height: u32,
  pub framerate: u32,
}

/// Which mode a camera opens in, before [`open_negotiated`] sets the one the call wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
  /// The mode with the highest frame rate (a range's top, so platforms that match ranges by
  /// their top accept it).
  HighestFrameRate,
  /// The largest mode, at its highest frame rate.
  HighestResolution,
  /// Whatever the platform lists first.
  FirstListed,
}

/// A platform's camera API as opening needs it: nokhwa (AVFoundation, Media Foundation)
/// implements it, tests fake it. Errors say why in the user's terms
/// (`camera_choice::open_error`).
pub trait CameraBackend {
  /// An opened camera.
  type Device;

  /// Opens `camera` in `start` mode, without delivering frames yet.
  fn open(&mut self, camera: &CameraInfo, start: StartMode) -> Result<Self::Device, CaptureError>;
  /// The modes the camera can be set to.
  fn modes(&mut self, device: &mut Self::Device) -> Result<Vec<OfferedMode>, CaptureError>;
  fn set_mode(&mut self, device: &mut Self::Device, mode: OfferedMode) -> Result<(), CaptureError>;
  /// Starts delivering frames.
  fn start_stream(&mut self, device: &mut Self::Device) -> Result<(), CaptureError>;
}

/// Opens `camera` and starts it in the offered mode closest to `wanted`: the first of
/// [`START_MODES`] the camera opens in, then the best ranked mode it accepts (a few are tried), else
/// the mode it opened in. Fails only when the camera doesn't open or start at all.
pub fn open_negotiated<B: CameraBackend>(
  backend: &mut B,
  camera: &CameraInfo,
  wanted: CaptureFormat,
) -> Result<B::Device, CaptureError> {
  let mut device = open_in_any_mode(backend, camera)?;
  match backend.modes(&mut device) {
    Ok(offered) => set_best_mode(backend, &mut device, camera, &offered, wanted),
    // The mode it opened in still works.
    Err(error) => warn!(camera = %camera.name, %error, "camera modes unknown; keeping the mode it opened in"),
  }
  backend.start_stream(&mut device)?;
  Ok(device)
}

/// Every offered mode we can convert, best first for the call's send target `wanted`. What a
/// camera delivers is scaled to the target before encoding, so more pixels than needed only cost
/// CPU: the smallest modes that cover the target come first, then the others from the largest
/// down. The same for the frame rate (the lowest rate at or above the target, else the highest
/// below), then the cheapest pixel format. Modes in formats we can't convert are left out.
pub fn rank_modes(offered: &[OfferedMode], wanted: CaptureFormat) -> Vec<OfferedMode> {
  let (wanted_width, wanted_height) = (
    u64::from(wanted.resolution.width),
    u64::from(wanted.resolution.height),
  );
  let wanted_rate = u64::from(wanted.framerate);
  let mut ranked: Vec<_> = offered
    .iter()
    .filter_map(|mode| {
      let rank = PIXEL_FORMAT_PREFERENCE
        .iter()
        .position(|format| *format == mode.pixel_format)?;
      let covers_size = u64::from(mode.width) >= wanted_width && u64::from(mode.height) >= wanted_height;
      let area = u64::from(mode.width) * u64::from(mode.height);
      // Covering modes by ascending area, then the others by descending area.
      let size_key = if covers_size {
        (0, area)
      } else {
        (1, u64::MAX - area)
      };
      let rate = u64::from(mode.framerate);
      let rate_key = if rate >= wanted_rate {
        (0, rate)
      } else {
        (1, u64::MAX - rate)
      };
      Some(((size_key, rate_key, rank), *mode))
    })
    .collect();
  ranked.sort_by_key(|(key, _)| *key);
  ranked.dedup_by_key(|(_, mode)| *mode);
  ranked.into_iter().map(|(_, mode)| mode).collect()
}

/// The best offered mode for `wanted` ([`rank_modes`]); `None` if we can convert none.
pub fn choose_mode(offered: &[OfferedMode], wanted: CaptureFormat) -> Option<OfferedMode> {
  rank_modes(offered, wanted).into_iter().next()
}

/// The modes AVFoundation (through nokhwa) can actually set. nokhwa lists a frame rate range
/// as both of its ends, but sets a mode only at a range's top: per size and pixel format only the
/// highest rate is kept. A lower one may be the bottom of the same range, which nokhwa rejects
/// ("Not Found/Rejected/Unsupported", #819). The call's rate is kept by dropping frames later.
pub fn range_tops(offered: &[OfferedMode]) -> Vec<OfferedMode> {
  let mut tops: Vec<OfferedMode> = Vec::new();
  for mode in offered {
    let same = |top: &&mut OfferedMode| {
      top.width == mode.width && top.height == mode.height && top.pixel_format == mode.pixel_format
    };
    match tops.iter_mut().find(same) {
      Some(top) => top.framerate = top.framerate.max(mode.framerate),
      None => tops.push(*mode),
    }
  }
  tops
}

/// The first of [`START_MODES`] the camera opens in; the first failure if none.
fn open_in_any_mode<B: CameraBackend>(
  backend: &mut B,
  camera: &CameraInfo,
) -> Result<B::Device, CaptureError> {
  let mut maybe_first_error = None;
  for start in START_MODES {
    match backend.open(camera, start) {
      Ok(device) => return Ok(device),
      Err(error) => {
        warn!(camera = %camera.name, ?start, %error, "camera did not open in this mode");
        // Only a mode the camera rejects is worth another way in; busy or gone stays so.
        let is_mode_error = matches!(error, CaptureError::NoUsableMode(_));
        maybe_first_error.get_or_insert(error);
        if !is_mode_error {
          break;
        }
      },
    }
  }
  Err(maybe_first_error.unwrap_or(CaptureError::NoDevice("camera")))
}

/// Sets the best ranked mode the camera accepts; keeps the current one when it accepts none.
fn set_best_mode<B: CameraBackend>(
  backend: &mut B,
  device: &mut B::Device,
  camera: &CameraInfo,
  offered: &[OfferedMode],
  wanted: CaptureFormat,
) {
  for mode in rank_modes(offered, wanted).into_iter().take(1 + MODE_RETRIES) {
    match backend.set_mode(device, mode) {
      Ok(()) => {
        info!(camera = %camera.name, width = mode.width, height = mode.height, framerate = mode.framerate, format = ?mode.pixel_format, "camera mode chosen");
        return;
      },
      Err(error) => warn!(camera = %camera.name, ?mode, %error, "camera refused the mode; trying the next"),
    }
  }
  warn!(camera = %camera.name, "camera refused the modes tried; using the one it opened in");
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::video::VideoResolution;

  const WANTED: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 1280,
      height: 720,
    },
    framerate: 30,
  };

  #[test]
  fn exact_match_in_another_format_beats_nothing() {
    let offered = [
      mode(PixelFormat::Yuyv, 1280, 720, 30),
      mode(PixelFormat::Yuyv, 640, 480, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn prefers_nv12_when_everything_else_ties() {
    let offered = [
      mode(PixelFormat::Mjpeg, 1280, 720, 30),
      mode(PixelFormat::Nv12, 1280, 720, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[1]));
  }

  #[test]
  fn closest_resolution_wins_over_frame_rate() {
    let offered = [
      mode(PixelFormat::Nv12, 1920, 1080, 30),
      mode(PixelFormat::Nv12, 1280, 960, 15),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[1]));
  }

  #[test]
  fn closest_frame_rate_then_higher_on_a_tie() {
    let offered = [
      mode(PixelFormat::Nv12, 1280, 720, 15),
      mode(PixelFormat::Nv12, 1280, 720, 20),
      mode(PixelFormat::Nv12, 1280, 720, 40),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[2]));
  }

  #[test]
  fn empty_list_gives_nothing() {
    assert_eq!(choose_mode(&[], WANTED), None);
  }

  #[test]
  fn only_mjpeg() {
    let offered = [
      mode(PixelFormat::Mjpeg, 1280, 720, 30),
      mode(PixelFormat::Mjpeg, 640, 480, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn only_vga_at_15() {
    let offered = [mode(PixelFormat::Yuyv, 640, 480, 15)];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn only_4k() {
    let offered = [mode(PixelFormat::Nv12, 3840, 2160, 30)];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn only_1080p_at_60() {
    let offered = [mode(PixelFormat::Mjpeg, 1920, 1080, 60)];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn raw_rgb_is_chosen_when_nothing_cheaper_exists() {
    let offered = [
      mode(PixelFormat::Mjpeg, 1280, 720, 30),
      mode(PixelFormat::Bgrx, 1280, 720, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[1]));
  }

  #[test]
  fn ranking_lists_every_convertible_mode_best_first() {
    let offered = [
      mode(PixelFormat::Mjpeg, 1920, 1080, 30),
      mode(PixelFormat::Yuyv, 1280, 720, 30),
      mode(PixelFormat::Yuyv, 640, 480, 30),
      mode(PixelFormat::Yuyv, 1280, 720, 30),
    ];
    assert_eq!(
      rank_modes(&offered, WANTED),
      [offered[1], offered[0], offered[2]],
      "covering sizes first, then the largest below; the duplicate once"
    );
  }

  #[test]
  fn range_tops_keep_the_highest_rate_per_size_and_format() {
    // nokhwa lists a 15–30 fps range as 15 and 30.
    let offered = [
      mode(PixelFormat::Yuyv, 640, 480, 15),
      mode(PixelFormat::Yuyv, 640, 480, 30),
      mode(PixelFormat::Mjpeg, 640, 480, 15),
      mode(PixelFormat::Yuyv, 1280, 720, 15),
      mode(PixelFormat::Yuyv, 1280, 720, 30),
    ];
    assert_eq!(range_tops(&offered), [offered[1], offered[2], offered[4]]);
  }

  /// Opening against a fake shaped like nokhwa's AVFoundation backend, which rejects the bottom of
  /// a frame rate range it lists itself (#819).
  mod opening {
    use super::*;
    use crate::video_capture::CameraKind;

    /// The send target of a small call layout: exactly the bottom of the camera's ranges.
    const SMALL: CaptureFormat = CaptureFormat {
      resolution: VideoResolution {
        width: 640,
        height: 480,
      },
      framerate: 15,
    };

    #[test]
    fn a_rejected_mode_falls_back_to_one_the_camera_offers() {
      let mut backend = FakeAvFoundation::with_sizes(&[(640, 480), (1280, 720)]);
      let device = open_negotiated(&mut backend, &camera(), SMALL).unwrap();
      assert_eq!(
        backend.opens,
        [StartMode::HighestFrameRate],
        "opened at a range's top, not at the first mode listed"
      );
      assert_eq!(
        backend.set,
        [
          mode(PixelFormat::Yuyv, 640, 480, 15),
          mode(PixelFormat::Yuyv, 640, 480, 30)
        ],
        "the rejected 640x480@15 is followed by an offered mode"
      );
      assert_eq!(device.mode, mode(PixelFormat::Yuyv, 640, 480, 30));
      assert!(device.is_streaming);
    }

    #[test]
    fn range_tops_are_accepted_at_once() {
      let mut backend = FakeAvFoundation::with_sizes(&[(640, 480), (1280, 720)]);
      backend.lists_tops_only = true;
      let device = open_negotiated(&mut backend, &camera(), SMALL).unwrap();
      assert_eq!(backend.set, [mode(PixelFormat::Yuyv, 640, 480, 30)]);
      assert!(device.is_streaming);
    }

    /// A MacBook Pro's own camera as nokhwa 0.10.11 lists it: every size with a 15–30 fps range,
    /// listed as both ends, so "the first mode listed" is the 640x480@15 of the report.
    #[test]
    fn the_macbook_camera_opens_at_720p30() {
      let mut backend = FakeAvFoundation::with_sizes(&[
        (640, 480),
        (1280, 720),
        (1760, 1328),
        (1328, 1760),
        (1552, 1552),
        (1920, 1080),
        (1080, 1920),
      ]);
      assert_eq!(
        backend.listed().first(),
        Some(&mode(PixelFormat::Yuyv, 640, 480, 15))
      );
      let device = open_negotiated(&mut backend, &camera(), WANTED).unwrap();
      assert_eq!(backend.set, [mode(PixelFormat::Yuyv, 1280, 720, 30)]);
      assert_eq!(device.mode, mode(PixelFormat::Yuyv, 1280, 720, 30));
    }

    #[test]
    fn opening_at_the_first_mode_listed_is_rejected_like_in_the_report() {
      let mut backend = FakeAvFoundation::with_sizes(&[(640, 480)]);
      let result = backend.open(&camera(), StartMode::FirstListed);
      assert!(
        matches!(&result, Err(CaptureError::NoUsableMode(message)) if message.contains("640x480@15FPS")),
        "the fake behaves like the camera in #819"
      );
    }

    #[test]
    fn a_camera_rejecting_the_first_start_mode_opens_in_the_next() {
      let mut backend = FakeAvFoundation::with_sizes(&[(1280, 720)]);
      backend.rejected_starts = vec![StartMode::HighestFrameRate];
      let device = open_negotiated(&mut backend, &camera(), WANTED).unwrap();
      assert_eq!(
        backend.opens,
        [StartMode::HighestFrameRate, StartMode::HighestResolution]
      );
      assert_eq!(device.mode, mode(PixelFormat::Yuyv, 1280, 720, 30));
    }

    #[test]
    fn a_camera_rejecting_every_mode_still_starts_in_the_one_it_opened_in() {
      let mut backend = FakeAvFoundation::with_sizes(&[(1280, 720)]);
      backend.rejects_every_change = true;
      let device = open_negotiated(&mut backend, &camera(), SMALL).unwrap();
      assert_eq!(device.mode, mode(PixelFormat::Yuyv, 1280, 720, 30));
      assert_eq!(backend.set.len(), 2, "both modes offered were tried");
      assert!(device.is_streaming);
    }

    #[test]
    fn a_camera_that_opens_in_no_mode_says_so() {
      let mut backend = FakeAvFoundation::with_sizes(&[(1280, 720)]);
      backend.rejected_starts = START_MODES.to_vec();
      let result = open_negotiated(&mut backend, &camera(), WANTED);
      assert!(matches!(result, Err(CaptureError::NoUsableMode(_))));
      assert_eq!(backend.opens, START_MODES, "every way in was tried");
    }

    #[test]
    fn a_busy_camera_is_not_tried_in_other_modes() {
      let mut backend = FakeAvFoundation::with_sizes(&[(1280, 720)]);
      backend.is_busy = true;
      let result = open_negotiated(&mut backend, &camera(), WANTED);
      assert!(matches!(result, Err(CaptureError::InUse(_))));
      assert_eq!(backend.opens, [StartMode::HighestFrameRate]);
    }

    /// YUYV ranges from 15 to 30 fps per size, listed as both ends like nokhwa does.
    #[derive(Default)]
    struct FakeAvFoundation {
      sizes: Vec<(u32, u32)>,
      lists_tops_only: bool,
      rejected_starts: Vec<StartMode>,
      rejects_every_change: bool,
      is_busy: bool,
      opens: Vec<StartMode>,
      set: Vec<OfferedMode>,
    }

    struct FakeDevice {
      mode: OfferedMode,
      is_streaming: bool,
    }

    impl FakeAvFoundation {
      fn with_sizes(sizes: &[(u32, u32)]) -> Self {
        Self {
          sizes: sizes.to_vec(),
          ..Self::default()
        }
      }

      fn listed(&self) -> Vec<OfferedMode> {
        let rates: &[u32] = if self.lists_tops_only { &[30] } else { &[15, 30] };
        self
          .sizes
          .iter()
          .flat_map(|&(width, height)| {
            rates
              .iter()
              .map(move |&rate| mode(PixelFormat::Yuyv, width, height, rate))
          })
          .collect()
      }

      /// Only a range's top can be set.
      fn accept(mode: OfferedMode) -> Result<(), CaptureError> {
        if mode.framerate == 30 {
          return Ok(());
        }
        Err(CaptureError::NoUsableMode(format!(
          "Could not set device property CameraFormat with value {}x{}@{}FPS, YUYV Format: Not Found/Rejected/Unsupported",
          mode.width, mode.height, mode.framerate
        )))
      }
    }

    impl CameraBackend for FakeAvFoundation {
      type Device = FakeDevice;

      fn open(&mut self, _camera: &CameraInfo, start: StartMode) -> Result<FakeDevice, CaptureError> {
        self.opens.push(start);
        if self.is_busy {
          return Err(CaptureError::InUse("Already in use".into()));
        }
        if self.rejected_starts.contains(&start) {
          return Err(CaptureError::NoUsableMode("Cannot fulfill request".into()));
        }
        let listed = self.listed();
        let maybe_pick = match start {
          StartMode::HighestFrameRate => listed.iter().max_by_key(|mode| (mode.framerate, mode.width)),
          StartMode::HighestResolution => listed.iter().max_by_key(|mode| (mode.width, mode.framerate)),
          StartMode::FirstListed => listed.first(),
        };
        let mode = *maybe_pick.ok_or_else(|| CaptureError::NoUsableMode("nothing listed".into()))?;
        Self::accept(mode)?;
        Ok(FakeDevice {
          mode,
          is_streaming: false,
        })
      }

      fn modes(&mut self, _device: &mut FakeDevice) -> Result<Vec<OfferedMode>, CaptureError> {
        Ok(self.listed())
      }

      fn set_mode(&mut self, device: &mut FakeDevice, mode: OfferedMode) -> Result<(), CaptureError> {
        self.set.push(mode);
        if self.rejects_every_change {
          return Err(CaptureError::NoUsableMode("Lock Rejected".into()));
        }
        Self::accept(mode)?;
        device.mode = mode;
        Ok(())
      }

      fn start_stream(&mut self, device: &mut FakeDevice) -> Result<(), CaptureError> {
        device.is_streaming = true;
        Ok(())
      }
    }

    fn camera() -> CameraInfo {
      CameraInfo {
        id: "built-in".into(),
        name: "Built-in Camera".into(),
        kind: CameraKind::BuiltIn,
      }
    }
  }

  fn mode(pixel_format: PixelFormat, width: u32, height: u32, framerate: u32) -> OfferedMode {
    OfferedMode {
      pixel_format,
      width,
      height,
      framerate,
    }
  }
}
