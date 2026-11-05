use leptos::prelude::*;

use teamcraft_proto::SourcePictureHash;

use crate::model::{ConversationId, PresenceStatus, User};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvatarSize {
  Small,
  Medium,
  Large,
  /// The largest picture: a profile card's header, and the user's own tile before a call.
  ExtraLarge,
}

impl AvatarSize {
  fn class(self) -> &'static str {
    match self {
      Self::Small => "avatar avatar-sm",
      Self::Medium => "avatar avatar-md",
      Self::Large => "avatar avatar-lg",
      Self::ExtraLarge => "avatar avatar-xl",
    }
  }
}

/// Generated avatar (initials on a hue derived from the user) with an optional presence dot.
#[component]
pub fn Avatar(
  user: User,
  #[prop(default = AvatarSize::Medium)] size: AvatarSize,
  #[prop(default = false)] show_presence: bool,
  /// An integration source's own picture, which goes before the account's (docs/bridge.md, "Source
  /// identity").
  #[prop(default = None)]
  maybe_source_picture: Option<(ConversationId, SourcePictureHash)>,
  /// Whether the account's profile picture may show; off for a stand-in `user` whose ID is not the
  /// account pictured (a source with no bot to stand for it shows its initials).
  #[prop(default = true)]
  shows_profile_picture: bool,
) -> impl IntoView {
  let presence = user.presence;
  let id = user.id;
  // Their picture, once loaded (signed-in builds); the generated initials until then.
  let picture = Memo::new(move |_| {
    shown_picture(
      || {
        maybe_source_picture
          .and_then(|(conversation, sha256)| crate::source_pictures::picture_url(conversation, sha256))
      },
      || crate::profiles::use_profiles().and_then(|hub| hub.avatar_url(id)),
      shows_profile_picture,
    )
  });
  let initials = user.initials();
  // Both are always there, one hidden: an avatar built again by a list or row that renders anew
  // keeps its `<img>` (only changed attributes are written) instead of loading and flashing it
  // (issue #687).
  view! {
    <span class=size.class() style:--hue=user.avatar_hue.to_string() aria-hidden="true">
      <img class="avatar-img" src=move || picture.get() alt="" hidden=move || picture.with(Option::is_none) />
      <span class="avatar-initials" hidden=move || picture.with(Option::is_some)>{initials}</span>
      {show_presence.then(|| view! { <PresenceDot presence=presence /> })}
    </span>
  }
}

/// The picture an avatar shows: a source's own, else the account's profile picture where it may
/// show, else none (the initials). Each is asked for only when it is needed.
fn shown_picture(
  source_picture: impl FnOnce() -> Option<String>,
  profile_picture: impl FnOnce() -> Option<String>,
  shows_profile_picture: bool,
) -> Option<String> {
  source_picture().or_else(|| shows_profile_picture.then(profile_picture).flatten())
}

#[component]
pub fn PresenceDot(presence: PresenceStatus) -> impl IntoView {
  view! { <span class=format!("presence presence-{}", presence.slug()) aria-hidden="true"></span> }
}

#[cfg(test)]
mod tests {
  use super::*;

  const SOURCE: &str = "blob:source";
  const PROFILE: &str = "blob:profile";

  #[test]
  fn a_source_picture_goes_before_the_profile_picture() {
    assert_eq!(
      shown_picture(some(SOURCE), some(PROFILE), true),
      Some(SOURCE.to_owned())
    );
    assert_eq!(
      shown_picture(some(SOURCE), some(PROFILE), false),
      Some(SOURCE.to_owned())
    );
  }

  #[test]
  fn without_a_source_picture_the_profile_picture_shows_only_where_allowed() {
    assert_eq!(shown_picture(none, some(PROFILE), true), Some(PROFILE.to_owned()));
    assert_eq!(
      shown_picture(none, || panic!("not asked when it may not show"), false),
      None,
      "the initials show"
    );
  }

  fn some(url: &'static str) -> impl FnOnce() -> Option<String> {
    move || Some(url.to_owned())
  }

  fn none() -> Option<String> {
    None
  }
}
