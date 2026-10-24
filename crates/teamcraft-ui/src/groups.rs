//! User groups for the mention picker: loaded once per session, refreshed when asked.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{ListUserGroupsResponse, UserGroupResponse};

use crate::reactive::SetIfAlive;
use crate::rest::RestClient;

#[derive(Clone, Copy)]
pub struct GroupsHub {
  client: StoredValue<RestClient, LocalStorage>,
  groups: RwSignal<Vec<UserGroupResponse>>,
}

impl GroupsHub {
  pub fn new(client: RestClient) -> Self {
    let hub = Self {
      client: StoredValue::new_local(client),
      groups: RwSignal::new(Vec::new()),
    };
    hub.refresh();
    hub
  }

  /// The groups known so far. Reactive.
  pub fn groups(&self) -> Vec<UserGroupResponse> {
    self.groups.get()
  }

  pub fn refresh(&self) {
    let hub = *self;
    spawn_local(async move {
      match hub
        .client
        .get_value()
        .get::<ListUserGroupsResponse>("/api/v1/user-groups")
        .await
      {
        Ok(list) => hub.groups.set_if_alive(list.groups),
        Err(error) => tracing::debug!(%error, "user groups not loaded"),
      }
    });
  }
}

pub fn use_groups() -> Option<GroupsHub> {
  use_context::<GroupsHub>()
}
