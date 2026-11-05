//! When an animated image in the conversation moves (#201): the user's setting, what they did to
//! this one image, and whether anybody can see it. Kept apart from the view so the rules are tested.

use crate::prefs::AnimatedImages;

/// What the user did to one image in the current view; it wins over the setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayChoice {
  #[default]
  Unset,
  Playing,
  Paused,
}

/// What decides whether one image moves right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayState {
  /// The setting after reduced motion ([`AnimatedImages::effective`]).
  pub mode: AnimatedImages,
  pub choice: PlayChoice,
  /// A mouse is over it or the keyboard's focus is in it.
  pub is_hovered: bool,
  /// On screen and not covered by the lightbox: nothing runs for an image nobody sees.
  pub is_visible: bool,
}

impl PlayState {
  pub fn plays(self) -> bool {
    self.is_visible
      && match self.choice {
        PlayChoice::Playing => true,
        PlayChoice::Paused => false,
        PlayChoice::Unset => match self.mode {
          AnimatedImages::Autoplay => true,
          AnimatedImages::OnHover => self.is_hovered,
          AnimatedImages::OnTap => false,
        },
      }
  }

  /// A still image whose only way to start is its button shows the button without a hover.
  pub fn needs_button(self) -> bool {
    self.choice != PlayChoice::Playing && self.mode == AnimatedImages::OnTap
  }
}

/// The choice after the button was pressed, by whether the image moves or is on its way to.
pub fn toggled(is_moving: bool) -> PlayChoice {
  if is_moving {
    PlayChoice::Paused
  } else {
    PlayChoice::Playing
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const AUTOPLAY: PlayState = PlayState {
    mode: AnimatedImages::Autoplay,
    choice: PlayChoice::Unset,
    is_hovered: false,
    is_visible: true,
  };

  #[test]
  fn the_setting_decides_until_the_user_chooses() {
    assert!(AUTOPLAY.plays());
    let hover = PlayState {
      mode: AnimatedImages::OnHover,
      ..AUTOPLAY
    };
    assert!(!hover.plays());
    assert!(
      PlayState {
        is_hovered: true,
        ..hover
      }
      .plays()
    );
    let tap = PlayState {
      mode: AnimatedImages::OnTap,
      ..AUTOPLAY
    };
    assert!(
      !PlayState {
        is_hovered: true,
        ..tap
      }
      .plays()
    );
  }

  #[test]
  fn a_choice_wins_over_setting_and_hover() {
    let paused = PlayState {
      choice: PlayChoice::Paused,
      is_hovered: true,
      ..AUTOPLAY
    };
    assert!(!paused.plays());
    let playing = PlayState {
      mode: AnimatedImages::OnTap,
      choice: PlayChoice::Playing,
      ..AUTOPLAY
    };
    assert!(playing.plays());
  }

  #[test]
  fn nothing_moves_where_nobody_sees_it() {
    let hidden = PlayState {
      is_visible: false,
      ..AUTOPLAY
    };
    assert!(!hidden.plays());
    assert!(
      !PlayState {
        choice: PlayChoice::Playing,
        ..hidden
      }
      .plays()
    );
  }

  #[test]
  fn reduced_motion_only_changes_the_default() {
    assert_eq!(AnimatedImages::Autoplay.effective(true), AnimatedImages::OnTap);
    assert_eq!(
      AnimatedImages::Autoplay.effective(false),
      AnimatedImages::Autoplay
    );
    assert_eq!(AnimatedImages::OnHover.effective(true), AnimatedImages::OnHover);
    assert_eq!(AnimatedImages::OnTap.effective(false), AnimatedImages::OnTap);
  }

  #[test]
  fn only_tap_to_play_keeps_the_button_on_a_still_image() {
    let tap = PlayState {
      mode: AnimatedImages::OnTap,
      ..AUTOPLAY
    };
    assert!(tap.needs_button());
    assert!(
      !PlayState {
        choice: PlayChoice::Playing,
        ..tap
      }
      .needs_button()
    );
    assert!(!AUTOPLAY.needs_button());
  }

  #[test]
  fn the_button_flips_what_is_happening() {
    assert_eq!(toggled(true), PlayChoice::Paused);
    assert_eq!(toggled(false), PlayChoice::Playing);
  }
}
