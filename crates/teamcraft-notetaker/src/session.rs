//! One call the notetaker is in: it joins as an ordinary participant (its own DAVE keys, bound to
//! its device, verified by everybody), takes the decrypted audio of every speaker, cuts it into
//! pieces and has them transcribed while the call runs. It leaves when asked to, when a host
//! removes it, when nobody else is left, or after the longest call time; then it transcribes what
//! is left and hands the transcript back. Audio lives in memory only until its piece is
//! transcribed (e2ee.md §8.2).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::{AudioOrigin, CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{JoinTicketSource, NativeCall, NativeCallConfig};
use teamcraft_media::call_resilience::JoinTicket;
use teamcraft_proto::{CallId, ConversationId, DeviceId, MediaCapabilities, NotetakerPolicy, UserId};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::capture::{AudioPiece, Capture, CaptureSettings, PIECE_SAMPLE_RATE_HZ};
use crate::openrouter::{OpenRouterClient, OpenRouterError, TranscriptionRequest, Usage};
use crate::transcript::{Segment, Transcript};
use crate::wav::encode_wav;

/// How often idle speakers, the people left and the time limits are looked at.
const TICK: Duration = Duration::from_millis(250);

/// What a session needs to run.
pub struct SessionParams {
  pub conversation: ConversationId,
  pub call_id: CallId,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// The policy when the session started; models and language stay fixed for the call.
  pub policy: NotetakerPolicy,
  pub capture: CaptureSettings,
  pub limits: SessionLimits,
  /// What a previous run of this session heard (a restart in the call).
  pub prior: Transcript,
  /// How long the session ran before, so times continue.
  pub prior_elapsed: Duration,
}

/// Time limits of a session.
#[derive(Debug, Clone, Copy)]
pub struct SessionLimits {
  pub max_call: Duration,
  pub alone: Duration,
  pub join: Duration,
}

/// How to reach the call: the prepared join and the keys.
pub struct CallAccess {
  pub ticket: JoinTicket,
  pub signer: CallKeySigner,
  pub devices: Vec<teamcraft_crypto::identity::VerifiedDevice>,
  pub tickets: Arc<dyn JoinTicketSource>,
}

/// A running session, from the worker's side.
pub struct SessionHandle {
  pub conversation: ConversationId,
  pub call_id: CallId,
  stop: watch::Sender<Option<StopReason>>,
}

/// Where sessions report, all of them into one queue of the worker.
pub type SessionEvents = mpsc::UnboundedSender<(ConversationId, SessionEvent)>;

/// What a session reports while it runs.
#[derive(Debug)]
pub enum SessionEvent {
  /// The call's keys arrived: the notetaker hears the call now.
  Secured,
  /// A piece was transcribed (the worker keeps the transcript in its store).
  Segment(Segment),
  /// A piece could not be transcribed.
  PieceFailed,
  /// The session left the call and transcribed what was left: its last event.
  Ended(Box<SessionOutcome>),
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
  /// A member sent the notetaker away.
  Requested,
  RemovedByHost,
  EndedByHost,
  /// Nobody else has been in the call for a while.
  Alone,
  MaxDuration,
  /// The policy turned the notetaker off.
  Disabled,
  /// The notetaker is shutting down.
  Shutdown,
  /// The call's keys never arrived.
  JoinTimeout,
  ConnectionLost(String),
}

/// What a session leaves behind.
#[derive(Debug)]
pub struct SessionOutcome {
  pub conversation: ConversationId,
  pub call_id: CallId,
  pub reason: StopReason,
  pub transcript: Transcript,
  pub usage: Usage,
  pub elapsed: Duration,
  /// Whether the notetaker ever heard the call (its keys arrived).
  pub was_secured: bool,
}

/// A piece back from transcription (its audio already dropped), with its speaker.
type Transcribed = (
  AudioPiece,
  Option<UserId>,
  Result<crate::openrouter::Transcription, OpenRouterError>,
);

/// Signs this device's DAVE key with the device key the core holds.
struct CoreSigner(CallKeySigner);

