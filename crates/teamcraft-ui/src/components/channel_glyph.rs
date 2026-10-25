//! What stands for a channel in the sidebar and the channel header: its picture once it has
//! loaded, otherwise the icon of its kind. Decorative: the name next to it names the channel.

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::model::ConversationId;
use crate::space_images::{ImageOwner, use_space_images};

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
