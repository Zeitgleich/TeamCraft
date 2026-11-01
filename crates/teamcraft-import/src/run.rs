//! Running the import (docs/import-synology.md §8, e2ee.md §4.10): conversation by conversation,
//! oldest posts first, every step recorded in the progress database, so a run that stops goes on
//! where it was and a second run changes nothing.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use teamcraft_core::Client;
use teamcraft_core::api::{ApiError, GatewayConnection, ServerApi};
use teamcraft_core::client::direct_uploads::{DirectUpload, DirectUploadError, DirectUploadProgress};
use teamcraft_core::client::files::UploadSource;
use teamcraft_core::client::import::ImportDraft;
use teamcraft_core::client::workspace_admin::ChannelOptions;
use teamcraft_core::client::{UserAddWait, pending_user_additions};
use teamcraft_core::error::CoreError;
use teamcraft_core::imported::{ImportSource, ImportedOrigin, imported_message_id};
use teamcraft_core::store::{AttachmentRef, LocalStore, MessageContent, MessageMeta};
use teamcraft_proto::{
  ChannelRole, ConversationId, CreateSpaceRequest, GatewayClientMessage, HistoryPolicy, Id, IdempotencyKey,
  ImportMemberStateRequest, JoinPolicy, MAX_IMPORT_BATCH, MessageId, ProvisionStatus, SpaceId,
  UpdateConversationRequest, UserId, UserKind,
};
use tracing::{info, warn};

use crate::convert::{People, PreparedPost, prepare};
use crate::export::{Export, Post, SourceId};
use crate::mapping::Mapping;
use crate::plan::{Item, Plan, Target, plan};
use crate::progress::{Progress, ProgressError, Step};

/// Bytes of ciphertext one batch carries at most, well below the server's request limit (4 MiB by
/// default) with base64's third on top.
const MAX_BATCH_BYTES: usize = 2 * 1024 * 1024;
/// How long a run waits by default for a member's device to let the import account into an
/// existing conversation before it goes on with the next one (a later run tries again).
pub const DEFAULT_JOIN_WAIT: Duration = Duration::from_secs(120);
/// Heartbeats keep the gateway open; under common proxy idle timeouts.
const HEARTBEAT: Duration = Duration::from_secs(20);
/// Retries of a request that failed for a network reason, with a growing pause.
const NETWORK_RETRIES: u32 = 8;
const RETRY_PAUSE: Duration = Duration::from_secs(2);
const MAX_RETRY_PAUSE: Duration = Duration::from_secs(120);
/// Pictures up to this size get a thumbnail (they are read whole for it).
const MAX_THUMBNAIL_SOURCE_BYTES: u64 = 40 * 1024 * 1024;

