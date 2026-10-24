//! The brand's image beside its name (top bar, sign-in card): the logo, else the icon, in the
//! theme's variant. Until it loaded, and when it fails, the caller's fallback shows instead.

use leptos::prelude::*;

use crate::brand::{Brand, image_src, request_image};

/// The brand image, or `fallback` while there is none (no asset, still loading, failed to load).
#[component]
pub fn BrandLogo(class: &'static str, #[prop(into)] fallback: ViewFn) -> impl IntoView {
  let brand = Brand::use_current().0;
  let failed = RwSignal::new(false);
  // Desktop: fetch the images through the shell (an effect, not while rendering); a new brand
  // (another server) also gets a fresh try after a failed one.
  Effect::new(move |_| {
    brand.with(|view| {
      for url in [view.header_image(false), view.header_image(true)]
        .into_iter()
        .flatten()
      {
        request_image(url);
      }
    });
    failed.set(false);
  });
  let light = Memo::new(move |_| brand.with(|view| view.header_image(false).and_then(image_src)));
  let dark = Memo::new(move |_| {
    brand.with(|view| {
      view
        .header_image(true)
        .filter(|dark| Some(*dark) != view.header_image(false))
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
