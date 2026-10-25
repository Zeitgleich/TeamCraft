use std::cell::Cell;

use leptos::prelude::*;

use teamcraft_proto::VideoTrackKind;

use crate::calls::{
  CallKind, CallLayout, CallRequest, Participant, ScreenContentMode, ScreenShareSound,
  selected_camera_is_rear,
};
use crate::components::call_prejoin::call_heading;
use crate::components::call_video::{
  BackgroundPicker, LiveVideo, VideoSource, VideoTile, can_share_screen, can_show_video,
};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::{User, UserId};
use crate::reactive::use_interval;
use crate::state::{Overlay, use_app_state};

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
  let is_share_layout = move || call.is_sharing_screen.get() || remote_sharer.get().is_some();

  // Real calls report who is speaking; the demo rotates the active speaker among people who are not
  // muted.
  let is_real = !untrack(|| state.read(|backend| backend.has_simulated_calls()));
  let has_effects = untrack(|| state.read(|backend| backend.has_background_effects()));
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
      speaking.set(loudest);
    }
  });
  let privacy_code = Memo::new(move |_| {
    state.read(|backend| backend.active_call().and_then(|active| active.maybe_privacy_code))
  });
  let is_connecting = Memo::new(move |_| is_real && state.read(|backend| backend.active_call().is_none()));
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
      speaking.set(None);
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
  let remote_tile = move |tile: TileData, is_large: bool| {
    let participant = tile.participant.unwrap_or(Participant {
      user: tile.user.id,
      is_muted: false,
      is_camera_on: true,
      has_hand_raised: false,
      is_sharing_screen: false,
    });
    let id = tile.user.id;
    view! {
      <VideoTile
        user=tile.user
        is_large=is_large
        is_muted=participant.is_muted
        is_camera_on=participant.is_camera_on
        has_hand_raised=participant.has_hand_raised
        is_speaking=Signal::derive(move || speaking.get() == Some(id))
        is_reconnecting=Signal::derive(move || reconnecting.get().contains(&id))
        video=Memo::new(move |_| track_of(id, VideoTrackKind::Camera))
      />
    }
  };

  let stage = move || {
    let remote_tiles = remote.get();
    if is_share_layout() {
      let maybe_sharer = remote_sharer.get();
      let maybe_screen = maybe_sharer
        .as_ref()
        .and_then(|tile| track_of(tile.user.id, VideoTrackKind::Screen));
      let sharer_name = match maybe_sharer {
        Some(tile) => tile.user.display_name,
        None => String::new(),
      };
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
        let focus = speaking
          .get()
          .and_then(|id| remote_tiles.iter().position(|tile| tile.user.id == id))
          .unwrap_or(0);
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
        <span class="e2ee-pill" title=move || i18n.t("call-encrypted-detail")>
          <Icon name=IconName::Shield />
          <span>{move || i18n.t("call-encrypted")}</span>
          {move || {
            let code = if is_real { privacy_code.get() } else { Some("48 20 17".to_owned()) };
            code.map(|code| view! { <span class="mono" translate="no">{i18n.t_with("call-verification-code", &[("code", &code)])}</span> })
          }}
        </span>
        <Show when=move || is_connecting.get() fallback=|| ()>
          <span class="muted-text" role="status">{move || i18n.t("call-connecting")}</span>
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
      <div class="call-stage-wrap">{stage}</div>
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
          <BackgroundPicker effect=call.effect />
        </div>
      </Show>
      <footer class="call-controls" role="toolbar" aria-label=move || i18n.t("call-controls-label")>
        <ControlButton
          is_off=Signal::derive(move || !call.is_mic_on.get())
          icon=Signal::derive(move || if call.is_mic_on.get() { IconName::Mic } else { IconName::MicOff })
          label=Signal::derive(move || if call.is_mic_on.get() { i18n.t("control-mic-mute") } else { i18n.t("control-mic-unmute") })
          on_press=move || call.set_muted(call.is_mic_on.get_untracked())
        />
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
        <Show when=move || cameras.with(|cameras| cameras.len() > 1) fallback=|| ()>
          <ControlButton
            icon=IconName::SwitchCamera
            label=Signal::derive(move || i18n.t("control-camera-flip"))
            on_press=move || cameras.with_untracked(|cameras| call.flip_camera(cameras))
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

#[component]
fn ControlButton(
  #[prop(into)] icon: Signal<IconName>,
  #[prop(into)] label: Signal<String>,
  on_press: impl Fn() + 'static,
  #[prop(into, default = Signal::stored(false))] is_off: Signal<bool>,
  #[prop(into, default = Signal::stored(false))] is_active: Signal<bool>,
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
    </button>
  }
}
