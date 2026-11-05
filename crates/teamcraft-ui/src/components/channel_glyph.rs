//! What stands for a channel in the sidebar and the channel header: its picture once it has
//! loaded, otherwise the icon of its kind. Decorative: the name next to it names the channel.

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::model::{ConversationId, ConversationKind};
use crate::space_images::{ImageOwner, use_space_images};

/// The icon of a channel kind without a picture; `None` for kinds that are not channels (their
/// glyph is a person or a group). Log and bot channels keep the console icon until a picture is set.
pub fn channel_fallback_icon(kind: ConversationKind) -> Option<IconName> {
  match kind {
    ConversationKind::Public => Some(IconName::Hash),
    ConversationKind::Private => Some(IconName::Lock),
    ConversationKind::Integration => Some(IconName::Terminal),
    ConversationKind::Direct | ConversationKind::Group => None,
  }
}

/// The picture of `conversation` in `maybe_version`, or `fallback` while there is none (or it is
/// still loading, or the demo has no server). `class` sizes the wrapper.
#[component]
pub fn ChannelGlyph(
  conversation: ConversationId,
  #[prop(into)] maybe_version: Signal<Option<String>>,
  fallback: IconName,
  #[prop(optional)] class: &'static str,
) -> impl IntoView {
  let maybe_hub = use_space_images();
  let url = move || {
    maybe_hub.and_then(|hub| hub.image_url(ImageOwner::Channel(conversation), maybe_version.get().as_deref()))
  };
  move || match url() {
    Some(src) => view! {
      <span class=format!("channel-glyph {class}") aria-hidden="true">
        <img class="channel-glyph-image" src=src alt="" draggable="false" />
      </span>
    }
    .into_any(),
    None => view! { <Icon name=fallback class=class /> }.into_any(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn channels_fall_back_to_the_icon_of_their_kind() {
    assert_eq!(
      channel_fallback_icon(ConversationKind::Public),
      Some(IconName::Hash)
    );
    assert_eq!(
      channel_fallback_icon(ConversationKind::Private),
      Some(IconName::Lock)
    );
    assert_eq!(
      channel_fallback_icon(ConversationKind::Integration),
      Some(IconName::Terminal)
    );
  }

  #[test]
  fn people_have_no_channel_icon() {
    assert_eq!(channel_fallback_icon(ConversationKind::Direct), None);
    assert_eq!(channel_fallback_icon(ConversationKind::Group), None);
  }
}
