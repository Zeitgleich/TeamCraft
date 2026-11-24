//! One call the recorder bot is in (e2ee.md §8.3, #2273): it joins as an ordinary participant (its
//! own DAVE keys, bound to its device, verified by everybody), muted, and records what it hears
//! with the call recording pipeline every app uses (`teamcraft-media::call_recording`): the mix of
//! everybody, sealed on disk as it is written. Starting the take sends the signed recording notice,
//! so everybody in the call sees "Recording" with the bot's name, and the server's audit log has it.
//!
//! - The take starts once the call's keys arrived (before that the bot hears nothing), and ends
//!   when the session does. At the policy's longest length it is finished and handed over, and the
//!   next part starts at once, announced again.
//! - The session leaves when the worker says so (the policy changed, the bot left the conversation,
//!   shutdown), when a host removes it or ends the call, when nobody else has been in the call for a
//!   while, when the call's keys never arrive, when writing fails, and when the SFU doesn't confirm
//!   its notice for a while: the bot never records without the others being told.
//! - A lost connection or an SFU restart is the call driver's business: it rejoins and announces
//!   the recording again; the take goes on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use teamcraft_bot_runtime::call::CallAccess;
use teamcraft_media::audio_io::AudioSink;
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::call_recording::{CallRecorder, CallRecordingError, CallRecordingTake};
use teamcraft_proto::{CallId, ConversationId, DeviceId, UserId};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

/// How often the people left, the take and the time limits are looked at.
const TICK: Duration = Duration::from_millis(250);

/// What a session needs to run.
pub struct SessionParams {
  pub conversation: ConversationId,
  pub call_id: CallId,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// The policy's longest recording: one part of the call.
  pub max_take: Duration,
  /// Where takes are written, sealed.
  pub folder: PathBuf,
  pub limits: SessionLimits,
}

/// Time limits of a session.
#[derive(Debug, Clone, Copy)]
pub struct SessionLimits {
  /// Nobody else has been in the call for this long: leave.
  pub alone: Duration,
  /// The call's keys haven't arrived after this long: leave.
  pub join: Duration,
  /// The SFU hasn't confirmed the recording notice for this long: stop, the others may not know.
  pub unconfirmed: Duration,
}

/// A running session, from the worker's side.
pub struct SessionHandle {
  pub call_id: CallId,
  stop: watch::Sender<Option<StopReason>>,
}

/// Where sessions report, all of them into one queue of the worker.
pub type SessionEvents = mpsc::UnboundedSender<(ConversationId, SessionEvent)>;

/// What a session reports while it runs.
#[derive(Debug)]
pub enum SessionEvent {
  /// A take is finished: the worker posts it.
  Take(FinishedTake),
  /// The session left the call (its takes are handed over before): its last event.
  Ended(SessionOutcome),
}

/// A finished take, sealed on disk; only this value holds its key.
#[derive(Debug)]
pub struct FinishedTake {
  pub take: CallRecordingTake,
  /// Its number within the session, from 1.
  pub part: u32,
  /// The take reached the policy's longest length and the next one started.
  pub is_at_limit: bool,
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
  /// The policy doesn't let the bot record any more.
  Disabled,
  /// The bot is no longer in the conversation.
  NotMember,
  RemovedByHost,
  EndedByHost,
  /// Nobody else has been in the call for a while.
  Alone,
  /// The bot is shutting down.
  Shutdown,
  /// The call's keys never arrived.
  JoinTimeout,
  /// The SFU didn't confirm the recording notice: the others may not see it.
  NoticeUnconfirmed,
  /// Writing the take failed (a full disk).
  WriteFailed(String),
  ConnectionLost(String),
}

/// How a session ended.
#[derive(Debug)]
pub struct SessionOutcome {
  pub call_id: CallId,
  pub reason: StopReason,
  /// Whether the bot ever heard the call (its keys arrived).
  pub was_secured: bool,
}

/// The session's own state while it runs.
struct Running {
  params: SessionParams,
  events: SessionEvents,
  /// Who is in the call, by device.
  users_by_device: HashMap<DeviceId, UserId>,
  maybe_recorder: Option<CallRecorder>,
  part: u32,
  started: Instant,
  maybe_alone_since: Option<Instant>,
  maybe_unconfirmed_since: Option<Instant>,
  was_secured: bool,
}

impl SessionHandle {
  /// Asks the session to leave; the first reason given counts.
  pub fn stop(&self, reason: StopReason) {
    self.stop.send_if_modified(|current| {
      if current.is_some() {
        return false;
      }
      *current = Some(reason);
      true
    });
  }

  pub fn is_stopping(&self) -> bool {
    self.stop.borrow().is_some()
  }
}

