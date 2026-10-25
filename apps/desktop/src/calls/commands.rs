//! Tauri commands for calls. Errors cross as plain text, like the core's commands.

use std::sync::Arc;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, State};
use teamcraft_client_api::calls::{BackgroundEffect as UiBackgroundEffect, MediaDevices, OrgBackgrounds};
use teamcraft_client_api::remote::{CallAction, CallSnapshot};
use teamcraft_client_api::remote::{ChatAnswer, ChatQuery};
use teamcraft_media::effects::BackgroundImage;
use teamcraft_proto::MediaStreamId;

use super::{CallHost, ChannelId, CoreJoinTickets, push_to_talk, video};
use crate::core::commands::{CommandError, CoreHost};

#[tauri::command(rename_all = "snake_case")]
pub fn call_snapshot(calls: State<'_, Arc<CallHost>>) -> CallSnapshot {
  calls.snapshot()
}

/// Lists microphones and speakers again; the snapshot keeps the result.
#[tauri::command(rename_all = "snake_case")]
pub async fn call_media_devices(calls: State<'_, Arc<CallHost>>) -> Result<MediaDevices, CommandError> {
  Ok(calls.refresh_devices().await?)
}

/// Streams another device's video track to the webview through `frames`, one binary message
/// per frame (format: `calls::video::video_message`): H.264 for WebCodecs, or with `prefer_jpeg`
/// JPEG images for webviews without it.
#[tauri::command(rename_all = "snake_case")]
pub async fn call_watch_video(
  calls: State<'_, Arc<CallHost>>,
  stream_id: MediaStreamId,
  maybe_max_layer: Option<u8>,
  prefer_jpeg: bool,
  frames: Channel,
) -> Result<(), CommandError> {
  let channel = ChannelId(frames.id());
  let send = move |message| match frames.send(InvokeResponseBody::Raw(message)) {
    Ok(()) => true,
    Err(error) => {
      tracing::debug!(%error, "video frame not delivered to the webview");
      false
    },
  };
  let sink = if prefer_jpeg {
    calls.ensure_video_codec().await?;
    video::jpeg_sink(send)
  } else {
    video::h264_sink(send)
  };
  calls.watch_video(stream_id, channel, maybe_max_layer, sink).await;
  Ok(())
}

/// Streams the own camera, as the call sends it (with background effects), to the webview's self
/// view through `frames`: JPEG images in the format of `call_watch_video`.
#[tauri::command(rename_all = "snake_case")]
pub fn call_watch_camera(calls: State<'_, Arc<CallHost>>, frames: Channel) {
  let channel = ChannelId(frames.id());
  calls.watch_camera(channel, move |message| {
    match frames.send(InvokeResponseBody::Raw(message)) {
      Ok(()) => true,
      Err(error) => {
        tracing::debug!(%error, "self view image not delivered to the webview");
        false
      },
    }
  });
}

/// Stops the self view; `channel` is the id of the `frames` channel it was started with.
#[tauri::command(rename_all = "snake_case")]
pub fn call_unwatch_camera(calls: State<'_, Arc<CallHost>>, channel: u32) {
  calls.unwatch_camera(ChannelId(channel));
}

#[tauri::command(rename_all = "snake_case")]
pub async fn call_unwatch_video(
  calls: State<'_, Arc<CallHost>>,
  stream_id: MediaStreamId,
  channel: u32,
) -> Result<(), CommandError> {
  calls.unwatch_video(stream_id, ChannelId(channel)).await;
  Ok(())
}

/// Applies `requested` as far as the organization's rule allows (admin.md §3.5) and loads the
/// picture it needs: the user's own (decrypted) or the organization's (hash-checked).
async fn apply_background(
  core: &CoreHost,
  calls: &CallHost,
  requested: UiBackgroundEffect,
) -> Result<(), CommandError> {
  let org = org_backgrounds(core).await;
  let effect = org.effective(requested);
  match load_background_picture(core, effect).await {
    Ok(maybe_image) => {
      calls.set_background_effect(effect, maybe_image, org.is_required());
      Ok(())
    },
    // A required picture that can't be fetched or doesn't match the signed hash: hide the room
    // anyway (strong blur, or the camera goes off) rather than send it.
    Err(error) if org.is_required() => {
      calls.fail_closed_background().await;
      Err(error.into())
    },
    Err(error) => Err(error.into()),
  }
}

/// Opens the system's video effects panel (macOS 14+: Portrait, background replacement, Studio
/// Light), where the user turns them off; AppKit wants it on the main thread.
#[cfg(target_os = "macos")]
fn open_system_video_effects(app: &AppHandle) -> Result<(), String> {
  use avcapture::{CaptureDevice, CaptureSystemUserInterface};

  app
    .run_on_main_thread(|| {
      if let Err(error) = CaptureDevice::show_system_user_interface(CaptureSystemUserInterface::VideoEffects)
      {
        tracing::warn!(%error, "the video effects panel did not open");
      }
    })
    .map_err(|error| error.to_string())
}

