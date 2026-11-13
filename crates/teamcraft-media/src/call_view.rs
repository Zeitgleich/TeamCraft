//! Applies the media engine's events to the UI's [`CallView`]: who is in the call, speaking,
//! sharing, sending video or reconnecting. The engine reports devices; the UI shows people.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use teamcraft_client_api::call_view::CallView;
use teamcraft_client_api::calls::{
  ActiveCall, CallEndReason, CallStatusSounds, EncryptionFailure, Participant, RemoteVideoTrack,
  ScreenShareSound,
};
use teamcraft_proto::{DeviceId, MediaStreamId, UserId, VideoCodec, VideoTrackKind};

use crate::call_cues::StatusCues;
use crate::call_engine::{CallEngineEvent, OwnVideoEnd};
use crate::call_resilience::CallTransport;

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
  /// Others' devices whose call key didn't verify: kept out of the encryption.
  refused: BTreeSet<DeviceId>,
  /// Others' tracks whose capture stalled, as their frames say (#1805).
  stalled_captures: Vec<(DeviceId, VideoTrackKind)>,
  /// Own tracks whose capture stalled (#1805): the hint stays while any of them is.
  own_stalled: Vec<VideoTrackKind>,
  /// The video codecs this device decodes; empty: not said, every track counts as decodable.
  video_decoders: Vec<VideoCodec>,
}

impl CallRoster {
  pub fn new(own_device: DeviceId) -> Self {
    Self {
      own_device: Some(own_device),
      ..Self::default()
    }
  }

