//! One page at a time from a directory list of the server (#298): the first page when the query
//! changes, the next on request, and never more rows than a view can keep light.

use leptos::prelude::*;
use serde::de::DeserializeOwned;
use teamcraft_client_api::failure::FailureKind;
use teamcraft_client_api::list_load::{ListLoad, LoadFailure};
use teamcraft_proto::{
  DirectoryChannel, DirectoryChannelsResponse, DirectoryInvitation, DirectoryInvitationsResponse,
  DirectoryPeopleResponse, DirectoryPerson,
};

use crate::feedback::{ActionFailure, report_failure};
use crate::i18n::use_i18n;
use crate::reactive::{SetIfAlive, UpdateIfAlive};
use crate::rest::{RestClient, RestError};
use crate::session::use_session;

/// Rows one view holds at most; past this, search and filters narrow the list down.
const MAX_LOADED_ROWS: usize = 1000;

/// An answer of a paged directory route.
pub trait DirectoryPage: DeserializeOwned + 'static {
  type Row: Clone + Send + Sync + 'static;

  /// The rows and the marker of the next page.
  fn into_parts(self) -> (Vec<Self::Row>, Option<String>);
}

impl DirectoryPage for DirectoryPeopleResponse {
  type Row = DirectoryPerson;

  fn into_parts(self) -> (Vec<Self::Row>, Option<String>) {
    (self.people, self.maybe_next_after)
  }
}

impl DirectoryPage for DirectoryChannelsResponse {
  type Row = DirectoryChannel;

  fn into_parts(self) -> (Vec<Self::Row>, Option<String>) {
    (self.channels, self.maybe_next_after)
  }
}

impl DirectoryPage for DirectoryInvitationsResponse {
  type Row = DirectoryInvitation;

  fn into_parts(self) -> (Vec<Self::Row>, Option<String>) {
    (self.invitations, self.maybe_next_after.map(|id| id.to_string()))
  }
}

/// The rows of one list and how far it is.
pub struct Pager<T: Send + Sync + 'static> {
  pub rows: RwSignal<Vec<T>>,
  pub load: RwSignal<ListLoad>,
  maybe_next: RwSignal<Option<String>>,
  is_loading_more: RwSignal<bool>,
  /// Which query the newest request belongs to: a late answer to an older one is dropped.
  generation: StoredValue<u64>,
  reloads: RwSignal<u32>,
  path: Signal<String>,
  /// Taken where the component is built: context can't be read in an async task or a later event.
  maybe_client: StoredValue<Option<RestClient>, LocalStorage>,
}

impl<T: Send + Sync + 'static> Clone for Pager<T> {
  fn clone(&self) -> Self {
    *self
  }
}

impl<T: Send + Sync + 'static> Copy for Pager<T> {}

