//! The unread or mention count on the rail, the sidebar and in lists. It pops when the number goes
//! up while it is on screen, not when it first renders, so opening a view doesn't set off a burst.

use leptos::prelude::*;

/// How loud a badge is. Red means "someone needs you" (a mention or a direct message); plain unread
/// is quiet, so red keeps its meaning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeTone {
  #[default]
  Mention,
  Unread,
}

impl BadgeTone {
  /// Red when `has_mention`, quiet otherwise.
  pub fn of(has_mention: bool) -> Self {
    if has_mention { Self::Mention } else { Self::Unread }
  }
}

/// A number badge (`.badge-count`) that is absent at zero.
#[component]
pub fn CountBadge(
  #[prop(into)] count: Signal<u32>,
  #[prop(into, optional)] tone: Signal<BadgeTone>,
  /// Hidden from screen readers when the surrounding control already says the number.
  #[prop(optional)]
  is_decorative: bool,
  /// The largest number shown; above it the badge says "{cap}+", so it keeps its size (the rail's
  /// space tiles, #715).
  #[prop(optional)]
  maybe_cap: Option<u32>,
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
        class:is-quiet=move || tone.get() == BadgeTone::Unread
        class:is-bumped=move || is_bumped.get()
        aria-hidden=is_decorative.then_some("true")
        on:animationend=move |_| is_bumped.set(false)
      >
        {move || capped_label(count.get(), maybe_cap)}
      </span>
    </Show>
  }
}

/// The text of a badge for `count`: the number, or "{cap}+" above the cap.
fn capped_label(count: u32, maybe_cap: Option<u32>) -> String {
  match maybe_cap {
    Some(cap) if count > cap => format!("{cap}+"),
    _ => count.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_capped_badge_says_plus_above_its_cap() {
    assert_eq!(capped_label(1, Some(9)), "1");
    assert_eq!(capped_label(9, Some(9)), "9");
    assert_eq!(capped_label(10, Some(9)), "9+");
    assert_eq!(capped_label(1_234, None), "1234");
    assert_eq!(capped_label(99, Some(99)), "99");
    assert_eq!(capped_label(100, Some(99)), "99+");
  }
}
