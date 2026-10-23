//! The desktop window's title bar. By default TeamCraft's top bar is the title bar: the shell
//! creates the window without native decorations and the UI draws the window controls (macOS keeps
//! its real traffic lights over the top bar). "Use the system title bar" brings the native one back.
//! Shared by the shell, which owns the window, and the UI, which draws the controls.

use serde::{Deserialize, Serialize};

/// The window controls when the desktop environment says nothing (and on Windows).
const DEFAULT_LAYOUT: &str = ":minimize,maximize,close";

/// Who draws the title bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleBar {
  /// TeamCraft's top bar is the title bar.
  #[default]
  Custom,
  /// The operating system's title bar above the top bar.
  System,
}

/// What the UI needs to draw the title bar. `None` in the browser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowChrome {
  /// What the window was created with; a change in the settings may only apply after a restart.
  pub title_bar: TitleBar,
  pub platform: WindowPlatform,
  /// Controls the UI draws, by side. Empty with the system title bar and on macOS.
  pub controls: ControlLayout,
  pub is_maximized: bool,
  pub is_fullscreen: bool,
  /// Whether switching the title bar applies at once or at the next start.
  pub is_live_switchable: bool,
}

/// The conventions the title bar follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowPlatform {
  Windows,
  /// Native traffic lights on the left, over the top bar.
  Macos,
  Linux,
}

/// The window buttons on each side, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlLayout {
  pub left: Vec<WindowButton>,
  pub right: Vec<WindowButton>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowButton {
  Minimize,
  /// Maximize, or restore when maximized.
  Maximize,
  Close,
}

/// What the UI asks the shell to do with the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
  Minimize,
  ToggleMaximize,
  /// Same as the system's close button: may only hide the window to the tray.
  Close,
  /// Moves the window with the pointer (a press on an empty part of the title bar).
  StartDragging,
}

impl TitleBar {
  /// A packager's default (`TEAMCRAFT_TITLE_BAR`): `system` or `custom`, anything else is ignored.
  pub fn parse(text: &str) -> Option<Self> {
    match text.trim().to_ascii_lowercase().as_str() {
      "system" | "native" => Some(Self::System),
      "custom" => Some(Self::Custom),
      _ => None,
    }
  }
}

impl ControlLayout {
  /// Windows: minimize, maximize, close on the right.
  pub fn windows() -> Self {
    Self::from_decoration_layout(DEFAULT_LAYOUT)
  }

  /// GTK's `gtk-decoration-layout` (GNOME's `button-layout`, which KDE also writes for GTK apps):
  /// `left:right`, buttons separated by commas, e.g. `close,minimize:` or `appmenu:minimize,close`.
  /// Unknown entries (`appmenu`, `icon`, `spacer`) are skipped; a layout without any known button
  /// falls back to the default, so the window can always be closed.
  pub fn from_decoration_layout(layout: &str) -> Self {
    let (left, right) = layout.split_once(':').unwrap_or(("", layout));
    let parsed = Self {
      left: buttons(left),
      right: buttons(right),
    };
    if parsed.left.is_empty() && parsed.right.is_empty() {
      return Self::from_decoration_layout(DEFAULT_LAYOUT);
    }
    parsed
  }
}

fn buttons(side: &str) -> Vec<WindowButton> {
  let mut buttons = Vec::new();
  for name in side.split(',') {
    let button = match name.trim() {
      "minimize" => WindowButton::Minimize,
      "maximize" => WindowButton::Maximize,
      "close" => WindowButton::Close,
      _ => continue,
    };
    if !buttons.contains(&button) {
      buttons.push(button);
    }
  }
  buttons
}

#[cfg(test)]
mod tests {
  use super::*;

  mod decoration_layout {
    use super::*;

    #[test]
    fn gnome_default_has_only_close_on_the_right() {
      let layout = ControlLayout::from_decoration_layout("appmenu:close");
      assert!(layout.left.is_empty());
      assert_eq!(layout.right, [WindowButton::Close]);
    }

    #[test]
    fn kde_default_order_is_kept() {
      let layout = ControlLayout::from_decoration_layout("icon:minimize,maximize,close");
      assert_eq!(
        layout.right,
        [
          WindowButton::Minimize,
          WindowButton::Maximize,
          WindowButton::Close
        ]
      );
    }

    #[test]
    fn left_side_layout() {
      let layout = ControlLayout::from_decoration_layout("close,minimize,maximize:");
      assert_eq!(
        layout.left,
        [
          WindowButton::Close,
          WindowButton::Minimize,
          WindowButton::Maximize
        ]
      );
      assert!(layout.right.is_empty());
    }

    #[test]
    fn both_sides() {
      let layout = ControlLayout::from_decoration_layout("close:maximize , minimize");
      assert_eq!(layout.left, [WindowButton::Close]);
      assert_eq!(layout.right, [WindowButton::Maximize, WindowButton::Minimize]);
    }

    #[test]
    fn without_a_colon_everything_is_on_the_right() {
      let layout = ControlLayout::from_decoration_layout("minimize,close");
      assert!(layout.left.is_empty());
      assert_eq!(layout.right, [WindowButton::Minimize, WindowButton::Close]);
    }

    #[test]
    fn empty_or_unknown_falls_back_to_the_default() {
      for layout in ["", ":", "appmenu:", "menu,spacer:icon"] {
        assert_eq!(
          ControlLayout::from_decoration_layout(layout),
          ControlLayout::windows()
        );
      }
    }

    #[test]
    fn duplicates_are_dropped() {
      let layout = ControlLayout::from_decoration_layout(":close,close");
      assert_eq!(layout.right, [WindowButton::Close]);
    }
  }

  #[test]
  fn title_bar_from_the_environment() {
    assert_eq!(TitleBar::parse("system"), Some(TitleBar::System));
    assert_eq!(TitleBar::parse(" Native "), Some(TitleBar::System));
    assert_eq!(TitleBar::parse("custom"), Some(TitleBar::Custom));
    assert_eq!(TitleBar::parse(""), None);
    assert_eq!(TitleBar::parse("yes"), None);
  }

  #[test]
  fn wire_format() {
    let json = serde_json::to_string(&(
      TitleBar::System,
      WindowAction::ToggleMaximize,
      WindowButton::Close,
    ))
    .unwrap_or_default();
    assert_eq!(json, r#"["system","toggle_maximize","close"]"#);
  }
}
