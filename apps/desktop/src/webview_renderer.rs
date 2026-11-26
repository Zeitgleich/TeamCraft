//! Linux: how WebKitGTK hands the painted page to the compositor, chosen before GTK starts
//! (`startup_env.rs` sets the variables) and named in the log once logging runs (#2416).
//!
//! - [`WebviewRenderer::GpuBuffers`], WebKitGTK's default: the page is painted on the GPU and the
//!   compositor gets the GPU buffers (DMA-BUF) without a copy.
//! - [`WebviewRenderer::SharedMemory`] (`WEBKIT_DMABUF_RENDERER_FORCE_SHM=1`): still painted on
//!   the GPU, accelerated canvas and WebCodecs included, but copied into shared memory for the
//!   compositor. The compositor never imports the driver's tiled or compressed buffers. Costs a copy
//!   per painted frame: in a call about 6 % of one core more at 30 frames a second.
//! - [`WebviewRenderer::Compatible`] (`WEBKIT_DISABLE_DMABUF_RENDERER=1`): painted on the CPU.
//!   Call video then comes as JPEG images, because WebKitGTK 2.54 kills its web process when
//!   WebCodecs is asked about a codec in this mode (#824, `webview_recovery.rs`).
//!
//! An Intel GPU on Wayland flickered heavily in a call, under GNOME and KDE Plasma alike, where the
//! page repaints with every video frame (#2416); there the default is shared memory. AMD GPUs and
//! X11 keep the GPU buffers, which work well there. `TEAMCRAFT_WEBKIT_RENDERER` picks one by hand (`gpu`, `shared-memory`, `compatible`),
//! `TEAMCRAFT_WEBKIT_DMABUF=0` still means `compatible`, and WebKit's own variables, when set, are
//! kept as they are (apps/desktop/packaging/linux/README.md, "Rendering").

use std::ffi::OsStr;
use std::fmt;
use std::path::Path;

/// The user's choice of renderer: `gpu`, `shared-memory` or `compatible`.
pub const RENDERER_CHOICE_VARIABLE: &str = "TEAMCRAFT_WEBKIT_RENDERER";
/// The older switch: `0` means the compatible renderer, anything else changes nothing.
pub const DMABUF_CHOICE_VARIABLE: &str = "TEAMCRAFT_WEBKIT_DMABUF";
/// WebKitGTK's switch for its DMA-BUF renderer; anything but `0` turns it off.
pub const DMABUF_VARIABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
/// WebKitGTK's switch for handing the compositor shared memory; anything but `0` turns it on.
pub const SHARED_MEMORY_VARIABLE: &str = "WEBKIT_DMABUF_RENDERER_FORCE_SHM";
/// PCI vendor id of Intel.
const INTEL_VENDOR_ID: &str = "0x8086";
/// Where the kernel lists the GPUs.
const DRM_CLASS_DIR: &str = "/sys/class/drm";

/// How the painted page reaches the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebviewRenderer {
  GpuBuffers,
  SharedMemory,
  Compatible,
}

/// Why a renderer is in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererReason {
  /// WebKitGTK's default on this system.
  Default,
  /// The fallback for an Intel GPU on Wayland (#2416).
  IntelWayland,
  /// `TEAMCRAFT_WEBKIT_RENDERER` or `TEAMCRAFT_WEBKIT_DMABUF`.
  UserChoice,
  /// WebKit's own variables, set outside the app.
  WebkitVariable,
}

/// What the choice depends on, read from the environment and the system.
#[derive(Clone, Debug, Default)]
pub struct RendererInputs<'a> {
  pub maybe_renderer_choice: Option<&'a OsStr>,
  pub maybe_dmabuf_choice: Option<&'a OsStr>,
  pub maybe_dmabuf: Option<&'a OsStr>,
  pub maybe_shared_memory: Option<&'a OsStr>,
  pub session: Session,
  /// `XDG_CURRENT_DESKTOP`, such as `ubuntu:GNOME`.
  pub desktop: String,
  pub maybe_gpu: Option<Gpu>,
}

