//! The brand's image beside its name (top bar, sign-in card): the logo, else the icon, in the
//! theme's variant; or only the icon, for the brand's tile on the sign-in card. Until it loaded,
//! and when it fails, the caller's fallback shows instead.

use leptos::prelude::*;

use teamcraft_client_api::brand::BrandView;

use crate::brand::{Brand, image_src, request_image};

/// The brand image, or `fallback` while there is none (no asset, still loading, failed to load).
#[component]
pub fn BrandLogo(
  class: &'static str,
  #[prop(into)] fallback: ViewFn,
  /// Which image: the logo or icon beside the name (the default), or only the icon.
  #[prop(optional)]
  image: BrandImage,
) -> impl IntoView {
  let brand = Brand::use_current().0;
  let failed = RwSignal::new(false);
  // Desktop: fetch the images through the shell (an effect, not while rendering); a new brand
  // (another server) also gets a fresh try after a failed one.
  Effect::new(move |_| {
    brand.with(|view| {
      for url in [image.of(view, false), image.of(view, true)]
        .into_iter()
        .flatten()
      {
        request_image(url);
      }
    });
    failed.set(false);
  });
  let light = Memo::new(move |_| brand.with(|view| image.of(view, false).and_then(image_src)));
  let dark = Memo::new(move |_| {
    brand.with(|view| {
      image
        .of(view, true)
        .filter(|dark| Some(*dark) != image.of(view, false))
        .and_then(image_src)
    })
  });
  let name = move || brand.with(|view| view.display_name.clone());
  move || {
    match (light.get(), failed.get()) {
    (Some(light), false) => view! {
      <img class=format!("{class} brand-light") src=light alt=name() on:error=move |_| failed.set(true) />
      {dark.get().map(|dark| view! { <img class=format!("{class} brand-dark") src=dark alt="" aria-hidden="true" on:error=move |_| failed.set(true) /> })}
    }
    .into_any(),
    _ => fallback.run(),
  }
  }
}

/// Which of the brand's images [`BrandLogo`] shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrandImage {
  /// The logo, else the icon: the image beside the name.
  #[default]
  Header,
  /// Only the icon: a square, for the brand's tile.
  Icon,
}

impl BrandImage {
  fn of(self, view: &BrandView, is_dark: bool) -> Option<&str> {
    match self {
      Self::Header => view.header_image(is_dark),
      Self::Icon => view.asset("icon", is_dark),
    }
  }
}
