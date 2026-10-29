//! The video quality menu of another device's video (#967), like a video player's: Auto, or at
//! most a number of lines (1080p, 720p, 360p) among the layers the sender offers. Auto shows the
//! quality arriving now, "Auto (720p)", from the media layer's stats. The menu itself is
//! [`crate::components::video_quality`].

use teamcraft_client_api::calls::VideoQuality;
use teamcraft_client_api::video_stats::{VideoSize, VideoStatsReport};
use teamcraft_proto::MediaStreamId;

use crate::video_stats::Words;

/// The lines of a picture as a quality menu names them: its short side (720 for "720p").
pub fn lines_of(width: u16, height: u16) -> u16 {
  width.min(height)
}

/// The menu's choices for a track whose layers are `layers` (width, height): Auto, then each
/// quality the sender offers, best first. A track whose layers are unknown offers Auto only.
pub fn quality_choices(layers: &[(u16, u16)]) -> Vec<VideoQuality> {
  let mut lines: Vec<u16> = layers
    .iter()
    .map(|(width, height)| lines_of(*width, *height))
    .collect();
  lines.sort_unstable_by(|a, b| b.cmp(a));
  lines.dedup();
  std::iter::once(VideoQuality::Auto)
    .chain(lines.into_iter().map(VideoQuality::Lines))
    .collect()
}

/// The lines of the layer of `stream_id` arriving now, as `report` tells; `None` while nothing
/// arrives or the report doesn't say.
pub fn arriving_lines(report: &VideoStatsReport, stream_id: MediaStreamId) -> Option<u16> {
  let received = report.received(stream_id)?;
  let layer = received.maybe_layer?;
  received
    .layers
    .iter()
    .find(|offered| offered.layer == layer)
    .map(|offered| {
      let VideoSize { width, height } = offered.size;
      lines_of(width, height)
    })
}

/// How a choice reads in the menu: "Auto (720p)" while Auto and the arriving quality is known,
/// "Auto" otherwise, "720p" for a fixed one. `words` translates a key with arguments.
pub fn quality_label(choice: VideoQuality, maybe_arriving_lines: Option<u16>, words: Words<'_>) -> String {
  match (choice.max_lines(), maybe_arriving_lines) {
    (Some(lines), _) => words("video-quality-lines", &[("lines", &lines.to_string())]),
    (None, Some(lines)) => words("video-quality-auto-now", &[("lines", &lines.to_string())]),
    (None, None) => words("video-quality-auto", &[]),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::video_stats::{OfferedLayer, ReceivedVideoStats, VideoTrackStats};
  use teamcraft_proto::{VideoCodec, VideoTrackKind};

  use super::*;

  const STREAM: MediaStreamId = MediaStreamId(7);

  #[test]
  fn the_menu_offers_auto_and_each_quality_sent_best_first() {
    let full_hd = [(640, 360), (1280, 720), (1920, 1080)];
    assert_eq!(
      quality_choices(&full_hd),
      vec![
        VideoQuality::Auto,
        VideoQuality::Lines(1080),
        VideoQuality::Lines(720),
        VideoQuality::Lines(360)
      ]
    );
    // A portrait phone's 720p is 720 lines too; layers of the same lines count once.
    assert_eq!(
      quality_choices(&[(360, 640), (720, 1280), (720, 1280)]),
      vec![
        VideoQuality::Auto,
        VideoQuality::Lines(720),
        VideoQuality::Lines(360)
      ]
    );
    assert_eq!(quality_choices(&[]), vec![VideoQuality::Auto]);
  }

  #[test]
  fn auto_names_the_quality_arriving_now() {
    let words = |key: &str, args: &[(&str, &str)]| match args.first() {
      Some((_, value)) => format!("{key}:{value}"),
      None => key.to_owned(),
    };
    assert_eq!(
      quality_label(VideoQuality::Auto, Some(720), &words),
      "video-quality-auto-now:720"
    );
    assert_eq!(
      quality_label(VideoQuality::Auto, None, &words),
      "video-quality-auto"
    );
    assert_eq!(
      quality_label(VideoQuality::Lines(1080), Some(720), &words),
      "video-quality-lines:1080"
    );
  }

  #[test]
  fn the_arriving_quality_comes_from_the_received_layer() {
    let report = |maybe_layer| VideoStatsReport {
      tracks: vec![VideoTrackStats::Received(ReceivedVideoStats {
        stream_id: STREAM,
        kind: VideoTrackKind::Camera,
        codec: VideoCodec::H264,
        layers: [(0, 640, 360), (1, 1280, 720), (2, 1920, 1080)]
          .into_iter()
          .map(|(layer, width, height)| OfferedLayer {
            layer,
            size: VideoSize { width, height },
          })
          .collect(),
        maybe_layer,
        maybe_max_layer: Some(2),
        packets: 0,
        bytes: 0,
        packets_lost: 0,
        frames: 0,
        frames_discarded: 0,
        maybe_jitter_ms: None,
      })],
      ..VideoStatsReport::default()
    };
    assert_eq!(arriving_lines(&report(Some(1)), STREAM), Some(720));
    assert_eq!(arriving_lines(&report(None), STREAM), None);
    assert_eq!(arriving_lines(&report(Some(2)), MediaStreamId(8)), None);
  }
}
