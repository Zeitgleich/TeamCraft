//! The desktop window's title bar. By default TeamCraft's top bar is the title bar: the shell
//! creates the window without native decorations and the UI draws the window controls (macOS keeps
//! its real traffic lights over the top bar). "Use the system title bar" brings the native one back.
//! Shared by the shell, which owns the window, and the UI, which draws the controls.

use serde::{Deserialize, Serialize};

/// The window controls when the desktop environment says nothing (and on Windows).
const DEFAULT_LAYOUT: &str = ":minimize,maximize,close";
/// KWin's decoration settings: the group in `kwinrc` and its button keys.
const KWIN_DECORATION_GROUP: &str = "[org.kde.kdecoration2]";
const KWIN_BUTTONS_LEFT_KEY: &str = "ButtonsOnLeft";
const KWIN_BUTTONS_RIGHT_KEY: &str = "ButtonsOnRight";
/// KWin's defaults for keys `kwinrc` leaves out: window menu and "on all desktops" on the left,
/// help, minimize, maximize and close on the right.
const KWIN_DEFAULT_LEFT: &str = "MS";
const KWIN_DEFAULT_RIGHT: &str = "HIAX";

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

/// How the tray icon looks on Linux and Windows (#726): the coloured mark, or a white or black
/// silhouette that matches a panel the colour doesn't suit. macOS always shows a template icon in
/// the menu bar's own colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayIconStyle {
  #[default]
  Colour,
  White,
  Black,
  /// A style of a newer app; shown as the colour mark.
  #[serde(other)]
  Unknown,
}

impl TrayIconStyle {
  /// The styles offered in the settings, in their order.
  pub const CHOICES: [Self; 3] = [Self::Colour, Self::White, Self::Black];
}

/// What the UI needs to draw the title bar. `None` in the browser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowChrome {
  /// What the window was created with; a change in the settings may only apply after a restart.
  pub title_bar: TitleBar,
  pub platform: WindowPlatform,
  /// Whose look the controls follow on Linux; [`LinuxDesktop::Other`] everywhere else.
  #[serde(default)]
  pub linux_desktop: LinuxDesktop,
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

/// The Linux desktop environment, for the look of the window controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDesktop {
  /// GNOME and its derivatives: libadwaita's round, lightly filled buttons.
  Gnome,
  /// KDE Plasma: Breeze's flat glyphs with a round highlight.
  Kde,
  /// Anything else (and a value from a newer shell): drawn like GNOME.
  #[default]
  #[serde(other)]
  Other,
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

impl LinuxDesktop {
  /// From `XDG_CURRENT_DESKTOP` (a colon-separated list such as `ubuntu:GNOME`), falling back to
  /// `XDG_SESSION_DESKTOP` (a single name such as `plasma`) when the first names nothing known.
  pub fn from_xdg(maybe_current: Option<&str>, maybe_session: Option<&str>) -> Self {
    let from_current = maybe_current
      .into_iter()
      .flat_map(|current| current.split(':'))
      .find_map(Self::from_desktop_name);
    from_current
      .or_else(|| maybe_session.and_then(Self::from_session_name))
      .unwrap_or(Self::Other)
  }

  /// One entry of `XDG_CURRENT_DESKTOP`, as registered by freedesktop.org.
  fn from_desktop_name(name: &str) -> Option<Self> {
    match name.trim().to_ascii_lowercase().as_str() {
      "kde" => Some(Self::Kde),
      "gnome" | "gnome-classic" | "gnome-flashback" => Some(Self::Gnome),
      _ => None,
    }
  }

