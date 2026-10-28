use std::cell::Cell;
use std::rc::Rc;

use leptos::ev;
use leptos::prelude::*;

use crate::calls::{
  CallEvent, CallKind, CallPreview, CallRequest, DevicePresence, MediaDevice, PreviewCamera,
  selected_camera_is_rear,
};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::call_video::{
  BackgroundPicker, PickerSize, VideoSource, VideoTile, can_choose_speakers, can_show_video,
};
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::meetings::MeetingsHub;
use crate::model::User;
use crate::reactive::{is_document_visible, use_interval_while, use_page_visible};
use crate::state::use_app_state;

const METER_BARS: usize = 14;
/// How often the meter reads the level: about eight times a second, smooth enough for speech.
const METER_TICK_MS: u64 = 120;
/// How long the speaker button says "playing": the test sound (1.4 s) and a little start-up.
const SPEAKER_TEST_MS: u64 = 1800;
/// The camera field, which "Choose camera" on a camera error focuses.
const CAMERA_FIELD_ID: &str = "prejoin-camera";

/// Lobby before a call: check camera, microphone and speaker, pick a background, then join.
#[component]
pub fn PreJoinScreen(request: CallRequest) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let me = state.me();
  let maybe_meetings = use_context::<MeetingsHub>();
  let conversation = Memo::new(move |_| state.read(|backend| backend.conversation(request.conversation)));
  // Browsers name their devices only once the page may use them, so the lists can fill in later.
  let devices = Memo::new(move |_| state.read(|backend| backend.media_devices()));
  let camera = Memo::new(move |_| {
    devices.with(|devices| InputOffer::new(devices.cameras.clone(), devices.camera_presence))
  });
  let microphone = Memo::new(move |_| {
    devices.with(|devices| InputOffer::new(devices.microphones.clone(), devices.microphone_presence))
  });
  let speakers = Memo::new(move |_| {
    devices.with(|devices| InputOffer::new(devices.speakers.clone(), DevicePresence::Unknown))
  });
  let has_effects = untrack(|| state.read(|backend| backend.effects_offer().has_preview()));
  let participants = Memo::new(move |_| {
    state
      .read(|backend| backend.call_participants(request.conversation))
      .len()
  });

  let is_real = !untrack(|| state.read(|backend| backend.has_simulated_calls()));

  // The lists may be old: the desktop app lists on request, and a camera may have been plugged in
  // or a permission changed since. Browsers also report changes themselves (`devicechange`).
  call.emit(CallEvent::RefreshDevices);
  // Coming back to the window (after allowing the camera in the address bar or the system
  // settings, which fires no `devicechange`) lists them again, while this screen is open.
  let refresh_on_return = [
    window_event_listener(ev::focus, move |_| call.emit(CallEvent::RefreshDevices)),
    window_event_listener_untyped("visibilitychange", move |_| {
      if is_document_visible() {
        call.emit(CallEvent::RefreshDevices);
      }
    }),
  ];
  on_cleanup(move || refresh_on_return.into_iter().for_each(|handle| handle.remove()));

  // Show the first device of each kind when nothing was chosen before: the one the system uses.
  Effect::new(move |_| {
    let devices = devices.get();
    for (signal, list) in [
      (call.microphone_id, &devices.microphones),
      (call.speaker_id, &devices.speakers),
    ] {
      if signal.get_untracked().is_empty()
        && let Some(first) = list.first()
      {
        signal.set(first.id.clone());
      }
    }
    // The desktop lists the camera it opens by default first (#819); telling it again would only
    // restart the check's camera.
    if call.camera_id.get_untracked().is_empty()
      && let Some(first) = devices.cameras.first()
    {
      call.show_camera_in_use(first.id.clone());
    }
  });
  // "Choose camera" on a camera error: the camera field takes the focus.
  Effect::new(move |_| {
    if call.is_camera_menu_open.get() {
      call.is_camera_menu_open.set(false);
      focus_camera_field();
    }
  });
  let pick_camera = move |id: String| {
    call.choose_camera(id.clone());
    state.settings.prefs.update(|prefs| prefs.calls.camera_id = id);
  };
  keep_off_while_unavailable(camera, call.is_camera_on, move |is_on| call.set_camera(is_on));
  keep_off_while_unavailable(microphone, call.is_mic_on, move |is_on| call.set_muted(!is_on));

  // #442: the real camera (as the call would send it) and the microphone's level, on this device
  // only. The check runs while the screen is visible and ends with it; a join takes the camera
  // and microphone over.
  let is_page_visible = use_page_visible();
  if is_real {
    let preview = Memo::new(move |_| CallPreview {
      is_camera_on: is_page_visible.get() && call.is_camera_on.get() && camera.with(InputOffer::is_usable),
      is_mic_on: is_page_visible.get() && call.is_mic_on.get() && microphone.with(InputOffer::is_usable),
    });
    Effect::new(move |_| call.emit(CallEvent::Preview(preview.get())));
    on_cleanup(move || call.emit(CallEvent::Preview(CallPreview::default())));
  }
  let preview_camera = Memo::new(move |_| state.read(|backend| backend.call_preview_camera()));

  let title = move || {
    let maybe_meeting_title = maybe_meetings.and_then(|hub| hub.title_for(request.conversation));
    let name = conversation.get().map(|conv| conv.name).unwrap_or_default();
    call_heading(i18n, request.kind, maybe_meeting_title.as_deref(), &name)
  };

  // The microphone's level while it is on and the screen visible: measured on this device, or
  // made up in the demo. A muted microphone shows an empty meter.
  let level = RwSignal::new(0_usize);
  let tick = Rc::new(Cell::new(7_u32));
  use_interval_while(
    METER_TICK_MS,
    move || is_page_visible.get() && call.is_mic_on.get(),
    move || {
      let bars = if is_real {
        meter_bars(state.read(|backend| backend.call_preview_level()))
      } else {
        let next = tick.get().wrapping_mul(1_103_515_245).wrapping_add(12_345);
        tick.set(next);
        (next >> 16) as usize % (METER_BARS - 2) + 2
      };
      level.set(bars);
    },
  );
  Effect::new(move |_| {
    if !call.is_mic_on.get() {
      level.set(0);
    }
  });

  let is_testing_speaker = RwSignal::new(false);
  let test_speaker = move |_| {
    is_testing_speaker.set(true);
    // Browsers start sound only inside the click: the context is made here, before any `await`.
    #[cfg(target_arch = "wasm32")]
    if crate::web_calls::is_in_page() {
      crate::speaker_test::prime();
    }
    // The backend plays the sound and reports a failure (`feedback::report_failure`).
    call.emit(CallEvent::TestSpeakers(call.speaker_id.get_untracked()));
    set_timeout(
      move || {
        // The pre-join screen may be gone by then; `try_set` ignores a disposed signal.
        let _ = is_testing_speaker.try_set(false);
      },
      std::time::Duration::from_millis(SPEAKER_TEST_MS),
    );
  };

  // Real calls show the camera once its first picture arrived (who joins until then); the demo
  // draws a pretend camera image.
  let is_live = move || is_real && preview_camera.get() == PreviewCamera::Live && can_show_video();
  let shows_camera =
    move || call.is_camera_on.get() && camera.with(InputOffer::is_usable) && (!is_real || is_live());
  let own_video = Memo::new(move |_| is_live().then_some(VideoSource::OwnCamera));
  let camera_status = Signal::derive(move || {
    camera
      .with(|offer| offer.unavailable_key(InputKind::Camera))
      .unwrap_or_else(|| {
        let check = if is_real && can_show_video() {
          Some(preview_camera.get())
        } else {
          None
        };
        camera_status_key(call.is_camera_on.get(), check)
      })
  });
  let preview_me = me.clone();

  view! {
    <section class="prejoin" aria-labelledby="prejoin-title">
      <div class="prejoin-preview">
        <Show
          when=shows_camera
          fallback=move || view! {
            <SelfPreview user=preview_me.clone() is_muted=Signal::derive(move || !call.is_mic_on.get()) status_key=camera_status />
          }
        >
          <VideoTile
            user=me.clone()
            is_self=true
            is_large=true
            is_muted=Signal::derive(move || !call.is_mic_on.get())
            is_camera_on=call.is_camera_on
            effect=call.effect
            is_mirrored=Signal::derive(move || {
              call.mirror_self_view.get() && !devices.with(|devices| selected_camera_is_rear(&devices.cameras, &call.camera_id.get()))
            })
            video=own_video
          />
        </Show>
        <div class="preview-controls" role="group" aria-label=move || i18n.t("prejoin-quick-controls")>
          <button
            class="round-btn"
            class:off=move || !call.is_mic_on.get()
            disabled=move || !microphone.with(InputOffer::is_usable)
            title=move || microphone.with(|offer| offer.hint_key(InputKind::Microphone)).map(|key| i18n.t(key))
            aria-pressed=move || (!call.is_mic_on.get()).to_string()
            aria-label=move || if call.is_mic_on.get() { i18n.t("control-mic-mute") } else { i18n.t("control-mic-unmute") }
            on:click=move |_| call.set_muted(call.is_mic_on.get_untracked())
          >
            <Icon name=Signal::derive(move || if call.is_mic_on.get() { IconName::Mic } else { IconName::MicOff }) />
          </button>
          <Show when=move || devices.with(|devices| devices.cameras.len() > 1) fallback=|| ()>
            <button
              class="round-btn"
              aria-label=move || i18n.t("control-camera-flip")
              title=move || i18n.t("control-camera-flip")
              on:click=move |_| {
                devices.with_untracked(|devices| call.flip_camera(&devices.cameras));
                let id = call.camera_id.get_untracked();
                state.settings.prefs.update(|prefs| prefs.calls.camera_id = id);
              }
            >
              <Icon name=IconName::SwitchCamera />
            </button>
          </Show>
          <button
            class="round-btn"
            class:off=move || !call.is_camera_on.get()
            disabled=move || !camera.with(InputOffer::is_usable)
            title=move || camera.with(|offer| offer.hint_key(InputKind::Camera)).map(|key| i18n.t(key))
            aria-pressed=move || (!call.is_camera_on.get()).to_string()
            aria-label=move || if call.is_camera_on.get() { i18n.t("control-camera-off") } else { i18n.t("control-camera-on") }
            on:click=move |_| call.set_camera(!call.is_camera_on.get_untracked())
          >
            <Icon name=Signal::derive(move || if call.is_camera_on.get() { IconName::Video } else { IconName::VideoOff }) />
          </button>
        </div>
      </div>
      <div class="prejoin-panel">
        <h1 id="prejoin-title">{title}</h1>
        <p class="muted-text">
          {move || {
            let count = participants.get();
            if request.kind == CallKind::Direct {
              i18n.t("prejoin-direct-hint")
            } else if count == 0 {
              i18n.t("prejoin-nobody")
            } else {
              i18n.t_count("prejoin-people-in-call", count)
            }
          }}
        </p>
        <div class="field">
          <label for=CAMERA_FIELD_ID>{move || i18n.t("prejoin-camera")}</label>
          <DeviceSelect id=CAMERA_FIELD_ID kind=InputKind::Camera offer=camera selected=call.camera_id on_pick=Callback::new(pick_camera) />
        </div>
        <div class="field">
          <label for="prejoin-mic">{move || i18n.t("prejoin-microphone")}</label>
          <DeviceSelect id="prejoin-mic" kind=InputKind::Microphone offer=microphone selected=call.microphone_id on_pick=Callback::new(move |id: String| call.emit(CallEvent::Microphone(id))) />
          <Show when=move || microphone.with(InputOffer::is_usable) fallback=|| ()>
          <div
            class="meter"
            role="meter"
            aria-label=move || i18n.t("prejoin-mic-level")
            aria-valuemin="0"
            aria-valuemax=METER_BARS.to_string()
            aria-valuenow=move || level.get().to_string()
          >
            {(0..METER_BARS)
              .map(|bar| view! { <span class="meter-bar" class:lit=move || bar < level.get()></span> })
              .collect_view()}
          </div>
          </Show>
        </div>
        <div class="field">
          // A browser that can't route sound (Safari) has no choice to offer; the test still plays
          // on the system's output.
          {if can_choose_speakers() {
            view! { <label for="prejoin-speaker">{move || i18n.t("prejoin-speaker")}</label> }.into_any()
          } else {
            view! { <span class="field-title">{move || i18n.t("prejoin-speaker")}</span> }.into_any()
          }}
          <div class="field-row">
            {can_choose_speakers().then(|| view! {
              <DeviceSelect id="prejoin-speaker" kind=InputKind::Speakers offer=speakers selected=call.speaker_id on_pick=Callback::new(move |id: String| call.emit(CallEvent::Speakers(id))) />
            })}
            <button type="button" class="btn" on:click=test_speaker disabled=move || is_testing_speaker.get()>
              <Icon name=IconName::Volume />
              <span>
                {move || if is_testing_speaker.get() { i18n.t("prejoin-testing-speaker") } else { i18n.t("prejoin-test-speaker") }}
              </span>
            </button>
          </div>
        </div>
        {has_effects.then(|| view! {
          <div class="field">
            <span class="field-title" id="prejoin-effects-title">{move || i18n.t("prejoin-effects")}</span>
            <BackgroundPicker effect=call.effect size=PickerSize::Compact />
            <p class="muted-text small">{move || i18n.t("prejoin-effects-hint")}</p>
          </div>
        })}
        <label class="checkbox">
          <input
            type="checkbox"
            prop:checked=move || call.mirror_self_view.get()
            on:change=move |ev| call.mirror_self_view.set(event_target_checked(&ev))
          />
          <span>{move || i18n.t("prejoin-mirror")}</span>
        </label>
        <p class="e2ee-note">
          <Icon name=IconName::Shield />
          <span>{move || i18n.t("call-encrypted")}</span>
        </p>
        <div class="prejoin-actions">
          <button class="btn" on:click=move |_| call.leave()>
            {move || i18n.t("cancel")}
          </button>
          <button class="btn btn-primary" data-autofocus on:click=move |_| call.enter()>
            {move || i18n.t("prejoin-join")}
          </button>
        </div>
      </div>
    </section>
  }
}

