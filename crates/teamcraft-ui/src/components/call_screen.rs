use std::cell::Cell;

use leptos::prelude::*;

use teamcraft_proto::VideoTrackKind;

use crate::calls::{
  CallEvent, CallKind, CallLayout, CallRequest, MediaDevice, Participant, ScreenContentMode,
  ScreenShareResolution, ScreenShareSound, selected_camera_is_rear,
};
use crate::components::call_audio_menu::AudioMenu;
use crate::components::call_chat::{CallChat, CallChatPanel, CallChatToast};
use crate::components::call_notetaker::{NotetakerBanner, ask_notetaker_in, use_notetaker_in_call};
use crate::components::call_prejoin::call_heading;
use crate::components::call_video::{
  BackgroundPicker, LiveVideo, PickerSize, VideoSource, VideoTile, can_share_screen, can_show_video,
};
use crate::components::count_badge::CountBadge;
use crate::components::icons::{Icon, IconName};
use crate::components::video_quality::VideoQualityMenu;
use crate::components::video_stats::{VideoStatsHub, VideoStatsOverlay};
use crate::i18n::use_i18n;
use crate::model::{User, UserId};
use crate::reactive::use_interval;
use crate::state::{Overlay, use_app_state};
use crate::video_stats::StatsTarget;

const SPEAKER_ROTATE_MS: u64 = 2400;
const CLOCK_TICK_MS: u64 = 1000;

/// Everything a tile needs, for remote participants and for the user.
#[derive(Clone, Debug, PartialEq)]
struct TileData {
  user: User,
  participant: Option<Participant>,
}

