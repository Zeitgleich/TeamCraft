//! What stands for a space in the rail, the sidebar head and the quick switcher: its picture once it
//! has loaded, otherwise the letter tile in the space's color.

use leptos::prelude::*;

use crate::model::{Space, initials};
use crate::space_images::{ImageOwner, use_space_images};

/// The picture or the initials of a space. Decorative: the surrounding control names the space.
/// The caller's element carries the tile's shape and `--hue` background.
#[component]
pub fn SpaceTileContent(space: Space) -> impl IntoView {
  let maybe_hub = use_space_images();
  let id = space.id;
  let maybe_version = space.maybe_image_version.clone();
  let letters = initials(&space.name);
  let url = move || maybe_hub.and_then(|hub| hub.image_url(ImageOwner::Space(id), maybe_version.as_deref()));
  move || match url() {
    Some(src) => {
      view! { <img class="space-image" src=src alt="" aria-hidden="true" draggable="false" /> }.into_any()
    },
    None => view! { <span aria-hidden="true">{letters.clone()}</span> }.into_any(),
  }
}

/// A small tile (sidebar head, quick switcher).
#[component]
pub fn SpaceTile(space: Space) -> impl IntoView {
  let hue = space.hue.to_string();
  view! {
    <span class="space-tile" style:--hue=hue aria-hidden="true">
      <SpaceTileContent space=space />
    </span>
  }
}
