//! Tauri commands for calls. Errors cross as plain text, like the core's commands.

use std::sync::Arc;

use tauri::State;
use teamcraft_client_api::calls::MediaDevices;
use teamcraft_client_api::remote::{CallAction, CallSnapshot};

use super::CallHost;
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

#[tauri::command]
pub async fn call_action(
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
    CallAction::Unknown => return Err(CommandError::from("unknown call action".to_owned())),
  }
  Ok(())
}
