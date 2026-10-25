use teamcraft_proto::{ConversationId, DeviceId, MessageId, MlsMessageId, MlsMessageKind, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// One entry of a conversation's MLS delivery queue. The payload is opaque to the server.
#[derive(Debug, Clone)]
pub struct StoredMlsMessage {
  pub id: MlsMessageId,
  pub conversation_id: ConversationId,
  pub epoch: u64,
  pub kind: MlsMessageKind,
  pub sender_device_id: DeviceId,
  pub maybe_recipient_device_id: Option<DeviceId>,
  pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct NewMlsMessage<'a> {
  pub id: MlsMessageId,
  pub kind: MlsMessageKind,
  pub maybe_recipient: Option<DeviceId>,
  pub payload: &'a [u8],
}

/// Result of posting a commit.
#[derive(Debug, Clone)]
pub enum CommitOutcome {
  /// The commit won its epoch; the group is now in `new_epoch`.
  Accepted { new_epoch: u64 },
  /// Somebody else committed for that epoch first.
  EpochConflict { current_epoch: u64 },
}

/// Result of posting a proposal or application message.
#[derive(Debug, Clone)]
pub enum PostOutcome {
  Accepted,
  /// The group is not in the epoch the message was created for.
  EpochConflict {
    current_epoch: u64,
  },
}

/// Which entries a reader gets.
#[derive(Debug, Clone)]
pub struct MlsBacklogQuery {
  pub conversation: ConversationId,
  pub maybe_after: Option<MlsMessageId>,
  /// Entries before this message ID predate the reader's membership.
  pub maybe_not_before: Option<MessageId>,
  /// Welcomes are only visible to the owner of the addressed device.
  pub reader: UserId,
  pub limit: i64,
}

impl Database {
  /// Appends a commit (and the welcomes it creates) if the group is still in `epoch`.
  ///
  /// The epoch counter is advanced with a conditional UPDATE in the same transaction as the insert,
  /// so of several commits for one epoch exactly one succeeds.
  pub async fn post_mls_commit(
    &self,
    conversation: ConversationId,
    epoch: u64,
    sender: DeviceId,
    commit: &NewMlsMessage<'_>,
    welcomes: &[NewMlsMessage<'_>],
    maybe_group_info: Option<&[u8]>,
  ) -> Result<CommitOutcome, DbError> {
    let epoch_db = epoch_to_db(epoch)?;
    let mut tx = self.pool().begin().await?;
    ensure_head(&mut tx, conversation).await?;
    let advanced = sqlx::query_scalar!(
      "UPDATE mls_epoch_heads SET epoch = epoch + 1 WHERE conversation_id = $1 AND epoch = $2 RETURNING epoch",
      to_db(conversation),
      epoch_db,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(new_epoch) = advanced else {
      let current = current_epoch_in(&mut tx, conversation).await?;
      return Ok(CommitOutcome::EpochConflict {
        current_epoch: current,
      });
    };

    insert_message(&mut tx, conversation, epoch_db, sender, commit).await?;
    for welcome in welcomes {
      insert_message(&mut tx, conversation, new_epoch, sender, welcome).await?;
    }
    if let Some(group_info) = maybe_group_info {
      store_group_info(&mut tx, conversation, new_epoch, group_info).await?;
    }
    tx.commit().await?;
    Ok(CommitOutcome::Accepted {
      new_epoch: u64::try_from(new_epoch).unwrap_or(u64::MAX),
    })
  }

  /// Appends a proposal or application message if the group is still in `epoch`. A concurrent
  /// commit waits for this transaction (shared row lock), so the message lands before the epoch moves.
  pub async fn post_mls_message(
    &self,
    conversation: ConversationId,
    epoch: u64,
    sender: DeviceId,
    message: &NewMlsMessage<'_>,
  ) -> Result<PostOutcome, DbError> {
    let epoch_db = epoch_to_db(epoch)?;
    let mut tx = self.pool().begin().await?;
    ensure_head(&mut tx, conversation).await?;
    let current = sqlx::query_scalar!(
      "SELECT epoch FROM mls_epoch_heads WHERE conversation_id = $1 FOR SHARE",
      to_db(conversation),
    )
    .fetch_one(&mut *tx)
    .await?;
    if current != epoch_db {
      return Ok(PostOutcome::EpochConflict {
        current_epoch: u64::try_from(current).unwrap_or(u64::MAX),
      });
    }
    insert_message(&mut tx, conversation, epoch_db, sender, message).await?;
    tx.commit().await?;
    Ok(PostOutcome::Accepted)
  }

  /// Stores the group information for `epoch` if the group is still in it (a member publishing it
  /// later than its commit). With `is_replacement`, information that holds epoch 0 is replaced
  /// (the caller checked that this is allowed).
  pub async fn put_mls_group_info(
    &self,
    conversation: ConversationId,
    epoch: u64,
    group_info: &[u8],
    is_replacement: bool,
  ) -> Result<PostOutcome, DbError> {
    let epoch_db = epoch_to_db(epoch)?;
    let mut tx = self.pool().begin().await?;
    ensure_head(&mut tx, conversation).await?;
    let current = sqlx::query_scalar!(
      "SELECT epoch FROM mls_epoch_heads WHERE conversation_id = $1 FOR SHARE",
      to_db(conversation),
    )
    .fetch_one(&mut *tx)
    .await?;
    if current != epoch_db {
      return Ok(PostOutcome::EpochConflict {
        current_epoch: u64::try_from(current).unwrap_or(u64::MAX),
      });
    }
    if !store_group_info(&mut tx, conversation, epoch_db, group_info).await? {
      // Another founder's information holds epoch 0 (see `store_group_info`).
      if !(is_replacement && epoch_db == 0) {
        return Ok(PostOutcome::EpochConflict { current_epoch: epoch });
      }
      sqlx::query!(
        "UPDATE mls_group_infos SET group_info = $2, updated_at = now() WHERE conversation_id = $1 AND epoch = 0",
        to_db(conversation),
        group_info,
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(PostOutcome::Accepted)
  }

  /// The group information for the group's current epoch, if a member published one.
  pub async fn current_mls_group_info(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<(u64, Vec<u8>)>, DbError> {
    let row = sqlx::query!(
      r#"SELECT g.epoch, g.group_info FROM mls_group_infos g
         JOIN mls_epoch_heads h ON h.conversation_id = g.conversation_id AND h.epoch = g.epoch
         WHERE g.conversation_id = $1"#,
      to_db(conversation),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| (u64::try_from(row.epoch).unwrap_or(u64::MAX), row.group_info)))
  }

  pub async fn current_epoch(&self, conversation: ConversationId) -> Result<u64, DbError> {
    let epoch = sqlx::query_scalar!(
      "SELECT epoch FROM mls_epoch_heads WHERE conversation_id = $1",
      to_db(conversation),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(epoch.map_or(0, |epoch| u64::try_from(epoch).unwrap_or(u64::MAX)))
  }

  /// Backlog of a conversation, oldest first; `limit + 1` rows so callers can tell if more exist.
  pub async fn list_mls_messages(&self, query: &MlsBacklogQuery) -> Result<Vec<StoredMlsMessage>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.id, m.conversation_id, m.epoch, m.kind, m.sender_device_id, m.recipient_device_id, m.payload
         FROM mls_messages m
         WHERE m.conversation_id = $1
           AND ($2::bigint IS NULL OR m.id > $2)
           AND ($3::bigint IS NULL OR m.id >= $3)
           AND (m.kind <> 'welcome' OR EXISTS (
                 SELECT 1 FROM devices d WHERE d.id = m.recipient_device_id AND d.user_id = $4))
         ORDER BY m.id ASC LIMIT $5"#,
      to_db(query.conversation),
      query.maybe_after.map(to_db),
      query.maybe_not_before.map(to_db),
      to_db(query.reader),
      query.limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        stored_from_parts(
          row.id,
          row.conversation_id,
          row.epoch,
          row.kind,
          row.sender_device_id,
          row.recipient_device_id,
          row.payload,
        )
      })
      .collect()
  }

  /// Welcomes addressed to one device across all conversations, oldest first (`limit + 1` rows).
  pub async fn list_welcomes_for_device(
    &self,
    device: DeviceId,
    maybe_after: Option<MlsMessageId>,
    limit: i64,
  ) -> Result<Vec<StoredMlsMessage>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, conversation_id, epoch, kind, sender_device_id, recipient_device_id, payload
         FROM mls_messages
         WHERE recipient_device_id = $1 AND kind = 'welcome' AND ($2::bigint IS NULL OR id > $2)
         ORDER BY id ASC LIMIT $3"#,
      to_db(device),
      maybe_after.map(to_db),
      limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        stored_from_parts(
          row.id,
          row.conversation_id,
          row.epoch,
          row.kind,
          row.sender_device_id,
          row.recipient_device_id,
          row.payload,
        )
      })
      .collect()
  }

  /// Owners of the given devices (to check welcome recipients and address gateway events).
  pub async fn device_owners(&self, devices: &[DeviceId]) -> Result<Vec<(DeviceId, UserId)>, DbError> {
    let ids: Vec<i64> = devices.iter().map(|d| to_db(*d)).collect();
    let rows = sqlx::query!(
      "SELECT id, user_id FROM devices WHERE id = ANY($1) AND revoked_at IS NULL",
      &ids,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.id)?, from_db(row.user_id)?)))
      .collect()
  }
}