/// The heading of the pre-join and call screens: the meeting's title, else the conversation's
/// name in a sentence, else a plain "Call" (a conversation whose name isn't known yet).
pub fn call_heading(
  i18n: I18n,
  kind: CallKind,
  maybe_meeting_title: Option<&str>,
  conversation_name: &str,
) -> String {
  match heading(kind, maybe_meeting_title, conversation_name) {
    Heading::Title(title) => title.to_owned(),
    Heading::Named(key, name) => i18n.t_with(key, &[("name", name)]),
    Heading::Plain(key) => i18n.t(key),
  }
}

/// What the user's tile says while it shows no camera image. `maybe_check`: how far the pre-join
/// check got with the camera, where one runs (`None`: the demo, or a page that can't show the
/// camera before joining).
fn camera_status_key(is_camera_on: bool, maybe_check: Option<PreviewCamera>) -> &'static str {
  match (is_camera_on, maybe_check) {
    (false, _) => "prejoin-camera-off",
    (true, None) => "prejoin-camera-at-join",
    (true, Some(PreviewCamera::Failed)) => "prejoin-camera-failed",
    (true, Some(_)) => "prejoin-camera-starting",
  }
}

/// How many of the meter's bars a level (0.0 to 1.0) lights.
fn meter_bars(level: f32) -> usize {
  // Invariant: clamped to 0..=METER_BARS before the cast.
  (level * METER_BARS as f32).round().clamp(0.0, METER_BARS as f32) as usize
}