impl<T: Clone + Send + Sync + 'static> Pager<T> {
  /// Follows `path` (the route with its query): a new path loads anew. The rows stay until the
  /// answer, so a filter doesn't flash placeholders. Without a session nothing can be asked: the
  /// list reads as failed.
  pub fn new<P: DirectoryPage<Row = T>>(path: Signal<String>) -> Self {
    let pager = Self {
      rows: RwSignal::new(Vec::new()),
      load: RwSignal::new(ListLoad::Loading),
      maybe_next: RwSignal::new(None),
      is_loading_more: RwSignal::new(false),
      generation: StoredValue::new(0),
      reloads: RwSignal::new(0),
      path,
      maybe_client: StoredValue::new_local(
        use_session().map(|session| RestClient::new(session.authenticator())),
      ),
    };
    Effect::new(move |_| {
      pager.reloads.track();
      let path = pager.path.get();
      pager.fetch::<P>(path, false);
    });
    pager
  }

  /// The client to act on a row with; `None` without a session.
  pub fn client(&self) -> Option<RestClient> {
    self.maybe_client.get_value()
  }

  pub fn reload(&self) {
    self.load.set(ListLoad::Loading);
    self.reloads.update(|count| *count += 1);
  }

  pub fn load_more<P: DirectoryPage<Row = T>>(&self) {
    let Some(next) = self.maybe_next.get_untracked() else {
      return;
    };
    if self.is_loading_more.get_untracked() {
      return;
    }
    self.is_loading_more.set(true);
    let path = format!("{}&after={}", self.path.get_untracked(), encoded(&next));
    self.fetch::<P>(path, true);
  }

  pub fn is_loading_more(&self) -> bool {
    self.is_loading_more.get()
  }

  pub fn has_more(&self) -> bool {
    self.maybe_next.with(Option::is_some) && !self.is_at_limit()
  }

  /// More exist than a view keeps: the user narrows the list instead.
  pub fn is_at_limit(&self) -> bool {
    self.maybe_next.with(Option::is_some) && self.rows.with(Vec::len) >= MAX_LOADED_ROWS
  }

  /// A row left the list on the server.
  pub fn remove(&self, is_gone: impl Fn(&T) -> bool) {
    self.rows.update_if_alive(|rows| rows.retain(|row| !is_gone(row)));
  }

  fn fetch<P: DirectoryPage<Row = T>>(&self, path: String, is_next_page: bool) {
    let pager = *self;
    if !is_next_page {
      self.generation.update_value(|generation| *generation += 1);
      // The next page asked for belonged to the old query: its answer will be dropped.
      self.is_loading_more.set(false);
    }
    let mine = self.generation.get_value();
    let Some(client) = self.client() else {
      self.load.set(ListLoad::Failed {
        failure: LoadFailure::Other,
      });
      return;
    };
    leptos::task::spawn_local(async move {
      let answer: Result<P, RestError> = client.get(&path).await;
      if pager.generation.try_get_value() != Some(mine) {
        return;
      }
      match answer {
        Ok(page) => {
          let (rows, maybe_next) = page.into_parts();
          pager.rows.update_if_alive(|shown| {
            if is_next_page {
              shown.extend(rows);
            } else {
              *shown = rows;
            }
          });
          pager.maybe_next.set_if_alive(maybe_next);
          pager.load.set_if_alive(ListLoad::Ready);
        },
        // The next page failing leaves what is shown, and its button to try again.
        Err(refusal) if is_next_page => report_failure(refusal.to_failure()),
        Err(refusal) => {
          pager.rows.update_if_alive(Vec::clear);
          pager.maybe_next.set_if_alive(None);
          pager.load.set_if_alive(ListLoad::Failed {
            failure: load_failure(&refusal.to_failure()),
          });
        },
      }
      pager.is_loading_more.set_if_alive(false);
    });
  }
}

/// The route with its query; values left empty are left out. Always has the `?`, so a next-page
/// marker can be added with `&`.
pub fn query_path(route: &str, parameters: &[(&str, &str)]) -> String {
  let query = parameters
    .iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(key, value)| format!("{key}={}", encoded(value)))
    .collect::<Vec<_>>()
    .join("&");
  format!("{route}?{query}")
}

fn encoded(value: &str) -> String {
  String::from(js_sys::encode_uri_component(value))
}

/// "Show more", or why there is no more to show.
#[component]
pub fn PagerFooter<T: Clone + Send + Sync + 'static>(
  pager: Pager<T>,
  on_more: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <Show when=move || pager.has_more() fallback=|| ()>
      <div class="directory-more">
        <button
          type="button"
          class="btn"
          prop:disabled=move || pager.is_loading_more()
          aria-busy=move || pager.is_loading_more().then_some("true")
          on:click=move |_| on_more.run(())
        >
          {move || i18n.t("directory-show-more")}
        </button>
      </div>
    </Show>
    <Show when=move || pager.is_at_limit() fallback=|| ()>
      <p class="directory-more muted-text" role="status">{move || i18n.t("directory-narrow-down")}</p>
    </Show>
  }
}

fn load_failure(failure: &ActionFailure) -> LoadFailure {
  match failure.kind {
    FailureKind::Network => LoadFailure::Network,
    FailureKind::Server(_) => LoadFailure::Server,
    _ => LoadFailure::Other,
  }
}
