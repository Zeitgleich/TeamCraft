//! The link preview under the composer's draft (#2073): the card the message will carry, with a
//! button that takes it away. The core makes it (e2ee.md §12); this only shows what the core holds
//! for the draft, and nothing in the browser, which can't read other sites.

use leptos::prelude::*;
use teamcraft_client_api::actions::{ComposerLinkPreview, LinkPreview};

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::message_links::{as_permalink, own_hosts};
use crate::model::{ConversationId, ThreadId};
use crate::object_url::ObjectUrl;
use crate::state::use_app_state;

/// The composer's link preview: loading, ready or nothing.
#[component]
pub fn ComposerLinkPreviewCard(
  conversation: ConversationId,
  maybe_thread: Option<ThreadId>,
) -> impl IntoView {
  let state = use_app_state();
  let shown = Memo::new(move |_| {
    let preview = state.read(|backend| backend.composer_link_preview(conversation, maybe_thread));
    // A link to a message of this server shows that message instead, once sent (message_row.rs).
    let url = match &preview {
      ComposerLinkPreview::Loading { url } => url.as_str(),
      ComposerLinkPreview::Ready { preview } => preview.url.as_str(),
      ComposerLinkPreview::None | ComposerLinkPreview::Unknown => return ComposerLinkPreview::None,
    };
    if as_permalink(url, &own_hosts()).is_some() {
      return ComposerLinkPreview::None;
    }
    preview
  });
  let remove = Callback::new(move |()| {
    state.write(|backend| backend.remove_composer_link_preview(conversation, maybe_thread))
  });
  move || match shown.get() {
    ComposerLinkPreview::Loading { url } => Some(view! { <LoadingCard url=url remove=remove /> }.into_any()),
    ComposerLinkPreview::Ready { preview } => {
      Some(view! { <ReadyCard preview=preview remove=remove /> }.into_any())
    },
    ComposerLinkPreview::None | ComposerLinkPreview::Unknown => None,
  }
}

/// While the page is read: the site, and that the preview comes. Sending now sends no preview.
#[component]
fn LoadingCard(url: String, remove: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let host = host_of(&url).to_owned();
  view! {
    <div class="composer-link-preview is-loading" role="status" aria-busy="true">
      <span class="composer-link-thumb composer-link-thumb-empty" aria-hidden="true">
        <Icon name=IconName::Link />
      </span>
      <div class="composer-link-text">
        <span class="composer-link-site" translate="no">{host}</span>
        <span class="composer-link-loading">{move || i18n.t("composer-link-preview-loading")}</span>
      </div>
      <RemoveButton remove=remove />
    </div>
  }
}

#[component]
fn ReadyCard(preview: LinkPreview, remove: Callback<()>) -> impl IntoView {
  let picture = preview.maybe_image.as_deref().and_then(ObjectUrl::from_thumbnail);
  let image = picture.as_ref().map(|url| url.as_str().to_owned());
  // Revoked when the card goes.
  StoredValue::new_local(picture);
  let site = preview
    .maybe_site_name
    .clone()
    .unwrap_or_else(|| host_of(&preview.url).to_owned());
  let title = preview.maybe_title.clone().unwrap_or_else(|| preview.url.clone());
  view! {
    <div class="composer-link-preview">
      {match image {
        Some(url) => view! { <img class="composer-link-thumb" src=url alt="" /> }.into_any(),
        None => view! {
          <span class="composer-link-thumb composer-link-thumb-empty" aria-hidden="true">
            <Icon name=IconName::Link />
          </span>
        }.into_any(),
      }}
      <div class="composer-link-text">
        {(!site.is_empty()).then(|| view! { <span class="composer-link-site" translate="no">{site}</span> })}
        <span class="composer-link-title" translate="no">{title}</span>
        {preview.maybe_description.map(|text| view! { <span class="composer-link-description" translate="no">{text}</span> })}
      </div>
      <RemoveButton remove=remove />
    </div>
  }
}

#[component]
fn RemoveButton(remove: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      type="button"
      class="icon-btn icon-btn-small composer-link-remove"
      aria-label=move || i18n.t("composer-link-preview-remove")
      title=move || i18n.t("composer-link-preview-remove")
      on:click=move |_| remove.run(())
    >
      <Icon name=IconName::Close />
    </button>
  }
}

/// The host of a web link, for naming the site (`https://user@docs.example.com:8443/a` →
/// `docs.example.com`); the link itself when it has none.
fn host_of(url: &str) -> &str {
  let Some((_, rest)) = url.split_once("://") else {
    return url;
  };
  let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
  let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
  if host_port.starts_with('[') {
    // An IPv6 literal keeps its brackets.
    return host_port
      .split_once(']')
      .map_or(host_port, |(host, _)| &host_port[..=host.len()]);
  }
  host_port.split(':').next().unwrap_or(host_port)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_the_host_of_a_link() {
    assert_eq!(host_of("https://docs.example.com/guide?x=1"), "docs.example.com");
    assert_eq!(
      host_of("https://user@docs.example.com:8443/a"),
      "docs.example.com"
    );
    assert_eq!(host_of("http://[2001:db8::1]:80/x"), "[2001:db8::1]");
    assert_eq!(host_of("https://example.com#top"), "example.com");
    assert_eq!(host_of("not a link"), "not a link");
  }
}