/// Joins the call (`config`, from [`native_config`]) and runs the session in a task of its own.
pub fn start(params: SessionParams, config: NativeCallConfig, events: SessionEvents) -> SessionHandle {
  let (stop, stop_rx) = watch::channel(None);
  let (conversation, call_id) = (params.conversation, params.call_id);
  tokio::spawn(async move {
    let outcome = run(params, config, events.clone(), stop_rx).await;
    // A worker that is gone posts nothing anyway.
    let _ = events.send((conversation, SessionEvent::Ended(outcome)));
  });
  SessionHandle { call_id, stop }
}

/// The native call: muted, everybody's audio mixed for the recording, only the conversation's
/// verified devices trusted (e2ee.md §8).
pub fn native_config(params: &SessionParams, access: CallAccess) -> Result<NativeCallConfig, String> {
  // The mix is made and then dropped (no speakers); the recording gets it on the way.
  teamcraft_bot_runtime::call::bot_call_config(params.user_id, params.device_id, access, AudioSink::Discard)
}

async fn run(
  params: SessionParams,
  config: NativeCallConfig,
  events: SessionEvents,
  mut stop: watch::Receiver<Option<StopReason>>,
) -> SessionOutcome {
  let mut running = Running::new(params, events);
  let mut call = match NativeCall::join(config).await {
    Ok(call) => call,
    Err(error) => {
      return running
        .finish(None, StopReason::ConnectionLost(error.to_string()))
        .await;
    },
  };
  info!(conversation_id = %running.params.conversation, call_id = %running.params.call_id, "joined the call");
  let mut tick = tokio::time::interval(TICK);
  tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
  let reason = loop {
    tokio::select! {
      changed = stop.changed() => {
        let maybe_reason = stop.borrow().clone();
        match (changed, maybe_reason) {
          (Ok(()), Some(reason)) => break reason,
          (Ok(()), None) => {},
          // The worker is gone: nobody will post the take, but leave cleanly.
          (Err(_), _) => break StopReason::Shutdown,
        }
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(event) => if let Some(reason) = running.on_event(event, &call) {
          break reason;
        },
        None => break StopReason::ConnectionLost("the call ended".into()),
      },
      _ = tick.tick() => if let Some(reason) = running.on_tick(&call).await {
        break reason;
      },
    }
  };
  info!(conversation_id = %running.params.conversation, ?reason, "leaving the call");
  running.finish(Some(call), reason).await
}

impl Running {
  fn new(params: SessionParams, events: SessionEvents) -> Self {
    Self {
      params,
      events,
      users_by_device: HashMap::new(),
      maybe_recorder: None,
      part: 0,
      started: Instant::now(),
      maybe_alone_since: None,
      maybe_unconfirmed_since: None,
      was_secured: false,
    }
  }

  /// Handles one event of the call; a reason to leave, if it gives one.
  fn on_event(&mut self, event: CallEngineEvent, call: &NativeCall) -> Option<StopReason> {
    match event {
      CallEngineEvent::Joined { participants, .. } => {
        for participant in participants {
          self
            .users_by_device
            .insert(participant.device_id, participant.user_id);
        }
      },
      CallEngineEvent::ParticipantJoined(participant) => {
        self
          .users_by_device
          .insert(participant.device_id, participant.user_id);
      },
      CallEngineEvent::ParticipantLeft(device) => {
        self.users_by_device.remove(&device);
      },
      // The keys arrived: the bot hears the call now, and records from here on.
      CallEngineEvent::Secured { .. } if !self.was_secured => {
        self.was_secured = true;
        if let Err(reason) = self.start_take(call) {
          return Some(reason);
        }
      },
      CallEngineEvent::OwnRecordingUnconfirmed(is_unconfirmed) => {
        self.maybe_unconfirmed_since = match (is_unconfirmed, self.maybe_unconfirmed_since) {
          (false, _) => None,
          (true, Some(since)) => Some(since),
          (true, None) => Some(Instant::now()),
        };
      },
      CallEngineEvent::RemovedByHost { .. } => return Some(StopReason::RemovedByHost),
      CallEngineEvent::EndedByHost { .. } => return Some(StopReason::EndedByHost),
      CallEngineEvent::ConnectionReplaced => {
        return Some(StopReason::ConnectionLost("joined on another connection".into()));
      },
      // Video, reactions and the rest are not the recorder's business.
      _ => {},
    }
    None
  }

