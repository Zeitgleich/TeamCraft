//! Settings that follow the account on the mobile apps (issue #945, e2ee.md §6.4). The rules and the
//! mapping onto the account's keys are `teamcraft_client_api::mobile_settings`; the app hands over
//! its current values and stores what comes back.

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::calls::CallStatusSounds as CallStatusSoundsApi;
use teamcraft_client_api::mobile_settings::{
  AnimatedImages, ListGrouping, ListLayout, MobileSettings, MobileSyncState, sync_mobile_settings,
};

use crate::TeamCraft;
use crate::calls::CallStatusSounds;
use crate::error::TeamCraftError;

/// The settings of this device that follow the account, with the app's defaults.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncedAppSettings {
  pub list_layout: AppListLayout,
  pub list_grouping: AppListGrouping,
  pub animated_images: AppAnimatedImages,
  pub shares_typing: bool,
  pub first_name_is_mention: bool,
  pub channel_mentions_notify: bool,
  pub keywords: Vec<String>,
  /// Ask before `@channel` in conversations with more members than this; 0 never asks.
  pub confirm_channel_over: u32,
  pub call_status_sounds: CallStatusSounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppListLayout {
  Spaces,
  List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppListGrouping {
  Mixed,
  BySpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppAnimatedImages {
  Autoplay,
  OnTap,
}

/// What [`TeamCraft::sync_settings`] decided.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingsSyncOutcome {
  /// What this device should hold now: store the values that differ from what it has.
  pub settings: SyncedAppSettings,
  /// Opaque; store it and hand it to the next call, also across restarts.
  pub state: String,
}

#[uniffi::export]
impl TeamCraft {
  /// Moves when the account's synced settings changed (another device, or this one's write). 0
  /// until this device merged the account once: settings don't follow it before. Cheap; read it
  /// after [`crate::EventListener::on_change`] and call [`Self::sync_settings`] when it moved.
  pub async fn settings_mark(&self) -> Result<u64, TeamCraftError> {
    self.read(|reads| Ok(reads.backend()?.settings_mark())).await
  }

  /// Brings this device's settings and the account's in step (the account is end-to-end encrypted,
  /// e2ee.md §6.4). Call it at start, when [`Self::settings_mark`] moved, and when the user changed
  /// one of these settings (after a short rest, so a row of changes costs one upload). `state` is
  /// what the last call returned, or empty the first time. Before the device merged the account
  /// once nothing changes, and the changes wait for the next call.
  pub async fn sync_settings(
    &self,
    local: SyncedAppSettings,
    state: String,
  ) -> Result<SettingsSyncOutcome, TeamCraftError> {
    let unchanged = SettingsSyncOutcome {
      settings: local.clone(),
      state: state.clone(),
    };
    let maybe_outcome = self
      .read(move |reads| {
        let backend = reads.backend()?;
        // The device can't yet tell an empty account from one it hasn't heard of.
        if backend.settings_mark() == 0 {
          return Ok(None);
        }
        Ok(Some(sync_mobile_settings(
          local.into(),
          &backend.account_settings(),
          MobileSyncState::decode(&state),
          backend.me().id,
        )))
      })
      .await?;
    let Some(outcome) = maybe_outcome else {
      return Ok(unchanged);
    };
    if !outcome.writes.is_empty() {
      let writes = outcome.writes;
      self
        .act_locally(move |reads| {
          reads.backend()?.write_settings(writes);
          Ok(())
        })
        .await?;
    }
    Ok(SettingsSyncOutcome {
      settings: outcome.settings.into(),
      state: outcome.state.encode(),
    })
  }
}

impl From<SyncedAppSettings> for MobileSettings {
  fn from(app: SyncedAppSettings) -> Self {
    Self {
      list_layout: match app.list_layout {
        AppListLayout::Spaces => ListLayout::Spaces,
        AppListLayout::List => ListLayout::List,
      },
      list_grouping: match app.list_grouping {
        AppListGrouping::Mixed => ListGrouping::Mixed,
        AppListGrouping::BySpace => ListGrouping::BySpace,
      },
      animated_images: match app.animated_images {
        AppAnimatedImages::Autoplay => AnimatedImages::Autoplay,
        AppAnimatedImages::OnTap => AnimatedImages::OnTap,
      },
      shares_typing: app.shares_typing,
      first_name_is_mention: app.first_name_is_mention,
      channel_mentions_notify: app.channel_mentions_notify,
      keywords: app.keywords,
      confirm_channel_over: app.confirm_channel_over,
      call_status_sounds: match app.call_status_sounds {
        CallStatusSounds::All => CallStatusSoundsApi::All,
        CallStatusSounds::OwnOnly => CallStatusSoundsApi::OwnOnly,
        CallStatusSounds::Off => CallStatusSoundsApi::Off,
      },
    }
  }
}

impl From<MobileSettings> for SyncedAppSettings {
  fn from(settings: MobileSettings) -> Self {
    Self {
      list_layout: match settings.list_layout {
        ListLayout::Spaces => AppListLayout::Spaces,
        ListLayout::List => AppListLayout::List,
      },
      list_grouping: match settings.list_grouping {
        ListGrouping::Mixed => AppListGrouping::Mixed,
        ListGrouping::BySpace => AppListGrouping::BySpace,
      },
      animated_images: match settings.animated_images {
        AnimatedImages::Autoplay => AppAnimatedImages::Autoplay,
        AnimatedImages::OnTap => AppAnimatedImages::OnTap,
      },
      shares_typing: settings.shares_typing,
      first_name_is_mention: settings.first_name_is_mention,
      channel_mentions_notify: settings.channel_mentions_notify,
      keywords: settings.keywords,
      confirm_channel_over: settings.confirm_channel_over,
      call_status_sounds: match settings.call_status_sounds {
        CallStatusSoundsApi::OwnOnly => CallStatusSounds::OwnOnly,
        CallStatusSoundsApi::Off => CallStatusSounds::Off,
        // `adopt` never takes the catch-all, so it can only be the default here.
        CallStatusSoundsApi::All | CallStatusSoundsApi::Unknown => CallStatusSounds::All,
      },
    }
  }
}
