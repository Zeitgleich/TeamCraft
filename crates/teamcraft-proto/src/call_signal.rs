//! In-call reactions and raised hands (issue #2324, e2ee.md §8.5): what a `sealed_signal` carries
//! once opened, and the limits of what the SFU relays.
//!
//! The SFU passes a sealed signal on to everybody else in the call and learns only that a device
//! sent one; the content is sealed under a key of the call's DAVE epoch
//! (`teamcraft_crypto::call_signal`).

use serde::{Deserialize, Serialize};

use crate::wire_enum::wire_enum;

/// The largest sealed signal the SFU relays. A signal is about 100 bytes sealed.
pub const MAX_SEALED_SIGNAL_BYTES: usize = 512;

/// What a `sealed_signal` carries, sealed: a counter and the signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSignalBody {
  /// Counts up per sender and DAVE epoch: a receiver drops a signal whose counter isn't above the
  /// last one it took from that sender, so the SFU can't replay an older one (a lowered hand
  /// raised again).
  pub seq: u64,
  pub signal: InCallSignal,
}

/// Something a participant shows everybody else in the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InCallSignal {
  /// A short-lived reaction that floats over the stage.
  Reaction { reaction: CallReaction },
  /// The hand is raised or lowered. Sent at each change, and again after each new epoch while it
  /// is raised, so devices that joined since see it.
  Hand { is_raised: bool },
  /// A signal this version doesn't know: ignored.
  #[serde(other)]
  Unknown,
}

wire_enum! {
  /// The reactions a call offers. A fixed set, not free text: a receiver shows only what it knows,
  /// so nobody can put arbitrary text over everybody's stage.
  pub enum CallReaction {
    ThumbsUp => "thumbs_up",
    Applause => "applause",
    Laugh => "laugh",
    Heart => "heart",
    Celebrate => "celebrate",
    Surprise => "surprise",
  }
}

impl CallReaction {
  /// Every reaction, in the picker's order.
  pub const ALL: [Self; 6] = [
    Self::ThumbsUp,
    Self::Applause,
    Self::Laugh,
    Self::Heart,
    Self::Celebrate,
    Self::Surprise,
  ];

  /// The emoji it shows as (user content, not an icon); `None` for one this version doesn't know.
  pub fn emoji(&self) -> Option<&'static str> {
    Some(match self {
      Self::ThumbsUp => "\u{1F44D}",
      Self::Applause => "\u{1F44F}",
      Self::Laugh => "\u{1F602}",
      Self::Heart => "\u{2764}\u{FE0F}",
      Self::Celebrate => "\u{1F389}",
      Self::Surprise => "\u{1F62E}",
      Self::Unknown(_) => return None,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn body_wire_format() {
    let reaction = SealedSignalBody {
      seq: 3,
      signal: InCallSignal::Reaction {
        reaction: CallReaction::Applause,
      },
    };
    let json = serde_json::to_string(&reaction).unwrap();
    assert_eq!(
      json,
      r#"{"seq":3,"signal":{"type":"reaction","reaction":"applause"}}"#
    );
    assert_eq!(serde_json::from_str::<SealedSignalBody>(&json).unwrap(), reaction);
    let hand = SealedSignalBody {
      seq: 4,
      signal: InCallSignal::Hand { is_raised: true },
    };
    assert_eq!(
      serde_json::to_string(&hand).unwrap(),
      r#"{"seq":4,"signal":{"type":"hand","is_raised":true}}"#
    );
  }

  #[test]
  fn unknown_signals_and_reactions_are_kept_apart() {
    let newer: SealedSignalBody =
      serde_json::from_str(r#"{"seq":1,"signal":{"type":"wave","to":"x"}}"#).unwrap();
    assert_eq!(newer.signal, InCallSignal::Unknown);
    let unknown: SealedSignalBody =
      serde_json::from_str(r#"{"seq":1,"signal":{"type":"reaction","reaction":"rocket"}}"#).unwrap();
    let InCallSignal::Reaction { reaction } = unknown.signal else {
      panic!("a reaction");
    };
    assert_eq!(reaction, CallReaction::Unknown("rocket".into()));
    assert_eq!(reaction.emoji(), None);
  }

  #[test]
  fn every_known_reaction_has_an_emoji_and_a_stable_name() {
    let names: Vec<&str> = CallReaction::ALL.iter().map(CallReaction::as_str).collect();
    assert_eq!(
      names,
      ["thumbs_up", "applause", "laugh", "heart", "celebrate", "surprise"]
    );
    assert!(
      CallReaction::ALL
        .iter()
        .all(|reaction| reaction.emoji().is_some())
    );
  }
}
