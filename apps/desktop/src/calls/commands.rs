//! Tauri commands for calls. Errors cross as plain text, like the core's commands.

use std::sync::Arc;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, State};
use teamcraft_client_api::calls::MediaDevices;
use teamcraft_client_api::remote::{CallAction, CallSnapshot};
use teamcraft_proto::MediaStreamId;

use super::{CallHost, push_to_talk, video};
use crate::core::commands::{CommandError, CoreHost};

#[tauri::command]
pub fn call_snapshot(calls: State<'_, Arc<CallHost>>) -> CallSnapshot {
  calls.snapshot()
}

/// Lists microphones and speakers again; the snapshot keeps the result.
#[tauri::command]
pub async fn call_media_devices(calls: State<'_, Arc<CallHost>>) -> Result<MediaDevices, CommandError> {
  Ok(calls.refresh_devices().await?)
}

/// Streams another device's video track to the webview through `frames`, one binary message
/// per frame (format: `calls::video::video_message`): H.264 for WebCodecs, or with `prefer_jpeg`
/// JPEG images for webviews without it.
#[tauri::command]
pub async fn call_watch_video(
  calls: State<'_, Arc<CallHost>>,
  stream_id: MediaStreamId,
  maybe_max_layer: Option<u8>,
  prefer_jpeg: bool,
  frames: Channel,
) -> Result<(), CommandError> {
  let send = move |message| {
    if let Err(error) = frames.send(InvokeResponseBody::Raw(message)) {
      tracing::debug!(%error, "video frame not delivered to the webview");
    }
  };
  let sink = if prefer_jpeg {
    calls.ensure_video_codec().await?;
    video::jpeg_sink(send)
  } else {
    video::h264_sink(send)
  };
  calls.watch_video(stream_id, maybe_max_layer, sink).await;
  Ok(())
}

#[tauri::command]
pub async fn call_unwatch_video(
  calls: State<'_, Arc<CallHost>>,
  stream_id: MediaStreamId,
) -> Result<(), CommandError> {
  calls.unwatch_video(stream_id).await;
  Ok(())
}

#[tauri::command]
pub async fn call_action(
  app: AppHandle,
  core: State<'_, CoreHost>,
  calls: State<'_, Arc<CallHost>>,
  action: CallAction,
) -> Result<(), CommandError> {
  match action {
    CallAction::Join { request } => {
      let preparation = core.current()?.prepare_call(request.conversation).await?;
      calls.join(request, preparation).await?;
    },
    CallAction::Leave => calls.leave().await,
    CallAction::Decline { conversation } => calls.decline(conversation),
    CallAction::SetMuted { is_muted } => calls.set_muted(is_muted).await,
    CallAction::SetDeafened { is_deafened } => calls.set_deafened(is_deafened).await,
    CallAction::SelectMicrophone { maybe_device } => calls.select_microphone(maybe_device).await?,
    CallAction::SelectSpeakers { maybe_device } => calls.select_speakers(maybe_device).await?,
    CallAction::SetCamera { is_on } => calls.set_camera(is_on).await?,
    CallAction::SelectCamera { maybe_device } => calls.select_camera(maybe_device).await?,
    CallAction::SetBackgroundEffect { effect } => calls.set_background_effect(effect),
    CallAction::StartScreenShare => calls.start_screen_share().await?,
    CallAction::StopScreenShare => calls.stop_screen_share().await,
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
    CallAction::Unknown => return Err(CommandError::from("unknown call action".to_owned())),
  }
  Ok(())
}