  /// Checks the take and the time limits.
  async fn on_tick(&mut self, call: &NativeCall) -> Option<StopReason> {
    if let Some(reason) = self.check_take(call).await {
      return Some(reason);
    }
    let now = Instant::now();
    let elapsed = now.saturating_duration_since(self.started);
    if !self.was_secured && elapsed >= self.params.limits.join {
      return Some(StopReason::JoinTimeout);
    }
    if self
      .maybe_unconfirmed_since
      .is_some_and(|since| now.saturating_duration_since(since) >= self.params.limits.unconfirmed)
    {
      return Some(StopReason::NoticeUnconfirmed);
    }
    let is_alone = !self
      .users_by_device
      .values()
      .any(|user| *user != self.params.user_id);
    match (is_alone, self.maybe_alone_since) {
      (false, _) => self.maybe_alone_since = None,
      (true, None) => self.maybe_alone_since = Some(now),
      (true, Some(since)) if now.saturating_duration_since(since) >= self.params.limits.alone => {
        return Some(StopReason::Alone);
      },
      (true, Some(_)) => {},
    }
    None
  }

  /// A take that ended by itself: at its longest length it is handed over and the next part
  /// starts; after a failed write the session ends.
  async fn check_take(&mut self, call: &NativeCall) -> Option<StopReason> {
    if !self.maybe_recorder.as_ref().is_some_and(CallRecorder::is_ended) {
      return None;
    }
    let recorder = self.maybe_recorder.take()?;
    let is_at_limit = recorder.is_at_limit();
    match finish_take(recorder).await {
      Ok(take) => self.hand_over(take, is_at_limit),
      Err(CallRecordingError::Empty) => {},
      Err(error) => return Some(StopReason::WriteFailed(error.to_string())),
    }
    if !is_at_limit {
      return Some(StopReason::WriteFailed("the recording stopped".into()));
    }
    self.start_take(call).err()
  }

  /// Starts a take and tells everybody: the call sends the signed notice.
  fn start_take(&mut self, call: &NativeCall) -> Result<(), StopReason> {
    self.part += 1;
    let path = take_path(&self.params.folder, self.params.call_id, self.part);
    // No kept key: a crash loses the take, and nothing on the disk can be read without the key.
    let recorder = CallRecorder::start(&path, self.params.max_take, None)
      .map_err(|error| StopReason::WriteFailed(error.to_string()))?;
    call.set_recording(Some(recorder.sink()));
    info!(conversation_id = %self.params.conversation, part = self.part, "recording the call");
    self.maybe_recorder = Some(recorder);
    Ok(())
  }

  fn hand_over(&self, take: CallRecordingTake, is_at_limit: bool) {
    let finished = FinishedTake {
      take,
      part: self.part,
      is_at_limit,
    };
    if let Err(error) = self
      .events
      .send((self.params.conversation, SessionEvent::Take(finished)))
    {
      // The worker is gone (shutdown): the take can't be posted; its file goes with it.
      let SessionEvent::Take(finished) = error.0.1 else {
        return;
      };
      warn!(conversation_id = %self.params.conversation, "a take could not be handed over");
      teamcraft_media::call_recording::delete_recording(&finished.take.path);
    }
  }

  /// Stops the recording (everybody is told), leaves, and hands over the last take.
  async fn finish(mut self, maybe_call: Option<NativeCall>, reason: StopReason) -> SessionOutcome {
    if let Some(call) = maybe_call {
      call.set_recording(None);
      if let Err(error) = call.leave().await {
        debug!(%error, "leaving the call failed");
      }
    }
    if let Some(recorder) = self.maybe_recorder.take() {
      match finish_take(recorder).await {
        Ok(take) => self.hand_over(take, false),
        Err(CallRecordingError::Empty) => {},
        Err(error) => {
          warn!(%error, conversation_id = %self.params.conversation, "the last take could not be finished")
        },
      }
    }
    SessionOutcome {
      call_id: self.params.call_id,
      reason,
      was_secured: self.was_secured,
    }
  }
}

/// Finishes the file off the async runtime (it waits for the recording thread).
async fn finish_take(recorder: CallRecorder) -> Result<CallRecordingTake, CallRecordingError> {
  tokio::task::spawn_blocking(move || recorder.finish())
    .await
    .unwrap_or(Err(CallRecordingError::Thread))
}

/// A take's file: the call and the part, so takes of one call never collide.
fn take_path(folder: &Path, call_id: CallId, part: u32) -> PathBuf {
  folder.join(format!("take-{call_id}-{part}.ogg"))
}

#[cfg(test)]
mod tests {
  use teamcraft_bot_runtime::test_call::{authenticators, call_config, id, start_sfu};
  use teamcraft_media::audio_io::AudioSource;

  use super::*;

  const CALL: u64 = 41;
  const SPEAKER: u64 = 1;
  const RECORDER: u64 = 2;
  const SPEECH: Duration = Duration::from_secs(3);
  const DEADLINE: Duration = Duration::from_secs(30);