/// The windowing system GTK will use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Session {
  Wayland,
  X11,
  #[default]
  Unknown,
}

/// The GPU that drives the display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gpu {
  /// PCI vendor id, such as `0x8086`.
  pub vendor: String,
  /// Kernel driver, such as `i915`, `xe` or `amdgpu`.
  pub driver: String,
}

/// One `/sys/class/drm/cardN` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DrmCard {
  gpu: Gpu,
  is_boot_display: bool,
}

impl WebviewRenderer {
  /// What the user asked for, if anything: the new variable first, then the older switch.
  fn chosen(inputs: &RendererInputs<'_>) -> Option<Self> {
    let by_name = inputs
      .maybe_renderer_choice
      .and_then(OsStr::to_str)
      .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
        "gpu" | "dmabuf" => Some(Self::GpuBuffers),
        "shared-memory" | "shm" => Some(Self::SharedMemory),
        "compatible" | "cpu" => Some(Self::Compatible),
        _ => None,
      });
    by_name.or_else(|| {
      inputs
        .maybe_dmabuf_choice
        .is_some_and(|value| value == "0")
        .then_some(Self::Compatible)
    })
  }

  /// The renderer WebKit will use with these variables.
  fn from_webkit_variables(inputs: &RendererInputs<'_>) -> Option<Self> {
    if is_on(inputs.maybe_dmabuf) {
      Some(Self::Compatible)
    } else if is_on(inputs.maybe_shared_memory) {
      Some(Self::SharedMemory)
    } else {
      None
    }
  }

  fn as_str(self) -> &'static str {
    match self {
      Self::GpuBuffers => "gpu-buffers",
      Self::SharedMemory => "shared-memory",
      Self::Compatible => "compatible",
    }
  }
}

impl fmt::Display for WebviewRenderer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

impl fmt::Display for RendererReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Default => "default",
      Self::IntelWayland => "intel-wayland",
      Self::UserChoice => "user-choice",
      Self::WebkitVariable => "webkit-variable",
    })
  }
}

impl fmt::Display for Session {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Wayland => "wayland",
      Self::X11 => "x11",
      Self::Unknown => "unknown",
    })
  }
}

impl RendererInputs<'_> {
  /// Whether this is the system the shared memory fallback is for.
  fn is_intel_wayland(&self) -> bool {
    let is_intel = self
      .maybe_gpu
      .as_ref()
      .is_some_and(|gpu| gpu.vendor.eq_ignore_ascii_case(INTEL_VENDOR_ID));
    is_intel && self.session == Session::Wayland
  }
}

/// Reads what the choice depends on. Environment values are borrowed from the caller's copies.
pub fn system_inputs<'a>(
  maybe_renderer_choice: Option<&'a OsStr>,
  maybe_dmabuf_choice: Option<&'a OsStr>,
  maybe_dmabuf: Option<&'a OsStr>,
  maybe_shared_memory: Option<&'a OsStr>,
) -> RendererInputs<'a> {
  RendererInputs {
    maybe_renderer_choice,
    maybe_dmabuf_choice,
    maybe_dmabuf,
    maybe_shared_memory,
    session: current_session(),
    desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
    maybe_gpu: display_gpu(Path::new(DRM_CLASS_DIR)),
  }
}

/// The variables to set before GTK starts for the chosen renderer: none when WebKit's own are set
/// or the default stays.
pub fn missing_variables(inputs: &RendererInputs<'_>) -> Vec<(&'static str, &'static str)> {
  if inputs.maybe_dmabuf.is_some() || inputs.maybe_shared_memory.is_some() {
    return Vec::new();
  }
  match choose(inputs).0 {
    WebviewRenderer::GpuBuffers => Vec::new(),
    WebviewRenderer::SharedMemory => vec![(SHARED_MEMORY_VARIABLE, "1")],
    WebviewRenderer::Compatible => vec![(DMABUF_VARIABLE, "1")],
  }
}