/// The in-call screen: gallery, active speaker or shared-screen layout with the control bar.
#[component]
pub fn CallScreen(request: CallRequest) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let conversation = Memo::new(move |_| state.read(|backend| backend.conversation(request.conversation)));
  let remote = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .call_participants(request.conversation)
        .into_iter()
        .filter_map(|participant| {
          backend.user(participant.user).map(|user| TileData {
            user,
            participant: Some(participant),
          })
        })
        .collect::<Vec<_>>()
    })
  });
  // The notetaker (e2ee.md §8.2): its account comes from the verified policy, its presence from
  // who is in the call; the badge, the banner and its tile say so to everybody.
  let others =
    Memo::new(move |_| remote.with(|tiles| tiles.iter().map(|tile| tile.user.id).collect::<Vec<_>>()));
  let notetaker = use_notetaker_in_call(request.conversation, others);
  let is_notetaker_asked = RwSignal::new(false);
  let is_notetaker_present =
    Memo::new(move |_| notetaker.get().is_some_and(|notetaker| notetaker.is_present));
  let remote_sharer = Memo::new(move |_| {
    remote
      .get()
      .into_iter()
      .find(|tile| tile.participant.as_ref().is_some_and(|p| p.is_sharing_screen))
  });
  let host_open = RwSignal::new(false);
  let has_meetings = use_context::<crate::meetings::MeetingsHub>().is_some();
  let can_add_people = Memo::new(move |_| {
    has_meetings
      && request.kind != CallKind::Huddle
      && conversation
        .get()
        .is_some_and(|conversation| conversation.kind.is_direct_like())
      && state.read(|backend| !backend.has_simulated_calls())
  });

  // Real calls report who is speaking; the demo rotates the active speaker among people who are not
  // muted.
  let is_real = !untrack(|| state.read(|backend| backend.has_simulated_calls()));
  let has_effects = untrack(|| state.read(|backend| backend.effects_offer().has_picker()));
  // Real video where the page shows it: others' tracks by person and kind, and the own camera once
  // the media layer sends it.
  let video_tracks = Memo::new(move |_| state.read(|backend| backend.call_video_tracks()));
  let track_of = move |user: UserId, kind: VideoTrackKind| {
    can_show_video()
      .then(|| {
        video_tracks
          .get()
          .into_iter()
          .find(|track| track.user == user && track.kind == kind)
          .map(|track| VideoSource::Remote(track.stream_id))
      })
      .flatten()
  };
  // Memos: a view that changed the same way (another backend revision) must not remount the video.
  let own_video = Memo::new(move |_| {
    let is_sending = state.read(|backend| backend.active_call().is_some_and(|active| active.is_camera_on));
    (can_show_video() && is_sending).then_some(VideoSource::OwnCamera)
  });
  let speaking = RwSignal::new(None::<UserId>);
  Effect::new(move |_| {
    if is_real {
      let loudest = state.read(|backend| {
        backend
          .active_call()
          .and_then(|active| active.active_speakers.first().copied())
      });
      // Every backend change runs this; only a new speaker may tell the stage (issue #687).
      if speaking.get_untracked() != loudest {
        speaking.set(loudest);
      }
    }
  });
  let privacy_code = Memo::new(move |_| {
    state.read(|backend| backend.active_call().and_then(|active| active.maybe_privacy_code))
  });
  let is_connecting = Memo::new(move |_| {
    is_real && !call.has_ended.get() && state.read(|backend| backend.active_call().is_none())
  });
  let is_reconnecting = Memo::new(move |_| {
    state.read(|backend| backend.active_call().is_some_and(|active| active.is_reconnecting))
  });
  let reconnecting = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .active_call()
        .map(|active| active.reconnecting)
        .unwrap_or_default()
    })
  });
  let rotation = Cell::new(0_usize);
  use_interval(SPEAKER_ROTATE_MS, move || {
    if is_real {
      return;
    }
    let candidates: Vec<UserId> = remote
      .get_untracked()
      .into_iter()
      .filter(|tile| tile.participant.as_ref().is_some_and(|p| !p.is_muted))
      .map(|tile| tile.user.id)
      .collect();
    if candidates.is_empty() {
      if speaking.get_untracked().is_some() {
        speaking.set(None);
      }
    } else {
      rotation.set(rotation.get() + 1);
      speaking.set(candidates.get(rotation.get() % candidates.len()).copied());
    }
  });
  let elapsed = RwSignal::new(0_u32);
  use_interval(CLOCK_TICK_MS, move || elapsed.update(|seconds| *seconds += 1));
  let clock = move || format!("{:02}:{:02}", elapsed.get() / 60, elapsed.get() % 60);

  let effects_open = RwSignal::new(false);
  let share_open = RwSignal::new(false);
  // Stats for nerds (#938) over the real video; the overlays find the hub.
  let video_stats = VideoStatsHub::provide(call.shows_video_stats);
  let has_video_stats = is_real && can_show_video();
  // The conversation's chat next to the video (#936).
  let chat = CallChat::new(state, request.conversation, call.is_chat_open);
  let screen_sound = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .active_call()
        .map(|active| active.screen_sound)
        .unwrap_or_default()
    })
  });
  let set_screen_mode = move |mode: ScreenContentMode| {
    call.screen_mode.set(mode);
    state
      .settings
      .prefs
      .update(|prefs| prefs.calls.screen_mode = mode);
  };
  let set_screen_resolution = move |resolution: ScreenShareResolution| {
    call.screen_resolution.set(resolution.clone());
    state
      .settings
      .prefs
      .update(|prefs| prefs.calls.screen_resolution = resolution);
  };
  // The 4K choice is offered where the workspace allows it (#565).
  let can_share_uhd = Memo::new(move |_| {
    state.read(|backend| backend.screen_share_policy().max_resolution == ScreenShareResolution::Uhd)
  });
  let set_share_sound = move |has_sound: bool| {
    call.share_sound.set(has_sound);
    state
      .settings
      .prefs
      .update(|prefs| prefs.calls.share_sound = has_sound);
  };

  let maybe_meetings = use_context::<crate::meetings::MeetingsHub>();
  let title = move || {
    let maybe_meeting_title = maybe_meetings.and_then(|hub| hub.title_for(request.conversation));
    let name = conversation.get().map(|conv| conv.name).unwrap_or_default();
    call_heading(i18n, request.kind, maybe_meeting_title.as_deref(), &name)
  };
  let participant_count = move || remote.get().len() + 1;

  let cameras = Memo::new(move |_| state.read(|backend| backend.media_devices()).cameras);
  // A phone flips between front and back with one tap; elsewhere the camera button has a menu of
  // the cameras (#819).
  let has_camera_menu = Memo::new(move |_| !state.is_compact() && cameras.with(|cameras| cameras.len() > 1));
  // The microphone's menu of microphones and speakers (#986) likewise stays off a phone's bar.
  let has_audio_menu = Memo::new(move |_| !state.is_compact());
  let has_camera_flip = Memo::new(move |_| state.is_compact() && cameras.with(|cameras| cameras.len() > 1));
  // The list may be old: a camera plugged in, an iPhone that came near.
  Effect::new(move |_| {
    if call.is_camera_menu_open.get() {
      effects_open.set(false);
      share_open.set(false);
      call.maybe_quality_menu.set(None);
      call.is_audio_menu_open.set(false);
      call.emit(CallEvent::RefreshDevices);
    }
  });
  // A video's quality menu (#967) takes the place of the others.
  Effect::new(move |_| {
    if call.maybe_quality_menu.with(Option::is_some) {
      effects_open.set(false);
      share_open.set(false);
      call.is_camera_menu_open.set(false);
      call.is_audio_menu_open.set(false);
    }
  });
  // So does the microphone and speaker menu (#986), which lists the devices itself.
  Effect::new(move |_| {
    if call.is_audio_menu_open.get() {
      effects_open.set(false);
      share_open.set(false);
      call.is_camera_menu_open.set(false);
      call.maybe_quality_menu.set(None);
    }
  });
  // The next call screen starts with the menus closed.
  on_cleanup(move || {
    // Gone only while the app itself is torn down.
    let _ = call.is_camera_menu_open.try_set(false);
    let _ = call.maybe_quality_menu.try_set(None);
    let _ = call.is_audio_menu_open.try_set(false);
  });
  let pick_camera = move |id: String| {
    call.is_camera_menu_open.set(false);
    call.choose_camera(id.clone());
    state.settings.prefs.update(|prefs| prefs.calls.camera_id = id);
  };
  // Only the front camera is mirrored; a back camera shows the scene as it is.
  let is_self_mirrored = Signal::derive(move || {
    call.mirror_self_view.get()
      && !cameras.with(|cameras| selected_camera_is_rear(cameras, &call.camera_id.get()))
  });

  let self_tile = move |is_large: bool| {
    view! {
      <VideoTile
        user=state.me()
        is_self=true
        is_large=is_large
        is_muted=Signal::derive(move || !call.is_mic_on.get())
        is_camera_on=call.is_camera_on
        has_hand_raised=call.has_hand_raised
        effect=call.effect
        is_mirrored=is_self_mirrored
        video=own_video
      />
    }
  };
  // A tile follows its person's microphone, camera and hand in place: muting someone must not build
  // the stage again, which would remount every video (issue #687).
  let participant_flag = move |id: UserId, flag: fn(&Participant) -> bool, fallback: bool| {
    Memo::new(move |_| {
      remote.with(|tiles| {
        tiles
          .iter()
          .find(|tile| tile.user.id == id)
          .and_then(|tile| tile.participant.as_ref())
          .map_or(fallback, flag)
      })
    })
  };
  let remote_tile = move |id: UserId, is_large: bool| {
    // Untracked: a new presence or name doesn't build the tile again.
    let maybe_user = remote.with_untracked(|tiles| {
      tiles
        .iter()
        .find(|tile| tile.user.id == id)
        .map(|tile| tile.user.clone())
    });
    let maybe_role = notetaker
      .with_untracked(|notetaker| notetaker.as_ref().is_some_and(|notetaker| notetaker.bot == id))
      .then(|| i18n.t("tile-notetaker"));
    maybe_user.map(|user| {
      view! {
        <VideoTile
          user=user
          maybe_role=maybe_role
          is_large=is_large
          is_muted=participant_flag(id, |participant| participant.is_muted, false)
          is_camera_on=participant_flag(id, |participant| participant.is_camera_on, true)
          has_hand_raised=participant_flag(id, |participant| participant.has_hand_raised, false)
          is_speaking=Signal::derive(move || speaking.get() == Some(id))
          is_reconnecting=Signal::derive(move || reconnecting.get().contains(&id))
          video=Memo::new(move |_| track_of(id, VideoTrackKind::Camera))
        />
      }
    })
  };
  // What the stage is built from: who is in the call (in order), and who shares their screen.
  let remote_ids =
    Memo::new(move |_| remote.with(|tiles| tiles.iter().map(|tile| tile.user.id).collect::<Vec<_>>()));
  let sharer = Memo::new(move |_| {
    remote_sharer.with(|maybe_tile| {
      maybe_tile
        .as_ref()
        .map(|tile| (tile.user.id, tile.user.display_name.clone()))
    })
  });
  let is_share_layout = move || call.is_sharing_screen.get() || sharer.with(Option::is_some);
  let shared_screen = Memo::new(move |_| {
    sharer
      .with(|maybe_sharer| maybe_sharer.as_ref().map(|(id, _)| *id))
      .and_then(|id| track_of(id, VideoTrackKind::Screen))
  });

  // The active speaker layout moves a tile only when the one in focus is someone else.
  let speaker_focus = Memo::new(move |_| {
    speaking
      .get()
      .and_then(|id| remote_ids.with(|ids| ids.iter().position(|candidate| *candidate == id)))
      .unwrap_or(0)
  });

  let stage = move || {
    let remote_tiles = remote_ids.get();
    if is_share_layout() {
      let maybe_screen = shared_screen.get();
      let sharer_name = sharer.get().map(|(_, name)| name).unwrap_or_default();
      let is_me_sharing = call.is_sharing_screen.get();
      let banner = if is_me_sharing {
        i18n.t("share-banner-self")
      } else {
        i18n.t_with("share-banner-remote", &[("name", &sharer_name)])
      };
      let label = i18n.t_with(
        "share-screen-label",
        &[(
          "name",
          &if is_me_sharing {
            me_name(state)
          } else {
            sharer_name
          },
        )],
      );
      let strip: Vec<_> = remote_tiles
        .into_iter()
        .map(|tile| remote_tile(tile, false).into_any())
        .collect();
      return view! {
        <div class="stage stage-share">
          <div class="share-main">
            <div class="share-banner" role="status">
              <Icon name=IconName::Monitor />
              <span>{banner}</span>
              {move || {
                let key = match screen_sound.get() {
                  ScreenShareSound::Shared => "share-sound-on",
                  ScreenShareSound::Unavailable => "share-sound-unavailable",
                  ScreenShareSound::Off | ScreenShareSound::Unknown => return None,
                };
                call.is_sharing_screen.get().then(|| view! { <span class="share-sound muted-text small">{i18n.t(key)}</span> })
              }}
              <Show when=move || call.is_sharing_screen.get() fallback=|| ()>
                <button class="btn btn-small" on:click=move |_| call.set_screen_share(false)>
                  {move || i18n.t("share-stop")}
                </button>
              </Show>
            </div>
            <div class="shared-screen" role="img" aria-label=label>
              // The own share shows no picture here; its stats do (send side).
              {is_me_sharing.then(|| view! { <VideoStatsOverlay target=StatsTarget::OwnScreen /> })}
              {match maybe_screen {
                Some(source) => view! { <LiveVideo source=source is_mirrored=false /> }.into_any(),
                None => view! {
              <div class="mock-window">
                <div class="mock-titlebar"><span></span><span></span><span></span></div>
                <div class="mock-body">
                  <div class="mock-sidebar"></div>
                  <div class="mock-content">
                    <div class="mock-line wide"></div>
                    <div class="mock-line"></div>
                    <div class="mock-line"></div>
                    <div class="mock-chart"></div>
                    <div class="mock-line wide"></div>
                    <div class="mock-line"></div>
                  </div>
                </div>
              </div>
                }
                .into_any(),
              }}
            </div>
          </div>
          <div class="filmstrip filmstrip-side">{self_tile(false)} {strip}</div>
        </div>
      }
      .into_any();
    }
    match call.layout.get() {
      CallLayout::Gallery => {
        let total = remote_tiles.len() + 1;
        let columns = match total {
          0..=1 => 1,
          2..=4 => 2,
          5..=9 => 3,
          _ => 4,
        };
        let tiles: Vec<_> = remote_tiles
          .into_iter()
          .map(|tile| remote_tile(tile, false).into_any())
          .collect();
        view! {
          <div class="stage stage-gallery" style:--cols=columns.to_string()>
            {tiles} {self_tile(false)}
          </div>
        }
        .into_any()
      },
      CallLayout::ActiveSpeaker => {
        let focus = speaker_focus.get();
        let mut others = remote_tiles;
        let main = if others.is_empty() {
          None
        } else {
          Some(others.remove(focus))
        };
        let strip: Vec<_> = others
          .into_iter()
          .map(|tile| remote_tile(tile, false).into_any())
          .collect();
        view! {
          <div class="stage stage-speaker">
            <div class="speaker-main">
              {match main {
                Some(tile) => remote_tile(tile, true).into_any(),
                None => self_tile(true).into_any(),
              }}
            </div>
            <div class="filmstrip">
              {strip}
              {self_tile(false)}
            </div>
          </div>
        }
        .into_any()
      },
    }
  };

  view! {
    <section class="call-screen" aria-labelledby="call-title">
      <header class="call-header">
        <div class="call-heading">
          <h1 id="call-title">{title}</h1>
          <span class="call-meta">
            <time aria-label=move || i18n.t_with("call-elapsed", &[("time", &clock())])>{clock}</time>
            <span>{move || i18n.t_count("call-participants", participant_count())}</span>
          </span>
        </div>
        <Show
          when=move || is_notetaker_present.get()
          fallback=move || view! {
            <span class="e2ee-pill" title=move || i18n.t("call-encrypted-detail")>
              <Icon name=IconName::Shield />
              <span>{move || i18n.t("call-encrypted")}</span>
              {move || {
                let code = if is_real { privacy_code.get() } else { Some("48 20 17".to_owned()) };
                code.map(|code| view! { <span class="mono" translate="no">{i18n.t_with("call-verification-code", &[("code", &code)])}</span> })
              }}
            </span>
          }
        >
          <span class="e2ee-pill is-notetaker" title=move || i18n.t("call-notetaker-badge-detail")>
            <Icon name=IconName::Alert />
            <span>{move || i18n.t("call-notetaker-badge")}</span>
          </span>
        </Show>
        <Show when=move || is_connecting.get() fallback=|| ()>
          <span class="muted-text" role="status">{move || i18n.t("call-connecting")}</span>
        </Show>
        <Show when=move || call.has_ended.get() fallback=|| ()>
          <span class="muted-text" role="status">{move || i18n.t("call-ended")}</span>
        </Show>
        <Show when=move || is_reconnecting.get() fallback=|| ()>
          <span class="muted-text" role="status">{move || i18n.t("call-reconnecting")}</span>
        </Show>
      </header>
      {move || {
        let hub = use_context::<crate::meetings::MeetingsHub>()?;
        let me = state.me().id;
        let meeting = hub.hosted_meeting_for(request.conversation, me)?;
        Some(view! {
          <div class="call-host">
            <button class="btn btn-small" aria-expanded=move || host_open.get().to_string() on:click=move |_| host_open.update(|open| *open = !*open)>
              {move || i18n.t("host-controls")}
            </button>
            <Show when=move || host_open.get() fallback=|| ()>
              <crate::components::meeting_controls::HostControls meeting=meeting conversation=request.conversation />
            </Show>
          </div>
        })
      }}
      <crate::components::add_to_call::CallMoveBanner />
      <NotetakerBanner conversation=request.conversation notetaker=notetaker is_asked=is_notetaker_asked />
      <div class="call-body">
        <div class="call-stage-wrap">{stage}</div>
        <Show when=move || chat.is_open.get() fallback=|| ()>
          <CallChatPanel chat=chat />
        </Show>
        <CallChatToast chat=chat />
      </div>
      <Show when=move || share_open.get() fallback=|| ()>
        <div class="effects-popover share-popover" role="dialog" aria-label=move || i18n.t("share-options")>
          <h2>{move || i18n.t("share-options")}</h2>
          <label class="checkbox">
            <input
              type="checkbox"
              prop:checked=move || call.share_sound.get()
              on:change=move |ev| set_share_sound(event_target_checked(&ev))
            />
            <span>
              {move || i18n.t("share-sound")}
              <small class="muted-text">{move || i18n.t("share-sound-hint")}</small>
            </span>
          </label>
          <label class="checkbox">
            <input
              type="checkbox"
              prop:checked=move || call.screen_mode.get() == ScreenContentMode::Motion
              on:change=move |ev| {
                set_screen_mode(if event_target_checked(&ev) { ScreenContentMode::Motion } else { ScreenContentMode::Detail })
              }
            />
            <span>
              {move || i18n.t("share-motion")}
              <small class="muted-text">{move || i18n.t("share-motion-hint")}</small>
            </span>
          </label>
          <Show when=move || can_share_uhd.get() fallback=|| ()>
            <label class="checkbox">
              <input
                type="checkbox"
                prop:checked=move || call.screen_resolution.get() == ScreenShareResolution::Uhd
                on:change=move |ev| {
                  set_screen_resolution(if event_target_checked(&ev) {
                    ScreenShareResolution::Uhd
                  } else {
                    ScreenShareResolution::Standard
                  })
                }
              />
              <span>
                {move || i18n.t("share-uhd")}
                <small class="muted-text">{move || i18n.t("share-uhd-hint")}</small>
              </span>
            </label>
          </Show>
          <button
            class="btn btn-primary"
            on:click=move |_| {
              share_open.set(false);
              call.set_screen_share(true);
            }
          >
            {move || i18n.t("share-choose")}
          </button>
        </div>
      </Show>
      <Show when=move || effects_open.get() fallback=|| ()>
        <div class="effects-popover" role="dialog" aria-label=move || i18n.t("call-effects")>
          <h2>{move || i18n.t("call-effects")}</h2>
          <BackgroundPicker effect=call.effect size=PickerSize::Compact />
        </div>
      </Show>
      {move || call.maybe_quality_menu.get().map(|stream_id| view! { <VideoQualityMenu stream_id=stream_id /> })}
      <Show when=move || has_audio_menu.get() && call.is_audio_menu_open.get() fallback=|| ()>
        <AudioMenu />
      </Show>
      <Show when=move || call.is_camera_menu_open.get() fallback=|| ()>
        <div class="effects-popover camera-popover" role="dialog" aria-label=move || i18n.t("control-camera-choose")>
          <h2 id="camera-menu-title">{move || i18n.t("prejoin-camera")}</h2>
          <div class="camera-options" role="radiogroup" aria-labelledby="camera-menu-title">
            {move || {
              let listed = cameras.get();
              if listed.is_empty() {
                return view! { <p class="muted-text">{i18n.t("prejoin-no-camera")}</p> }.into_any();
              }
              let default_id = listed[0].id.clone();
              listed
                .into_iter()
                .map(|camera| camera_option(camera, &default_id, call.camera_id, pick_camera))
                .collect_view()
                .into_any()
            }}
          </div>
        </div>
      </Show>
      <footer class="call-controls" role="toolbar" aria-label=move || i18n.t("call-controls-label")>
        // The microphone with its menu of microphones and speakers (#986).
        <div class="control-split">
          <ControlButton
            is_off=Signal::derive(move || !call.is_mic_on.get())
            icon=Signal::derive(move || if call.is_mic_on.get() { IconName::Mic } else { IconName::MicOff })
            label=Signal::derive(move || if call.is_mic_on.get() { i18n.t("control-mic-mute") } else { i18n.t("control-mic-unmute") })
            on_press=move || call.set_muted(call.is_mic_on.get_untracked())
          />
          <Show when=move || has_audio_menu.get() fallback=|| ()>
            <button
              class="control-btn control-menu-btn"
              class:active=move || call.is_audio_menu_open.get()
              aria-label=move || i18n.t("control-audio-choose")
              title=move || i18n.t("control-audio-choose")
              aria-haspopup="dialog"
              aria-expanded=move || call.is_audio_menu_open.get().to_string()
              on:click=move |_| call.is_audio_menu_open.update(|open| *open = !*open)
            >
              <Icon name=IconName::ChevronDown class="is-flipped" />
            </button>
          </Show>
        </div>
        <Show when=move || is_real fallback=|| ()>
          <ControlButton
            is_off=Signal::derive(move || call.is_deafened.get())
            icon=IconName::Headphones
            label=Signal::derive(move || if call.is_deafened.get() { i18n.t("control-undeafen") } else { i18n.t("control-deafen") })
            on_press=move || call.set_deafened(!call.is_deafened.get_untracked())
          />
        </Show>
        <ControlButton
          is_off=Signal::derive(move || !call.is_camera_on.get())
          icon=Signal::derive(move || if call.is_camera_on.get() { IconName::Video } else { IconName::VideoOff })
          label=Signal::derive(move || if call.is_camera_on.get() { i18n.t("control-camera-off") } else { i18n.t("control-camera-on") })
          on_press=move || call.set_camera(!call.is_camera_on.get_untracked())
        />
        <Show when=move || has_camera_flip.get() fallback=|| ()>
          <ControlButton
            icon=IconName::SwitchCamera
            label=Signal::derive(move || i18n.t("control-camera-flip"))
            on_press=move || {
              cameras.with_untracked(|cameras| call.flip_camera(cameras));
              let id = call.camera_id.get_untracked();
              state.settings.prefs.update(|prefs| prefs.calls.camera_id = id);
            }
          />
        </Show>
        <Show when=move || has_camera_menu.get() fallback=|| ()>
          <ControlButton
            is_active=call.is_camera_menu_open
            icon=IconName::SwitchCamera
            label=Signal::derive(move || i18n.t("control-camera-choose"))
            on_press=move || call.is_camera_menu_open.update(|open| *open = !*open)
          />
        </Show>
        <Show when=can_share_screen fallback=|| ()>
        <ControlButton
          is_active=Signal::derive(move || call.is_sharing_screen.get() || share_open.get())
          icon=IconName::Monitor
          label=Signal::derive(move || if call.is_sharing_screen.get() { i18n.t("share-stop") } else { i18n.t("share-start") })
          on_press=move || {
            if call.is_sharing_screen.get_untracked() {
              call.set_screen_share(false);
            } else {
              // What to share comes from the system picker; the options come first.
              effects_open.set(false);
              call.is_camera_menu_open.set(false);
              call.maybe_quality_menu.set(None);
              call.is_audio_menu_open.set(false);
              share_open.update(|open| *open = !*open);
            }
          }
        />
        </Show>
        // Raising a hand has no wire message yet; it stays a demo-only control.
        <Show when=move || !is_real fallback=|| ()>
          <ControlButton
            is_active=Signal::derive(move || call.has_hand_raised.get())
            icon=IconName::Hand
            label=Signal::derive(move || if call.has_hand_raised.get() { i18n.t("call-hand-lower") } else { i18n.t("call-hand-raise") })
            on_press=move || call.has_hand_raised.update(|on| *on = !*on)
          />
        </Show>
        {has_effects.then(|| view! {
          <ControlButton
            is_active=Signal::derive(move || effects_open.get())
            icon=IconName::Image
            label=Signal::derive(move || i18n.t("call-effects"))
            on_press=move || {
              share_open.set(false);
              call.is_camera_menu_open.set(false);
              call.maybe_quality_menu.set(None);
              call.is_audio_menu_open.set(false);
              effects_open.update(|open| *open = !*open);
            }
          />
        })}
        <ControlButton
          icon=Signal::derive(move || if call.layout.get() == CallLayout::Gallery { IconName::Spotlight } else { IconName::Grid })
          label=Signal::derive(move || {
            if call.layout.get() == CallLayout::Gallery { i18n.t("call-layout-speaker") } else { i18n.t("call-layout-gallery") }
          })
          on_press=move || {
            call
              .layout
              .update(|layout| {
                *layout = if *layout == CallLayout::Gallery { CallLayout::ActiveSpeaker } else { CallLayout::Gallery };
              })
          }
        />
        // A DM doesn't take more people: they go into a meeting room the call moves to.
        <Show when=move || can_add_people.get() fallback=|| ()>
          <ControlButton
            icon=IconName::UserPlus
            label=Signal::derive(move || i18n.t("call-add-people"))
            on_press=move || state.overlay.set(Overlay::AddToCall(request.conversation))
          />
        </Show>
        // The notetaker joins only where it is a member already (e2ee.md §8.2).
        <Show
          when=move || notetaker.get().is_some_and(|notetaker| notetaker.can_invite) && !is_notetaker_asked.get()
          fallback=|| ()
        >
          <ControlButton
            icon=IconName::Compose
            label=Signal::derive(move || i18n.t("call-notetaker-add"))
            on_press=move || ask_notetaker_in(request.conversation, is_notetaker_asked)
          />
        </Show>
        // A phone's control bar keeps to what a call needs.
        <Show when=move || has_video_stats && !state.is_compact() fallback=|| ()>
          <ControlButton
            is_active=call.shows_video_stats
            icon=IconName::Info
            label=Signal::derive(move || i18n.t("stats-title"))
            on_press=move || video_stats.toggle()
          />
        </Show>
        <ControlButton
          is_active=chat.is_open
          icon=IconName::Chat
          label=Signal::derive(move || match (chat.is_open.get(), chat.badge()) {
            (true, _) => i18n.t("call-chat-hide"),
            (false, 0) => i18n.t("call-chat-show"),
            (false, unread) => i18n.t_count("call-chat-show-unread", unread as usize),
          })
          on_press=move || chat.toggle()
        >
          <CountBadge count=Signal::derive(move || chat.badge()) tone=Signal::derive(move || chat.badge_tone()) is_decorative=true maybe_cap=99 />
        </ControlButton>
        <ControlButton
          icon=IconName::Minimize
          label=Signal::derive(move || i18n.t("call-minimize"))
          on_press=move || call.is_minimized.set(true)
        />
        <button class="leave-btn" on:click=move |_| call.leave()>
          <Icon name=IconName::Phone class="hangup" />
          <span>{move || i18n.t("call-leave")}</span>
        </button>
      </footer>
    </section>
  }
}

