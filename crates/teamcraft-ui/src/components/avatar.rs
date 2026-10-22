use leptos::prelude::*;

use crate::model::{PresenceStatus, User};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvatarSize {
  Small,
  Medium,
  Large,
}

impl AvatarSize {
  fn class(self) -> &'static str {
    match self {
      Self::Small => "avatar avatar-sm",
      Self::Medium => "avatar avatar-md",
      Self::Large => "avatar avatar-lg",
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
  view! {
    <span class=size.class() style:--hue=user.avatar_hue.to_string() aria-hidden="true">
      <span class="avatar-initials">{user.initials()}</span>
      {show_presence.then(|| view! { <PresenceDot presence=presence /> })}
    </span>
  }
}

#[component]
pub fn PresenceDot(presence: PresenceStatus) -> impl IntoView {
  view! { <span class=format!("presence presence-{}", presence.slug()) aria-hidden="true"></span> }
}