/// The session's own state while it runs.
struct Running {
  params: SessionParams,
  client: Arc<OpenRouterClient>,
  limiter: Arc<Semaphore>,
  events: SessionEvents,
  capture: Capture,
  /// Who is in the call, by device.
  users_by_device: HashMap<DeviceId, UserId>,
  transcriptions: JoinSet<Transcribed>,
  transcript: Transcript,
  usage: Usage,
  started: Instant,
  maybe_alone_since: Option<Instant>,
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

/// Joins the call and runs the session in a task of its own.
pub fn start(
  params: SessionParams,
  access: CallAccess,
  client: Arc<OpenRouterClient>,
  limiter: Arc<Semaphore>,
  events: SessionEvents,
) -> SessionHandle {
  let (stop, stop_rx) = watch::channel(None);
  let (conversation, call_id) = (params.conversation, params.call_id);
  tokio::spawn(async move {
    let outcome = run(params, access, client, limiter, events.clone(), stop_rx).await;
    // A worker that is gone posts nothing anyway; the transcript is in its store.
    let _ = events.send((conversation, SessionEvent::Ended(Box::new(outcome))));
  });
  SessionHandle {
    conversation,
    call_id,
    stop,
  }
}

async fn run(
  params: SessionParams,
  access: CallAccess,
  client: Arc<OpenRouterClient>,
  limiter: Arc<Semaphore>,
  events: SessionEvents,
  mut stop: watch::Receiver<Option<StopReason>>,
) -> SessionOutcome {
  let mut running = Running::new(params, client, limiter, events);
  let config = match native_config(&running.params, access) {
    Ok(config) => config,
    Err(reason) => return running.finish(StopReason::ConnectionLost(reason)).await,
  };
  let mut call = match NativeCall::join(config).await {
    Ok(call) => call,
    Err(error) => {
      return running
        .finish(StopReason::ConnectionLost(error.to_string()))
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
          // The worker is gone: nobody will post the result, but leave cleanly.
          (Err(_), _) => break StopReason::Shutdown,
        }
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(event) => if let Some(reason) = running.on_event(event) {
          break reason;
        },
        None => break StopReason::ConnectionLost("the call ended".into()),
      },
      Some(done) = running.transcriptions.join_next(), if !running.transcriptions.is_empty() => {
        running.on_transcribed(done);
      },
      _ = tick.tick() => if let Some(reason) = running.on_tick() {
        break reason;
      },
    }
  };
  info!(conversation_id = %running.params.conversation, ?reason, "leaving the call");
  if let Err(error) = call.leave().await {
    debug!(%error, "leaving the call failed");
  }
  running.finish(reason).await
}

impl Running {
  fn new(
    params: SessionParams,
    client: Arc<OpenRouterClient>,
    limiter: Arc<Semaphore>,
    events: SessionEvents,
  ) -> Self {
    let started = Instant::now()
      .checked_sub(params.prior_elapsed)
      .unwrap_or_else(Instant::now);
    Self {
      capture: Capture::new(params.capture),
      transcript: params.prior.clone(),
      params,
      client,
      limiter,
      events,
      users_by_device: HashMap::new(),
      transcriptions: JoinSet::new(),
      usage: Usage::default(),
      started,
      maybe_alone_since: None,
      was_secured: false,
    }
  }