fn me_name(state: crate::state::AppState) -> String {
  state.me().display_name
}

/// One camera in the call's camera menu, checked while it is the one in use (nothing chosen: the
/// default, `default_id`).
fn camera_option(
  camera: MediaDevice,
  default_id: &str,
  chosen: RwSignal<String>,
  pick: impl Fn(String) + Copy + 'static,
) -> impl IntoView {
  let id = camera.id;
  let is_default = id == default_id;
  let is_chosen = {
    let id = id.clone();
    Signal::derive(move || chosen.with(|chosen| *chosen == id || (chosen.is_empty() && is_default)))
  };
  view! {
    <button
      type="button"
      role="radio"
      class="camera-option"
      class:chosen=move || is_chosen.get()
      aria-checked=move || is_chosen.get().to_string()
      on:click=move |_| pick(id.clone())
    >
      <span class="camera-option-name">{camera.label}</span>
      <Show when=move || is_chosen.get() fallback=|| ()>
        <Icon name=IconName::Check />
      </Show>
    </button>
  }
}

#[component]
fn ControlButton(
  #[prop(into)] icon: Signal<IconName>,
  #[prop(into)] label: Signal<String>,
  on_press: impl Fn() + 'static,
  #[prop(into, default = Signal::stored(false))] is_off: Signal<bool>,
  #[prop(into, default = Signal::stored(false))] is_active: Signal<bool>,
  /// A badge on the button (the chat's unread count).
  #[prop(optional)]
  children: Option<Children>,
) -> impl IntoView {
  view! {
    <button
      class="control-btn"
      class:off=move || is_off.get()
      class:active=move || is_active.get()
      aria-label=move || label.get()
      title=move || label.get()
      aria-pressed=move || (is_off.get() || is_active.get()).to_string()
      on:click=move |_| on_press()
    >
      <Icon name=icon />
      {children.map(|children| children())}
    </button>
  }
}
