//! Audit entries for call recordings (issue #2117, e2ee.md §8.3, admin.md §3.13).
//!
//! **Who reports.** The SFU, not the recording app. Every compliant recording announces itself with
//! a signed notice through the SFU (`set_recording`), which the SFU reads (it is signed, not
//! encrypted) and reports when it turns a device's recording on or off. The SFU counts a notice
//! only when it names the call and the device that sent it, so the audit log says exactly what the
//! people in the call were shown, about the device that said it. A report from the recording app
//! itself could be left out or made up by a modified app, and would be a second statement next to
//! the notice that could disagree with it; a recording without any notice is the covert capture
//! nothing can prevent (e2ee.md §8.3, "Honest limit first").
//!
//! **One entry per recording.** The running recordings are kept in the database
//! (`open_call_recordings`), so a recording the recorder announces again (after a rejoin, a new
//! DAVE session, or a restarted SFU) keeps its start and writes no second "started"; its stop knows
//! how long it ran. A recording whose stop never comes ends when its device leaves the call: the
//! SFU's leave, the SFU's roster once it no longer has the device, or the SFU going silent
//! (`ended_by: "left"`).
//!
//! Metadata only: the call, the recording device, how many devices were in the call and how long it
//! ran. Never a file name, never anything of the recording.

use serde::Serialize;
use teamcraft_db::{CallRecordingStart, EndedCallRecording, RecordingAudit, RunningCallRecording};
use teamcraft_proto::{CallId, DeviceId, SfuRecordingReport, SfuRecordingState, SfuRosterReport};
use tracing::{info, warn};

use crate::error::ApiError;
use crate::state::AppState;

/// Audit action of a recording that started.
pub const RECORDING_STARTED: &str = "call_recording_started";
/// Audit action of a recording that ended.
pub const RECORDING_STOPPED: &str = "call_recording_stopped";

/// How a recording ended, in its audit entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingEnd {
  /// The recorder's notice said it stopped.
  Stopped,
  /// The recording device left the call without a stop.
  Left,
}

/// The detail of a "started" entry.
#[derive(Serialize)]
struct StartedDetail {
  call_id: CallId,
  device_id: DeviceId,
  participants: u32,
}

/// The detail of a "stopped" entry.
#[derive(Serialize)]
struct StoppedDetail {
  call_id: CallId,
  device_id: DeviceId,
  participants: u32,
  duration_secs: u64,
  ended_by: RecordingEnd,
}

/// The SFU reports that a device's recording started or stopped.
pub async fn report(state: &AppState, report: SfuRecordingReport) -> Result<(), ApiError> {
  let running = RunningCallRecording {
    call_id: report.call_id,
    device_id: report.device_id,
  };
  match report.state {
    SfuRecordingState::Recording => start(state, &report).await,
    SfuRecordingState::Stopped => end(state, running, report.participants, RecordingEnd::Stopped).await,
    // A newer SFU's state: nothing this server can write down.
    SfuRecordingState::Unknown(_) => Ok(()),
  }
}

/// A device left the call: a recording it didn't stop ends with it.
pub async fn device_left(state: &AppState, call_id: CallId, device_id: DeviceId) {
  let running = RunningCallRecording { call_id, device_id };
  if let Err(error) = end(state, running, 0, RecordingEnd::Left).await {
    warn!(%call_id, %device_id, %error, "ending the recording of a device that left failed");
  }
}

/// The SFU's roster: once it is settled (`is_settled`: the SFU has run long enough for its devices
/// to have rejoined after a restart), recordings of devices it doesn't have end as left. That
/// catches the stops and leaves a restart of either side lost.
pub async fn reconcile(state: &AppState, report: &SfuRosterReport, is_settled: bool) {
  if !is_settled {
    return;
  }
  let running = match state.database.running_call_recordings().await {
    Ok(running) => running,
    Err(error) => {
      warn!(%error, "reading the running call recordings failed");
      return;
    },
  };
  for recording in running {
    let is_on_sfu = report
      .calls
      .iter()
      .filter(|call| call.call_id == recording.call_id)
      .flat_map(|call| &call.participants)
      .any(|entry| entry.device_id == recording.device_id);
    if !is_on_sfu {
      device_left(state, recording.call_id, recording.device_id).await;
    }
  }
}

/// The SFU is gone (no roster in time) and its calls ended: so did their recordings.
pub async fn end_all(state: &AppState) {
  match state.database.running_call_recordings().await {
    Ok(running) => {
      for recording in running {
        device_left(state, recording.call_id, recording.device_id).await;
      }
    },
    Err(error) => warn!(%error, "reading the running call recordings failed"),
  }
}

async fn start(state: &AppState, report: &SfuRecordingReport) -> Result<(), ApiError> {
  let calls = state
    .maybe_calls
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("calls are not configured".into()))?;
  let conversation_id = calls
    .conversation_of(report.call_id)
    .ok_or_else(ApiError::call_not_found)?;
  let detail = serde_json::to_string(&StartedDetail {
    call_id: report.call_id,
    device_id: report.device_id,
    participants: report.participants,
  })
  .map_err(|error| ApiError::Internal(error.to_string()))?;
  let start = CallRecordingStart {
    call_id: report.call_id,
    device_id: report.device_id,
    user_id: report.user_id,
    conversation_id,
    participants: report.participants,
  };
  let is_new = state
    .database
    .start_call_recording(&start, audit(state, RECORDING_STARTED)?, &detail)
    .await?;
  if is_new {
    info!(call_id = %report.call_id, device_id = %report.device_id, "call recording started");
  }
  Ok(())
}

async fn end(
  state: &AppState,
  running: RunningCallRecording,
  participants: u32,
  ended_by: RecordingEnd,
) -> Result<(), ApiError> {
  let detail = |ended: &EndedCallRecording| {
    serde_json::to_string(&StoppedDetail {
      call_id: ended.call_id,
      device_id: ended.device_id,
      participants: ended.peak_participants,
      duration_secs: ended.duration_secs,
      ended_by,
    })
    // Invariant: the detail is plain numbers and strings, which always serialize.
    .unwrap_or_default()
  };
  let maybe_ended = state
    .database
    .end_call_recording(running, participants, audit(state, RECORDING_STOPPED)?, detail)
    .await?;
  if let Some(ended) = maybe_ended {
    info!(
      call_id = %ended.call_id,
      device_id = %ended.device_id,
      duration_secs = ended.duration_secs,
      ?ended_by,
      "call recording stopped"
    );
  }
  Ok(())
}

fn audit<'a>(state: &AppState, action: &'a str) -> Result<RecordingAudit<'a>, ApiError> {
  let id = state
    .ids
    .next_snowflake()
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  Ok(RecordingAudit { id, action })
}
