//! The calls as the UI sees them: a mirror of a [`CallSnapshot`] (ringing, running calls, the
//! active call, devices) and the actions the user takes. On desktop the media runs in the shell,
//! which emits `call-changed` when the mirror should re-read; in the browser it runs in the page
//! (`web_calls`), which hands over each new snapshot.
//!
//! Actions run one after the other, in the order the user took them: a camera turned on right
//! after joining reaches the media layer after the join. A failed action reaches the user
//! (issue #322), and so does a part of one that failed without undoing it (a join whose camera
//! didn't start, #453). Starting a screen share is the exception once its turn comes: it waits
//! for the user's choice in the system's picker for as long as they take, and mute, leaving or
//! stopping the share must not wait with it (issue #683), so it runs beside the queue. In the
//! browser the picker opens in the click itself, before the action is queued: Safari refuses it
//! once the click's user activation is gone (issue #930); the chosen screen is shared when the
//! action's turn comes.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use js_sys::Promise;
use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::calls::{
  ActiveCall, CallPreview, CallRequest, CameraFallback, EffectsOffer, HuddleInfo, IncomingCall, JoinMedia,
  MediaDevices, Participant, PreviewCamera, RemoteVideoTrack,
};
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_client_api::remote::{CALL_CHANGED_EVENT, CallAction, CallActionAnswer, CallSnapshot};
use teamcraft_client_api::video_stats::VideoStatsJson;
use teamcraft_proto::ScreenContentMode;

use crate::auth::tauri::{call, invoke, listen};

type ChangeNotifier = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct RemoteCalls {
  snapshot: Rc<RefCell<CallSnapshot>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  link: CallLink,
  /// Actions waiting for the one in flight.
  queue: Rc<RefCell<VecDeque<QueuedAction>>>,
  is_draining: Rc<Cell<bool>>,
  /// Screen share starts running beside the queue (their picker is open); pending all the same.
  screen_shares_starting: Rc<Cell<u32>>,
  join_failures: Rc<Cell<u64>>,
  /// The pre-join check's microphone level from the desktop shell (#442).
  #[cfg(target_arch = "wasm32")]
  shell_level: crate::shell_level::ShellLevel,
}

/// An action waiting for its turn.
struct QueuedAction {
  action: CallAction,
  /// The screen picker the page opened in the click of a `StartScreenShare` (issue #930): the
  /// promise of the chosen screen. Only in the browser, and only when the call module could open
  /// one; otherwise the screen is asked for when the action's turn comes.
  maybe_picked_screen: Option<Promise>,
}

/// Where the media runs.
#[derive(Clone)]
enum CallLink {
  /// The desktop shell, through its commands.
  Shell,
  /// This page (browser).
  #[cfg(target_arch = "wasm32")]
  Page(Rc<crate::web_calls::WebCallHost>),
}

impl RemoteCalls {
  /// Reads the first snapshot and follows the shell's `call-changed` events.
  pub async fn connect() -> Result<Self, String> {
    let snapshot: CallSnapshot = call("call_snapshot", &json!({})).await?;
    let calls = Self::new(snapshot, CallLink::Shell);
    // A check an earlier page left running (reloaded while the pre-join screen was open) ends:
    // nothing on screen shows the camera or the meter now.
    calls.act(CallAction::SetPreview {
      preview: CallPreview::default(),
    });
    let listener = calls.clone();
    listen(CALL_CHANGED_EVENT, move || listener.refresh());
    Ok(calls)
  }

  /// Calls with the media in this page.
  #[cfg(target_arch = "wasm32")]
  pub fn in_page(host: Rc<crate::web_calls::WebCallHost>) -> Self {
    let calls = Self::new(CallSnapshot::default(), CallLink::Page(host.clone()));
    // Not the calls themselves: the host must not keep what keeps it alive.
    let (snapshot, notify) = (calls.snapshot.clone(), calls.notify.clone());
    host.set_on_change(Rc::new(move |next| {
      *snapshot.borrow_mut() = next;
      let maybe_notify = notify.borrow().clone();
      if let Some(notify) = maybe_notify {
        notify();
      }
    }));
    calls
  }