  /// The video codecs this device decodes (`MediaCapabilities::video_decoders`): others' tracks in
  /// another codec are marked [`RemoteVideoTrack::is_undecodable`] (#1793).
  pub fn set_video_decoders(&mut self, decoders: Vec<VideoCodec>) {
    self.video_decoders = decoders;
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
        roster.stalled_captures.retain(|(device, _)| *device != device_id);
        active.stalled_videos = roster.stalled_users();
        roster.users_by_device.remove(&device_id);
        roster.sharing_screen.remove(&device_id);
        roster.reconnecting.remove(&device_id);
        roster.muted.remove(&device_id);
        active.reconnecting = roster.reconnecting_users();
        if roster.refused.remove(&device_id) {
          active.unverified = roster.refused_users();
        }
      },
      CallEngineEvent::RemoteCaptureStalled {
        device,
        kind,
        is_stalled,
      } => {
        roster
          .stalled_captures
          .retain(|(stalled, stalled_kind)| (*stalled, stalled_kind) != (device, &kind));
        if is_stalled {
          roster.stalled_captures.push((device, kind));
        }
        let users = roster.stalled_users();
        let has_changed = active.stalled_videos != users;
        active.stalled_videos = users;
        return has_changed;
      },
      CallEngineEvent::MemberRefused(device_id) => {
        roster.refused.insert(device_id);
        active.unverified = roster.refused_users();
        return true;
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
      CallEngineEvent::TransportChanged(transport) => {
        active.is_on_limited_network = transport == CallTransport::Fallback;
        return true;
      },
      CallEngineEvent::DecodeLimited(is_limited) => {
        let has_changed = active.is_video_limited_by_device != is_limited;
        active.is_video_limited_by_device = is_limited;
        return has_changed;
      },
      CallEngineEvent::CaptureStalled { kind, .. } => {
        if !roster.own_stalled.contains(&kind) {
          roster.own_stalled.push(kind);
        }
        let has_changed = !active.is_own_capture_stalled;
        active.is_own_capture_stalled = true;
        return has_changed;
      },
      CallEngineEvent::CaptureRecovered(kind) => {
        roster.own_stalled.retain(|stalled| *stalled != kind);
        let is_stalled = !roster.own_stalled.is_empty();
        let has_changed = active.is_own_capture_stalled != is_stalled;
        active.is_own_capture_stalled = is_stalled;
        return has_changed;
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
        active.maybe_encryption_failure = None;
      },
      CallEngineEvent::EncryptionFailed { retry_after } => {
        active.is_secured = false;
        active.maybe_encryption_failure = Some(EncryptionFailure {
          retry_after_secs: u32::try_from(retry_after.as_secs()).unwrap_or(u32::MAX),
        });
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
          active.maybe_screen_share_taken_over_by = None;
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
      // Another device's share replaced ours (one screen share per call): the UI says who.
      CallEngineEvent::OwnVideoEnded {
        kind: VideoTrackKind::Screen,
        end: OwnVideoEnd::TakenOver { by_user_id, .. },
      } => {
        roster.end_own_screen(active);
        active.maybe_screen_share_taken_over_by = Some(by_user_id);
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
        // The SFU's message names the request's subject ("unknown video track …"): the error
        // carries no request id (issue #976).
        tracing::warn!(error_code = ?error.error_code, message = %error.message, "the SFU rejected a call request");
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
              // The base layer is in the track's codec; every viewer must decode that one.
              is_undecodable: !self.video_decoders.is_empty() && !self.video_decoders.contains(&track.codec),
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
  /// People with a device refused: a person whose phone is fine but whose laptop's key is forged
  /// is shown as not verified all the same.
  fn refused_users(&self) -> Vec<UserId> {
    let users: BTreeSet<UserId> = self
      .refused
      .iter()
      .filter_map(|device| self.users_by_device.get(device).copied())
      .collect();
    users.into_iter().collect()
  }

  /// People with a track whose capture stalled (#1805).
  fn stalled_users(&self) -> Vec<UserId> {
    let users: BTreeSet<UserId> = self
      .stalled_captures
      .iter()
      .filter_map(|(device, _)| self.users_by_device.get(device).copied())
      .collect();
    users.into_iter().collect()
  }

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

/// What a screen share that just started shows of its sound: off when the user didn't ask for it,
/// shared when it is sent, and unavailable when it was asked for but the system or browser gave
/// none (Safari, Firefox, "share audio" unticked in Chromium's picker) or it couldn't be sent.
pub fn started_screen_sound(is_asked: bool, is_sent: bool) -> ScreenShareSound {
  match (is_asked, is_sent) {
    (false, _) => ScreenShareSound::Off,
    (true, true) => ScreenShareSound::Shared,
    (true, false) => ScreenShareSound::Unavailable,
  }
}

#[cfg(test)]
mod transport_tests {
  use teamcraft_client_api::call_view::CallView;
  use teamcraft_client_api::calls::{BackgroundEffect, CallKind, CallRequest};
  use teamcraft_proto::{ConversationId, Id, Snowflake};

  use super::*;

  #[test]
  fn the_fallback_shows_as_a_limited_network_until_a_rejoin_gets_quic_back() {
    let device = DeviceId::from_snowflake(Snowflake::new(1).unwrap());
    let mut roster = CallRoster::new(device);
    let mut view = CallView::default();
    view.start_active(
      CallRequest {
        conversation: ConversationId::from_snowflake(Snowflake::new(2).unwrap()),
        kind: CallKind::Group,
      },
      BackgroundEffect::default(),
      0,
    );
    let is_limited = |view: &CallView| view.maybe_active.as_ref().unwrap().is_on_limited_network;
    assert!(!is_limited(&view));
    assert!(roster.apply(
      &mut view,
      CallEngineEvent::TransportChanged(CallTransport::Fallback)
    ));
    assert!(is_limited(&view));
    assert!(roster.apply(&mut view, CallEngineEvent::TransportChanged(CallTransport::Quic)));
    assert!(!is_limited(&view));
  }

  /// #1762: a device that can't decode all it receives says so, calmly, until it can again; the
  /// same state again doesn't make the UI read.
  #[test]
  fn a_busy_device_shows_until_it_decodes_what_its_tiles_want() {
    let device = DeviceId::from_snowflake(Snowflake::new(1).unwrap());
    let mut roster = CallRoster::new(device);
    let mut view = CallView::default();
    view.start_active(
      CallRequest {
        conversation: ConversationId::from_snowflake(Snowflake::new(2).unwrap()),
        kind: CallKind::Group,
      },
      BackgroundEffect::default(),
      0,
    );
    let is_busy = |view: &CallView| view.maybe_active.as_ref().unwrap().is_video_limited_by_device;
    assert!(!is_busy(&view));
    assert!(roster.apply(&mut view, CallEngineEvent::DecodeLimited(true)));
    assert!(is_busy(&view));
    assert!(!roster.apply(&mut view, CallEngineEvent::DecodeLimited(true)));
    assert!(roster.apply(&mut view, CallEngineEvent::DecodeLimited(false)));
    assert!(!is_busy(&view));
    // #1805: an own capture that gives the others a still picture shows until it gives pictures
    // again (or its track goes, which the driver reports the same way).
    let is_stalled = |view: &CallView| view.maybe_active.as_ref().unwrap().is_own_capture_stalled;
    let stalled = CallEngineEvent::CaptureStalled {
      kind: VideoTrackKind::Screen,
      window: crate::capture_health::StallWindow {
        buffers: crate::capture_health::BufferCounts {
          unmapped: 150,
          ..Default::default()
        },
        ..Default::default()
      },
    };
    assert!(roster.apply(&mut view, stalled.clone()));
    assert!(is_stalled(&view));
    assert!(!roster.apply(&mut view, stalled));
    assert!(roster.apply(
      &mut view,
      CallEngineEvent::CaptureRecovered(VideoTrackKind::Screen)
    ));
    assert!(!is_stalled(&view));
    // Camera and screen stalled: the hint stays until both give pictures again.
    let camera = CallEngineEvent::CaptureStalled {
      kind: VideoTrackKind::Camera,
      window: crate::capture_health::StallWindow::default(),
    };
    let screen = CallEngineEvent::CaptureStalled {
      kind: VideoTrackKind::Screen,
      window: crate::capture_health::StallWindow::default(),
    };
    assert!(roster.apply(&mut view, camera));
    assert!(!roster.apply(&mut view, screen));
    assert!(!roster.apply(
      &mut view,
      CallEngineEvent::CaptureRecovered(VideoTrackKind::Camera)
    ));
    assert!(is_stalled(&view));
    assert!(roster.apply(
      &mut view,
      CallEngineEvent::CaptureRecovered(VideoTrackKind::Screen)
    ));
    assert!(!is_stalled(&view));
  }

  /// A refusal shows until the call is secured (#1555).
  #[test]
  fn an_encryption_failure_shows_until_secured() {
    let device = DeviceId::from_snowflake(Snowflake::new(1).unwrap());
    let mut roster = CallRoster::new(device);
    let mut view = CallView::default();
    view.start_active(
      CallRequest {
        conversation: ConversationId::from_snowflake(Snowflake::new(2).unwrap()),
        kind: CallKind::Group,
      },
      BackgroundEffect::default(),
      0,
    );
    let failure = |view: &CallView| view.maybe_active.as_ref().unwrap().maybe_encryption_failure;

    assert!(roster.apply(
      &mut view,
      CallEngineEvent::EncryptionFailed {
        retry_after: std::time::Duration::from_secs(300),
      }
    ));
    assert_eq!(
      failure(&view),
      Some(EncryptionFailure {
        retry_after_secs: 300
      })
    );
    assert!(roster.apply(
      &mut view,
      CallEngineEvent::Secured {
        epoch: 1,
        privacy_code: "1".to_owned(),
      }
    ));
    assert_eq!(failure(&view), None);
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

  mod started_screen_sound {
    use super::*;

    #[test]
    fn sound_not_asked_for_is_off() {
      assert_eq!(started_screen_sound(false, false), ScreenShareSound::Off);
    }

    #[test]
    fn sound_asked_for_and_sent_is_shared() {
      assert_eq!(started_screen_sound(true, true), ScreenShareSound::Shared);
    }

    /// Safari and Firefox capture no sound, and Chromium's picker lets the user untick it: the
    /// picture goes alone, and the user sees why there is no sound.
    #[test]
    fn sound_asked_for_but_not_given_is_unavailable() {
      assert_eq!(started_screen_sound(true, false), ScreenShareSound::Unavailable);
    }
  }

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
          maybe_publication_id: None,
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
            maybe_publication_id: None,
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

    /// One screen share per call: a share another device's replaced shows off and names who took
    /// over, until this device shares again.
    #[test]
    fn an_own_share_taken_over_says_by_whom_until_sharing_again() {
      let mut view = sharing_view();
      let mut roster = CallRoster::new(device(10));
      let taken_over = CallEngineEvent::OwnVideoEnded {
        kind: VideoTrackKind::Screen,
        end: OwnVideoEnd::TakenOver {
          by_user_id: user(3),
          by_device_id: device(30),
        },
      };
      assert!(roster.apply(&mut view, taken_over));
      let active = view.maybe_active.as_ref().unwrap();
      assert!(!active.is_sharing_screen);
      assert_eq!(active.screen_sound, ScreenShareSound::Off);
      assert_eq!(active.maybe_screen_share_taken_over_by, Some(user(3)));
      roster.apply(&mut view, own_screen(12));
      let active = view.maybe_active.as_ref().unwrap();
      assert_eq!(active.maybe_screen_share_taken_over_by, None);
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

  /// #1805: another device's stalled capture names its person until its frames say otherwise,
  /// or the device leaves.
  #[test]
  fn whose_video_isnt_updating_shows_until_it_is_again() {
    let mut view = active_view();
    let mut roster = CallRoster::new(device(1));
    roster.apply(&mut view, CallEngineEvent::ParticipantJoined(participant(4, 3)));
    let stalled = |is_stalled| CallEngineEvent::RemoteCaptureStalled {
      device: device(3),
      kind: VideoTrackKind::Screen,
      is_stalled,
    };
    let stalled_videos = |view: &CallView| view.maybe_active.as_ref().unwrap().stalled_videos.clone();
    assert!(roster.apply(&mut view, stalled(true)));
    assert_eq!(stalled_videos(&view), [user(4)]);
    assert!(!roster.apply(&mut view, stalled(true)));
    assert!(roster.apply(&mut view, stalled(false)));
    assert!(stalled_videos(&view).is_empty());
    roster.apply(&mut view, stalled(true));
    roster.apply(&mut view, CallEngineEvent::ParticipantLeft(device(3)));
    assert!(stalled_videos(&view).is_empty());
  }

  fn active_view() -> CallView {
    let mut view = CallView::default();
    view.start_active(request(), BackgroundEffect::None, 0);
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
      maybe_publication_id: None,
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