  /// `XDG_SESSION_DESKTOP` names the session file, so it also has the KDE and Ubuntu spellings.
  fn from_session_name(name: &str) -> Option<Self> {
    match name.trim().to_ascii_lowercase().as_str() {
      "kde" | "plasma" | "plasmawayland" | "plasmax11" => Some(Self::Kde),
      "gnome" | "gnome-xorg" | "gnome-wayland" | "gnome-classic" | "ubuntu" | "ubuntu-xorg" => {
        Some(Self::Gnome)
      },
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

  /// KWin's button order from the text of `kwinrc`: `ButtonsOnLeft` / `ButtonsOnRight` in
  /// `[org.kde.kdecoration2]`, one letter per button (`I` minimize, `A` maximize, `X` close; menu,
  /// help, spacers and the rest are skipped). A missing key means KWin's default for that side.
  /// `None` when the result has no known button, so the caller falls back to another source.
  pub fn from_kwinrc(text: &str) -> Option<Self> {
    let mut maybe_left = None;
    let mut maybe_right = None;
    let mut is_in_group = false;
    for line in text.lines().map(str::trim) {
      if line.starts_with('[') {
        is_in_group = line == KWIN_DECORATION_GROUP;
        continue;
      }
      let Some((key, value)) = line.split_once('=').filter(|_| is_in_group) else {
        continue;
      };
      match key.trim() {
        KWIN_BUTTONS_LEFT_KEY => maybe_left = Some(value.trim().to_owned()),
        KWIN_BUTTONS_RIGHT_KEY => maybe_right = Some(value.trim().to_owned()),
        _ => {},
      }
    }
    let parsed = Self {
      left: kwin_buttons(maybe_left.as_deref().unwrap_or(KWIN_DEFAULT_LEFT)),
      right: kwin_buttons(maybe_right.as_deref().unwrap_or(KWIN_DEFAULT_RIGHT)),
    };
    (!parsed.left.is_empty() || !parsed.right.is_empty()).then_some(parsed)
  }
}

fn kwin_buttons(letters: &str) -> Vec<WindowButton> {
  let mut buttons = Vec::new();
  for letter in letters.chars() {
    let button = match letter {
      'I' => WindowButton::Minimize,
      'A' => WindowButton::Maximize,
      'X' => WindowButton::Close,
      _ => continue,
    };
    if !buttons.contains(&button) {
      buttons.push(button);
    }
  }
  buttons
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

  #[test]
  fn tray_icon_styles_keep_their_wire_names() {
    for (style, name) in [
      (TrayIconStyle::Colour, "\"colour\""),
      (TrayIconStyle::White, "\"white\""),
      (TrayIconStyle::Black, "\"black\""),
    ] {
      assert_eq!(serde_json::to_string(&style).ok().as_deref(), Some(name));
      assert_eq!(serde_json::from_str::<TrayIconStyle>(name).ok(), Some(style));
    }
    assert_eq!(
      serde_json::from_str::<TrayIconStyle>("\"rainbow\"").ok(),
      Some(TrayIconStyle::Unknown)
    );
  }

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

  mod linux_desktop {
    use super::*;

    #[test]
    fn current_desktop_names() {
      for (current, expected) in [
        ("KDE", LinuxDesktop::Kde),
        ("GNOME", LinuxDesktop::Gnome),
        ("ubuntu:GNOME", LinuxDesktop::Gnome),
        ("GNOME-Classic:GNOME", LinuxDesktop::Gnome),
        ("pop:GNOME", LinuxDesktop::Gnome),
        ("kde", LinuxDesktop::Kde),
        ("X-Cinnamon", LinuxDesktop::Other),
        ("XFCE", LinuxDesktop::Other),
        ("", LinuxDesktop::Other),
      ] {
        assert_eq!(LinuxDesktop::from_xdg(Some(current), None), expected, "{current}");
      }
    }

    #[test]
    fn session_desktop_is_the_fallback() {
      assert_eq!(LinuxDesktop::from_xdg(None, Some("plasma")), LinuxDesktop::Kde);
      assert_eq!(
        LinuxDesktop::from_xdg(Some("X-Generic"), Some("ubuntu")),
        LinuxDesktop::Gnome
      );
      assert_eq!(
        LinuxDesktop::from_xdg(None, Some("cinnamon")),
        LinuxDesktop::Other
      );
      assert_eq!(LinuxDesktop::from_xdg(None, None), LinuxDesktop::Other);
    }

    #[test]
    fn current_desktop_wins_over_the_session() {
      assert_eq!(
        LinuxDesktop::from_xdg(Some("KDE"), Some("gnome")),
        LinuxDesktop::Kde
      );
    }

    #[test]
    fn unknown_wire_value_is_other() {
      let parsed: LinuxDesktop = serde_json::from_str(r#""cosmic""#).unwrap_or(LinuxDesktop::Kde);
      assert_eq!(parsed, LinuxDesktop::Other);
    }

    #[test]
    fn missing_field_is_other() {
      let json = r#"{"title_bar":"custom","platform":"linux","controls":{"left":[],"right":[]},
        "is_maximized":false,"is_fullscreen":false,"is_live_switchable":true}"#;
      let maybe_chrome = serde_json::from_str::<WindowChrome>(json).ok();
      assert_eq!(
        maybe_chrome.map(|chrome| chrome.linux_desktop),
        Some(LinuxDesktop::Other)
      );
    }
  }

  mod kwinrc {
    use super::*;

    #[test]
    fn buttons_from_the_decoration_group() {
      let text = "[Windows]\nButtonsOnLeft=X\n\n[org.kde.kdecoration2]\nButtonsOnLeft=XAI\n\
                  ButtonsOnRight=MS\nlibrary=org.kde.breeze\n";
      let maybe_layout = ControlLayout::from_kwinrc(text);
      assert_eq!(
        maybe_layout.as_ref().map(|layout| layout.left.as_slice()),
        Some(
          [
            WindowButton::Close,
            WindowButton::Maximize,
            WindowButton::Minimize
          ]
          .as_slice()
        )
      );
      assert_eq!(maybe_layout.map(|layout| layout.right), Some(Vec::new()));
    }

    #[test]
    fn missing_keys_use_kwin_defaults() {
      assert_eq!(ControlLayout::from_kwinrc(""), Some(ControlLayout::windows()));
      let maybe_layout = ControlLayout::from_kwinrc("[org.kde.kdecoration2]\nButtonsOnRight=X\n");
      assert_eq!(
        maybe_layout.map(|layout| layout.right),
        Some(vec![WindowButton::Close])
      );
    }

    #[test]
    fn no_known_button_is_none() {
      let text = "[org.kde.kdecoration2]\nButtonsOnLeft=MS\nButtonsOnRight=H_F\n";
      assert_eq!(ControlLayout::from_kwinrc(text), None);
    }

    #[test]
    fn garbage_is_tolerated() {
      let text = "\u{0}\u{feff}[org.kde.kdecoration2\n=\nButtonsOnRight\n[org.kde.kdecoration2]\n \
                  ButtonsOnRight = IAXX \n";
      let maybe_layout = ControlLayout::from_kwinrc(text);
      assert_eq!(
        maybe_layout.map(|layout| layout.right),
        Some(vec![
          WindowButton::Minimize,
          WindowButton::Maximize,
          WindowButton::Close
        ])
      );
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