/// The user's tile while there is no camera image: their picture and what the camera does.
#[component]
fn SelfPreview(
  user: User,
  #[prop(into)] is_muted: Signal<bool>,
  #[prop(into)] status_key: Signal<&'static str>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div
      class="tile tile-large prejoin-self"
      role="group"
      aria-label=move || {
        let mut parts = vec![i18n.t("tile-you"), i18n.t(status_key.get())];
        if is_muted.get() {
          parts.push(i18n.t("tile-muted"));
        }
        parts.join(", ")
      }
    >
      <div class="prejoin-self-body" aria-hidden="true">
        <Avatar user=user size=AvatarSize::ExtraLarge />
        <span class="prejoin-self-status">{move || i18n.t(status_key.get())}</span>
      </div>
      <div class="tile-label">
        <span class="tile-name">{move || i18n.t("tile-you")}</span>
        <Show when=move || is_muted.get() fallback=|| ()>
          <Icon name=IconName::MicOff class="tile-badge" />
        </Show>
      </div>
    </div>
  }
}

#[component]
fn DeviceSelect(
  id: &'static str,
  kind: InputKind,
  #[prop(into)] offer: Signal<InputOffer>,
  selected: RwSignal<String>,
  on_pick: Callback<String>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <select
      id=id
      class="input grow"
      disabled=move || !offer.with(InputOffer::is_usable)
      on:change=move |ev| {
        let value = event_target_value(&ev);
        selected.set(value.clone());
        on_pick.run(value);
      }
    >
      {move || match offer.get() {
        InputOffer::Devices(devices) => devices
          .into_iter()
          .map(|device| {
            let value = device.id.clone();
            view! {
              <option value=device.id selected=move || selected.get() == value>
                {device.label}
              </option>
            }
          })
          .collect_view()
          .into_any(),
        other => {
          let key = other.unavailable_key(kind).unwrap_or("calls-system-default");
          view! { <option value="" selected=true>{i18n.t(key)}</option> }.into_any()
        },
      }}
    </select>
  }
}

