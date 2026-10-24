//! Placeholders with the shape of the content that is on its way, instead of spinners (issue
//! #273): lists keep their size while loading, so nothing jumps when the data arrives.

use leptos::prelude::*;

/// A message: avatar and two lines.
#[component]
pub fn MessageSkeleton() -> impl IntoView {
  view! {
    <div class="msg-skeleton" aria-hidden="true">
      <span class="skeleton-avatar"></span>
      <span class="skeleton-lines"><span></span><span></span></span>
    </div>
  }
}

/// `rows` list rows (avatar and two lines each) for a list that is loading; announced once as busy.
#[component]
pub fn ListSkeleton(rows: usize, #[prop(into)] label: Signal<String>) -> impl IntoView {
  view! {
    <li class="list-skeleton" role="status" aria-busy="true" aria-label=move || label.get()>
      {(0..rows).map(|_| view! { <MessageSkeleton /> }).collect_view()}
    </li>
  }
}
