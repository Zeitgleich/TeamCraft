//! What a list shows while it has no rows (issue #297): placeholders while it loads, its empty
//! state only once it had an answer, the reason with a retry when it failed. A list with rows,
//! cached ones too, shows them and asks for no status at all.

use std::time::Duration;

use leptos::prelude::*;
use teamcraft_client_api::list_load::{ListLoad, ListPhase, LoadFailure};

use crate::components::skeleton::ListSkeleton;
use crate::i18n::use_i18n;

/// The phase of one list, with a deadline for a request that never answers.
#[derive(Clone, Copy)]
pub struct ListStatus {
  pub phase: Memo<ListPhase>,
  restarts: RwSignal<u32>,
}

impl ListStatus {
  /// `source` tells how far the list is; `scope` is what it asks for (a changed query waits anew).
  /// One timer runs per wait, only while placeholders show; no polling.
  pub fn new(
    is_empty: impl Fn() -> bool + Send + Sync + 'static,
    source: impl Fn() -> ListLoad + Send + Sync + 'static,
    scope: Signal<String>,
    deadline_ms: u64,
  ) -> Self {
    let is_empty = Memo::new(move |_| is_empty());
    let load = Memo::new(move |_| if is_empty.get() { source() } else { ListLoad::Ready });
    let is_overdue = RwSignal::new(false);
    let restarts = RwSignal::new(0_u32);
    let wait = StoredValue::new(0_u64);
    Effect::new(move |_| {
      scope.track();
      restarts.track();
      let is_loading = load.get() == ListLoad::Loading;
      wait.update_value(|current| *current += 1);
      let mine = wait.get_value();
      is_overdue.set(false);
      if is_loading {
        set_timeout(
          move || {
            // An answer or a newer wait took over; a closed view ignores it (`try_*`).
            if wait.try_get_value() == Some(mine) {
              let _ = is_overdue.try_set(true);
            }
          },
          Duration::from_millis(deadline_ms),
        );
      }
    });
    let phase =
      Memo::new(move |_| ListPhase::of(load.get().after_deadline(is_overdue.get()), is_empty.get()));
    Self { phase, restarts }
  }

  /// The user retries: placeholders again, with a new deadline.
  pub fn restart(self) {
    self.restarts.update(|count| *count += 1);
  }
}

/// Stands in for a list's rows. `children` is its empty state, if it has one of its own; `as_item` is for use inside a `<ul>`
/// (the empty state then renders as an item itself).
#[component]
pub fn ListPlaceholder(
  phase: Memo<ListPhase>,
  rows: usize,
  #[prop(into)] label: Signal<String>,
  on_retry: Callback<()>,
  #[prop(optional)] as_item: bool,
  #[prop(optional)] children: Option<ChildrenFn>,
) -> impl IntoView {
  move || match phase.get() {
    ListPhase::Rows => ().into_any(),
    ListPhase::Skeleton if as_item => view! { <ListSkeleton rows=rows label=label /> }.into_any(),
    ListPhase::Skeleton => view! {
      <ul class="skeleton-list">
        <ListSkeleton rows=rows label=label />
      </ul>
    }
    .into_any(),
    ListPhase::Empty => children.as_ref().map(|empty| empty()).into_any(),
    ListPhase::Failed(failure) => {
      view! { <ListFailed failure=failure on_retry=on_retry as_item=as_item /> }.into_any()
    },
  }
}

#[component]
fn ListFailed(failure: LoadFailure, on_retry: Callback<()>, as_item: bool) -> impl IntoView {
  let i18n = use_i18n();
  let content = view! {
    <p class="empty-title">{move || i18n.t("list-failed-title")}</p>
    <p class="empty-body">{move || i18n.t(failure_key(failure))}</p>
    <button class="btn" on:click=move |_| on_retry.run(())>{move || i18n.t("data-retry")}</button>
  };
  if as_item {
    view! { <li class="empty-state is-rich list-failed" role="alert">{content}</li> }.into_any()
  } else {
    view! { <div class="empty-state is-rich list-failed" role="alert">{content}</div> }.into_any()
  }
}

fn failure_key(failure: LoadFailure) -> &'static str {
  match failure {
    LoadFailure::Network => "list-failed-network",
    LoadFailure::Server => "list-failed-server",
    LoadFailure::TimedOut => "list-failed-timed-out",
    LoadFailure::Other => "list-failed-other",
  }
}
