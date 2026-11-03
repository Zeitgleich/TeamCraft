//! "Stats for nerds" (#938): the rows of the overlay on a call's video tiles, from the media
//! layer's [`VideoStatsReport`]s. The counters of two reports a second apart give the rates; a
//! value the platform doesn't tell shows as [`MISSING`]. The overlay
//! ([`crate::components::video_stats`]) shows the rows and copies them as text for bug reports.

use std::time::Duration;

use teamcraft_client_api::video_stats::{
  EncoderImplementation, ReceivedVideoStats, SentVideoStats, VideoSize, VideoStatsReport,
};
use teamcraft_proto::{MediaStreamId, VideoCodec, VideoTrackKind};

/// What a value the platform can't tell shows.
pub const MISSING: &str = "–";

/// Translates a message with arguments (the overlay's [`crate::i18n::I18n::t_with`]).
pub type Words<'a> = &'a dyn Fn(&str, &[(&str, &str)]) -> String;

/// Which video a tile shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatsTarget {
  OwnCamera,
  OwnScreen,
  Received(MediaStreamId),
}

/// The last two reports; the rates come from their difference.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsWindow {
  maybe_before: Option<VideoStatsReport>,
  maybe_now: Option<VideoStatsReport>,
}

/// How many pictures a tile draws per second, from its count of pictures drawn looked at now and
/// then. Time is passed in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DrawnRate {
  maybe_last: Option<(u64, Duration)>,
  maybe_fps: Option<f64>,
}

/// What a tile measures itself: how often it draws and how large the pictures are.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TileMeasure {
  pub maybe_drawn_fps: Option<f64>,
  pub maybe_drawn_size: Option<VideoSize>,
}

/// One line of the overlay: a message key for the label, and the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatsRow {
  pub label_key: &'static str,
  pub value: String,
}

/// A track in the two reports of a [`StatsWindow`]: now, and a moment before when it was there.
struct Sample<'a, T> {
  now: &'a T,
  maybe_before: Option<&'a T>,
  seconds: f64,
}

impl StatsWindow {
  /// Takes a new report. One whose clock is not ahead of the last (a new call) starts over.
  pub fn push(&mut self, report: VideoStatsReport) {
    let is_later = self
      .maybe_now
      .as_ref()
      .is_some_and(|now| report.at_ms > now.at_ms);
    self.maybe_before = if is_later { self.maybe_now.take() } else { None };
    self.maybe_now = Some(report);
  }

  pub fn clear(&mut self) {
    *self = Self::default();
  }

  /// Seconds between the two reports; `None` with fewer than two.
  fn seconds(&self) -> Option<f64> {
    let (before, now) = (self.maybe_before.as_ref()?, self.maybe_now.as_ref()?);
    Some(now.at_ms.saturating_sub(before.at_ms) as f64 / 1000.0)
  }

  fn sent(&self, kind: &VideoTrackKind) -> Option<Sample<'_, SentVideoStats>> {
    let now = self.maybe_now.as_ref()?.sent(kind)?;
    // A new stream (the camera restarted) counts from zero: no rate across it.
    let maybe_before = self
      .maybe_before
      .as_ref()
      .and_then(|before| before.sent(kind))
      .filter(|before| before.stream_id == now.stream_id);
    Some(Sample {
      now,
      maybe_before,
      seconds: self.seconds().unwrap_or_default(),
    })
  }

  fn received(&self, stream_id: MediaStreamId) -> Option<Sample<'_, ReceivedVideoStats>> {
    let now = self.maybe_now.as_ref()?.received(stream_id)?;
    let maybe_before = self
      .maybe_before
      .as_ref()
      .and_then(|before| before.received(stream_id));
    Some(Sample {
      now,
      maybe_before,
      seconds: self.seconds().unwrap_or_default(),
    })
  }
}

impl DrawnRate {
  /// Takes the count of pictures drawn so far, read at `at`.
  pub fn observe(&mut self, count: u64, at: Duration) {
    if let Some((last_count, last_at)) = self.maybe_last {
      let seconds = at.saturating_sub(last_at).as_secs_f64();
      self.maybe_fps = (seconds > 0.0 && count >= last_count).then(|| (count - last_count) as f64 / seconds);
    }
    self.maybe_last = Some((count, at));
  }

  pub fn fps(&self) -> Option<f64> {
    self.maybe_fps
  }
}

