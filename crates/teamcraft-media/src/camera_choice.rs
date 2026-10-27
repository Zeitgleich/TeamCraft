//! Which camera opens (#819): the cameras in a documented order of preference, the chosen one
//! first with one fallback, and why a camera didn't open in terms the user can act on.
//!
//! Default order, when nothing is chosen or the chosen camera is gone: the computer's own camera,
//! then cameras plugged in (USB), then cameras the platform doesn't describe, then an iPhone or
//! iPad (Continuity Camera, which asks on the phone first), Desk View, and virtual cameras last.
//! Within a kind the platform's own order stays: AVFoundation's discovery order on macOS, Media
//! Foundation's on Windows, the V4L2 device nodes on Linux.
//!
//! The kind comes from what the platform tells. On macOS nokhwa describes a camera as
//! `<manufacturer>: <model ID> - <AVFoundation device type>, <position> f<aperture>` (a MacBook's
//! own camera: `Apple Inc.: MacBook Pro-Kamera - AVCaptureDeviceTypeBuiltInWideAngleCamera,
//! Unspecified f0`): the device type (`…BuiltInWideAngleCamera`, `…ContinuityCamera`,
//! `…DeskViewCamera`, `…External`) decides, and an iPhone or iPad is also recognised by its model
//! ID (`iPhone15,2`) or its name ("Jan’s iPhone Camera"), because nokhwa finds Continuity cameras
//! among the external ones. Windows and Linux only name their cameras, so there only virtual
//! cameras (and phones) are told apart, by well-known names.
//!
//! If the first camera doesn't open (declined on the iPhone, in use, no mode we can set), the next
//! one that can stand in is tried once; [`CameraUse`] says which one delivers, so the user learns
//! about the switch.

use std::sync::{Arc, Mutex, PoisonError};

use tracing::{info, warn};

use crate::video_capture::{CameraInfo, CameraKind, CaptureError};

/// AVFoundation device types of the computer's own cameras.
const BUILT_IN_TYPES: [&str; 4] = [
  "AVCaptureDeviceTypeBuiltInWideAngleCamera",
  "AVCaptureDeviceTypeBuiltInUltraWideCamera",
  "AVCaptureDeviceTypeBuiltInTelephotoCamera",
  "AVCaptureDeviceTypeBuiltInTrueDepthCamera",
];
const CONTINUITY_TYPE: &str = "AVCaptureDeviceTypeContinuityCamera";
const DESK_VIEW_TYPE: &str = "AVCaptureDeviceTypeDeskViewCamera";
/// `AVCaptureDeviceTypeExternal` (macOS 14) and `AVCaptureDeviceTypeExternalUnknown` (before).
const EXTERNAL_TYPE: &str = "AVCaptureDeviceTypeExternal";
/// Apple's phones and tablets, as their model IDs start (`iPhone15,2`) and their names say.
const CONTINUITY_MODELS: [&str; 2] = ["iphone", "ipad"];
/// Words in the names of cameras made by software (lowercase).
const VIRTUAL_NAMES: [&str; 7] = [
  "virtual",
  "v4l2loopback",
  "dummy video",
  "snap camera",
  "manycam",
  "xsplit",
  "nvidia broadcast",
];
/// Words in the names of Desk View cameras (English and German macOS).
const DESK_VIEW_NAMES: [&str; 2] = ["desk view", "schreibtischansicht"];

/// The camera a capture opened, and the one it stands in for when that one didn't open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraUse {
  pub camera: CameraInfo,
  pub maybe_replaced: Option<CameraInfo>,
}

/// Where a capture that opens its camera later (on its own thread) tells which one it opened.
#[derive(Debug, Clone, Default)]
pub struct CameraReport(Arc<Mutex<Option<CameraUse>>>);

/// Why a camera didn't open, as far as the platform tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCause {
  /// Another app holds it.
  InUse,
  /// It rejected the modes asked for.
  NoUsableMode,
  /// It is no longer there.
  Gone,
  /// Anything else the platform reports.
  Other,
}

impl CameraKind {
  /// From the camera's `name` and the platform's `description` of it (see the module docs).
  pub fn classify(name: &str, description: &str) -> Self {
    let name = name.to_lowercase();
    let model = description.to_lowercase();
    if description.contains(DESK_VIEW_TYPE) || DESK_VIEW_NAMES.iter().any(|word| name.contains(word)) {
      Self::DeskView
    } else if description.contains(CONTINUITY_TYPE)
      || CONTINUITY_MODELS
        .iter()
        .any(|device| name.contains(device) || model_id(&model).is_some_and(|id| id.starts_with(device)))
    {
      Self::Continuity
    } else if VIRTUAL_NAMES.iter().any(|word| name.contains(word)) {
      Self::Virtual
    } else if BUILT_IN_TYPES.iter().any(|kind| description.contains(kind)) {
      Self::BuiltIn
    } else if description.contains(EXTERNAL_TYPE) {
      Self::External
    } else {
      Self::Unknown
    }
  }

