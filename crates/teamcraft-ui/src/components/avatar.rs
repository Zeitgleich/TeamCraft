use leptos::prelude::*;

use crate::model::{PresenceStatus, User};

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
) -> impl IntoView {
  let presence = user.presence;
  let id = user.id;
  // Their picture, once loaded (signed-in builds); the generated initials until then.
  let picture = Memo::new(move |_| crate::profiles::use_profiles().and_then(|hub| hub.avatar_url(id)));
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

#[component]
pub fn PresenceDot(presence: PresenceStatus) -> impl IntoView {
  view! { <span class=format!("presence presence-{}", presence.slug()) aria-hidden="true"></span> }
}
