//! The unread or mention count on the rail, the sidebar and in lists. It pops when the number goes
//! up while it is on screen, not when it first renders, so opening a view doesn't set off a burst.

use leptos::prelude::*;

/// A number badge (`.badge-count`) that is absent at zero.
#[component]
pub fn CountBadge(
  #[prop(into)] count: Signal<u32>,
  /// Hidden from screen readers when the surrounding control already says the number.
  #[prop(optional)]
  is_decorative: bool,
) -> impl IntoView {
  let is_bumped = RwSignal::new(false);
  let previous = StoredValue::new(count.get_untracked());
  Effect::new(move |_| {
    let now = count.get();
    if now > previous.get_value() {
      is_bumped.set(true);
    }
    previous.set_value(now);
  });
  view! {
    <Show when=move || { count.get() > 0 } fallback=|| ()>
      <span
        class="badge-count"
        class:is-bumped=move || is_bumped.get()
        aria-hidden=is_decorative.then_some("true")
        on:animationend=move |_| is_bumped.set(false)
      >
        {move || count.get()}
      </span>
    </Show>
  }
}