async fn ensure_head(tx: &mut sqlx::PgConnection, conversation: ConversationId) -> Result<(), DbError> {
  sqlx::query!(
    "INSERT INTO mls_epoch_heads (conversation_id, epoch) VALUES ($1, 0) ON CONFLICT DO NOTHING",
    to_db(conversation),
  )
  .execute(tx)
  .await?;
  Ok(())
}

/// Stores the group information of `epoch`; `false` when it was not stored. At epoch 0 the first
/// information wins: groups founded by two devices at once differ, and only the first one may be
/// joined (a repeat of the same bytes is fine). From epoch 1 on, the group is the same one and the
/// newest information replaces the older.
async fn store_group_info(
  tx: &mut sqlx::PgConnection,
  conversation: ConversationId,
  epoch: i64,
  group_info: &[u8],
) -> Result<bool, DbError> {
  let result = sqlx::query!(
    r#"INSERT INTO mls_group_infos (conversation_id, epoch, group_info) VALUES ($1, $2, $3)
       ON CONFLICT (conversation_id) DO UPDATE
         SET epoch = EXCLUDED.epoch, group_info = EXCLUDED.group_info, updated_at = now()
         WHERE mls_group_infos.epoch < EXCLUDED.epoch
            OR (mls_group_infos.epoch = EXCLUDED.epoch
                AND (EXCLUDED.epoch > 0 OR mls_group_infos.group_info = EXCLUDED.group_info))"#,
    to_db(conversation),
    epoch,
    group_info,
  )
  .execute(tx)
  .await?;
  Ok(result.rows_affected() > 0)
}