  fn new(snapshot: CallSnapshot, link: CallLink) -> Self {
    Self {
      snapshot: Rc::new(RefCell::new(snapshot)),
      notify: Rc::new(RefCell::new(None)),
      link,
      queue: Rc::default(),
      screen_shares_starting: Rc::default(),
      is_draining: Rc::default(),
      join_failures: Rc::default(),
      #[cfg(target_arch = "wasm32")]
      shell_level: crate::shell_level::ShellLevel::default(),
    }
  }

  pub fn set_change_notifier(&self, notify: ChangeNotifier) {
    *self.notify.borrow_mut() = Some(notify);
  }

  /// Whether the media runs in this page (the browser).
  pub fn is_in_page(&self) -> bool {
    !matches!(self.link, CallLink::Shell)
  }

  /// Which background effects the camera gets: every one through the shell; in the browser those
  /// the page can run, where it can run them at all (#439).
  pub fn effects_offer(&self) -> EffectsOffer {
    match &self.link {
      CallLink::Shell => EffectsOffer::Full,
      #[cfg(target_arch = "wasm32")]
      CallLink::Page(_) => browser_effects_offer(crate::web_calls::applies_effects()),
    }
  }

  pub fn join_failures(&self) -> u64 {
    self.join_failures.get()
  }

  pub fn has_pending_actions(&self) -> bool {
    self.is_draining.get() || !self.queue.borrow().is_empty() || self.screen_shares_starting.get() > 0
  }

  pub fn video_tracks(&self) -> Vec<RemoteVideoTrack> {
    self.snapshot.borrow().video_tracks.clone()
  }

  pub fn devices(&self) -> MediaDevices {
    self.snapshot.borrow().devices.clone()
  }

  pub fn active(&self) -> Option<ActiveCall> {
    self.snapshot.borrow().maybe_active.clone()
  }

  pub fn incoming(&self) -> Vec<IncomingCall> {
    self.snapshot.borrow().incoming.clone()
  }

  /// People in the call of a conversation: the media layer's view while this device is in it, else
  /// the server roster (without media state).
  pub fn participants(&self, conversation: ConversationId) -> Vec<Participant> {
    let snapshot = self.snapshot.borrow();
    if let Some(active) = snapshot
      .maybe_active
      .as_ref()
      .filter(|active| active.conversation == conversation)
    {
      return active.participants.clone();
    }
    roster_of(&snapshot, conversation)
      .into_iter()
      .map(|user| Participant {
        user,
        is_muted: false,
        is_camera_on: false,
        has_hand_raised: false,
        is_sharing_screen: false,
      })
      .collect()
  }

  /// The drop-in call running in a conversation, if anyone is in it.
  pub fn huddle(&self, conversation: ConversationId) -> Option<HuddleInfo> {
    let participants = roster_of(&self.snapshot.borrow(), conversation);
    (!participants.is_empty()).then_some(HuddleInfo { participants })
  }

  /// Whether the user is in a call (on any device), per the server roster.
  pub fn is_in_call(&self, user: UserId) -> bool {
    let snapshot = self.snapshot.borrow();
    snapshot
      .running
      .iter()
      .any(|running| running.participants.contains(&user))
      || snapshot.maybe_active.as_ref().is_some_and(|active| {
        active
          .participants
          .iter()
          .any(|participant| participant.user == user)
      })
  }

  /// Queues an action behind those still running.
  pub fn act(&self, action: CallAction) {
    self.enqueue(QueuedAction {
      action,
      maybe_picked_screen: None,
    });
  }

  /// Starts sharing the screen. Call it synchronously from the click: in the browser the picker
  /// opens right here, while the click's user activation lasts (issue #930), and the chosen
  /// screen is shared in the action's turn.
  pub fn start_screen_share(&self, has_sound: bool, content_mode: ScreenContentMode) {
    let maybe_picked_screen = self.pick_screen(content_mode, has_sound);
    self.enqueue(QueuedAction {
      action: CallAction::StartScreenShare {
        has_sound,
        content_mode,
      },
      maybe_picked_screen,
    });
  }

