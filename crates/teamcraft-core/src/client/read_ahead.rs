//! The first reads of a gateway connection, all at once (issue #714). A full sync used to ask one
//! thing after the other: roster, policy, welcomes, threads, counts, settings, key packages, key
//! bag, device list, presence, link requests, each a round trip. None of these reads needs another's
//! answer, so they go out together right after the handshake, and each step takes its answer from
//! here instead of asking again. What arrives after the handshake comes as gateway events either
//! way, so an answer read a moment earlier misses nothing.
//!
//! Each answer is taken at most once, and only by the step it was read for, with the same query (a
//! cursor that moved meanwhile makes the step ask again). Whatever no step took is dropped when the
//! connection is caught up.

use teamcraft_proto::{
  DeviceLinkRequest, DeviceListResponse, KeyBagResponse, KeyPackageStatusResponse, ListConversationsResponse,
  ListMlsMessagesQuery, ListMlsMessagesResponse, ListPresenceResponse, ListSpacesResponse, ListThreadsQuery,
  ListThreadsResponse, ListWorkspaceLogQuery, ListWorkspaceLogResponse, MlsMessageId, NotificationSettings,
  SyncHeadsResponse, UnreadResponse, UserId, UserSummary, WorkspaceLogKind,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::store::{LocalStore, Table};

/// Most people whose presence is read ahead: one batch of `GET /presence`.
const MAX_PRESENCE_AHEAD: usize = 200;

type Read<T> = Option<Result<T, ApiError>>;

/// What a full sync works through first: where the conversations stand, the spaces and the
/// conversations.
pub(super) struct FirstListing {
  pub(super) heads: Result<SyncHeadsResponse, ApiError>,
  pub(super) spaces: Result<ListSpacesResponse, ApiError>,
  pub(super) conversations: Result<ListConversationsResponse, ApiError>,
}

/// Answers read ahead, each waiting for its step.
#[derive(Default)]
pub(super) struct ReadAhead {
  pub(super) own: Read<UserSummary>,
  /// The roster and policy log after the version each was read from.
  pub(super) roster: Option<(u64, Result<ListWorkspaceLogResponse, ApiError>)>,
  pub(super) policy: Option<(u64, Result<ListWorkspaceLogResponse, ApiError>)>,
  /// The first page of welcomes after the cursor it was read from.
  pub(super) welcomes: Option<(Option<MlsMessageId>, Result<ListMlsMessagesResponse, ApiError>)>,
  pub(super) read_state: Option<(
    Result<ListThreadsResponse, ApiError>,
    Result<UnreadResponse, ApiError>,
  )>,
  pub(super) notification_settings: Read<NotificationSettings>,
  pub(super) key_packages: Read<KeyPackageStatusResponse>,
  pub(super) key_bag: Read<KeyBagResponse>,
  pub(super) own_device_list: Read<Option<DeviceListResponse>>,
  pub(super) link_requests: Read<Vec<DeviceLinkRequest>>,
  /// Presence of the people known before the connection.
  pub(super) presence: Option<(Vec<UserId>, Result<ListPresenceResponse, ApiError>)>,
}

impl ReadAhead {
  /// Drops what only the full sync takes, so a later step reads afresh; presence and link
  /// requests stay for the end of the connection's setup.
  pub(super) fn drop_sync_answers(&mut self) {
    *self = Self {
      link_requests: self.link_requests.take(),
      presence: self.presence.take(),
      ..Self::default()
    };
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads ahead what a fresh session's full sync and the connection after it ask for; returns
  /// the listings the sync starts with.
  pub(super) async fn read_ahead_fresh(&mut self) -> Result<FirstListing, CoreError> {
    let roster_after = self.workspace()?.roster.version;
    let policy_after = self.workspace()?.policy_version;
    let welcomes_after = self.sync.maybe_welcome_cursor;
    let known = self.known_people_ahead()?;
    let me = self.user_id();
    let device = self.device_id();
    let api = &self.api;
    let welcomes_query = ListMlsMessagesQuery {
      maybe_after: welcomes_after,
      maybe_limit: None,
    };
    let threads_query = self.threads_query();
    let roster_query = log_query(roster_after);
    let policy_query = log_query(policy_after);
    let (
      (heads, spaces, conversations),
      (own, roster, policy, welcomes),
      (threads, unread, settings, key_packages),
      (key_bag, device_list, links, presence),
    ) = Box::pin(async {
      futures_util::join!(
        async { futures_util::join!(api.sync_heads(), api.list_spaces(), api.list_conversations()) },
        async {
          futures_util::join!(
            api.user(me),
            api.list_workspace_log(&WorkspaceLogKind::Roster, &roster_query),
            api.list_workspace_log(&WorkspaceLogKind::Policy, &policy_query),
            api.list_welcomes(device, &welcomes_query),
          )
        },
        async {
          futures_util::join!(
            api.list_threads(&threads_query),
            api.unread(),
            api.notification_settings(),
            api.key_package_status(device),
          )
        },
        async {
          futures_util::join!(
            api.key_bag(),
            api.device_list(me),
            api.device_links(),
            presence_of(api, &known),
          )
        },
      )
    })
    .await;
    self.read_ahead = ReadAhead {
      own: Some(own),
      roster: Some((roster_after, roster)),
      policy: Some((policy_after, policy)),
      welcomes: Some((welcomes_after, welcomes)),
      read_state: Some((threads, unread)),
      notification_settings: Some(settings),
      key_packages: Some(key_packages),
      key_bag: Some(key_bag),
      own_device_list: Some(device_list),
      link_requests: Some(links),
      presence: presence.map(|answer| (known, answer)),
    };
    Ok(FirstListing {
      heads,
      spaces,
      conversations,
    })
  }

  /// What a resumed connection asks for once it is caught up: the own summary, the counts and
  /// threads (dropped again when there is a replay: `GET /unread` would count the replayed
  /// messages twice, see `resync_live_state`), presence of `known` and link requests.
  pub(super) async fn read_resumed(&self, known: Vec<UserId>) -> ReadAhead {
    let api = &self.api;
    let threads_query = self.threads_query();
    // On the heap: five HTTP futures side by side are large, and this one is nested in the
    // connection's own future.
    let (own, threads, unread, links, presence) = Box::pin(async {
      futures_util::join!(
        api.user(self.user_id()),
        api.list_threads(&threads_query),
        api.unread(),
        api.device_links(),
        presence_of(api, &known),
      )
    })
    .await;
    ReadAhead {
      own: Some(own),
      read_state: Some((threads, unread)),
      link_requests: Some(links),
      presence: presence.map(|answer| (known, answer)),
      ..ReadAhead::default()
    }
  }

  /// Drops what no step took (a step that didn't run this time): it is never used later.
  pub(super) fn drop_read_ahead(&mut self) {
    self.read_ahead = ReadAhead::default();
  }

  pub(super) fn threads_query(&self) -> ListThreadsQuery {
    ListThreadsQuery {
      maybe_limit: Some(self.settings.history_page_size),
      ..ListThreadsQuery::default()
    }
  }

  /// The people whose presence is read ahead: everyone known, if one batch holds them.
  pub(super) fn known_people_ahead(&self) -> Result<Vec<UserId>, CoreError> {
    let known: Vec<UserId> = self
      .store
      .list(Table::Users)?
      .into_iter()
      .filter_map(|(key, _)| key.parse().ok())
      .collect();
    Ok(if known.len() <= MAX_PRESENCE_AHEAD {
      known
    } else {
      Vec::new()
    })
  }
}

fn log_query(after: u64) -> ListWorkspaceLogQuery {
  ListWorkspaceLogQuery {
    maybe_after: Some(after),
    maybe_limit: None,
  }
}

async fn presence_of<A: ServerApi>(
  api: &A,
  users: &[UserId],
) -> Option<Result<ListPresenceResponse, ApiError>> {
  if users.is_empty() {
    return None;
  }
  Some(api.list_presence(users).await)
}
