//! Applies the media engine's events to the UI's [`CallView`]: who is in the call, speaking,
//! sharing, sending video or reconnecting. The engine reports devices; the UI shows people.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use teamcraft_client_api::call_view::CallView;
use teamcraft_client_api::calls::{
  ActiveCall, CallEndReason, CallStatusSounds, Participant, RemoteVideoTrack, ScreenShareSound,
};
use teamcraft_proto::{DeviceId, MediaStreamId, UserId, VideoTrackKind};

use crate::call_cues::StatusCues;
use crate::call_engine::CallEngineEvent;

/// Who is who in the active call: the engine reports devices, the UI shows people.
#[derive(Debug, Default)]
pub struct CallRoster {
  own_device: Option<DeviceId>,
  users_by_device: HashMap<DeviceId, UserId>,
  sharing_screen: BTreeSet<DeviceId>,
  /// Others' video tracks: who sends each and what it shows.
  video_devices: HashMap<MediaStreamId, (DeviceId, VideoTrackKind)>,
  /// This device's published screen track.
  maybe_own_screen: Option<MediaStreamId>,
  /// Others' devices whose connection was lost and who are rejoining.
  reconnecting: BTreeSet<DeviceId>,
  /// Others' devices whose microphone is muted (their own report).
  muted: BTreeSet<DeviceId>,
}

impl CallRoster {
  pub fn new(own_device: DeviceId) -> Self {
    Self {
      own_device: Some(own_device),
      ..Self::default()
    }
  }

  /// Updates the view's active call from one engine event; `true` if the UI should read again. Audio and
  /// video frames are not for this (the media drivers handle them) and return `false`.
  pub fn apply(&mut self, view: &mut CallView, event: CallEngineEvent) -> bool {
    let roster = self;
    if view.maybe_active.is_none() {
      return false;
    }
    roster.update_video_tracks(&mut view.video_tracks, &event);
    let Some(active) = view.maybe_active.as_mut() else {
      return false;
    };
    match event {
      CallEngineEvent::Joined { participants, .. } => {
        for participant in participants {
          roster
            .users_by_device
            .insert(participant.device_id, participant.user_id);
        }
      },
      CallEngineEvent::ParticipantJoined(participant) => {
        roster
          .users_by_device
          .insert(participant.device_id, participant.user_id);
      },
      CallEngineEvent::ParticipantLeft(device_id) => {
        roster.users_by_device.remove(&device_id);
        roster.sharing_screen.remove(&device_id);
        roster.reconnecting.remove(&device_id);
        roster.muted.remove(&device_id);
        active.reconnecting = roster.reconnecting_users();
      },
      CallEngineEvent::ParticipantMuted { device_id, is_muted } => {
        if is_muted {
          roster.muted.insert(device_id);
        } else {
          roster.muted.remove(&device_id);
        }
      },
      CallEngineEvent::Reconnecting => {
        active.is_reconnecting = true;
        // The SFU's answer to the rejoin names the muted devices again.
        roster.muted.clear();
        // Who else is away comes again with the SFU's answer to the rejoin.
        roster.reconnecting.clear();
        active.reconnecting.clear();
        return true;
      },
      CallEngineEvent::Reconnected => {
        active.is_reconnecting = false;
        return true;
      },
      CallEngineEvent::ParticipantReconnecting(device_id) => {
        roster.reconnecting.insert(device_id);
        active.reconnecting = roster.reconnecting_users();
        return true;
      },
      CallEngineEvent::ParticipantReconnected(device_id) => {
        roster.reconnecting.remove(&device_id);
        active.reconnecting = roster.reconnecting_users();
        return true;
      },
      CallEngineEvent::ActiveSpeakers(devices) => {
        let mut speakers = Vec::new();
        for user in devices
          .iter()
          .filter_map(|device| roster.users_by_device.get(device))
        {
          if !speakers.contains(user) {
            speakers.push(*user);
          }
        }
        active.active_speakers = speakers;
      },
      CallEngineEvent::Secured { privacy_code, .. } => {
        active.is_secured = true;
        active.maybe_privacy_code = Some(privacy_code);
      },
      CallEngineEvent::MuteChanged(is_muted) => {
        active.is_muted = is_muted;
        if !is_muted {
          active.maybe_muted_by_host = None;
        }
      },
      CallEngineEvent::MutedByHost { by_user_id } => active.maybe_muted_by_host = Some(by_user_id),
      // Our shared sound stopped without us (a host's "mute all"): the share goes on without it.
      CallEngineEvent::ScreenAudioChanged {
        device_id,
        is_shared: false,
      } if Some(device_id) == roster.own_device => active.screen_sound = ScreenShareSound::Off,
      CallEngineEvent::RemovedByHost { by_user_id } => {
        view.maybe_end_reason = Some(CallEndReason::RemovedByHost { by: by_user_id });
        return true;
      },
      CallEngineEvent::EndedByHost { by_user_id } => {
        view.maybe_end_reason = Some(CallEndReason::EndedByHost { by: by_user_id });
        return true;
      },
      CallEngineEvent::DeafenChanged(is_deafened) => active.is_deafened = is_deafened,
      CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Screen => {
        roster.sharing_screen.insert(track.device_id);
        if Some(track.device_id) == roster.own_device {
          roster.maybe_own_screen = Some(track.stream_id);
        }
      },
      // The SFU lost our share with the slot (a rejoin after its restart): the engine publishes it
      // again and the driver keeps capturing it, so it still shows as shared (with its sound, which
      // the engine asks for again too). Showing it stopped would hide a running share. Not marked
      // shared on the new publication instead: a share stopped meanwhile may still be confirmed.
      CallEngineEvent::VideoUnpublished(stream_id)
        if roster.maybe_own_screen == Some(stream_id) && active.is_reconnecting =>
      {
        roster.maybe_own_screen = None;
        return false;
      },
      // Our share ended without the stop button (from the system's indicator, or the SFU dropped
      // it): its sound stopped with it, and the UI must not show a share that isn't there.
      CallEngineEvent::VideoUnpublished(stream_id) if roster.maybe_own_screen == Some(stream_id) => {
        roster.maybe_own_screen = None;
        roster.end_own_screen(active);
      },
      // The driver stopped an own track whose camera or share went away by itself.
      CallEngineEvent::OwnVideoEnded { kind, .. } => match kind {
        VideoTrackKind::Camera => active.is_camera_on = false,
        VideoTrackKind::Screen => roster.end_own_screen(active),
        _ => return false,
      },
      // Remote tracks changed (see update_video_tracks); the snapshot lists them.
      CallEngineEvent::VideoPublished(_) | CallEngineEvent::VideoUnpublished(_) => {},
      CallEngineEvent::Error(error) => {
        tracing::warn!(error_code = ?error.error_code, "the SFU rejected a call request");
        return false;
      },
      CallEngineEvent::UntrustedRoster | CallEngineEvent::UnauthenticatedKeys => {
        // The apps always pass a roster and an authenticator; seeing these is a bug.
        tracing::warn!(?event, "call runs without full verification");
        return false;
      },
      _ => return false,
    }
    active.participants = roster.participants();
    true
  }

