//! What the desktop app on Linux needs to play a video attachment (#804). WebKitGTK plays `<video>`
//! through the system's GStreamer, and phone videos (MP4 or MOV with H.264 or HEVC) need a demuxer
//! and a decoder that a distribution doesn't always install. The shell looks them up in GStreamer's
//! registry; when a video doesn't start, the player names the packages that bring what is missing.
//! Shared by the shell, which looks, and the UI, which tells.

use serde::{Deserialize, Serialize};

use crate::updates::InstallKind;

/// What GStreamer can do for a phone video on this system, and where the missing parts come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoPlayback {
  /// Unpacks MP4 and MOV (`qtdemux`, from gst-plugins-good).
  pub has_mp4_demuxer: bool,
  /// Any H.264 decoder: `avdec_h264` (gst-libav), `openh264dec`, VA-API, V4L2.
  pub has_h264_decoder: bool,
  /// Any HEVC decoder (the iPhone's default).
  pub has_hevc_decoder: bool,
  pub source: CodecSource,
}

/// Where this system gets GStreamer plugins from, which decides the package names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodecSource {
  /// The runtime's `codecs-extra` extension (installed along with the GNOME runtime).
  Flatpak,
  /// Debian, Ubuntu and their derivatives.
  Debian,
  /// Fedora, RHEL and their derivatives; HEVC comes from RPM Fusion.
  Fedora,
  Arch,
  /// openSUSE; full codecs come from Packman.
  Suse,
  /// A distribution we have no package names for (and a value from a newer shell).
  #[default]
  #[serde(other)]
  Unknown,
}

/// A part of video playback a package brings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoPart {
  Mp4Demuxer,
  H264Decoder,
  HevcDecoder,
}

impl VideoPlayback {
  /// The parts that aren't there, in a fixed order.
  pub fn missing(&self) -> Vec<VideoPart> {
    [
      (VideoPart::Mp4Demuxer, self.has_mp4_demuxer),
      (VideoPart::H264Decoder, self.has_h264_decoder),
      (VideoPart::HevcDecoder, self.has_hevc_decoder),
    ]
    .into_iter()
    .filter(|(_, is_there)| !is_there)
    .map(|(part, _)| part)
    .collect()
  }

  /// The packages that bring the missing parts, each named once; empty when nothing is missing or
  /// the distribution is unknown.
  pub fn packages_to_install(&self) -> Vec<&'static str> {
    let mut packages: Vec<&'static str> = Vec::new();
    for part in self.missing() {
      for package in self.source.packages_for(part) {
        if !packages.contains(package) {
          packages.push(package);
        }
      }
    }
    packages
  }
}

impl CodecSource {
  /// The source for an install of `kind`; `maybe_os_release` is the text of `/etc/os-release`,
  /// read for installs whose package format doesn't say (AppImage, development builds).
  pub fn new(kind: InstallKind, maybe_os_release: Option<&str>) -> Self {
    match kind {
      InstallKind::Flatpak => Self::Flatpak,
      InstallKind::Deb => Self::Debian,
      // The rpm names Fedora's packages (packaging/linux/README.md).
      InstallKind::Rpm => Self::Fedora,
      _ => maybe_os_release.map_or(Self::Unknown, Self::from_os_release),
    }
  }

  /// The distribution family from `ID` and `ID_LIKE` in `/etc/os-release`.
  pub fn from_os_release(text: &str) -> Self {
    let ids: Vec<&str> = text
      .lines()
      .filter_map(|line| {
        let (key, value) = line.split_once('=')?;
        matches!(key.trim(), "ID" | "ID_LIKE").then_some(value)
      })
      .flat_map(|value| value.trim().trim_matches(['"', '\'']).split_whitespace())
      .collect();
    let has = |names: &[&str]| ids.iter().any(|id| names.contains(id));
    if has(&["debian", "ubuntu"]) {
      Self::Debian
    } else if has(&["fedora", "rhel", "centos"]) {
      Self::Fedora
    } else if has(&["arch"]) {
      Self::Arch
    } else if has(&["suse", "opensuse"]) || ids.iter().any(|id| id.starts_with("opensuse")) {
      Self::Suse
    } else {
      Self::Unknown
    }
  }