  fn enqueue(&self, queued: QueuedAction) {
    self.queue.borrow_mut().push_back(queued);
    if self.is_draining.replace(true) {
      return;
    }
    let calls = self.clone();
    spawn_local(async move {
      loop {
        let maybe_queued = calls.queue.borrow_mut().pop_front();
        let Some(queued) = maybe_queued else {
          break;
        };
        if is_carried_out_beside(&queued.action) {
          calls.carry_out_beside(queued);
          continue;
        }
        let is_join = matches!(queued.action, CallAction::Join { .. });
        let result = calls.carry_out(queued.action).await;
        calls.report_outcome(result, is_join);
      }
      // async-ok: a Cell the calls share, not a signal.
      calls.is_draining.set(false);
      calls.refresh();
    });
  }

  /// Opens the browser's screen picker now (the page's calls only; the desktop shell has its own).
  #[cfg_attr(
    not(target_arch = "wasm32"),
    expect(unused_variables, reason = "only the page's picker takes the mode and sound")
  )]
  fn pick_screen(&self, content_mode: ScreenContentMode, has_sound: bool) -> Option<Promise> {
    match &self.link {
      CallLink::Shell => None,
      #[cfg(target_arch = "wasm32")]
      CallLink::Page(_) => crate::web_calls::pick_screen(content_mode, has_sound),
    }
  }

  /// Carries out `queued` without holding the queue (see [`is_carried_out_beside`]).
  fn carry_out_beside(&self, queued: QueuedAction) {
    let calls = self.clone();
    calls
      .screen_shares_starting
      .set(calls.screen_shares_starting.get() + 1);
    spawn_local(async move {
      let result = calls.carry_out_queued(queued).await;
      // async-ok: a Cell the calls share, not a signal.
      let starting = calls.screen_shares_starting.get();
      // async-ok: a Cell the calls share, not a signal.
      calls.screen_shares_starting.set(starting.saturating_sub(1));
      calls.report_outcome(result, false);
      calls.refresh();
    });
  }

  /// Tells the user about a failed action, or the part of one that failed.
  fn report_outcome(&self, result: Result<CallActionAnswer, String>, is_join: bool) {
    match result {
      Ok(None) => {},
      // The action went through (the call runs); only a part of it didn't.
      Ok(Some(partial)) => crate::feedback::report_failure(ActionFailure::from_wire(&partial)),
      Err(error) => {
        if is_join {
          // async-ok: a Cell the calls share, not a signal.
          self.join_failures.set(self.join_failures.get() + 1);
        }
        crate::feedback::report_failure(ActionFailure::from_wire(&error));
      },
    }
  }

  /// Plays the speaker test, outside the action queue: a join right after it doesn't wait for the
  /// sound. A failure reaches the user.
  pub fn test_speakers(&self, maybe_device: Option<String>) {
    let link = self.link.clone();
    spawn_local(async move {
      let result = match link {
        CallLink::Shell => {
          let action = CallAction::TestSpeakers { maybe_device };
          call::<CallActionAnswer>("call_action", &json!({ "action": action }))
            .await
            .map(drop)
            .map_err(|error| ActionFailure::from_wire(&error))
        },
        // The page plays it itself, with the context the click started (speaker_test.rs).
        #[cfg(target_arch = "wasm32")]
        CallLink::Page(_) => crate::speaker_test::play(maybe_device).await,
      };
      if let Err(failure) = result {
        crate::feedback::report_failure(failure);
      }
    });
  }

  /// Reads what the call's video does (stats for nerds, #938), outside the action queue: the
  /// overlay reads it every second and must not wait behind a join or a screen picker. The answer
  /// stays text: the call screen's chunk reads it.
  pub fn fetch_video_stats(&self, done: Box<dyn FnOnce(Option<VideoStatsJson>)>) {
    let link = self.link.clone();
    spawn_local(async move {
      let maybe_json = match link {
        CallLink::Shell => invoke("call_video_stats", js_sys::Object::new().into())
          .await
          .inspect_err(|error| tracing::debug!(%error, "video stats not read"))
          .ok()
          .and_then(|answer| js_sys::JSON::stringify(&answer).ok())
          .and_then(|text| text.as_string()),
        #[cfg(target_arch = "wasm32")]
        CallLink::Page(_) => crate::web_calls::video_stats().await,
      };
      done(maybe_json.map(VideoStatsJson));
    });
  }

  pub fn join(&self, request: CallRequest, media: JoinMedia) {
    self.act(CallAction::join(request, media));
  }

  /// Starts, changes or ends the pre-join check (#442), in order with the other actions: a join
  /// right after it finds the check running and takes the camera and microphone over.
  pub fn set_preview(&self, preview: CallPreview) {
    #[cfg(target_arch = "wasm32")]
    if let CallLink::Shell = self.link {
      self.shell_level.follow(preview.is_mic_on);
    }
    self.act(CallAction::SetPreview { preview });
  }

  pub fn preview_camera(&self) -> PreviewCamera {
    self.snapshot.borrow().preview_camera
  }

  pub fn camera_fallback(&self) -> Option<CameraFallback> {
    self.snapshot.borrow().maybe_camera_fallback.clone()
  }

  /// The pre-join check's microphone level: the shell's newest reading, or the page's meter.
  pub fn preview_level(&self) -> f32 {
    match &self.link {
      #[cfg(target_arch = "wasm32")]
      CallLink::Shell => self.shell_level.get(),
      #[cfg(not(target_arch = "wasm32"))]
      CallLink::Shell => 0.0,
      #[cfg(target_arch = "wasm32")]
      CallLink::Page(_) => crate::web_calls::preview_level(),
    }
  }

  /// Carries out an action, with the screen the page's picker chose when there is one.
  async fn carry_out_queued(&self, queued: QueuedAction) -> Result<CallActionAnswer, String> {
    match (&self.link, queued.maybe_picked_screen, queued.action) {
      #[cfg(target_arch = "wasm32")]
      (
        CallLink::Page(host),
        Some(picked),
        CallAction::StartScreenShare {
          has_sound,
          content_mode,
        },
      ) => host.share_screen(has_sound, content_mode, picked).await,
      (_, _, action) => self.carry_out(action).await,
    }
  }

  async fn carry_out(&self, action: CallAction) -> Result<CallActionAnswer, String> {
    match &self.link {
      CallLink::Shell => call::<CallActionAnswer>("call_action", &json!({ "action": action })).await,
      #[cfg(target_arch = "wasm32")]
      CallLink::Page(host) => host.act(action).await,
    }
  }

  fn refresh(&self) {
    #[cfg(target_arch = "wasm32")]
    // The page's calls hand over every new snapshot themselves.
    if let CallLink::Page(_) = &self.link {
      return;
    }
    let calls = self.clone();
    spawn_local(async move {
      match call::<CallSnapshot>("call_snapshot", &json!({})).await {
        Ok(snapshot) => {
          if !calls.replace_snapshot(snapshot) {
            return;
          }
          calls.notify_changed();
        },
        Err(error) => tracing::warn!(%error, "could not read the call state"),
      }
    });
  }

  fn notify_changed(&self) {
    let maybe_notify = self.notify.borrow().clone();
    if let Some(notify) = maybe_notify {
      notify();
    }
  }

  /// Takes a fresh snapshot; returns whether it differs. An unchanged one re-renders nothing: every
  /// action refreshes, and most (a device or effect choice outside a call) change nothing the UI
  /// shows (issue #399).
  fn replace_snapshot(&self, snapshot: CallSnapshot) -> bool {
    let mut held = self.snapshot.borrow_mut();
    if *held == snapshot {
      return false;
    }
    *held = snapshot;
    true
  }
}