  /// This device's share ended: neither it nor its sound shows any more.
  fn end_own_screen(&mut self, active: &mut ActiveCall) {
    if let Some(device) = self.own_device {
      self.sharing_screen.remove(&device);
    }
    active.is_sharing_screen = false;
    active.screen_sound = ScreenShareSound::Off;
  }

  /// Keeps the list of others' video tracks (and which device sends each) current.
  fn update_video_tracks(
    &mut self,
    tracks: &mut BTreeMap<MediaStreamId, RemoteVideoTrack>,
    event: &CallEngineEvent,
  ) {
    match event {
      CallEngineEvent::VideoPublished(track) if Some(track.device_id) != self.own_device => {
        self
          .video_devices
          .insert(track.stream_id, (track.device_id, track.kind.clone()));
        if let Some(user) = self.users_by_device.get(&track.device_id) {
          tracks.insert(
            track.stream_id,
            RemoteVideoTrack {
              stream_id: track.stream_id,
              user: *user,
              kind: track.kind.clone(),
              layers: track
                .layers
                .iter()
                .map(|layer| (layer.width, layer.height))
                .collect(),
            },
          );
        }
      },
      CallEngineEvent::VideoUnpublished(stream_id) => {
        tracks.remove(stream_id);
        if let Some((device, VideoTrackKind::Screen)) = self.video_devices.remove(stream_id) {
          self.sharing_screen.remove(&device);
        }
      },
      CallEngineEvent::ParticipantLeft(device_id) => {
        self.video_devices.retain(|stream_id, (device, _)| {
          let keeps = device != device_id;
          if !keeps {
            tracks.remove(stream_id);
          }
          keeps
        });
      },
      _ => {},
    }
  }