  /// Handles one event of the call; a reason to leave, if it gives one.
  fn on_event(&mut self, event: CallEngineEvent) -> Option<StopReason> {
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
        if let Some(piece) = self.capture.remove(device) {
          self.transcribe(piece);
        }
        self.users_by_device.remove(&device);
      },
      CallEngineEvent::Secured { .. } if !self.was_secured => {
        self.was_secured = true;
        // The worker may be gone at shutdown; the session ends with the next tick then.
        self.emit(SessionEvent::Secured);
      },
      CallEngineEvent::AudioFrame {
        sender,
        origin: AudioOrigin::Microphone,
        sequence_number,
        opus,
      } => {
        let at_ms = self.elapsed_ms();
        if let Some(piece) = self.capture.push(sender, sequence_number, &opus, at_ms) {
          self.transcribe(piece);
        }
      },
      CallEngineEvent::RemovedByHost { .. } => return Some(StopReason::RemovedByHost),
      CallEngineEvent::EndedByHost { .. } => return Some(StopReason::EndedByHost),
      CallEngineEvent::ConnectionReplaced => {
        return Some(StopReason::ConnectionLost("joined on another connection".into()));
      },
      // Screen sound, video and the rest are not the notetaker's business.
      _ => {},
    }
    None
  }

  /// Ends idle speakers' pieces and checks the time limits.
  fn on_tick(&mut self) -> Option<StopReason> {
    for piece in self.capture.on_idle(self.elapsed_ms()) {
      self.transcribe(piece);
    }
    let now = Instant::now();
    let elapsed = now.saturating_duration_since(self.started);
    if !self.was_secured && elapsed >= self.params.prior_elapsed + self.params.limits.join {
      return Some(StopReason::JoinTimeout);
    }
    if elapsed >= self.params.limits.max_call {
      return Some(StopReason::MaxDuration);
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

  /// Sends a piece for transcription in the background. The audio is dropped once the request is
  /// done, whatever its outcome.
  fn transcribe(&mut self, piece: AudioPiece) {
    let maybe_user = self.users_by_device.get(&piece.device).copied();
    let client = Arc::clone(&self.client);
    let limiter = Arc::clone(&self.limiter);
    let model = self.params.policy.transcription_model.clone();
    let maybe_language = self.params.policy.maybe_language_code().map(str::to_owned);
    let is_zero_data_retention = self.params.policy.is_zero_data_retention;
    self.transcriptions.spawn(async move {
      // The semaphore is never closed, so a permit always comes.
      let _permit = limiter.acquire_owned().await;
      let wav = encode_wav(&piece.samples, PIECE_SAMPLE_RATE_HZ);
      let result = client
        .transcribe(TranscriptionRequest {
          model: &model,
          wav: &wav,
          maybe_language: maybe_language.as_deref(),
          is_zero_data_retention,
        })
        .await;
      let piece = AudioPiece {
        samples: Vec::new(),
        ..piece
      };
      (piece, maybe_user, result)
    });
  }

  fn on_transcribed(&mut self, done: Result<Transcribed, tokio::task::JoinError>) {
    let (piece, maybe_user, result) = match done {
      Ok(done) => done,
      Err(error) => {
        warn!(%error, "a transcription task failed");
        self.transcript.failed_pieces += 1;
        self.emit(SessionEvent::PieceFailed);
        return;
      },
    };
    match result {
      Ok(transcription) => {
        self.usage.add(&transcription.usage);
        let Some(user) = maybe_user else {
          debug!("a piece of a device that wasn't in the call's roster");
          return;
        };
        let segment = Segment {
          user,
          start_ms: piece.start_ms,
          end_ms: piece.end_ms,
          text: transcription.text,
        };
        self.transcript.add(segment.clone());
        self.emit(SessionEvent::Segment(segment));
      },
      Err(error) => {
        warn!(%error, conversation_id = %self.params.conversation, "a piece could not be transcribed");
        self.transcript.failed_pieces += 1;
        self.emit(SessionEvent::PieceFailed);
      },
    }
  }

  /// Transcribes what is left and hands everything back.
  async fn finish(mut self, reason: StopReason) -> SessionOutcome {
    for piece in self.capture.flush_all() {
      self.transcribe(piece);
    }
    while let Some(done) = self.transcriptions.join_next().await {
      self.on_transcribed(done);
    }
    let elapsed = Instant::now().saturating_duration_since(self.started);
    info!(
      conversation_id = %self.params.conversation,
      requests = self.usage.requests,
      audio_seconds = self.usage.audio_seconds,
      cost_usd = self.usage.cost_usd,
      "transcription of the call done"
    );
    SessionOutcome {
      conversation: self.params.conversation,
      call_id: self.params.call_id,
      reason,
      transcript: self.transcript,
      usage: self.usage,
      elapsed,
      was_secured: self.was_secured,
    }
  }

  fn emit(&self, event: SessionEvent) {
    let _ = self.events.send((self.params.conversation, event));
  }

  fn elapsed_ms(&self) -> u64 {
    u64::try_from(Instant::now().saturating_duration_since(self.started).as_millis()).unwrap_or(u64::MAX)
  }
}

/// The native call: muted, audio frames to the session, only the conversation's verified devices
/// trusted (e2ee.md §8).
fn native_config(params: &SessionParams, access: CallAccess) -> Result<NativeCallConfig, String> {
  let engine = CallEngineConfig {
    user_id: params.user_id,
    device_id: params.device_id,
    // Audio only: the notetaker neither sends nor watches video.
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let authenticator =
    DeviceCallKeyAuthenticator::from_verified(Arc::new(CoreSigner(access.signer)), access.devices)
      .map_err(|error| error.to_string())?;
  let mut config =
    NativeCallConfig::device_bound(access.ticket.sfu_url, access.ticket.token, engine, authenticator);
  config.certificate_hashes = access.ticket.certificate_hashes;
  config.source = AudioSource::Silence;
  config.sink = AudioSink::Frames;
  config.is_muted_at_start = true;
  config.maybe_ticket_source = Some(access.tickets);
  Ok(config)
}

impl CallBindingSigner for CoreSigner {
  fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    self.0.sign_call_key_binding(call_id, dave_public_key, not_after)
  }
}