fn focus_camera_field() {
  use wasm_bindgen::JsCast;

  let maybe_field = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.get_element_by_id(CAMERA_FIELD_ID))
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  if let Some(field) = maybe_field {
    // Focusing can only fail for an element that went away meanwhile.
    drop(field.focus());
  }
}

/// Turns an input off while there is none (or access is refused), and back on when one appears
/// if it was on before: plugging in a camera brings back the user's choice.
fn keep_off_while_unavailable(
  offer: Memo<InputOffer>,
  is_on: RwSignal<bool>,
  set_on: impl Fn(bool) + 'static,
) {
  let was_turned_off = StoredValue::new(false);
  Effect::new(move |_| {
    let is_usable = offer.with(InputOffer::is_usable);
    if !is_usable && is_on.get_untracked() {
      was_turned_off.set_value(true);
      set_on(false);
    } else if is_usable && was_turned_off.get_value() {
      was_turned_off.set_value(false);
      set_on(true);
    }
  });
}

/// What the pre-join screen offers for one kind of device, from the device listing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum InputOffer {
  /// Named devices to pick from.
  Devices(Vec<MediaDevice>),
  /// Devices the platform doesn't name (yet), or a listing that hasn't arrived: the default.
  SystemDefault,
  /// No device of this kind: nothing to pick, and the input stays off.
  Missing,
  /// The user or a site setting refused access: nothing to pick until they allow it.
  Blocked,
}

