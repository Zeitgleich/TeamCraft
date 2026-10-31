//! Which phone videos the system's GStreamer can play (#804). WebKitGTK plays `<video>` through the
//! same GStreamer installation and registry as this process, so what the registry offers here is
//! what the webview can autoplug: elements below `MARGINAL` rank (also through
//! `GST_PLUGIN_FEATURE_RANK`) are never picked by playbin and don't count.

use gstreamer as gst;

/// The container of phone videos (MP4, and QuickTime MOV from iPhones).
const MP4_CAPS: &str = "video/quicktime";
const H264_CAPS: &str = "video/x-h264";
const HEVC_CAPS: &str = "video/x-h265";

/// What the registry can autoplug for an MP4/MOV with H.264 or HEVC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackDecoders {
  pub has_mp4_demuxer: bool,
  pub has_h264_decoder: bool,
  pub has_hevc_decoder: bool,
}

/// Looks the elements up in the registry; `None` when GStreamer can't start at all.
pub fn playback_decoders() -> Option<PlaybackDecoders> {
  if let Err(error) = gst::init() {
    tracing::warn!(%error, "GStreamer is not available; videos can't play");
    return None;
  }
  let demuxers = factories(gst::ElementFactoryType::DEMUXER);
  let decoders = factories(gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_VIDEO);
  Some(PlaybackDecoders {
    has_mp4_demuxer: can_take(&demuxers, MP4_CAPS),
    has_h264_decoder: can_take(&decoders, H264_CAPS),
    has_hevc_decoder: can_take(&decoders, HEVC_CAPS),
  })
}

/// The autopluggable elements of a type.
fn factories(kind: gst::ElementFactoryType) -> Vec<gst::ElementFactory> {
  gst::ElementFactory::factories_with_type(kind, gst::Rank::MARGINAL)
    .into_iter()
    .collect()
}

fn can_take(factories: &[gst::ElementFactory], caps: &str) -> bool {
  let Ok(caps) = caps.parse::<gst::Caps>() else {
    return false;
  };
  factories.iter().any(|factory| factory.can_sink_any_caps(&caps))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unknown_caps_match_nothing() {
    if gst::init().is_err() {
      return;
    }
    let decoders = factories(gst::ElementFactoryType::DECODER);
    assert!(!can_take(&decoders, "video/x-teamcraft-none"));
  }
}