/// Server roster of a conversation's call, plus the media layer's view when this device is in it.
fn roster_of(snapshot: &CallSnapshot, conversation: ConversationId) -> Vec<UserId> {
  let mut users: Vec<UserId> = snapshot
    .running
    .iter()
    .find(|running| running.conversation == conversation)
    .map(|running| running.participants.clone())
    .unwrap_or_default();
  if let Some(active) = snapshot
    .maybe_active
    .as_ref()
    .filter(|active| active.conversation == conversation)
  {
    for participant in &active.participants {
      if !users.contains(&participant.user) {
        users.push(participant.user);
      }
    }
  }
  users
}

/// Whether `action` runs beside the queue once its turn comes: a screen share's start waits for
/// the user in the system's picker, which nobody may answer for a long time (issue #683: every
/// later action, leaving included, waited with it).
fn is_carried_out_beside(action: &CallAction) -> bool {
  matches!(action, CallAction::StartScreenShare { .. })
}

/// The effects of calls in the page: in the call, without the photos (their full pictures are
/// only in the apps) and without the pre-join preview, where the browser runs them at all.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn browser_effects_offer(applies_effects: bool) -> EffectsOffer {
  if applies_effects {
    EffectsOffer::InCall
  } else {
    EffectsOffer::Unavailable
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_client_api::calls::{BackgroundEffect, CallKind, RunningCall};
  use teamcraft_client_api::model::id_from_raw;

  /// #439: the browser's call module runs the effects in the call where the page can.
  #[test]
  fn the_browser_offers_effects_only_where_it_runs_them() {
    assert_eq!(browser_effects_offer(true), EffectsOffer::InCall);
    assert_eq!(browser_effects_offer(false), EffectsOffer::Unavailable);
    assert!(!browser_effects_offer(false).has_picker());
    assert!(!browser_effects_offer(true).has_preview());
  }

  #[test]
  fn only_a_screen_share_start_runs_beside_the_queue() {
    use teamcraft_proto::ScreenContentMode;

    assert!(is_carried_out_beside(&CallAction::StartScreenShare {
      has_sound: true,
      content_mode: ScreenContentMode::Detail,
    }));
    for action in [
      CallAction::StopScreenShare,
      CallAction::Leave,
      CallAction::SetMuted { is_muted: true },
      CallAction::SetCamera { is_on: true },
    ] {
      assert!(!is_carried_out_beside(&action), "{action:?}");
    }
  }

  /// The desktop shell opens its own picker; only the page's calls open the browser's in the click
  /// (issue #930).
  #[test]
  fn the_shell_opens_no_browser_picker() {
    let calls = RemoteCalls::new(CallSnapshot::default(), CallLink::Shell);
    for mode in [ScreenContentMode::Detail, ScreenContentMode::Motion] {
      for has_sound in [false, true] {
        assert!(
          calls.pick_screen(mode, has_sound).is_none(),
          "{mode:?} {has_sound}"
        );
      }
    }
  }

  #[test]
  fn the_roster_comes_from_the_server_and_the_media_layer() {
    let conversation: ConversationId = id_from_raw(7);
    let (carol, dave): (UserId, UserId) = (id_from_raw(2), id_from_raw(3));
    let snapshot = CallSnapshot {
      maybe_active: Some(ActiveCall {
        conversation,
        kind: CallKind::Group,
        is_muted: false,
        is_deafened: false,
        is_secured: true,
        maybe_privacy_code: None,
        participants: vec![Participant {
          user: dave,
          is_muted: true,
          is_camera_on: false,
          has_hand_raised: false,
          is_sharing_screen: false,
        }],
        active_speakers: Vec::new(),
        is_camera_on: false,
        is_sharing_screen: false,
        background: BackgroundEffect::default(),
        maybe_muted_by_host: None,
        screen_sound: Default::default(),
        is_reconnecting: false,
        reconnecting: Vec::new(),
        maybe_moved_to: None,
      }),
      incoming: Vec::new(),
      running: vec![RunningCall {
        conversation,
        participants: vec![carol],
        is_ringing: false,
      }],
      devices: MediaDevices::default(),
      video_tracks: Vec::new(),
      push_to_talk: Default::default(),
      maybe_end_reason: None,
      preview_camera: PreviewCamera::Off,
      maybe_camera_fallback: None,
    };
    assert_eq!(roster_of(&snapshot, conversation), vec![carol, dave]);
    assert!(roster_of(&snapshot, id_from_raw(8)).is_empty());
  }

  /// Issue #399: an action that changed nothing must not re-render the UI.
  #[test]
  fn only_a_changed_snapshot_counts_as_a_change() {
    let calls = RemoteCalls::new(CallSnapshot::default(), CallLink::Shell);
    assert!(!calls.replace_snapshot(CallSnapshot::default()));
    let ringing = CallSnapshot {
      running: vec![RunningCall {
        conversation: id_from_raw(7),
        participants: vec![id_from_raw(2)],
        is_ringing: true,
      }],
      ..CallSnapshot::default()
    };
    assert!(calls.replace_snapshot(ringing.clone()));
    assert!(!calls.replace_snapshot(ringing));
  }
}