  /// A speaker's tone goes through a real SFU and DAVE to the recorder bot, which records it: the
  /// speaker's app is told by the bot's signed notice (verified against its DAVE key) that the bot
  /// records, then that it stopped; the take decrypts to an Ogg file of about the speech's length.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_speaker_is_recorded_and_told_so_through_the_sfu() {
    let folder = scratch_folder("e2e");
    let (port, certificate_hash) = start_sfu().await;
    let mut authenticators = authenticators(&[SPEAKER, RECORDER]);
    let mut speaker = NativeCall::join(call_config(
      port,
      certificate_hash,
      CALL,
      SPEAKER,
      &[RECORDER],
      authenticators.remove(&SPEAKER).unwrap(),
    ))
    .await
    .unwrap();
    let mut config = call_config(
      port,
      certificate_hash,
      CALL,
      RECORDER,
      &[SPEAKER],
      authenticators.remove(&RECORDER).unwrap(),
    );
    // What the worker's config gives a bot: silent and muted, the mix for the recording only.
    config.source = AudioSource::Silence;
    config.sink = AudioSink::Discard;
    config.is_muted_at_start = true;
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let handle = start(params(&folder), config, events_tx);

    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut was_told = false;
    // Whether the speaker's app shows the bot recording right now.
    let mut is_shown = false;
    let mut maybe_take = None;
    let outcome = loop {
      tokio::select! {
        () = tokio::time::sleep_until(deadline) => panic!("the session took too long"),
        Some(event) = speaker.next_event() => {
          if let CallEngineEvent::RecordersChanged(recorders) = event {
            is_shown = shows_recorder(&recorders);
            let is_verified = recorders
              .iter()
              .any(|recorder| recorder.device_id == id::<DeviceId>(RECORDER) && recorder.is_verified);
            if is_shown && is_verified && !was_told {
              was_told = true;
              let stop = handle.stop.clone();
              tokio::spawn(async move {
                tokio::time::sleep(SPEECH).await;
                stop.send_replace(Some(StopReason::Shutdown));
              });
            }
          }
        },
        Some((conversation, event)) = events.recv() => {
          assert_eq!(conversation, id::<ConversationId>(7));
          match event {
            SessionEvent::Take(finished) => maybe_take = Some(finished),
            SessionEvent::Ended(outcome) => break outcome,
          }
        },
      }
    };
    assert!(was_told, "the speaker never saw the verified recording notice");
    assert!(outcome.was_secured);
    assert_eq!(outcome.reason, StopReason::Shutdown);
    let finished = maybe_take.expect("no take was handed over");
    assert_eq!(finished.part, 1);
    assert!(!finished.is_at_limit);
    assert!(
      finished.take.duration_ms >= 2_000,
      "{} ms",
      finished.take.duration_ms
    );
    // Sealed on disk: the file isn't the Ogg stream; decrypted, it is.
    let sealed = std::fs::read(&finished.take.path).unwrap();
    assert_ne!(&sealed[..4], b"OggS");
    let mut ogg = Vec::new();
    let size = finished.take.decrypt_to(&mut ogg).unwrap();
    assert_eq!(size, finished.take.size_bytes);
    assert_eq!(&ogg[..4], b"OggS");
    // The speaker hears the stop: the bot's indicator goes away (maybe already).
    let stopped = tokio::time::timeout(Duration::from_secs(10), async {
      while is_shown {
        match speaker.next_event().await {
          Some(CallEngineEvent::RecordersChanged(recorders)) => is_shown = shows_recorder(&recorders),
          Some(_) => {},
          None => break,
        }
      }
      !is_shown
    })
    .await;
    assert_eq!(stopped, Ok(true), "the speaker still sees the recording");
    speaker.leave().await.unwrap();
    teamcraft_media::call_recording::delete_recording(&finished.take.path);
    std::fs::remove_dir_all(&folder).unwrap();
  }

  #[test]
  fn takes_of_one_call_never_share_a_file() {
    let folder = Path::new("/recordings");
    let call: CallId = id(5);
    assert_ne!(take_path(folder, call, 1), take_path(folder, call, 2));
    assert!(take_path(folder, call, 1).starts_with(folder));
  }

  /// Whether a speaker's app shows the recorder bot as recording (not "left, may still listen").
  fn shows_recorder(recorders: &[teamcraft_media::call_recording_notice::Recorder]) -> bool {
    recorders
      .iter()
      .any(|recorder| recorder.device_id == id::<DeviceId>(RECORDER) && !recorder.has_left)
  }

  fn params(folder: &Path) -> SessionParams {
    SessionParams {
      conversation: id(7),
      call_id: id(CALL),
      user_id: id(RECORDER),
      device_id: id(RECORDER),
      max_take: Duration::from_secs(600),
      folder: folder.to_path_buf(),
      limits: SessionLimits {
        alone: Duration::from_secs(60),
        join: Duration::from_secs(20),
        unconfirmed: Duration::from_secs(30),
      },
    }
  }

  fn scratch_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("teamcraft-recorder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    folder
  }
}