  /// People all of whose devices in the call are reconnecting.
  fn reconnecting_users(&self) -> Vec<UserId> {
    let mut users: BTreeSet<UserId> = self
      .reconnecting
      .iter()
      .filter_map(|device| self.users_by_device.get(device).copied())
      .collect();
    users.retain(|user| {
      self
        .users_by_device
        .iter()
        .filter(|(_, owner)| *owner == user)
        .all(|(device, _)| self.reconnecting.contains(device))
    });
    users.into_iter().collect()
  }

  fn participants(&self) -> Vec<Participant> {
    let mut by_user: BTreeMap<UserId, Participant> = BTreeMap::new();
    for (device, user) in &self.users_by_device {
      let participant = by_user.entry(*user).or_insert(Participant {
        user: *user,
        // Muted when every device of the person in the call is.
        is_muted: true,
        is_camera_on: false,
        has_hand_raised: false,
        is_sharing_screen: false,
      });
      participant.is_muted &= self.muted.contains(device);
      participant.is_sharing_screen |= self.sharing_screen.contains(device);
      participant.is_camera_on |= self
        .video_devices
        .values()
        .any(|(sender, kind)| sender == device && *kind == VideoTrackKind::Camera);
    }
    by_user.into_values().collect()
  }
}

/// The status sounds the user's setting asks for.
pub fn status_cues(sounds: CallStatusSounds) -> StatusCues {
  match sounds {
    CallStatusSounds::All | CallStatusSounds::Unknown => StatusCues::All,
    CallStatusSounds::OwnOnly => StatusCues::OwnOnly,
    CallStatusSounds::Off => StatusCues::Off,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::calls::{BackgroundEffect, CallKind, CallRequest};
  use teamcraft_proto::{
    CallId, CallParticipant, ConversationId, Id, MediaStreamId, Snowflake, VideoCodec, VideoTrack,
  };

  use super::*;
  use crate::call_engine::OwnVideoEnd;

  mod media_events {
    use super::*;

    #[test]
    fn devices_become_people_and_speakers() {
      let mut view = active_view();
      let mut roster = CallRoster::default();
      roster.apply(
        &mut view,
        CallEngineEvent::Joined {
          call_id: call(),
          participants: vec![participant(1, 10), participant(1, 11)],
        },
      );
      roster.apply(&mut view, CallEngineEvent::ParticipantJoined(participant(2, 20)));
      roster.apply(
        &mut view,
        CallEngineEvent::ActiveSpeakers(vec![device(11), device(10), device(20)]),
      );
      roster.apply(
        &mut view,
        CallEngineEvent::VideoPublished(VideoTrack {
          stream_id: MediaStreamId(9),
          device_id: device(20),
          kind: VideoTrackKind::Screen,
          codec: VideoCodec::Vp9,
          layers: Vec::new(),
        }),
      );
      assert!(roster.apply(
        &mut view,
        CallEngineEvent::Secured {
          epoch: 1,
          privacy_code: "123".into(),
        },
      ));
      let active = view.maybe_active.clone().unwrap();
      assert_eq!(active.active_speakers, vec![user(1), user(2)]);
      assert_eq!(active.participants.len(), 2, "one entry per person");
      assert!(
        active
          .participants
          .iter()
          .any(|p| p.user == user(2) && p.is_sharing_screen)
      );
      assert!(active.is_secured);
      assert_eq!(active.maybe_privacy_code.as_deref(), Some("123"));
    }

    #[test]
    fn a_person_shows_muted_when_every_device_of_them_in_the_call_is() {
      let mut view = active_view();
      let mut roster = CallRoster::new(device(10));
      for event in [
        CallEngineEvent::ParticipantJoined(participant(2, 20)),
        CallEngineEvent::ParticipantJoined(participant(2, 21)),
        CallEngineEvent::ParticipantMuted {
          device_id: device(20),
          is_muted: true,
        },
      ] {
        roster.apply(&mut view, event);
      }
      let is_muted = |view: &CallView| view.maybe_active.as_ref().unwrap().participants[0].is_muted;
      assert!(!is_muted(&view), "the second device is not muted");
      roster.apply(
        &mut view,
        CallEngineEvent::ParticipantMuted {
          device_id: device(21),
          is_muted: true,
        },
      );
      assert!(is_muted(&view));
      roster.apply(&mut view, CallEngineEvent::Reconnecting);
      roster.apply(&mut view, CallEngineEvent::ParticipantJoined(participant(2, 20)));
      assert!(!is_muted(&view), "forgotten until the SFU says it again");
    }

    #[test]
    fn others_video_tracks_are_listed_until_unpublished() {
      let mut view = active_view();
      let mut roster = CallRoster::new(device(10));
      roster.apply(&mut view, CallEngineEvent::ParticipantJoined(participant(2, 20)));
      for (owner, stream) in [(20, 7), (10, 8)] {
        roster.apply(
          &mut view,
          CallEngineEvent::VideoPublished(VideoTrack {
            stream_id: MediaStreamId(stream),
            device_id: device(owner),
            kind: VideoTrackKind::Camera,
            codec: VideoCodec::H264,
            layers: Vec::new(),
          }),
        );
      }
      let tracks: Vec<MediaStreamId> = view.video_tracks.keys().copied().collect();
      assert_eq!(tracks, vec![MediaStreamId(7)], "own track not listed");
      assert_eq!(view.video_tracks[&MediaStreamId(7)].user, user(2));
      let others = &view.maybe_active.as_ref().unwrap().participants;
      assert!(others.iter().any(|p| p.user == user(2) && p.is_camera_on));
      roster.apply(&mut view, CallEngineEvent::VideoUnpublished(MediaStreamId(7)));
      assert!(view.video_tracks.is_empty());
      // Participants are recomputed with the next roster change.
      roster.apply(&mut view, CallEngineEvent::ParticipantJoined(participant(3, 30)));
      let others = &view.maybe_active.as_ref().unwrap().participants;
      assert!(others.iter().all(|p| !p.is_camera_on));
    }

    #[test]
    fn a_share_ended_from_the_system_ends_in_the_ui_with_its_sound() {
      let mut view = sharing_view();
      let mut roster = CallRoster::new(device(10));
      roster.apply(&mut view, own_screen(8));
      // The capture ended without the stop button; the media driver unpublished the track.
      roster.apply(&mut view, CallEngineEvent::VideoUnpublished(MediaStreamId(8)));
      let active = view.maybe_active.as_ref().unwrap();
      assert!(!active.is_sharing_screen);
      assert_eq!(active.screen_sound, ScreenShareSound::Off);
    }

    /// Issue #920: after an SFU restart the engine publishes the share again on a new stream; the
    /// UI must not show it stopped while it is sent.
    #[test]
    fn a_share_lost_with_the_slot_stays_shown_while_it_is_sent_again() {
      let mut view = sharing_view();
      let mut roster = CallRoster::new(device(10));
      roster.apply(&mut view, own_screen(8));
      // The engine's rejoin without the slot: the old stream goes, the track is published again.
      roster.apply(&mut view, CallEngineEvent::Reconnecting);
      roster.apply(&mut view, CallEngineEvent::VideoUnpublished(MediaStreamId(8)));
      roster.apply(&mut view, CallEngineEvent::Reconnected);
      let is_shown = |view: &CallView| {
        let active = view.maybe_active.as_ref().unwrap();
        (active.is_sharing_screen, active.screen_sound)
      };
      assert_eq!(is_shown(&view), (true, ScreenShareSound::Shared));
      roster.apply(&mut view, own_screen(9));
      assert_eq!(is_shown(&view), (true, ScreenShareSound::Shared));
      // The new stream is the share now: ending it from the system ends it in the UI.
      roster.apply(&mut view, CallEngineEvent::VideoUnpublished(MediaStreamId(8)));
      assert_eq!(
        is_shown(&view),
        (true, ScreenShareSound::Shared),
        "the old stream is gone"
      );
      roster.apply(&mut view, CallEngineEvent::VideoUnpublished(MediaStreamId(9)));
      assert_eq!(is_shown(&view), (false, ScreenShareSound::Off));
    }

    #[test]
    fn an_own_camera_that_ended_by_itself_shows_off() {
      let mut view = active_view();
      if let Some(active) = view.maybe_active.as_mut() {
        active.is_camera_on = true;
        active.is_sharing_screen = true;
      }
      let mut roster = CallRoster::new(device(10));
      let ended = CallEngineEvent::OwnVideoEnded {
        kind: VideoTrackKind::Camera,
        end: OwnVideoEnd::WhileSending,
      };
      assert!(roster.apply(&mut view, ended));
      let active = view.maybe_active.as_ref().unwrap();
      assert!(!active.is_camera_on);
      assert!(active.is_sharing_screen, "the share goes on");
    }

    #[test]
    fn an_own_share_that_ended_by_itself_shows_off_with_its_sound() {
      let mut view = sharing_view();
      if let Some(active) = view.maybe_active.as_mut() {
        active.is_camera_on = true;
      }
      let mut roster = CallRoster::new(device(10));
      let ended = CallEngineEvent::OwnVideoEnded {
        kind: VideoTrackKind::Screen,
        end: OwnVideoEnd::BeforeFirstPicture,
      };
      assert!(roster.apply(&mut view, ended));
      let active = view.maybe_active.as_ref().unwrap();
      assert!(!active.is_sharing_screen);
      assert_eq!(active.screen_sound, ScreenShareSound::Off);
      assert!(active.is_camera_on, "the camera goes on");
    }

    #[test]
    fn reconnecting_shows_for_this_device_and_for_people_whose_devices_are_all_away() {
      let mut view = active_view();
      let mut roster = CallRoster::default();
      let joined = CallEngineEvent::Joined {
        call_id: call(),
        participants: vec![participant(2, 20), participant(2, 21), participant(3, 30)],
      };
      roster.apply(&mut view, joined);
      let reconnecting = |view: &mut CallView, roster: &mut CallRoster, device_id: i64| {
        roster.apply(view, CallEngineEvent::ParticipantReconnecting(device(device_id)));
        view.maybe_active.as_ref().unwrap().reconnecting.clone()
      };
      assert_eq!(reconnecting(&mut view, &mut roster, 30), vec![user(3)]);
      // One of two devices away: the person is still there.
      assert_eq!(reconnecting(&mut view, &mut roster, 20), vec![user(3)]);
      roster.apply(&mut view, CallEngineEvent::ParticipantReconnected(device(30)));
      assert!(view.maybe_active.as_ref().unwrap().reconnecting.is_empty());

      assert!(roster.apply(&mut view, CallEngineEvent::Reconnecting));
      assert!(view.maybe_active.as_ref().unwrap().is_reconnecting);
      roster.apply(&mut view, CallEngineEvent::Reconnected);
      assert!(!view.maybe_active.as_ref().unwrap().is_reconnecting);
    }

    #[test]
    fn audio_frames_do_not_wake_the_ui() {
      let mut view = active_view();
      let event = CallEngineEvent::UplinkEstimate {
        bps: 1,
        loss_percent: 0,
      };
      assert!(!CallRoster::default().apply(&mut view, event));
    }

    #[test]
    fn events_after_leaving_are_ignored() {
      let mut view = active_view();
      view.end_active();
      let event = CallEngineEvent::ParticipantJoined(participant(2, 20));
      assert!(!CallRoster::default().apply(&mut view, event));
      assert!(view.maybe_active.is_none());
    }
  }

  fn active_view() -> CallView {
    let mut view = CallView::default();
    view.start_active(request(), BackgroundEffect::None);
    view
  }

  /// An active call in which this device shares its screen with sound.
  fn sharing_view() -> CallView {
    let mut view = active_view();
    if let Some(active) = view.maybe_active.as_mut() {
      active.is_sharing_screen = true;
      active.screen_sound = ScreenShareSound::Shared;
    }
    view
  }

  /// This device's (device 10's) screen track, published on `stream`.
  fn own_screen(stream: u32) -> CallEngineEvent {
    CallEngineEvent::VideoPublished(VideoTrack {
      stream_id: MediaStreamId(stream),
      device_id: device(10),
      kind: VideoTrackKind::Screen,
      codec: VideoCodec::H264,
      layers: Vec::new(),
    })
  }

  fn request() -> CallRequest {
    CallRequest {
      conversation: conversation(5),
      kind: CallKind::Group,
    }
  }

  fn participant(user_id: i64, device_id: i64) -> CallParticipant {
    CallParticipant {
      user_id: user(user_id),
      device_id: device(device_id),
      audio_stream_id: MediaStreamId(1),
    }
  }

  fn call() -> CallId {
    CallId::from_snowflake(Snowflake::new(99).unwrap())
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