/// The renderer in use and why, once the variables are set (after the re-execution).
pub fn in_effect(inputs: &RendererInputs<'_>) -> (WebviewRenderer, RendererReason) {
  let (chosen, reason) = choose(inputs);
  let actual = WebviewRenderer::from_webkit_variables(inputs).unwrap_or(WebviewRenderer::GpuBuffers);
  if actual == chosen {
    (actual, reason)
  } else {
    (actual, RendererReason::WebkitVariable)
  }
}

/// Logs the renderer and what it was chosen by, once per start, for "Report a problem" and
/// support: flicker or a dark window is first a question of this path (#2416).
pub fn log_renderer() {
  let renderer_choice = std::env::var_os(RENDERER_CHOICE_VARIABLE);
  let dmabuf_choice = std::env::var_os(DMABUF_CHOICE_VARIABLE);
  let dmabuf = std::env::var_os(DMABUF_VARIABLE);
  let shared_memory = std::env::var_os(SHARED_MEMORY_VARIABLE);
  let inputs = system_inputs(
    renderer_choice.as_deref(),
    dmabuf_choice.as_deref(),
    dmabuf.as_deref(),
    shared_memory.as_deref(),
  );
  let (renderer, reason) = in_effect(&inputs);
  let (gpu_vendor, gpu_driver) = inputs.maybe_gpu.as_ref().map_or(("unknown", "unknown"), |gpu| {
    (gpu.vendor.as_str(), gpu.driver.as_str())
  });
  tracing::info!(
    %renderer,
    %reason,
    session = %inputs.session,
    desktop = %inputs.desktop,
    gpu_vendor,
    gpu_driver,
    webkit = %teamcraft_desktop_native::webkit_version(),
    "webview renderer"
  );
}

/// The renderer for these inputs and why, ignoring WebKit's own variables.
fn choose(inputs: &RendererInputs<'_>) -> (WebviewRenderer, RendererReason) {
  if let Some(renderer) = WebviewRenderer::chosen(inputs) {
    (renderer, RendererReason::UserChoice)
  } else if inputs.is_intel_wayland() {
    (WebviewRenderer::SharedMemory, RendererReason::IntelWayland)
  } else {
    (WebviewRenderer::GpuBuffers, RendererReason::Default)
  }
}

/// WebKit reads its switches as on for any value but `0`.
fn is_on(maybe_value: Option<&OsStr>) -> bool {
  maybe_value.is_some_and(|value| value != "0")
}

/// The windowing system GTK picks: `GDK_BACKEND` first, then the session's display variables.
fn current_session() -> Session {
  session_from(
    std::env::var("GDK_BACKEND").ok().as_deref(),
    std::env::var_os("WAYLAND_DISPLAY").is_some(),
    std::env::var_os("DISPLAY").is_some(),
  )
}

fn session_from(maybe_gdk_backend: Option<&str>, has_wayland: bool, has_x11: bool) -> Session {
  match maybe_gdk_backend.map(|backend| backend.split(',').next().unwrap_or_default().trim()) {
    Some("x11") => Session::X11,
    Some("wayland") => Session::Wayland,
    _ if has_wayland => Session::Wayland,
    _ if has_x11 => Session::X11,
    _ => Session::Unknown,
  }
}

/// The GPU driving the display: the one the firmware booted on, else the first one listed. A
/// laptop with two GPUs shows its panel on the integrated one, which is the boot display.
fn display_gpu(drm_dir: &Path) -> Option<Gpu> {
  let mut cards = read_cards(drm_dir);
  cards.sort_by_key(|(name, _)| card_number(name));
  pick_display_gpu(cards.into_iter().map(|(_, card)| card).collect())
}

fn pick_display_gpu(cards: Vec<DrmCard>) -> Option<Gpu> {
  let maybe_boot = cards.iter().position(|card| card.is_boot_display);
  let index = maybe_boot.unwrap_or(0);
  cards.into_iter().nth(index).map(|card| card.gpu)
}