  /// Whether the camera may replace one that didn't open. Desk View shows the desk and a virtual
  /// camera whatever its software sends: neither is a stand-in for the user's face.
  pub fn can_stand_in(self) -> bool {
    !matches!(self, Self::DeskView | Self::Virtual)
  }
}

impl CameraReport {
  pub fn set(&self, used: CameraUse) {
    *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(used);
  }

  /// The camera opened last; `None` before one opened.
  pub fn get(&self) -> Option<CameraUse> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }
}

/// `cameras` in the order of preference (module docs); stable within a kind.
pub fn rank_cameras(mut cameras: Vec<CameraInfo>) -> Vec<CameraInfo> {
  cameras.sort_by_key(|camera| camera.kind);
  cameras
}

/// The cameras to try, in order: the chosen one if it is listed (else the first of `ranked`),
/// then the best other camera that can stand in for it. A choice that isn't listed any more (an
/// unplugged camera, an id from an older version) counts as no choice.
pub fn open_order(ranked: &[CameraInfo], maybe_chosen: Option<&str>) -> Vec<CameraInfo> {
  let maybe_listed = maybe_chosen.and_then(|id| ranked.iter().find(|camera| camera.id == id));
  if maybe_chosen.is_some() && maybe_listed.is_none() {
    info!("the chosen camera is not listed; using the default");
  }
  let Some(first) = maybe_listed.or(ranked.first()) else {
    return Vec::new();
  };
  let maybe_fallback = ranked
    .iter()
    .find(|camera| camera.id != first.id && camera.kind.can_stand_in());
  std::iter::once(first).chain(maybe_fallback).cloned().collect()
}

/// Opens the first camera of `order` that `open` gets going and says which one it is. Fails with
/// the first camera's error when none opens (the one the user chose, or the default), or
/// [`CaptureError::NoDevice`] when there is none.
pub fn open_first<T>(
  order: &[CameraInfo],
  mut open: impl FnMut(&CameraInfo) -> Result<T, CaptureError>,
) -> Result<(T, CameraUse), CaptureError> {
  let mut maybe_failed: Option<(&CameraInfo, CaptureError)> = None;
  for camera in order {
    match open(camera) {
      Ok(opened) => {
        let maybe_replaced = maybe_failed.map(|(failed, _)| failed.clone());
        if let Some(replaced) = &maybe_replaced {
          info!(camera = %camera.name, replaced = %replaced.name, "another camera stands in");
        }
        let used = CameraUse {
          camera: camera.clone(),
          maybe_replaced,
        };
        return Ok((opened, used));
      },
      Err(error) => {
        warn!(camera = %camera.name, %error, "camera did not open");
        if maybe_failed.is_none() {
          maybe_failed = Some((camera, error));
        }
      },
    }
  }
  Err(maybe_failed.map_or(CaptureError::NoDevice("camera"), |(_, error)| error))
}

/// The error for a camera of `kind` that didn't open for `cause`, worded for the hint the user
/// gets. An iPhone camera that doesn't come up was most likely declined on the phone (or the
/// phone is locked or away): the platform reports that like any other failure.
pub fn open_error(kind: CameraKind, cause: OpenCause, detail: String) -> CaptureError {
  match (cause, kind) {
    (OpenCause::InUse, _) => CaptureError::InUse(detail),
    (OpenCause::NoUsableMode, _) => CaptureError::NoUsableMode(detail),
    (OpenCause::Gone | OpenCause::Other, CameraKind::Continuity) => CaptureError::NotConnected(detail),
    (OpenCause::Gone, _) => CaptureError::NoDevice("camera"),
    (OpenCause::Other, _) => CaptureError::Device(detail),
  }
}