  /// The packages that bring `part` here.
  fn packages_for(self, part: VideoPart) -> &'static [&'static str] {
    match (self, part) {
      (Self::Flatpak, _) => &["org.freedesktop.Platform.codecs-extra"],
      (Self::Debian, VideoPart::Mp4Demuxer) => &["gstreamer1.0-plugins-good"],
      (Self::Debian, _) => &["gstreamer1.0-libav"],
      (Self::Fedora, VideoPart::Mp4Demuxer) => &["gstreamer1-plugins-good"],
      // Fedora's own repositories (with the Cisco OpenH264 repository it enables by default).
      (Self::Fedora, VideoPart::H264Decoder) => &["gstreamer1-plugin-openh264"],
      // Fedora's libavcodec has no HEVC; RPM Fusion's has.
      (Self::Fedora, VideoPart::HevcDecoder) => &["gstreamer1-plugin-libav", "libavcodec-freeworld"],
      (Self::Arch, VideoPart::Mp4Demuxer) => &["gst-plugins-good"],
      (Self::Arch, _) => &["gst-libav"],
      (Self::Suse, VideoPart::Mp4Demuxer) => &["gstreamer-plugins-good"],
      (Self::Suse, _) => &["gstreamer-plugins-libav"],
      (Self::Unknown, _) => &[],
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod sources {
    use super::*;

    #[test]
    fn package_formats_decide_without_os_release() {
      assert_eq!(
        CodecSource::new(InstallKind::Flatpak, Some("ID=fedora")),
        CodecSource::Flatpak
      );
      assert_eq!(CodecSource::new(InstallKind::Deb, None), CodecSource::Debian);
      assert_eq!(CodecSource::new(InstallKind::Rpm, None), CodecSource::Fedora);
    }

    #[test]
    fn appimages_read_os_release() {
      assert_eq!(
        CodecSource::new(InstallKind::AppImage, Some("ID=arch\n")),
        CodecSource::Arch
      );
      assert_eq!(
        CodecSource::new(InstallKind::AppImage, None),
        CodecSource::Unknown
      );
      assert_eq!(
        CodecSource::new(InstallKind::Unpackaged, Some("ID=fedora")),
        CodecSource::Fedora
      );
    }

    #[test]
    fn derivatives_follow_id_like() {
      let mint = "NAME=\"Linux Mint\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
      assert_eq!(CodecSource::from_os_release(mint), CodecSource::Debian);
      let alma = "ID=\"almalinux\"\nID_LIKE=\"rhel centos fedora\"\n";
      assert_eq!(CodecSource::from_os_release(alma), CodecSource::Fedora);
      assert_eq!(
        CodecSource::from_os_release("ID=endeavouros\nID_LIKE=arch"),
        CodecSource::Arch
      );
      assert_eq!(
        CodecSource::from_os_release("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\""),
        CodecSource::Suse
      );
    }

    #[test]
    fn unknown_and_malformed_text_is_unknown() {
      for text in ["", "ID=nixos", "garbage\n=\nID", "VERSION_ID=debian"] {
        assert_eq!(
          CodecSource::from_os_release(text),
          CodecSource::Unknown,
          "{text:?}"
        );
      }
    }

    #[test]
    fn wire_values_are_stable() {
      assert_eq!(serde_json::to_string(&CodecSource::Fedora).unwrap(), "\"fedora\"");
      let newer: CodecSource = serde_json::from_str("\"nixos\"").unwrap();
      assert_eq!(newer, CodecSource::Unknown);
    }
  }

  mod packages {
    use super::*;

    #[test]
    fn nothing_missing_needs_nothing() {
      assert!(
        playback(true, true, true, CodecSource::Debian)
          .packages_to_install()
          .is_empty()
      );
    }

    #[test]
    fn debian_names_libav_once_for_both_decoders() {
      let report = playback(true, false, false, CodecSource::Debian);
      assert_eq!(report.missing(), [VideoPart::H264Decoder, VideoPart::HevcDecoder]);
      assert_eq!(report.packages_to_install(), ["gstreamer1.0-libav"]);
    }

    #[test]
    fn fedora_hevc_comes_from_rpm_fusion() {
      let report = playback(true, true, false, CodecSource::Fedora);
      assert_eq!(
        report.packages_to_install(),
        ["gstreamer1-plugin-libav", "libavcodec-freeworld"]
      );
    }

    #[test]
    fn a_missing_demuxer_comes_first() {
      let report = playback(false, false, true, CodecSource::Arch);
      assert_eq!(report.packages_to_install(), ["gst-plugins-good", "gst-libav"]);
    }

    #[test]
    fn flatpak_names_the_extension() {
      let report = playback(true, false, false, CodecSource::Flatpak);
      assert_eq!(
        report.packages_to_install(),
        ["org.freedesktop.Platform.codecs-extra"]
      );
    }

    #[test]
    fn unknown_distributions_name_no_package() {
      assert!(
        playback(true, false, false, CodecSource::Unknown)
          .packages_to_install()
          .is_empty()
      );
    }
  }

  fn playback(demuxer: bool, h264: bool, hevc: bool, source: CodecSource) -> VideoPlayback {
    VideoPlayback {
      has_mp4_demuxer: demuxer,
      has_h264_decoder: h264,
      has_hevc_decoder: hevc,
      source,
    }
  }
}