/// The `cardN` entries (not their connectors, `card0-DP-1`) with their vendor and driver.
fn read_cards(drm_dir: &Path) -> Vec<(String, DrmCard)> {
  let Ok(entries) = std::fs::read_dir(drm_dir) else {
    return Vec::new();
  };
  entries
    .filter_map(Result::ok)
    .filter_map(|entry| {
      let name = entry.file_name().to_str()?.to_owned();
      card_number(&name)?;
      let device = entry.path().join("device");
      let vendor = std::fs::read_to_string(device.join("vendor"))
        .ok()?
        .trim()
        .to_owned();
      let driver = std::fs::read_link(device.join("driver"))
        .ok()
        .and_then(|link| link.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_owned());
      let is_boot_display =
        std::fs::read_to_string(device.join("boot_vga")).is_ok_and(|value| value.trim() == "1");
      Some((
        name,
        DrmCard {
          gpu: Gpu { vendor, driver },
          is_boot_display,
        },
      ))
    })
    .collect()
}

/// `card0` → 0; anything else (connectors, render nodes) → none.
fn card_number(name: &str) -> Option<u32> {
  name.strip_prefix("card")?.parse().ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  mod choice {
    use super::*;

    #[test]
    fn intel_on_wayland_gets_shared_memory() {
      assert_eq!(
        missing_variables(&intel_wayland()),
        vec![(SHARED_MEMORY_VARIABLE, "1")]
      );
      let kde = RendererInputs {
        desktop: "KDE".to_owned(),
        maybe_gpu: Some(gpu("0x8086", "i915")),
        ..intel_wayland()
      };
      assert_eq!(
        choose(&kde),
        (WebviewRenderer::SharedMemory, RendererReason::IntelWayland)
      );
    }

    #[test]
    fn other_systems_keep_the_gpu_buffers() {
      let amd = RendererInputs {
        maybe_gpu: Some(gpu("0x1002", "amdgpu")),
        ..intel_wayland()
      };
      let x11 = RendererInputs {
        session: Session::X11,
        ..intel_wayland()
      };
      let unknown_gpu = RendererInputs {
        maybe_gpu: None,
        ..intel_wayland()
      };
      for inputs in [amd, x11, unknown_gpu] {
        assert!(missing_variables(&inputs).is_empty(), "{inputs:?}");
        assert_eq!(
          choose(&inputs),
          (WebviewRenderer::GpuBuffers, RendererReason::Default)
        );
      }
    }

    #[test]
    fn the_user_chooses_by_name() {
      let with = |value: &'static str| RendererInputs {
        maybe_renderer_choice: Some(OsStr::new(value)),
        ..intel_wayland()
      };
      assert!(missing_variables(&with("gpu")).is_empty());
      assert_eq!(
        missing_variables(&with("Shared-Memory")),
        vec![(SHARED_MEMORY_VARIABLE, "1")]
      );
      assert_eq!(
        missing_variables(&with("compatible")),
        vec![(DMABUF_VARIABLE, "1")]
      );
      // Not a renderer: the automatic choice.
      assert_eq!(
        missing_variables(&with("fast")),
        vec![(SHARED_MEMORY_VARIABLE, "1")]
      );
    }

    #[test]
    fn the_older_switch_still_means_compatible() {
      let inputs = RendererInputs {
        maybe_dmabuf_choice: zero(),
        ..RendererInputs::default()
      };
      assert_eq!(missing_variables(&inputs), vec![(DMABUF_VARIABLE, "1")]);
      let not_zero = RendererInputs {
        maybe_dmabuf_choice: one(),
        ..RendererInputs::default()
      };
      assert!(missing_variables(&not_zero).is_empty());
      // The new variable wins over the older one.
      let both = RendererInputs {
        maybe_renderer_choice: Some(OsStr::new("gpu")),
        maybe_dmabuf_choice: zero(),
        ..RendererInputs::default()
      };
      assert!(missing_variables(&both).is_empty());
    }

    #[test]
    fn webkits_own_variables_are_kept() {
      for (maybe_dmabuf, maybe_shared_memory) in
        [(zero(), None), (one(), None), (None, zero()), (None, one())]
      {
        let inputs = RendererInputs {
          maybe_renderer_choice: Some(OsStr::new("compatible")),
          maybe_dmabuf,
          maybe_shared_memory,
          ..intel_wayland()
        };
        assert!(missing_variables(&inputs).is_empty());
      }
    }
  }

  mod log {
    use super::*;

    #[test]
    fn after_the_restart_the_reason_is_found_again() {
      let inputs = RendererInputs {
        maybe_shared_memory: one(),
        ..intel_wayland()
      };
      assert_eq!(
        in_effect(&inputs),
        (WebviewRenderer::SharedMemory, RendererReason::IntelWayland)
      );
      let chosen = RendererInputs {
        maybe_dmabuf_choice: zero(),
        maybe_dmabuf: one(),
        ..RendererInputs::default()
      };
      assert_eq!(
        in_effect(&chosen),
        (WebviewRenderer::Compatible, RendererReason::UserChoice)
      );
      assert_eq!(
        in_effect(&RendererInputs::default()),
        (WebviewRenderer::GpuBuffers, RendererReason::Default)
      );
    }

    #[test]
    fn webkit_variables_set_outside_are_named() {
      let inputs = RendererInputs {
        maybe_dmabuf: one(),
        ..intel_wayland()
      };
      assert_eq!(
        in_effect(&inputs),
        (WebviewRenderer::Compatible, RendererReason::WebkitVariable)
      );
      // `0` switches nothing on: the GPU buffers, against the fallback's choice.
      let zero = RendererInputs {
        maybe_shared_memory: zero(),
        ..intel_wayland()
      };
      assert_eq!(
        in_effect(&zero),
        (WebviewRenderer::GpuBuffers, RendererReason::WebkitVariable)
      );
    }
  }

  mod system {
    use super::*;

    #[test]
    fn the_session_follows_gdk_backend_then_the_displays() {
      assert_eq!(session_from(Some("x11"), true, true), Session::X11);
      assert_eq!(session_from(Some("wayland,x11"), false, true), Session::Wayland);
      assert_eq!(session_from(None, true, true), Session::Wayland);
      assert_eq!(session_from(Some("*"), false, true), Session::X11);
      assert_eq!(session_from(None, false, false), Session::Unknown);
    }

    #[test]
    fn the_boot_display_gpu_is_picked() {
      let intel = DrmCard {
        gpu: gpu("0x8086", "i915"),
        is_boot_display: true,
      };
      let nvidia = DrmCard {
        gpu: gpu("0x10de", "nvidia"),
        is_boot_display: false,
      };
      assert_eq!(
        pick_display_gpu(vec![nvidia.clone(), intel]),
        Some(gpu("0x8086", "i915"))
      );
      assert_eq!(pick_display_gpu(vec![nvidia]), Some(gpu("0x10de", "nvidia")));
      assert_eq!(pick_display_gpu(Vec::new()), None);
    }

    #[test]
    fn only_cards_count() {
      assert_eq!(card_number("card1"), Some(1));
      assert_eq!(card_number("card0-eDP-1"), None);
      assert_eq!(card_number("renderD128"), None);
    }

    #[test]
    fn a_missing_drm_directory_is_no_gpu() {
      assert_eq!(display_gpu(Path::new("/nonexistent/drm")), None);
    }
  }

  fn intel_wayland() -> RendererInputs<'static> {
    RendererInputs {
      session: Session::Wayland,
      desktop: "GNOME".to_owned(),
      maybe_gpu: Some(gpu("0x8086", "xe")),
      ..RendererInputs::default()
    }
  }

  fn gpu(vendor: &str, driver: &str) -> Gpu {
    Gpu {
      vendor: vendor.to_owned(),
      driver: driver.to_owned(),
    }
  }

  fn zero() -> Option<&'static OsStr> {
    Some(OsStr::new("0"))
  }

  fn one() -> Option<&'static OsStr> {
    Some(OsStr::new("1"))
  }
}
