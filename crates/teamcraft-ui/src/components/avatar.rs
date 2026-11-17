use leptos::prelude::*;

use teamcraft_proto::SourcePictureHash;

use crate::model::{ConversationId, PresenceStatus, User};
use crate::reactive::{items_by_key, keyed_item};

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
///
/// Built once and updated in place: `user` is reactive, so a new presence, status or name of the
/// person changes the dot or the initials, never the `<img>`. An avatar built again loads its
/// picture again, which users see as flicker (issue #687); a caller that shows a person who
/// changes passes a signal of them instead of building the avatar again.
#[component]
pub fn Avatar(
  #[prop(into)] user: Signal<User>,
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
  let id = Memo::new(move |_| user.with(|user| user.id));
  // Their picture, once loaded (signed-in builds); the generated initials until then.
  let picture = Memo::new(move |_| {
    shown_picture(
      || {
        maybe_source_picture
          .and_then(|(conversation, sha256)| crate::source_pictures::picture_url(conversation, sha256))
      },
      || crate::profiles::use_profiles().and_then(|hub| hub.avatar_url(id.get())),
      shows_profile_picture,
    )
  });
  let initials = Memo::new(move |_| user.with(User::initials));
  let hue = Memo::new(move |_| user.with(|user| user.avatar_hue));
  let presence = Memo::new(move |_| user.with(|user| user.presence));
  // Both are always there, one hidden: the `<img>` keeps its picture while the initials show.
  view! {
    <span class=size.class() style:--hue=move || hue.get().to_string() aria-hidden="true">
      <img class="avatar-img" src=move || picture.get() alt="" hidden=move || picture.with(Option::is_none) />
      <span class="avatar-initials" hidden=move || picture.with(Option::is_some)>{move || initials.get()}</span>
      {show_presence.then(|| view! { <PresenceDot presence=presence /> })}
    </span>
  }
}

/// The avatar of a person who may not be known yet: built when they are, then updated in place
/// like [`Avatar`]; built again only for another person.
#[component]
pub fn MaybeAvatar(
  #[prop(into)] user: Signal<Option<User>>,
  #[prop(default = AvatarSize::Medium)] size: AvatarSize,
  #[prop(default = false)] show_presence: bool,
) -> impl IntoView {
  let maybe_id = Memo::new(move |_| user.with(|maybe_user| maybe_user.as_ref().map(|user| user.id)));
  move || {
    maybe_id.get()?;
    let first = user.get_untracked()?;
    // The last one known while the person goes: the avatar is about to be removed.
    let shown = Memo::new(move |previous: Option<&User>| {
      user
        .get()
        .or_else(|| previous.cloned())
        .unwrap_or_else(|| first.clone())
    });
    Some(view! { <Avatar user=shown size=size show_presence=show_presence /> })
  }
}

/// Overlapping avatars of a few people, one per person, each built once and updated in place.
#[component]
pub fn AvatarStack(#[prop(into)] users: Signal<Vec<User>>, size: AvatarSize) -> impl IntoView {
  let by_id = items_by_key(users, |user: &User| user.id);
  view! {
    <For each=move || users.get() key=|user| user.id let:user>
      <Avatar user=keyed_item(by_id, user.id, user) size=size />
    </For>
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

/// A person's presence as a coloured dot; follows `presence` in place.
#[component]
pub fn PresenceDot(#[prop(into)] presence: Signal<PresenceStatus>) -> impl IntoView {
  view! { <span class=move || format!("presence presence-{}", presence.get().slug()) aria-hidden="true"></span> }
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
