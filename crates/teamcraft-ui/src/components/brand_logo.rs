//! The brand's image beside its name (top bar, sign-in card): the logo, else the icon, in the
//! theme's variant. Until it loaded, and when it fails, the caller's fallback shows instead.

use leptos::prelude::*;

use crate::brand::{Brand, image_src};

/// The brand image, or `fallback` while there is none (no asset, still loading, failed to load).
#[component]
pub fn BrandLogo(class: &'static str, #[prop(into)] fallback: ViewFn) -> impl IntoView {
  let brand = Brand::use_current().0;
  let failed = RwSignal::new(false);
  let light = Memo::new(move |_| brand.with(|view| view.header_image(false).and_then(image_src)));
  let dark = Memo::new(move |_| {
    brand.with(|view| {
      view
        .header_image(true)
        .filter(|dark| Some(*dark) != view.header_image(false))
        .and_then(image_src)
    })
  });
  // A new brand (after changing the server) gets a fresh try.
  Effect::new(move |_| {
    light.track();
    failed.set(false);
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