/// Which device a select or toggle is for, for its wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputKind {
  Camera,
  Microphone,
  Speakers,
}

/// How a call heading reads, before translation.
#[derive(Debug, PartialEq, Eq)]
enum Heading<'a> {
  Title(&'a str),
  Named(&'static str, &'a str),
  Plain(&'static str),
}

impl InputOffer {
  fn new(devices: Vec<MediaDevice>, presence: DevicePresence) -> Self {
    if !devices.is_empty() {
      return Self::Devices(devices);
    }
    match presence {
      DevicePresence::Missing => Self::Missing,
      DevicePresence::Blocked => Self::Blocked,
      DevicePresence::Unknown | DevicePresence::Present | DevicePresence::Hidden => Self::SystemDefault,
    }
  }

  fn is_usable(&self) -> bool {
    matches!(self, Self::Devices(_) | Self::SystemDefault)
  }

  /// The i18n key that says why the device can't be used; `None` while it can.
  fn unavailable_key(&self, kind: InputKind) -> Option<&'static str> {
    Some(match (self, kind) {
      (Self::Devices(_) | Self::SystemDefault, _) | (_, InputKind::Speakers) => return None,
      (Self::Missing, InputKind::Camera) => "prejoin-no-camera",
      (Self::Missing, InputKind::Microphone) => "prejoin-no-microphone",
      (Self::Blocked, InputKind::Camera) => "prejoin-camera-blocked",
      (Self::Blocked, InputKind::Microphone) => "prejoin-microphone-blocked",
    })
  }

  /// The i18n key of the toggle's tooltip: what to do to use the device; `None` while it can.
  fn hint_key(&self, kind: InputKind) -> Option<&'static str> {
    Some(match (self, kind) {
      (Self::Devices(_) | Self::SystemDefault, _) | (_, InputKind::Speakers) => return None,
      (Self::Missing, InputKind::Camera) => "prejoin-no-camera-hint",
      (Self::Missing, InputKind::Microphone) => "prejoin-no-microphone-hint",
      (Self::Blocked, InputKind::Camera) => "prejoin-camera-blocked-hint",
      (Self::Blocked, InputKind::Microphone) => "prejoin-microphone-blocked-hint",
    })
  }
}