/// The model ID in a macOS description (`Apple Inc.: iPhone15,2 - AVCaptureDeviceType…`).
fn model_id(description: &str) -> Option<&str> {
  let (_, rest) = description.split_once(": ")?;
  rest.split_whitespace().next()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// As a MacBook Pro with German macOS describes its camera (a no-break space in the name).
  const BUILT_IN: &str =
    "Apple Inc.: MacBook\u{a0}Pro-Kamera - AVCaptureDeviceTypeBuiltInWideAngleCamera, Unspecified f0";
  const IPHONE: &str =
    "Apple Inc.: iPhone15,2 - AVCaptureDeviceTypeExternal, Unspecified f1.7799999713897705";
  const DESK_VIEW: &str = "Apple Inc.: iPhone15,2 - AVCaptureDeviceTypeDeskViewCamera, Unspecified f0";
  const USB: &str =
    "Logitech: UVC Camera VendorID_1133 ProductID_2085 - AVCaptureDeviceTypeExternal, Unspecified f0";

  mod kinds {
    use super::*;

    #[test]
    fn macos_cameras_by_device_type_and_model() {
      assert_eq!(
        CameraKind::classify("MacBook\u{a0}Pro-Kamera", BUILT_IN),
        CameraKind::BuiltIn
      );
      assert_eq!(
        CameraKind::classify(
          "iPhone von Jan Kamera",
          "Apple Inc.: Unknown - AVCaptureDeviceTypeExternal, Unspecified f0"
        ),
        CameraKind::Continuity,
        "a phone among the external cameras, told by its name"
      );
      assert_eq!(
        CameraKind::classify("Jan’s iPhone Camera", IPHONE),
        CameraKind::Continuity
      );
      assert_eq!(
        CameraKind::classify(
          "iPhone",
          "Apple Inc.: iPhone - AVCaptureDeviceTypeContinuityCamera, Back f0"
        ),
        CameraKind::Continuity
      );
      assert_eq!(
        CameraKind::classify("Jan’s iPhone Desk View Camera", DESK_VIEW),
        CameraKind::DeskView
      );
      assert_eq!(CameraKind::classify("Logitech BRIO", USB), CameraKind::External);
      assert_eq!(
        CameraKind::classify(
          "OBS Virtual Camera",
          "OBS Project: obs - AVCaptureDeviceTypeExternal, Unspecified f0"
        ),
        CameraKind::Virtual
      );
    }

    #[test]
    fn other_platforms_by_name_only() {
      assert_eq!(
        CameraKind::classify("Integrated Camera", "MediaFoundation Camera"),
        CameraKind::Unknown
      );
      assert_eq!(
        CameraKind::classify("OBS Virtual Camera", "MediaFoundation Camera"),
        CameraKind::Virtual
      );
      assert_eq!(
        CameraKind::classify("Dummy video device (0x0000)", ""),
        CameraKind::Virtual
      );
      assert_eq!(
        CameraKind::classify("HD Webcam: HD Webcam", ""),
        CameraKind::Unknown
      );
    }

    #[test]
    fn desk_view_and_virtual_cameras_do_not_stand_in() {
      assert!(CameraKind::BuiltIn.can_stand_in());
      assert!(CameraKind::Continuity.can_stand_in());
      assert!(!CameraKind::DeskView.can_stand_in());
      assert!(!CameraKind::Virtual.can_stand_in());
    }
  }

  mod order {
    use super::*;

    /// The case of #819: macOS listed the iPhone first.
    #[test]
    fn the_built_in_camera_is_the_default_ahead_of_the_iphone() {
      let ranked = rank_cameras(vec![
        camera("iphone", CameraKind::Continuity),
        camera("desk", CameraKind::DeskView),
        camera("obs", CameraKind::Virtual),
        camera("built-in", CameraKind::BuiltIn),
      ]);
      assert_eq!(ids(&ranked), ["built-in", "iphone", "desk", "obs"]);
      assert_eq!(ids(&open_order(&ranked, None)), ["built-in", "iphone"]);
    }

    #[test]
    fn the_platform_order_stays_within_a_kind() {
      let ranked = rank_cameras(vec![
        camera("video2", CameraKind::Unknown),
        camera("loopback", CameraKind::Virtual),
        camera("video0", CameraKind::Unknown),
      ]);
      assert_eq!(ids(&ranked), ["video2", "video0", "loopback"]);
    }

    #[test]
    fn a_remembered_choice_comes_first_with_the_default_behind_it() {
      let ranked = rank_cameras(vec![
        camera("built-in", CameraKind::BuiltIn),
        camera("usb", CameraKind::External),
        camera("iphone", CameraKind::Continuity),
      ]);
      assert_eq!(ids(&open_order(&ranked, Some("iphone"))), ["iphone", "built-in"]);
      assert_eq!(ids(&open_order(&ranked, Some("built-in"))), ["built-in", "usb"]);
    }

    #[test]
    fn a_choice_that_is_gone_falls_back_to_the_default() {
      let ranked = [
        camera("built-in", CameraKind::BuiltIn),
        camera("iphone", CameraKind::Continuity),
      ];
      // 1.0.8 stored indexes ("0") as ids on macOS.
      assert_eq!(ids(&open_order(&ranked, Some("0"))), ["built-in", "iphone"]);
    }

    #[test]
    fn only_cameras_that_can_stand_in_are_fallbacks() {
      let ranked = [
        camera("iphone", CameraKind::Continuity),
        camera("desk", CameraKind::DeskView),
        camera("obs", CameraKind::Virtual),
      ];
      assert_eq!(ids(&open_order(&ranked, None)), ["iphone"]);
      // Chosen on purpose, a virtual camera is still opened.
      assert_eq!(ids(&open_order(&ranked, Some("obs"))), ["obs", "iphone"]);
      assert!(open_order(&[], Some("obs")).is_empty());
    }
  }

  mod fallback {
    use super::*;

    /// #819: declined on the iPhone, the built-in camera takes over and says so.
    #[test]
    fn a_failing_first_camera_hands_over_to_the_next() {
      let order = [
        camera("iphone", CameraKind::Continuity),
        camera("built-in", CameraKind::BuiltIn),
      ];
      let mut tried = Vec::new();
      let (opened, used) = open_first(&order, |camera| {
        tried.push(camera.id.clone());
        match camera.kind {
          CameraKind::Continuity => Err(CaptureError::NotConnected("declined".into())),
          _ => Ok(camera.id.clone()),
        }
      })
      .unwrap();
      assert_eq!(opened, "built-in");
      assert_eq!(tried, ["iphone", "built-in"]);
      assert_eq!(used.camera.id, "built-in");
      assert_eq!(
        used.maybe_replaced.map(|camera| camera.id).as_deref(),
        Some("iphone")
      );
    }

    #[test]
    fn the_first_camera_opening_replaces_nothing() {
      let order = [
        camera("built-in", CameraKind::BuiltIn),
        camera("iphone", CameraKind::Continuity),
      ];
      let mut tried = 0;
      let (_, used) = open_first(&order, |_| {
        tried += 1;
        Ok(())
      })
      .unwrap();
      assert_eq!(tried, 1, "the fallback isn't touched");
      assert_eq!(used.camera.id, "built-in");
      assert_eq!(used.maybe_replaced, None);
    }

    #[test]
    fn when_none_opens_the_first_cameras_reason_is_reported() {
      let order = [
        camera("iphone", CameraKind::Continuity),
        camera("built-in", CameraKind::BuiltIn),
      ];
      let result = open_first(&order, |camera| -> Result<(), _> {
        match camera.kind {
          CameraKind::Continuity => Err(CaptureError::NotConnected("declined".into())),
          _ => Err(CaptureError::InUse("busy".into())),
        }
      });
      assert!(matches!(result, Err(CaptureError::NotConnected(_))));
      assert!(matches!(
        open_first(&[], |_| Ok(())),
        Err(CaptureError::NoDevice("camera"))
      ));
    }

    #[test]
    fn the_report_keeps_the_camera_opened_last() {
      let report = CameraReport::default();
      assert_eq!(report.get(), None);
      let used = CameraUse {
        camera: camera("built-in", CameraKind::BuiltIn),
        maybe_replaced: None,
      };
      report.clone().set(used.clone());
      assert_eq!(report.get(), Some(used));
    }
  }

  mod errors {
    use super::*;

    #[test]
    fn causes_map_to_what_the_user_can_do() {
      let error = |kind, cause| open_error(kind, cause, "detail".into());
      assert!(matches!(
        error(CameraKind::BuiltIn, OpenCause::InUse),
        CaptureError::InUse(_)
      ));
      assert!(matches!(
        error(CameraKind::Continuity, OpenCause::NoUsableMode),
        CaptureError::NoUsableMode(_)
      ));
      assert!(matches!(
        error(CameraKind::Continuity, OpenCause::Other),
        CaptureError::NotConnected(_)
      ));
      assert!(matches!(
        error(CameraKind::Continuity, OpenCause::Gone),
        CaptureError::NotConnected(_)
      ));
      assert!(matches!(
        error(CameraKind::External, OpenCause::Gone),
        CaptureError::NoDevice(_)
      ));
      assert!(matches!(
        error(CameraKind::Unknown, OpenCause::Other),
        CaptureError::Device(_)
      ));
    }
  }

  fn camera(id: &str, kind: CameraKind) -> CameraInfo {
    CameraInfo {
      id: id.into(),
      name: format!("{id} camera"),
      kind,
    }
  }

  fn ids(cameras: &[CameraInfo]) -> Vec<&str> {
    cameras.iter().map(|camera| camera.id.as_str()).collect()
  }
}