async fn current_epoch_in(tx: &mut sqlx::PgConnection, conversation: ConversationId) -> Result<u64, DbError> {
  let epoch = sqlx::query_scalar!(
    "SELECT epoch FROM mls_epoch_heads WHERE conversation_id = $1",
    to_db(conversation)
  )
  .fetch_one(tx)
  .await?;
  Ok(u64::try_from(epoch).unwrap_or(u64::MAX))
}

async fn insert_message(
  tx: &mut sqlx::PgConnection,
  conversation: ConversationId,
  epoch: i64,
  sender: DeviceId,
  message: &NewMlsMessage<'_>,
) -> Result<(), DbError> {
  sqlx::query!(
    r#"INSERT INTO mls_messages (id, conversation_id, epoch, kind, sender_device_id, recipient_device_id, payload)
       VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    to_db(message.id),
    to_db(conversation),
    epoch,
    message.kind.as_str(),
    to_db(sender),
    message.maybe_recipient.map(to_db),
    message.payload,
  )
  .execute(tx)
  .await?;
  Ok(())
}

fn epoch_to_db(epoch: u64) -> Result<i64, DbError> {
  i64::try_from(epoch).map_err(|_| DbError::InvalidStoredValue("epoch out of range".into()))
}

fn stored_from_parts(
  id: i64,
  conversation_id: i64,
  epoch: i64,
  kind: String,
  sender_device_id: i64,
  recipient_device_id: Option<i64>,
  payload: Vec<u8>,
) -> Result<StoredMlsMessage, DbError> {
  Ok(StoredMlsMessage {
    id: from_db(id)?,
    conversation_id: from_db(conversation_id)?,
    epoch: u64::try_from(epoch).map_err(|_| DbError::InvalidStoredValue("negative epoch".into()))?,
    kind: MlsMessageKind::from(kind),
    sender_device_id: from_db(sender_device_id)?,
    maybe_recipient_device_id: from_db_opt(recipient_device_id)?,
    payload,
  })
}