fn heading<'a>(
  kind: CallKind,
  maybe_meeting_title: Option<&'a str>,
  conversation_name: &'a str,
) -> Heading<'a> {
  if let Some(title) = maybe_meeting_title
    .map(str::trim)
    .filter(|title| !title.is_empty())
  {
    return Heading::Title(title);
  }
  let name = conversation_name.trim();
  match (kind, name.is_empty()) {
    (CallKind::Huddle, true) => Heading::Plain("call-title-huddle-unnamed"),
    (_, true) => Heading::Plain("call-title-unnamed"),
    (CallKind::Direct, false) => Heading::Named("call-title-direct", name),
    (CallKind::Group, false) => Heading::Named("call-title-group", name),
    (CallKind::Huddle, false) => Heading::Named("call-title-huddle", name),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod devices {
    use super::*;

    #[test]
    fn a_computer_without_a_camera_offers_none_and_says_so() {
      let offer = InputOffer::new(Vec::new(), DevicePresence::Missing);
      assert_eq!(offer, InputOffer::Missing);
      assert!(!offer.is_usable());
      assert_eq!(
        offer.unavailable_key(InputKind::Camera),
        Some("prejoin-no-camera")
      );
      assert_eq!(offer.hint_key(InputKind::Camera), Some("prejoin-no-camera-hint"));
      assert_eq!(
        offer.unavailable_key(InputKind::Microphone),
        Some("prejoin-no-microphone")
      );
    }

    #[test]
    fn a_refused_device_is_blocked_not_missing() {
      let offer = InputOffer::new(Vec::new(), DevicePresence::Blocked);
      assert!(!offer.is_usable());
      assert_eq!(
        offer.unavailable_key(InputKind::Camera),
        Some("prejoin-camera-blocked")
      );
      assert_eq!(
        offer.hint_key(InputKind::Microphone),
        Some("prejoin-microphone-blocked-hint")
      );
    }

    #[test]
    fn unnamed_or_unlisted_devices_offer_the_system_default() {
      for presence in [
        DevicePresence::Hidden,
        DevicePresence::Unknown,
        DevicePresence::Present,
      ] {
        let offer = InputOffer::new(Vec::new(), presence);
        assert_eq!(offer, InputOffer::SystemDefault, "{presence:?}");
        assert!(offer.is_usable());
        assert_eq!(offer.unavailable_key(InputKind::Camera), None);
        assert_eq!(offer.hint_key(InputKind::Camera), None);
      }
    }

    #[test]
    fn named_devices_win_over_the_presence() {
      let camera = MediaDevice {
        id: "cam-1".into(),
        label: "Camera".into(),
      };
      let offer = InputOffer::new(vec![camera.clone()], DevicePresence::Missing);
      assert_eq!(offer, InputOffer::Devices(vec![camera]));
      assert!(offer.is_usable());
    }

    #[test]
    fn speakers_are_never_reported_missing() {
      assert_eq!(InputOffer::Missing.unavailable_key(InputKind::Speakers), None);
      assert_eq!(InputOffer::Blocked.hint_key(InputKind::Speakers), None);
    }

    #[test]
    fn every_key_is_translated() {
      let en = crate::i18n::joined_source(crate::i18n::Locale::En);
      let de = crate::i18n::joined_source(crate::i18n::Locale::De);
      let offers = [InputOffer::Missing, InputOffer::Blocked];
      let kinds = [InputKind::Camera, InputKind::Microphone];
      let keys = offers
        .iter()
        .flat_map(|offer| {
          kinds
            .iter()
            .flat_map(move |kind| [offer.unavailable_key(*kind), offer.hint_key(*kind)])
        })
        .flatten()
        .chain([
          "prejoin-camera-off",
          "prejoin-camera-at-join",
          "prejoin-camera-starting",
          "prejoin-camera-failed",
          "call-title-unnamed",
          "call-title-huddle-unnamed",
        ]);
      for key in keys {
        for (name, source) in [("en", &en), ("de", &de)] {
          assert!(
            source.contains(&format!("\n{key} =")),
            "{key} missing in i18n/{name}"
          );
        }
      }
    }
  }

  mod preview {
    use super::*;

    #[test]
    fn the_tile_says_how_far_the_camera_check_got() {
      assert_eq!(
        camera_status_key(false, Some(PreviewCamera::Live)),
        "prejoin-camera-off"
      );
      assert_eq!(
        camera_status_key(true, Some(PreviewCamera::Off)),
        "prejoin-camera-starting"
      );
      assert_eq!(
        camera_status_key(true, Some(PreviewCamera::Starting)),
        "prejoin-camera-starting"
      );
      assert_eq!(
        camera_status_key(true, Some(PreviewCamera::Failed)),
        "prejoin-camera-failed"
      );
      // Without a check (the camera can't be shown before joining) it starts with the call.
      assert_eq!(camera_status_key(true, None), "prejoin-camera-at-join");
    }

    #[test]
    fn the_meter_lights_bars_by_level() {
      assert_eq!(meter_bars(0.0), 0);
      assert_eq!(meter_bars(1.0), METER_BARS);
      assert_eq!(meter_bars(0.5), METER_BARS / 2);
      assert_eq!(meter_bars(-0.2), 0);
      assert_eq!(meter_bars(7.0), METER_BARS);
    }
  }

  mod headings {
    use super::*;

    #[test]
    fn a_meeting_is_called_by_its_title() {
      assert_eq!(
        heading(CallKind::Group, Some("Weekly sync"), ""),
        Heading::Title("Weekly sync")
      );
      assert_eq!(
        heading(CallKind::Group, Some("Weekly sync"), "Project A"),
        Heading::Title("Weekly sync")
      );
    }

    #[test]
    fn a_named_conversation_is_in_the_sentence() {
      assert_eq!(
        heading(CallKind::Group, None, "Project A"),
        Heading::Named("call-title-group", "Project A")
      );
      assert_eq!(
        heading(CallKind::Direct, Some("  "), "Alice"),
        Heading::Named("call-title-direct", "Alice")
      );
      assert_eq!(
        heading(CallKind::Huddle, None, "Project A"),
        Heading::Named("call-title-huddle", "Project A")
      );
    }

    #[test]
    fn without_a_name_there_is_no_dangling_preposition() {
      assert_eq!(
        heading(CallKind::Group, None, ""),
        Heading::Plain("call-title-unnamed")
      );
      assert_eq!(
        heading(CallKind::Direct, None, " "),
        Heading::Plain("call-title-unnamed")
      );
      assert_eq!(
        heading(CallKind::Huddle, None, ""),
        Heading::Plain("call-title-huddle-unnamed")
      );
    }
  }
}