impl<T> Sample<'_, T> {
  /// Per second between the two reports, of a counter read from either.
  fn rate(&self, counter: impl Fn(&T) -> u64) -> Option<f64> {
    let before = counter(self.maybe_before?);
    let now = counter(self.now);
    (self.seconds > 0.0 && now >= before).then(|| (now - before) as f64 / self.seconds)
  }

  /// How much a counter grew between the two reports.
  fn growth(&self, counter: impl Fn(&T) -> u64) -> Option<u64> {
    counter(self.now).checked_sub(counter(self.maybe_before?))
  }
}

/// The overlay's rows for `target`; `None` while the media layer reports no such track.
pub fn stats_rows(
  window: &StatsWindow,
  target: StatsTarget,
  measure: TileMeasure,
  words: Words<'_>,
) -> Option<Vec<StatsRow>> {
  let report = window.maybe_now.as_ref()?;
  match target {
    StatsTarget::OwnCamera => Some(sent_rows(report, &window.sent(&VideoTrackKind::Camera)?, words)),
    StatsTarget::OwnScreen => Some(sent_rows(report, &window.sent(&VideoTrackKind::Screen)?, words)),
    StatsTarget::Received(stream_id) => Some(received_rows(
      report,
      &window.received(stream_id)?,
      measure,
      words,
    )),
  }
}

/// The rows as text for the clipboard: the title, then one aligned line per row.
pub fn stats_text(title: &str, rows: &[StatsRow], label: impl Fn(&str) -> String) -> String {
  let labels: Vec<String> = rows.iter().map(|row| label(row.label_key)).collect();
  let width = labels
    .iter()
    .map(|label| label.chars().count())
    .max()
    .unwrap_or(0);
  let mut text = title.to_owned();
  for (label, row) in labels.iter().zip(rows) {
    let padding = width - label.chars().count();
    text.push_str(&format!("\n{label}{:padding$}  {}", "", row.value));
  }
  text
}

fn sent_rows(
  report: &VideoStatsReport,
  sample: &Sample<'_, SentVideoStats>,
  words: Words<'_>,
) -> Vec<StatsRow> {
  let sent = sample.now;
  let maybe_top = sent.top_active_layer(sample.maybe_before);
  let top_layer = maybe_top.map(|layer| layer.layer);
  let layer_pictures = |stats: &SentVideoStats| {
    stats
      .layers
      .iter()
      .find(|layer| Some(layer.layer) == top_layer)
      .map_or(0, |layer| layer.pictures)
  };
  let sent_fps = maybe_top.and_then(|_| sample.rate(layer_pictures));
  let capture_fps = sample.rate(|stats| stats.maybe_frames_captured.unwrap_or_default());
  let capture_fps = capture_fps.filter(|_| sent.maybe_frames_captured.is_some());
  let active = sample
    .maybe_before
    .map(|before| layer_list(sent.active_layers(Some(before)).iter().map(|layer| layer.layer)));
  let target = layer_list(
    sent
      .layers
      .iter()
      .filter(|layer| layer.is_target)
      .map(|layer| layer.layer),
  );
  let encoder = sent.maybe_encoder.as_ref().map_or_else(
    || MISSING.to_owned(),
    |encoder| {
      let key = match encoder.implementation {
        EncoderImplementation::Hardware => "stats-encoder-hardware",
        EncoderImplementation::Software => "stats-encoder-software",
        EncoderImplementation::Browser => "stats-encoder-browser",
      };
      words(key, &[("name", &encoder.name)])
    },
  );
  let uplink = report.maybe_uplink;
  vec![
    row(
      "stats-resolution",
      words(
        "stats-value-sent-resolution",
        &[
          ("sent", &or_missing(maybe_top.map(|layer| size_text(layer.size)))),
          ("capture", &or_missing(sent.maybe_capture.map(size_text))),
        ],
      ),
    ),
    row(
      "stats-frame-rate",
      words(
        "stats-value-sent-frame-rate",
        &[
          ("fps", &or_missing(sent_fps.map(fps_text))),
          ("capture", &or_missing(capture_fps.map(fps_text))),
        ],
      ),
    ),
    row(
      "stats-bitrate",
      words(
        "stats-value-sent-bitrate",
        &[
          (
            "rate",
            &or_missing(sample.rate(|stats| stats.bytes * 8).map(bitrate_text)),
          ),
          (
            "estimate",
            &or_missing(uplink.map(|uplink| bitrate_text(f64::from(uplink.estimate_bps)))),
          ),
        ],
      ),
    ),
    row("stats-codec", codec_text(&sent.codec)),
    row("stats-encoder", encoder),
    row(
      "stats-layers",
      words(
        "stats-value-sent-layers",
        &[("active", &or_missing(active)), ("target", &target)],
      ),
    ),
    row(
      "stats-loss",
      words(
        "stats-value-uplink-loss",
        &[(
          "loss",
          &or_missing(uplink.map(|uplink| percent_text(f64::from(uplink.loss_percent)))),
        )],
      ),
    ),
    row("stats-network", network_text(report.maybe_rtt_ms, None, words)),
    row(
      "stats-frames",
      words(
        "stats-value-frames",
        &[
          ("dropped", &sent.pictures_dropped.to_string()),
          (
            "black",
            &or_missing(sent.maybe_frames_black.map(|black| black.to_string())),
          ),
        ],
      ),
    ),
  ]
}