/// What a run is asked to do.
pub struct RunOptions {
  /// How a mention of somebody without an account reads, in the team's language.
  pub deleted_name: String,
  /// How long to wait for a member's device to let the import account in ([`DEFAULT_JOIN_WAIT`]).
  pub join_wait: Duration,
  /// Files above this size are left out and named on their message.
  pub maybe_max_file_bytes: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
  #[error(transparent)]
  Core(#[from] CoreError),
  #[error(transparent)]
  Progress(#[from] ProgressError),
  #[error(transparent)]
  Export(#[from] crate::export::ExportError),
  #[error("{0}")]
  Problem(String),
}

/// The import against one server, with its client, export, mapping and progress.
pub struct Importer<'a, A: ServerApi, S: LocalStore> {
  pub client: &'a mut Client<A, S>,
  pub export: &'a Export,
  pub mapping: &'a Mapping,
  pub progress: &'a mut Progress,
  people: People,
  gateway: Option<A::Gateway>,
  last_heartbeat: Instant,
  spaces: HashMap<String, SpaceId>,
  join_wait: Duration,
  maybe_max_file_bytes: Option<u64>,
}

impl<'a, A: ServerApi, S: LocalStore> Importer<'a, A, S> {
  /// Resolves the mapped accounts (creating those the identity provider knows) and connects.
  pub async fn start(
    client: &'a mut Client<A, S>,
    export: &'a Export,
    mapping: &'a Mapping,
    progress: &'a mut Progress,
    options: RunOptions,
  ) -> Result<Self, RunError> {
    let accounts = resolve_accounts(client, mapping, progress).await?;
    let mut importer = Self {
      client,
      export,
      mapping,
      progress,
      people: People {
        accounts,
        bots: export.bots.clone(),
        deleted_name: options.deleted_name.clone(),
      },
      gateway: None,
      last_heartbeat: Instant::now(),
      spaces: HashMap::new(),
      join_wait: options.join_wait,
      maybe_max_file_bytes: options.maybe_max_file_bytes,
    };
    importer.keep_gateway().await;
    Ok(importer)
  }

  /// The plan with the accounts as they are now.
  pub fn plan(&self) -> Plan {
    let accounts: HashMap<SourceId, UserId> = self
      .people
      .accounts
      .iter()
      .map(|(source, (user, _))| (*source, *user))
      .collect();
    plan(self.export, self.mapping, &accounts)
  }

  /// Imports every item of the plan. A conversation that fails is noted and the next one goes on.
  pub async fn run(&mut self) -> Result<Plan, RunError> {
    let plan = self.plan();
    let total = plan.items.len();
    for (index, item) in plan.items.iter().enumerate() {
      info!(
        item = index + 1,
        of = total,
        source = item.source,
        "importing a conversation"
      );
      match self.import_item(item).await {
        Ok(()) => {},
        Err(RunError::Core(error)) if is_network(&error) => return Err(RunError::Core(error)),
        Err(error) => {
          warn!(source = item.source, %error, "a conversation failed; going on with the next");
          self
            .progress
            .note(&item.key(), now_ms(), &format!("failed: {error}"))?;
        },
      }
    }
    Ok(plan)
  }

  /// One pass of what waits after the import (`serve`): additions of people who set up the app
  /// meanwhile, then leaving every conversation where nobody is missing any more.
  pub async fn tend(&mut self, plan: &Plan) -> Result<(), RunError> {
    // Also for conversations a run before this version imported: their waiting additions of
    // guests get the history (e2ee.md §4.10, "Guests in imported conversations").
    for item in &plan.items {
      if let Some(conversation) = self.progress.conversation(&item.key())? {
        self.grant_history(item, conversation)?;
        if self.progress.is_done(&item.key(), Step::Finished)? {
          self.client.note_import_keys(conversation)?;
        }
      }
    }
    self.keep_gateway().await;
    self.client.apply_pending_changes().await?;
    self.keep_gateway().await;
    for item in &plan.items {
      if let Some(conversation) = self.progress.conversation(&item.key())? {
        self.try_leave(item, conversation).await?;
      }
    }
    Ok(())
  }

  /// Keeps the gateway connected (new devices and commits come through it) and sends heartbeats.
  pub async fn keep_gateway(&mut self) {
    if self.gateway.is_none() {
      match self.client.connect_gateway().await {
        Ok((connection, _)) => {
          self.gateway = Some(connection);
          self.last_heartbeat = Instant::now();
        },
        Err(error) => {
          warn!(%error, "gateway connection failed; trying again later");
          return;
        },
      }
    }
    let Some(connection) = self.gateway.as_mut() else {
      return;
    };
    if let Err(error) = self.client.pump(connection).await {
      warn!(%error, "gateway connection lost");
      self.gateway = None;
      return;
    }
    if self.last_heartbeat.elapsed() >= HEARTBEAT {
      self.last_heartbeat = Instant::now();
      if let Some(connection) = self.gateway.as_mut()
        && let Err(error) = connection.send(&GatewayClientMessage::Heartbeat).await
      {
        warn!(%error, "heartbeat failed");
        self.gateway = None;
      }
    }
  }

  async fn import_item(&mut self, item: &Item) -> Result<(), RunError> {
    let key = item.key();
    if self.progress.is_done(&key, Step::Left)? {
      // Done, and the import account is no member any more.
      return Ok(());
    }
    let conversation = match self.progress.conversation(&key)? {
      Some(conversation) => conversation,
      None => {
        let conversation = self.open_target(item).await?;
        self.progress.set_conversation(&key, conversation)?;
        conversation
      },
    };
    self.grant_history(item, conversation)?;
    if !self.ensure_in_group(item, conversation).await? {
      self.progress.note(
        &key,
        now_ms(),
        "waits for a member's device to let the import account in",
      )?;
      return Ok(());
    }
    if item.has_history && !self.progress.is_done(&key, Step::History)? {
      self.import_history(item, conversation).await?;
      self.progress.mark_done(&key, Step::History)?;
    }
    if !self.progress.is_done(&key, Step::Finished)? {
      self.client.finish_import(conversation).await?;
      self.progress.mark_done(&key, Step::Finished)?;
    }
    if !self.progress.is_done(&key, Step::Members)? {
      self.add_members(item, conversation).await?;
      self.progress.mark_done(&key, Step::Members)?;
    }
    if !self.progress.is_done(&key, Step::MemberState)? {
      self.set_member_state(item, conversation).await?;
      self.progress.mark_done(&key, Step::MemberState)?;
    }
    self.try_leave(item, conversation).await
  }

  /// Makes or finds the conversation of `item`.
  async fn open_target(&mut self, item: &Item) -> Result<ConversationId, RunError> {
    match &item.target {
      Target::NewChannel {
        space,
        name,
        is_public,
        maybe_topic,
      } => {
        let space = self.space(space).await?;
        let options = ChannelOptions {
          name: name.clone(),
          is_private: !is_public,
          maybe_topic: maybe_topic.clone(),
          maybe_category: None,
          maybe_history: Some(HistoryPolicy::Full),
          is_integration: false,
          maybe_idempotency_key: Some(idempotency_key(&item.key())),
        };
        Ok(self.client.create_channel_with(space, options).await?)
      },
      Target::ExistingChannel { space, name } => {
        let space_id = self.space_id(space).await?.ok_or_else(|| {
          RunError::Problem(format!(
            "the space {space:?} of the channel to merge into doesn't exist"
          ))
        })?;
        let channels = self.client_api_channels(space_id).await?;
        channels
          .into_iter()
          .find(|(_, maybe_name)| maybe_name.as_deref() == Some(name.as_str()))
          .map(|(id, _)| id)
          .ok_or_else(|| RunError::Problem(format!("the channel {space}/{name} doesn't exist")))
      },
      Target::Dm { participants } => {
        let response = self.client.open_import_dm(participants.to_vec()).await?;
        let id = response.id;
        self.grant_history(item, id)?;
        self.client.set_up_import_dm(response, participants).await?;
        Ok(id)
      },
      Target::Notes { user } => {
        let response = self.client.open_import_dm(vec![*user]).await?;
        let id = response.id;
        self.grant_history(item, id)?;
        self.client.set_up_import_dm(response, &[*user]).await?;
        Ok(id)
      },
    }
  }

  /// Notes on the import device that the import made `conversation` and whom the mapping names in
  /// it, so they get its history also as guests (e2ee.md §4.10, "Guests in imported
  /// conversations"). Not for a channel merged into: the import changes no members there.
  fn grant_history(&mut self, item: &Item, conversation: ConversationId) -> Result<(), RunError> {
    let people: Vec<UserId> = match &item.target {
      Target::NewChannel { .. } => item.members.clone(),
      Target::Dm { participants } => participants.to_vec(),
      Target::Notes { user } => vec![*user],
      Target::ExistingChannel { .. } => return Ok(()),
    };
    self.client.grant_imported_history(conversation, &people)?;
    Ok(())
  }

  async fn client_api_channels(
    &mut self,
    space: SpaceId,
  ) -> Result<Vec<(ConversationId, Option<String>)>, RunError> {
    Ok(
      self
        .client
        .api()
        .admin_channel_names(space)
        .await
        .map_err(CoreError::from)?,
    )
  }

  /// The import account in the conversation's group. A new channel or DM has it already; an
  /// existing one asks to be let in (a workspace admin's self-addition, which a member's device
  /// commits) and waits a while for it.
  async fn ensure_in_group(&mut self, item: &Item, conversation: ConversationId) -> Result<bool, RunError> {
    if self.client.has_group(conversation)? {
      return Ok(true);
    }
    let me = self.client.user_id();
    if let Target::ExistingChannel { .. } = item.target {
      self.client.admin_add_member(conversation, me).await?;
    }
    let deadline = Instant::now() + self.join_wait;
    while Instant::now() < deadline {
      self.keep_gateway().await;
      self.client.apply_pending_changes().await?;
      if self.client.has_group(conversation)? {
        return Ok(true);
      }
      tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(self.client.has_group(conversation)?)
  }

  async fn import_history(&mut self, item: &Item, conversation: ConversationId) -> Result<(), RunError> {
    let posts = self.export.posts(item.source)?;
    let mut batch: Vec<Pending> = Vec::new();
    let mut batch_bytes = 0;
    let mut stickers: HashMap<String, AttachmentRef> = HashMap::new();
    let total = posts.len();
    let started = Instant::now();
    for (index, post) in posts.iter().enumerate() {
      if self.progress.message_of(post.id)?.is_some() {
        continue;
      }
      // A webhook writing into a DM has no place in it in TeamCraft (a reply to it goes in at
      // the top).
      if item.target.is_direct() && self.people.bots.contains_key(&post.user_id) {
        continue;
      }
      let prepared = prepare(post, self.export, &self.people);
      // A reply needs its root's server ID: send what waits first if the root is among it.
      if let Some(root) = prepared.maybe_root
        && batch.iter().any(|pending| pending.prepared.source_id == root)
      {
        self.send_batch(item, conversation, &mut batch).await?;
        batch_bytes = 0;
      }
      let pending = self
        .upload_files(conversation, post, prepared, &mut stickers)
        .await?;
      batch_bytes += pending.estimated_bytes();
      batch.push(pending);
      if batch.len() >= MAX_IMPORT_BATCH || batch_bytes >= MAX_BATCH_BYTES {
        self.send_batch(item, conversation, &mut batch).await?;
        batch_bytes = 0;
        self.keep_gateway().await;
        info!(
          source = item.source,
          done = index + 1,
          of = total,
          seconds = started.elapsed().as_secs(),
          "posts imported"
        );
      }
    }
    self.send_batch(item, conversation, &mut batch).await
  }

  /// Uploads the post's files from the export (a sticker once per conversation); a file the export
  /// lacks or that can't be uploaded is noted on the message.
  async fn upload_files(
    &mut self,
    conversation: ConversationId,
    post: &Post,
    prepared: PreparedPost,
    stickers: &mut HashMap<String, AttachmentRef>,
  ) -> Result<Pending, RunError> {
    let mut attachments = Vec::new();
    let mut missing = Vec::new();
    for file in &prepared.files {
      let is_sticker = file.path.starts_with(self.export.root.join("stickers"));
      if is_sticker && let Some(known) = stickers.get(&file.file_name) {
        attachments.push(known.clone());
        continue;
      }
      let Ok(metadata) = std::fs::metadata(&file.path) else {
        missing.push(file.file_name.clone());
        continue;
      };
      if self.maybe_max_file_bytes.is_some_and(|max| metadata.len() > max) {
        missing.push(file.file_name.clone());
        continue;
      }
      match self.upload_one(conversation, file, metadata.len()).await {
        Ok(attachment) => {
          if is_sticker {
            stickers.insert(file.file_name.clone(), attachment.clone());
          }
          attachments.push(attachment);
        },
        Err(error) => {
          warn!(post = post.id, %error, "a file could not be uploaded; the message names it");
          missing.push(file.file_name.clone());
        },
      }
    }
    Ok(Pending {
      prepared,
      attachments,
      missing,
    })
  }

  async fn upload_one(
    &mut self,
    conversation: ConversationId,
    file: &crate::convert::FileToUpload,
    size_bytes: u64,
  ) -> Result<AttachmentRef, CoreError> {
    let (maybe_thumbnail, maybe_dimensions) = if file.is_image && size_bytes <= MAX_THUMBNAIL_SOURCE_BYTES {
      match std::fs::read(&file.path) {
        Ok(bytes) => (
          teamcraft_core::thumbnail::thumbnail(&bytes, &file.media_type),
          teamcraft_core::thumbnail::image_dimensions(&bytes, &file.media_type),
        ),
        Err(_) => (None, None),
      }
    } else {
      (None, None)
    };
    let mut attempt = 0;
    loop {
      let mut upload = DirectUpload::new(
        conversation,
        UploadSource::Path {
          path: file.path.display().to_string(),
        },
        file.file_name.clone(),
        file.media_type.clone(),
        size_bytes,
      )
      .with_picture(maybe_thumbnail.clone(), maybe_dimensions);
      match self.drive_upload(&mut upload).await {
        Ok(attachment) => return Ok(attachment),
        Err(error) if matches!(*error, DirectUploadError::Offline(_)) && attempt < NETWORK_RETRIES => {
          self.client.abandon_direct_upload(&upload).await;
          attempt += 1;
          warn!(%error, attempt, "an upload failed for the network; trying again");
          tokio::time::sleep(pause(attempt)).await;
        },
        Err(error) => {
          self.client.abandon_direct_upload(&upload).await;
          return Err(match *error {
            DirectUploadError::Offline(error) | DirectUploadError::Failed(error) => error,
            other => CoreError::InvalidRequest(other.to_string()),
          });
        },
      }
    }
  }

  async fn drive_upload(
    &mut self,
    upload: &mut DirectUpload,
  ) -> Result<AttachmentRef, Box<DirectUploadError>> {
    loop {
      let mut step = self.client.start_direct_upload(upload).await.map_err(Box::new)?;
      let sent = step.sent().await;
      match self
        .client
        .finish_direct_upload(upload, step, sent)
        .await
        .map_err(Box::new)?
      {
        DirectUploadProgress::MoreToSend => continue,
        DirectUploadProgress::AllSent => {
          return self.client.complete_direct_upload(upload).await.map_err(Box::new);
        },
      }
    }
  }

  /// Encrypts and stores `batch`, retrying network failures, and records the IDs.
  async fn send_batch(
    &mut self,
    item: &Item,
    conversation: ConversationId,
    batch: &mut Vec<Pending>,
  ) -> Result<(), RunError> {
    if batch.is_empty() {
      return Ok(());
    }
    let mut drafts = Vec::with_capacity(batch.len());
    for pending in batch.iter() {
      let maybe_thread_root = match pending.prepared.maybe_root {
        Some(root) => self.progress.message_of(root)?,
        None => None,
      };
      drafts.push(pending.draft(maybe_thread_root));
    }
    let mut attempt = 0;
    let ids = loop {
      match self.client.send_imported(conversation, &drafts).await {
        Ok(ids) => break ids,
        Err(error) if is_network(&error) && attempt < NETWORK_RETRIES => {
          attempt += 1;
          warn!(%error, attempt, "storing posts failed for the network; trying again");
          tokio::time::sleep(pause(attempt)).await;
          self.keep_gateway().await;
        },
        Err(error) => return Err(error.into()),
      }
    };
    let posts: Vec<(SourceId, MessageId, i64)> = batch
      .iter()
      .zip(ids)
      .map(|(pending, id)| (pending.prepared.source_id, id, pending.prepared.sent_at_ms))
      .collect();
    let missing: Vec<(SourceId, String)> = batch
      .iter()
      .flat_map(|pending| {
        pending
          .missing
          .iter()
          .map(|name| (pending.prepared.source_id, name.clone()))
      })
      .collect();
    self.progress.record_posts(item.source, &posts, &missing)?;
    batch.clear();
    Ok(())
  }

  /// New channels get their members with the full history; a merged channel keeps its members.
  /// A DM's or the notes' participants were added when its group was founded.
  async fn add_members(&mut self, item: &Item, conversation: ConversationId) -> Result<(), RunError> {
    let Target::NewChannel { .. } = item.target else {
      return Ok(());
    };
    let me = self.client.user_id();
    let mut people: Vec<(UserId, bool)> = Vec::with_capacity(item.members.len());
    for user in item.members.iter().filter(|user| **user != me) {
      // A guest joins the space as a guest: the server refuses them any other role.
      let kind = self.client.api().user(*user).await.map_err(CoreError::from)?.kind;
      people.push((*user, kind == UserKind::Guest));
    }
    self
      .client
      .add_people_to_channel(conversation, &people, HistoryPolicy::Full, true)
      .await?;
    Ok(())
  }

  /// Makes the most active member who has set up the app the new channel's admin, so the import
  /// account can leave (a channel keeps an admin). Signing a role needs the person's identity key:
  /// nobody with the app yet waits for a later pass.
  async fn ensure_admin(&mut self, item: &Item, conversation: ConversationId) -> Result<bool, RunError> {
    let key = item.key();
    if !matches!(item.target, Target::NewChannel { .. }) || self.progress.is_done(&key, Step::Admin)? {
      return Ok(true);
    }
    let me = self.client.user_id();
    for candidate in item.admin_candidates.iter().filter(|user| **user != me) {
      let has_app = !self
        .client
        .api()
        .list_user_devices(*candidate)
        .await
        .map_err(CoreError::from)?
        .devices
        .is_empty();
      if !has_app {
        continue;
      }
      self
        .client
        .set_channel_role(conversation, *candidate, ChannelRole::Admin)
        .await?;
      self.progress.mark_done(&key, Step::Admin)?;
      return Ok(true);
    }
    self.progress.note(
      &key,
      now_ms(),
      "waits for a member with the app to become channel admin",
    )?;
    Ok(false)
  }

  async fn set_member_state(&mut self, item: &Item, conversation: ConversationId) -> Result<(), RunError> {
    let favorites: HashSet<(SourceId, SourceId)> = self.export.favorites.iter().copied().collect();
    let mutes: HashSet<(SourceId, SourceId)> = self.export.mutes.iter().copied().collect();
    // Only people who are members in TeamCraft: a merged channel keeps its own members.
    let members: HashSet<UserId> = self
      .client
      .api()
      .list_members(conversation)
      .await
      .map_err(CoreError::from)?
      .members
      .into_iter()
      .map(|member| member.user.id)
      .collect();
    let synology_members: Vec<SourceId> = self
      .export
      .channels
      .get(&item.source)
      .map(|channel| channel.members().to_vec())
      .unwrap_or_default();
    for source_user in synology_members {
      let Some((user, _)) = self.people.accounts.get(&source_user).cloned() else {
        continue;
      };
      if !members.contains(&user) {
        continue;
      }
      let maybe_read_through = match self.export.reads.get(&(item.source, source_user)) {
        Some(at) => self.progress.read_through(item.source, *at)?,
        None => None,
      };
      let request = ImportMemberStateRequest {
        maybe_read_through,
        is_favorite: favorites.contains(&(item.source, source_user)),
        is_muted: mutes.contains(&(item.source, source_user)),
      };
      if request == ImportMemberStateRequest::default() {
        continue;
      }
      self
        .client
        .set_import_member_state(conversation, user, &request)
        .await?;
    }
    Ok(())
  }

  /// Leaves the conversation once nobody waits for the import device any more; notes who does.
  async fn try_leave(&mut self, item: &Item, conversation: ConversationId) -> Result<(), RunError> {
    let key = item.key();
    if self.progress.is_done(&key, Step::Left)? || !self.progress.is_done(&key, Step::MemberState)? {
      return Ok(());
    }
    self.progress.clear_notes(&key)?;
    // A channel keeps an admin; an archived one takes no role change, so the admin comes first.
    if !self.ensure_admin(item, conversation).await? {
      return Ok(());
    }
    if item.is_archived
      && matches!(item.target, Target::NewChannel { .. })
      && !self.progress.is_done(&key, Step::Archived)?
    {
      self
        .client
        .update_channel(
          conversation,
          &UpdateConversationRequest {
            maybe_is_archived: Some(true),
            ..UpdateConversationRequest::default()
          },
        )
        .await?;
      self.progress.mark_done(&key, Step::Archived)?;
    }

    let waiting = pending_user_additions(self.client.store(), conversation)?;
    if !waiting.is_empty() {
      for (user, wait) in waiting {
        let name = self.username_of(user);
        let why = match wait {
          UserAddWait::NoDevice | UserAddWait::NeverSetUp => "hasn't set up the app yet",
          UserAddWait::NoKeyPackage => "has no key package left",
          UserAddWait::Unverifiable => "has no device that passes the checks",
        };
        self
          .progress
          .note(&key, now_ms(), &format!("waits for {name}, who {why}"))?;
      }
      return Ok(());
    }
    if !self.client.has_group(conversation)? {
      return Ok(());
    }
    let in_group: HashSet<UserId> = self.client.group_users(conversation)?.into_iter().collect();
    // A merged channel keeps its members: nobody of the old system is waited for there.
    let expected: &[UserId] = match item.target {
      Target::ExistingChannel { .. } => &[],
      _ => &item.members,
    };
    if let Some(missing) = expected.iter().find(|user| !in_group.contains(user)) {
      let name = self.username_of(*missing);
      self.progress.note(
        &key,
        now_ms(),
        &format!("waits for {name} to be in the encryption group"),
      )?;
      return Ok(());
    }
    match self.client.leave_after_import(conversation).await {
      Ok(()) => {
        info!(source = item.source, "the import account leaves the conversation");
        self.progress.mark_done(&key, Step::Left)?;
      },
      Err(error) => {
        self
          .progress
          .note(&key, now_ms(), &format!("could not leave yet: {error}"))?;
      },
    }
    Ok(())
  }

  fn username_of(&self, user: UserId) -> String {
    self
      .people
      .accounts
      .values()
      .find(|(id, _)| *id == user)
      .map_or_else(|| user.to_string(), |(_, name)| name.clone())
  }

  async fn space(&mut self, name: &str) -> Result<SpaceId, RunError> {
    if let Some(space) = self.space_id(name).await? {
      let me = self.client.user_id();
      // An admin adds themselves; already being a member is fine.
      if let Err(error) = self.client.add_space_members(space, &[me], false).await {
        warn!(%error, "joining the space");
      }
      return Ok(space);
    }
    let created = self
      .client
      .create_space(&CreateSpaceRequest {
        name: name.to_owned(),
        maybe_icon: None,
        maybe_color: None,
        maybe_description: None,
        join_policy: JoinPolicy::InviteOnly,
        maybe_idempotency_key: Some(idempotency_key(&format!("space:{name}"))),
      })
      .await?;
    self.spaces.insert(name.to_owned(), created.id);
    Ok(created.id)
  }

  async fn space_id(&mut self, name: &str) -> Result<Option<SpaceId>, RunError> {
    if let Some(space) = self.spaces.get(name) {
      return Ok(Some(*space));
    }
    let spaces = self
      .client
      .api()
      .admin_space_names()
      .await
      .map_err(CoreError::from)?;
    for (id, space) in spaces {
      self.spaces.insert(space, id);
    }
    Ok(self.spaces.get(name).copied())
  }
}

/// A post whose files are up.
struct Pending {
  prepared: PreparedPost,
  attachments: Vec<AttachmentRef>,
  missing: Vec<String>,
}

impl Pending {
  fn draft(&self, maybe_thread_root: Option<MessageId>) -> ImportDraft {
    let origin = ImportedOrigin {
      source: ImportSource::Synology,
      source_id: self.prepared.source_id.to_string(),
      author: self.prepared.author.clone(),
      sent_at_ms: self.prepared.sent_at_ms,
      maybe_edited_at_ms: self.prepared.maybe_edited_at_ms,
      reactions: self.prepared.reactions.clone(),
      missing_files: self.missing.clone(),
    };
    ImportDraft {
      content: MessageContent::Text {
        body: self.prepared.body.clone(),
        mentions: self.prepared.mentions.clone(),
        meta: MessageMeta {
          attachments: self.attachments.clone(),
          maybe_imported: Some(Box::new(origin)),
          ..MessageMeta::default()
        },
      },
      maybe_thread_root,
    }
  }

  /// What the message will weigh, roughly: text, thumbnails, the origin and the encryption.
  fn estimated_bytes(&self) -> usize {
    let thumbnails: usize = self
      .attachments
      .iter()
      .filter_map(|attachment| attachment.maybe_thumbnail.as_ref())
      .map(|thumbnail| thumbnail.as_slice().len())
      .sum();
    2 * (self.prepared.body.len() + thumbnails) + 1024
  }
}

/// The plan of a dry run: accounts looked up, not created (a name the server doesn't know yet is
/// created at the run, if the identity provider has it), and the spaces and channels to merge into
/// checked. Problems are noted for the report; nothing changes on the server.
pub async fn dry_plan<A: ServerApi, S: LocalStore>(
  client: &mut Client<A, S>,
  export: &Export,
  mapping: &Mapping,
  progress: &Progress,
) -> Result<Plan, RunError> {
  let mut accounts = HashMap::new();
  for (source, name) in &mapping.users {
    match client
      .api()
      .admin_find_user(name)
      .await
      .map_err(CoreError::from)?
    {
      Some(user) => {
        accounts.insert(*source, user);
      },
      None => progress.note(
        "users",
        now_ms(),
        &format!("{name}: not known to the server yet (created at the run if the identity provider has it)"),
      )?,
    }
  }
  let plan = plan(export, mapping, &accounts);
  let spaces: HashMap<String, SpaceId> = client
    .api()
    .admin_space_names()
    .await
    .map_err(CoreError::from)?
    .into_iter()
    .map(|(id, name)| (name, id))
    .collect();
  // Two new channels of one space with the same name, or a new one named like an existing one: the
  // operator decides (`teamcraft_name` in the mapping); the run would make both.
  let mut new_names: HashMap<(String, String), Vec<SourceId>> = HashMap::new();
  for item in &plan.items {
    if let Target::NewChannel { space, name, .. } = &item.target {
      new_names
        .entry((space.clone(), name.to_lowercase()))
        .or_default()
        .push(item.source);
    }
  }
  for item in &plan.items {
    let key = item.key();
    progress.clear_notes(&key)?;
    if let Target::NewChannel { space, name, .. } = &item.target {
      let same = &new_names[&(space.clone(), name.to_lowercase())];
      if same.len() > 1 {
        let others: Vec<String> = same
          .iter()
          .filter(|source| **source != item.source)
          .map(ToString::to_string)
          .collect();
        progress.note(
          &key,
          now_ms(),
          &format!(
            "problem: Synology channel(s) {} get the same name in {space:?}",
            others.join(", ")
          ),
        )?;
      }
      if let Some(space_id) = spaces.get(space) {
        let existing = client
          .api()
          .admin_channel_names(*space_id)
          .await
          .map_err(CoreError::from)?;
        if existing.iter().any(|(_, maybe_name)| {
          maybe_name
            .as_deref()
            .is_some_and(|other| other.eq_ignore_ascii_case(name))
        }) {
          progress.note(
            &key,
            now_ms(),
            &format!("problem: {space}/{name} exists already; a second channel of that name would be made"),
          )?;
        }
      }
    }
    match &item.target {
      Target::NewChannel { space, .. } if !spaces.contains_key(space) => {
        progress.note(&key, now_ms(), &format!("the space {space:?} will be created"))?;
      },
      Target::ExistingChannel { space, name } => match spaces.get(space) {
        None => progress.note(
          &key,
          now_ms(),
          &format!("problem: the space {space:?} doesn't exist"),
        )?,
        Some(space_id) => {
          let channels = client
            .api()
            .admin_channel_names(*space_id)
            .await
            .map_err(CoreError::from)?;
          if !channels
            .iter()
            .any(|(_, maybe_name)| maybe_name.as_deref() == Some(name.as_str()))
          {
            progress.note(
              &key,
              now_ms(),
              &format!("problem: the channel {space}/{name} doesn't exist"),
            )?;
          }
        },
      },
      _ => {},
    }
  }
  Ok(plan)
}

/// The mapped accounts: created from the identity provider where they don't exist yet. A name the
/// provider doesn't know is a problem of the mapping, noted and left out.
async fn resolve_accounts<A: ServerApi, S: LocalStore>(
  client: &mut Client<A, S>,
  mapping: &Mapping,
  progress: &Progress,
) -> Result<HashMap<SourceId, (UserId, String)>, RunError> {
  let mut accounts = HashMap::new();
  let names: Vec<(SourceId, String)> = mapping
    .users
    .iter()
    .map(|(source, name)| (*source, name.clone()))
    .collect();
  for chunk in names.chunks(teamcraft_proto::MAX_PROVISION_BATCH) {
    let provisioned = client
      .provision_users(chunk.iter().map(|(_, name)| name.clone()).collect())
      .await?;
    for ((source, name), answer) in chunk.iter().zip(provisioned) {
      match (answer.status, answer.maybe_user_id) {
        (ProvisionStatus::Created | ProvisionStatus::Existing, Some(user)) => {
          accounts.insert(*source, (user, name.clone()));
        },
        (status, _) => {
          warn!(%status, "a mapped user name has no usable account");
          progress.note("users", now_ms(), &format!("{name}: {status}"))?;
        },
      }
    }
  }
  Ok(accounts)
}

fn idempotency_key(key: &str) -> IdempotencyKey {
  let derived = imported_message_id(
    ConversationId::from_snowflake(teamcraft_proto::Snowflake::ZERO),
    ImportSource::Synology,
    key,
  );
  IdempotencyKey::from_bytes(*derived.as_bytes())
}

fn is_network(error: &CoreError) -> bool {
  matches!(
    error,
    CoreError::Api(ApiError::Network(_) | ApiError::Intercepted { .. })
  ) || error.is_transient()
}

fn pause(attempt: u32) -> Duration {
  RETRY_PAUSE
    .saturating_mul(2_u32.saturating_pow(attempt.saturating_sub(1)))
    .min(MAX_RETRY_PAUSE)
}

pub fn now_ms() -> i64 {
  i64::try_from(
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis(),
  )
  .unwrap_or(i64::MAX)
}
