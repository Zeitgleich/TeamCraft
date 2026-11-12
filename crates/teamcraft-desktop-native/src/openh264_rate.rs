//! A running OpenH264 encoder's bitrate and frame rate, changed in place (#1781): the `openh264`
//! crate builds its encoder once and offers no setter, only `SetOption` through its raw API. A new
//! encoder starts with a keyframe, and a screen share that stepped its rate under a dipping uplink
//! sent nine keyframes in seventeen pictures that way. In place, the next picture refers to the
//! last one, as WebRTC's OpenH264 wrapper does it (`SetRates`).

use std::os::raw::{c_int, c_void};
use std::ptr::addr_of_mut;

use openh264::encoder::Encoder;
use openh264_sys2::{
  ENCODER_OPTION, ENCODER_OPTION_BITRATE, ENCODER_OPTION_FRAME_RATE, ENCODER_OPTION_MAX_BITRATE,
  SBitrateInfo, SPATIAL_LAYER_ALL,
};
use thiserror::Error;

/// OpenH264's `cmResultSuccess`.
const RESULT_SUCCESS: c_int = 0;

#[derive(Debug, Error)]
pub enum OpenH264RateError {
  /// The encoder refused the option: it wasn't initialized yet (no frame encoded), or the value
  /// is out of its range.
  #[error("OpenH264 refused its {option}: code {code}")]
  Refused { option: &'static str, code: i32 },
  #[error("a bitrate of {0} bit/s is out of OpenH264's range")]
  OutOfRange(u32),
}

/// Encodes with `encoder` at `bitrate_bps` (the target and the most it may send) and `framerate`
/// from the next frame on, without a keyframe. Needs an encoder that encoded a frame (OpenH264
/// sets itself up with the first one); before that it refuses.
pub fn set_openh264_rate(
  encoder: &mut Encoder,
  bitrate_bps: u32,
  framerate: f32,
) -> Result<(), OpenH264RateError> {
  let bitrate = c_int::try_from(bitrate_bps).map_err(|_| OpenH264RateError::OutOfRange(bitrate_bps))?;
  let current = bitrate_option(encoder, ENCODER_OPTION_BITRATE)?;
  // The target never above the most: a higher one raises the most first, a lower one lowers it
  // after the target.
  if bitrate > current {
    set_bitrate_option(encoder, ENCODER_OPTION_MAX_BITRATE, "maximum bitrate", bitrate)?;
    set_bitrate_option(encoder, ENCODER_OPTION_BITRATE, "bitrate", bitrate)?;
  } else {
    set_bitrate_option(encoder, ENCODER_OPTION_BITRATE, "bitrate", bitrate)?;
    set_bitrate_option(encoder, ENCODER_OPTION_MAX_BITRATE, "maximum bitrate", bitrate)?;
  }
  let mut rate = framerate.max(1.0);
  set_option(
    encoder,
    ENCODER_OPTION_FRAME_RATE,
    "frame rate",
    addr_of_mut!(rate).cast(),
  )
}

/// The encoder's target bitrate for all its layers.
#[allow(unsafe_code)]
fn bitrate_option(encoder: &mut Encoder, option: ENCODER_OPTION) -> Result<c_int, OpenH264RateError> {
  let mut info = SBitrateInfo {
    iLayer: SPATIAL_LAYER_ALL,
    iBitrate: 0,
  };
  // SAFETY: `raw_api` hands out the encoder's own API table, valid for the encoder's lifetime;
  // `&mut Encoder` keeps anybody else from using it meanwhile. `GetOption` writes the bitrate into
  // `info`, which lives across the call, and keeps no pointer to it.
  let code = unsafe { encoder.raw_api().get_option(option, addr_of_mut!(info).cast()) };
  if code != RESULT_SUCCESS {
    return Err(OpenH264RateError::Refused {
      option: "bitrate",
      code,
    });
  }
  Ok(info.iBitrate)
}

fn set_bitrate_option(
  encoder: &mut Encoder,
  option: ENCODER_OPTION,
  name: &'static str,
  bitrate: c_int,
) -> Result<(), OpenH264RateError> {
  let mut info = SBitrateInfo {
    iLayer: SPATIAL_LAYER_ALL,
    iBitrate: bitrate,
  };
  set_option(encoder, option, name, addr_of_mut!(info).cast())
}

/// `SetOption` with `value`, of the type `option` takes.
#[allow(unsafe_code)]
fn set_option(
  encoder: &mut Encoder,
  option: ENCODER_OPTION,
  name: &'static str,
  value: *mut c_void,
) -> Result<(), OpenH264RateError> {
  // SAFETY: as in `bitrate_option`: the API table is the encoder's own and used by us alone.
  // `value` points to a live `SBitrateInfo` (the bitrate options) or `f32` (the frame rate) of
  // the caller's, which `SetOption` reads during the call and doesn't keep.
  let code = unsafe { encoder.raw_api().set_option(option, value) };
  if code != RESULT_SUCCESS {
    return Err(OpenH264RateError::Refused { option: name, code });
  }
  Ok(())
}