fn received_rows(
  report: &VideoStatsReport,
  sample: &Sample<'_, ReceivedVideoStats>,
  measure: TileMeasure,
  words: Words<'_>,
) -> Vec<StatsRow> {
  let received = sample.now;
  let layer_size = received.maybe_layer.and_then(|current| {
    received
      .layers
      .iter()
      .find(|offered| offered.layer == current)
      .map(|offered| offered.size)
  });
  let loss = sample
    .growth(|stats| stats.packets_lost)
    .zip(sample.growth(|stats| stats.packets))
    .map(|(lost, arrived)| {
      let expected = lost + arrived;
      if expected == 0 {
        0.0
      } else {
        lost as f64 * 100.0 / expected as f64
      }
    });
  vec![
    row(
      "stats-resolution",
      or_missing(measure.maybe_drawn_size.or(layer_size).map(size_text)),
    ),
    row(
      "stats-frame-rate",
      words(
        "stats-value-received-frame-rate",
        &[
          (
            "fps",
            &or_missing(sample.rate(|stats| stats.frames).map(fps_text)),
          ),
          ("drawn", &or_missing(measure.maybe_drawn_fps.map(fps_text))),
        ],
      ),
    ),
    row(
      "stats-bitrate",
      or_missing(sample.rate(|stats| stats.bytes * 8).map(bitrate_text)),
    ),
    row("stats-codec", codec_text(&received.codec)),
    row(
      "stats-layer",
      words(
        "stats-value-received-layer",
        &[
          (
            "layer",
            &or_missing(received.maybe_layer.map(|layer| layer.to_string())),
          ),
          (
            "offered",
            &layer_list(received.layers.iter().map(|offered| offered.layer)),
          ),
          (
            "max",
            &or_missing(received.maybe_max_layer.map(|layer| layer.to_string())),
          ),
        ],
      ),
    ),
    row("stats-loss", or_missing(loss.map(percent_text))),
    row(
      "stats-network",
      network_text(report.maybe_rtt_ms, received.maybe_jitter_ms, words),
    ),
    row(
      "stats-frames",
      words(
        "stats-value-frames",
        &[
          ("dropped", &received.frames_discarded.to_string()),
          ("black", MISSING),
        ],
      ),
    ),
  ]
}

fn row(label_key: &'static str, value: String) -> StatsRow {
  StatsRow { label_key, value }
}

fn network_text(maybe_rtt_ms: Option<u32>, maybe_jitter_ms: Option<f32>, words: Words<'_>) -> String {
  words(
    "stats-value-network",
    &[
      ("rtt", &or_missing(maybe_rtt_ms.map(|rtt| format!("{rtt} ms")))),
      (
        "jitter",
        &or_missing(maybe_jitter_ms.map(|jitter| format!("{jitter:.1} ms"))),
      ),
    ],
  )
}

fn or_missing(maybe_value: Option<String>) -> String {
  maybe_value.unwrap_or_else(|| MISSING.to_owned())
}

fn size_text(size: VideoSize) -> String {
  format!("{}×{}", size.width, size.height)
}

fn fps_text(fps: f64) -> String {
  format!("{fps:.1}")
}

fn percent_text(percent: f64) -> String {
  format!("{percent:.1} %")
}