/// Other systems have no video effects of their own.
#[cfg(not(target_os = "macos"))]
fn open_system_video_effects(_app: &AppHandle) -> Result<(), String> {
  Ok(())
}

/// The decoded picture `effect` needs, if any.
async fn load_background_picture(
  core: &CoreHost,
  effect: UiBackgroundEffect,
) -> Result<Option<Arc<BackgroundImage>>, String> {
  let picture = match effect {
    UiBackgroundEffect::Custom(id) => core.current()?.background_picture(id).await?,
    UiBackgroundEffect::Org(id) => core.current()?.org_background_picture(id).await?,
    _ => return Ok(None),
  };
  tauri::async_runtime::spawn_blocking(move || video::decode_background(&picture))
    .await
    .map_err(|error| error.to_string())?
    .map(Some)
}

/// The organization's backgrounds and rule from the verified policy; without a running core
/// nothing is known, and nothing is restricted.
async fn org_backgrounds(core: &CoreHost) -> OrgBackgrounds {
  let Ok(runtime) = core.current() else {
    return OrgBackgrounds::default();
  };
  match runtime.query(ChatQuery::OrgBackgrounds).await {
    Ok(ChatAnswer::OrgBackgrounds { backgrounds }) => backgrounds,
    _ => OrgBackgrounds::default(),
  }
}

/// A small JPEG of a preset or one of the user's own backgrounds, for the picker.
#[tauri::command(rename_all = "snake_case")]
pub async fn call_background_preview(
  core: State<'_, CoreHost>,
  effect: UiBackgroundEffect,
  width: u32,
  height: u32,
) -> Result<tauri::ipc::Response, CommandError> {
  let maybe_picture = match effect {
    UiBackgroundEffect::Custom(id) => Some(core.current()?.background_picture(id).await?),
    UiBackgroundEffect::Org(id) => Some(core.current()?.org_background_picture(id).await?),
    _ => None,
  };
  let (width, height) = (width.clamp(16, 640), height.clamp(16, 360));
  let jpeg = tauri::async_runtime::spawn_blocking(move || {
    video::background_preview(effect, maybe_picture.as_deref(), width, height)
  })
  .await
  .map_err(|error| error.to_string())??;
  Ok(tauri::ipc::Response::new(jpeg))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn call_action(
  app: AppHandle,
  core: State<'_, CoreHost>,
  calls: State<'_, Arc<CallHost>>,
  action: CallAction,
) -> Result<(), CommandError> {
  match action {
    CallAction::Join { request } => {
      let runtime = core.current()?;
      let preparation = runtime.prepare_call(request.conversation).await?;
      let tickets = Arc::new(CoreJoinTickets::new(runtime, request.conversation));
      calls.join(request, preparation, tickets).await?;
    },
    CallAction::Leave => calls.leave().await,
    CallAction::Decline { conversation } => calls.decline(conversation),
    CallAction::SetMuted { is_muted } => calls.set_muted(is_muted).await,
    CallAction::SetDeafened { is_deafened } => calls.set_deafened(is_deafened).await,
    CallAction::SelectMicrophone { maybe_device } => calls.select_microphone(maybe_device).await?,
    CallAction::SelectSpeakers { maybe_device } => calls.select_speakers(maybe_device).await?,
    CallAction::SetCamera { is_on } => {
      if is_on {
        // The current choice under the current rule (it may have changed since it was made).
        let current = calls
          .snapshot()
          .maybe_active
          .map(|active| active.background)
          .unwrap_or_default();
        apply_background(&core, &calls, current).await?;
      }
      calls.set_camera(is_on).await?;
    },
    CallAction::SelectCamera { maybe_device } => calls.select_camera(maybe_device).await?,
    CallAction::SetBackgroundEffect { effect } => apply_background(&core, &calls, effect).await?,
    CallAction::StartScreenShare { has_sound } => calls.start_screen_share(has_sound).await?,
    CallAction::StopScreenShare => calls.stop_screen_share().await,
    CallAction::OpenSystemVideoEffects => open_system_video_effects(&app)?,
    CallAction::SetPushToTalk {
      is_enabled,
      maybe_shortcut,
    } => {
      let current = calls.snapshot().push_to_talk;
      let shortcut = maybe_shortcut.unwrap_or(current.shortcut.clone());
      let maybe_previous = current.is_global.then_some(current.shortcut.as_str());
      let maybe_new = is_enabled.then_some(shortcut.as_str());
      let is_global = push_to_talk::register(&app, &calls, maybe_previous, maybe_new).await;
      calls.set_push_to_talk(is_enabled, shortcut, is_global).await;
    },
    CallAction::SetTalking { is_talking } => calls.set_talking(is_talking).await,
    CallAction::SetStatusSounds { sounds } => calls.set_status_sounds(sounds).await,
    CallAction::RefreshDevices => drop(calls.refresh_devices().await?),
    CallAction::TestSpeakers { maybe_device } => calls.test_speakers(maybe_device).await?,
    CallAction::Unknown => return Err(CommandError::from("unknown call action".to_owned())),
  }
  Ok(())
}