/// `850 kbit/s` below a megabit, `1.25 Mbit/s` above.
fn bitrate_text(bps: f64) -> String {
  if bps < 1_000_000.0 {
    format!("{:.0} kbit/s", bps / 1000.0)
  } else {
    format!("{:.2} Mbit/s", bps / 1_000_000.0)
  }
}

fn codec_text(codec: &VideoCodec) -> String {
  match codec {
    VideoCodec::H264 => "H.264".to_owned(),
    VideoCodec::Vp9 => "VP9".to_owned(),
    VideoCodec::Av1 => "AV1".to_owned(),
    VideoCodec::Unknown(name) => name.clone(),
  }
}

/// `0, 1, 2`; [`MISSING`] for none.
fn layer_list(layers: impl Iterator<Item = u8>) -> String {
  let listed: Vec<String> = layers.map(|layer| layer.to_string()).collect();
  if listed.is_empty() {
    MISSING.to_owned()
  } else {
    listed.join(", ")
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::video_stats::{
    OfferedLayer, SentLayerStats, UplinkStats, VideoEncoderInfo, VideoTrackStats,
  };

  use super::*;
  use crate::i18n::{I18n, Locale};

  const REMOTE: MediaStreamId = MediaStreamId(12);

  mod rates {
    use super::*;

    #[test]
    fn an_own_camera_shows_what_its_top_layer_sends_and_the_encoder() {
      let mut window = StatsWindow::default();
      window.push(report(
        1_000,
        sent_camera(5, [(30, 3_000), (30, 9_000), (0, 0)], 30),
        None,
      ));
      window.push(report(
        2_000,
        sent_camera(5, [(60, 6_000), (60, 34_000), (0, 0)], 60),
        None,
      ));
      let rows = english(&window, StatsTarget::OwnCamera);
      assert_eq!(value(&rows, "stats-resolution"), "640×360 (capture 1920×1080)");
      assert_eq!(value(&rows, "stats-frame-rate"), "30.0 fps (capture 30.0)");
      // (6000 + 34000) - (3000 + 9000) bytes in a second.
      assert_eq!(value(&rows, "stats-bitrate"), "224 kbit/s (estimate 2.50 Mbit/s)");
      assert_eq!(value(&rows, "stats-codec"), "H.264");
      assert_eq!(value(&rows, "stats-encoder"), "vtenc_h264 (hardware)");
      assert_eq!(value(&rows, "stats-layers"), "0, 1 (target 0, 1)");
      assert_eq!(value(&rows, "stats-loss"), "2.0 % (uplink)");
      assert_eq!(value(&rows, "stats-network"), "RTT 34 ms · jitter –");
      assert_eq!(value(&rows, "stats-frames"), "dropped 4 · black 7");
    }

    #[test]
    fn a_first_report_has_no_rates_yet() {
      let mut window = StatsWindow::default();
      window.push(report(
        1_000,
        sent_camera(5, [(30, 3_000), (0, 0), (0, 0)], 30),
        None,
      ));
      let rows = english(&window, StatsTarget::OwnCamera);
      assert_eq!(value(&rows, "stats-frame-rate"), "– fps (capture –)");
      assert_eq!(value(&rows, "stats-bitrate"), "– (estimate 2.50 Mbit/s)");
      assert_eq!(value(&rows, "stats-layers"), "– (target 0, 1)");
      // The layer that sent anything so far stands for the size.
      assert_eq!(value(&rows, "stats-resolution"), "320×180 (capture 1920×1080)");
    }

    #[test]
    fn a_restarted_camera_starts_its_rates_over() {
      let mut window = StatsWindow::default();
      window.push(report(
        1_000,
        sent_camera(5, [(900, 90_000), (0, 0), (0, 0)], 900),
        None,
      ));
      window.push(report(
        2_000,
        sent_camera(6, [(30, 3_000), (0, 0), (0, 0)], 30),
        None,
      ));
      let rows = english(&window, StatsTarget::OwnCamera);
      assert_eq!(value(&rows, "stats-bitrate"), "– (estimate 2.50 Mbit/s)");
    }

    #[test]
    fn a_report_of_a_new_call_starts_the_window_over() {
      let mut window = StatsWindow::default();
      window.push(report(9_000, None, Some(received(1_000, 10, 0, 100))));
      window.push(report(1_000, None, Some(received(500, 5, 0, 50))));
      let rows = english(&window, StatsTarget::Received(REMOTE));
      assert_eq!(value(&rows, "stats-bitrate"), MISSING);
    }

    #[test]
    fn a_received_track_shows_its_layer_loss_and_jitter() {
      let mut window = StatsWindow::default();
      window.push(report(1_000, None, Some(received(1_000, 300, 2, 100))));
      window.push(report(3_000, None, Some(received(251_000, 498, 4, 160))));
      let measure = TileMeasure {
        maybe_drawn_fps: Some(29.5),
        maybe_drawn_size: Some(VideoSize {
          width: 1280,
          height: 720,
        }),
      };
      let rows = stats_rows(&window, StatsTarget::Received(REMOTE), measure, &en_words()).unwrap();
      assert_eq!(value(&rows, "stats-resolution"), "1280×720");
      assert_eq!(value(&rows, "stats-frame-rate"), "30.0 fps (drawn 29.5)");
      assert_eq!(value(&rows, "stats-bitrate"), "1.00 Mbit/s");
      assert_eq!(value(&rows, "stats-layer"), "1 of 0, 1, 2 (asked ≤ 2)");
      // 2 of 200 packets.
      assert_eq!(value(&rows, "stats-loss"), "1.0 %");
      assert_eq!(value(&rows, "stats-network"), "RTT 34 ms · jitter 4.5 ms");
      assert_eq!(value(&rows, "stats-frames"), "dropped 3 · black –");
    }

    #[test]
    fn a_received_track_without_a_drawn_size_shows_its_layers() {
      let mut window = StatsWindow::default();
      window.push(report(1_000, None, Some(received(1_000, 300, 0, 100))));
      let rows = english(&window, StatsTarget::Received(REMOTE));
      assert_eq!(value(&rows, "stats-resolution"), "640×360");
      assert_eq!(value(&rows, "stats-frame-rate"), "– fps (drawn –)");
      assert_eq!(value(&rows, "stats-loss"), MISSING);
    }

    #[test]
    fn a_track_the_report_lacks_has_no_rows() {
      let mut window = StatsWindow::default();
      assert_eq!(
        stats_rows(
          &window,
          StatsTarget::OwnScreen,
          TileMeasure::default(),
          &en_words()
        ),
        None
      );
      window.push(report(1_000, None, None));
      assert_eq!(
        stats_rows(
          &window,
          StatsTarget::OwnScreen,
          TileMeasure::default(),
          &en_words()
        ),
        None
      );
      assert_eq!(
        stats_rows(
          &window,
          StatsTarget::Received(REMOTE),
          TileMeasure::default(),
          &en_words()
        ),
        None
      );
    }

    #[test]
    fn a_tile_counts_the_pictures_it_draws_per_second() {
      let mut rate = DrawnRate::default();
      rate.observe(100, Duration::from_secs(10));
      assert_eq!(rate.fps(), None);
      rate.observe(130, Duration::from_millis(11_000));
      assert_eq!(rate.fps(), Some(30.0));
      // A reused element with a new count: no rate rather than a wrong one.
      rate.observe(5, Duration::from_secs(12));
      assert_eq!(rate.fps(), None);
    }
  }

  mod text {
    use super::*;

    #[test]
    fn values_are_formatted_for_nerds() {
      assert_eq!(bitrate_text(850_400.0), "850 kbit/s");
      assert_eq!(bitrate_text(1_254_000.0), "1.25 Mbit/s");
      assert_eq!(fps_text(29.97), "30.0");
      assert_eq!(percent_text(0.44), "0.4 %");
      assert_eq!(layer_list([].into_iter()), MISSING);
      assert_eq!(codec_text(&VideoCodec::Unknown("hevc".into())), "hevc");
    }

    #[test]
    fn the_copied_text_aligns_its_values() {
      let rows = vec![
        row("stats-codec", "H.264".to_owned()),
        row("stats-packet", "1.0 %".to_owned()),
      ];
      let text = stats_text("Stats for nerds", &rows, |key| match key {
        "stats-codec" => "Codec".to_owned(),
        _ => "Packet loss".to_owned(),
      });
      assert_eq!(text, "Stats for nerds\nCodec        H.264\nPacket loss  1.0 %");
    }

    /// The name is the one users know from video sites (#938).
    #[test]
    fn the_overlay_is_called_stats_for_nerds() {
      assert_eq!(I18n::new(Locale::En).t("stats-title"), "Stats for nerds");
      assert_eq!(I18n::new(Locale::De).t("stats-title"), "Statistiken für Nerds");
    }

    /// Every value message resolves in both languages with its placeholders filled.
    #[test]
    fn every_value_message_is_translated() {
      for locale in [Locale::En, Locale::De] {
        let i18n = I18n::new(locale);
        let words = |key: &str, args: &[(&str, &str)]| i18n.t_with(key, args);
        let mut window = StatsWindow::default();
        window.push(report(
          1_000,
          sent_camera(5, [(1, 1), (1, 1), (0, 0)], 1),
          Some(received(1, 1, 0, 1)),
        ));
        window.push(report(
          2_000,
          sent_camera(5, [(2, 2), (2, 2), (0, 0)], 2),
          Some(received(2, 2, 0, 2)),
        ));
        for target in [StatsTarget::OwnCamera, StatsTarget::Received(REMOTE)] {
          let rows = stats_rows(&window, target, TileMeasure::default(), &words).unwrap();
          for row in rows {
            assert_ne!(
              i18n.t(row.label_key),
              row.label_key,
              "{locale:?} lacks {}",
              row.label_key
            );
            assert!(!row.value.contains("stats-"), "{locale:?}: {}", row.value);
            assert!(!row.value.contains('{'), "{locale:?}: {}", row.value);
          }
        }
      }
    }
  }

  fn english(window: &StatsWindow, target: StatsTarget) -> Vec<StatsRow> {
    stats_rows(window, target, TileMeasure::default(), &en_words()).unwrap()
  }

  fn en_words() -> impl Fn(&str, &[(&str, &str)]) -> String {
    let i18n = I18n::new(Locale::En);
    move |key, args| i18n.t_with(key, args)
  }

  fn value(rows: &[StatsRow], label_key: &str) -> String {
    rows
      .iter()
      .find(|row| row.label_key == label_key)
      .map(|row| row.value.clone())
      .unwrap_or_else(|| panic!("no row {label_key}"))
  }

  fn report(
    at_ms: u64,
    maybe_sent: Option<SentVideoStats>,
    maybe_received: Option<ReceivedVideoStats>,
  ) -> VideoStatsReport {
    VideoStatsReport {
      at_ms,
      maybe_rtt_ms: Some(34),
      maybe_uplink: Some(UplinkStats {
        estimate_bps: 2_500_000,
        loss_percent: 2,
      }),
      tracks: maybe_sent
        .map(VideoTrackStats::Sent)
        .into_iter()
        .chain(maybe_received.map(VideoTrackStats::Received))
        .collect(),
    }
  }

  /// An own camera on `stream`: three layers (the top one not a target) with (pictures, bytes)
  /// each, and frames captured.
  fn sent_camera(stream: u32, layers: [(u64, u64); 3], captured: u64) -> Option<SentVideoStats> {
    Some(SentVideoStats {
      stream_id: MediaStreamId(stream),
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::H264,
      maybe_encoder: Some(VideoEncoderInfo {
        name: "vtenc_h264".to_owned(),
        implementation: EncoderImplementation::Hardware,
      }),
      maybe_capture: Some(VideoSize {
        width: 1920,
        height: 1080,
      }),
      layers: (0u8..)
        .zip(layers)
        .map(|(layer, (pictures, bytes))| SentLayerStats {
          layer,
          size: layer_size(layer),
          is_target: layer < 2,
          pictures,
          bytes,
        })
        .collect(),
      packets: 0,
      bytes: layers.iter().map(|(_, bytes)| bytes).sum(),
      maybe_frames_captured: Some(captured),
      pictures_dropped: 4,
      maybe_frames_black: Some(7),
    })
  }

  fn received(bytes: u64, packets: u64, lost: u64, frames: u64) -> ReceivedVideoStats {
    ReceivedVideoStats {
      stream_id: REMOTE,
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::H264,
      layers: (0..3)
        .map(|layer| OfferedLayer {
          layer,
          size: layer_size(layer),
        })
        .collect(),
      maybe_layer: Some(1),
      maybe_max_layer: Some(2),
      packets,
      bytes,
      packets_lost: lost,
      frames,
      frames_discarded: 3,
      maybe_jitter_ms: Some(4.5),
    }
  }

  fn layer_size(layer: u8) -> VideoSize {
    VideoSize {
      width: 320 << layer,
      height: 180 << layer,
    }
  }
}
