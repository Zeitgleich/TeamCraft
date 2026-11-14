//! A meeting guest's camera, microphone and speakers before a host lets them in (#1928): the
//! pre-join screen ([`PreJoinView`], the same one members get) with the guest's name under the
//! preview; "Join now" knocks. While the guest waits, a small preview stays and the settings stay
//! changeable. The guest has no account yet, so no backend: the check runs on the page's call
//! module alone, which then goes on into the call with the guest's choices (admission joins with
//! them, no second screen or prompt).
//!
//! The views load with the call screens' module (`areas.rs`), which the meeting needs next anyway.

use std::rc::Rc;

use leptos::ev::SubmitEvent;
use leptos::prelude::*;

use crate::calls::CallState;
use crate::components::call_prejoin::{
  AudioOptions, DeviceAccess, DeviceCheck, DeviceFeed, DevicePreview, PageDevices, PreJoinView, VideoOptions,
};
use crate::components::guest_join::GuestNameField;
use crate::components::settings_tabs::apply_call_prefs;
use crate::form_errors::FormErrors;
use crate::i18n::use_i18n;
use crate::model::{DeviceSetup, PresenceStatus, User, id_from_raw};
use crate::remote_calls::RemoteCalls;
use crate::settings::Settings;

/// The id of the guest's name form; "Join now" submits it, its errors focus it.
pub const FORM_ID: &str = "guest-join-form";
/// The avatar colour of a guest who has no account yet.
const GUEST_HUE: u16 = 210;

/// The page's call module and the guest's choices, from the first screen into the call.
#[derive(Clone)]
pub struct GuestDevices {
  pub calls: RemoteCalls,
  pub call: CallState,
  pub settings: Settings,
  revision: RwSignal<u64>,
  /// Whether the guest turned the camera and microphone on: the browser asks only then, after
  /// the page said why.
  access: RwSignal<DeviceAccess>,
}

/// The name form of the first screen, kept by the join page.
#[derive(Clone)]
pub struct GuestForm {
  pub devices: GuestDevices,
  pub name: RwSignal<String>,
  pub errors: FormErrors,
  pub is_joining: Signal<bool>,
  pub submit: StoredValue<Rc<dyn Fn(SubmitEvent)>, LocalStorage>,
  pub maybe_sign_in: Option<String>,
}

impl GuestDevices {
  /// The page's call module, when this browser can make calls; the guest's last choices from this
  /// browser apply (start muted, background, devices).
  pub fn for_page() -> Option<Self> {
    let calls = page_calls()?;
    let call = CallState::new();
    let settings = Settings::load();
    apply_call_prefs(call, None, &settings.prefs.get_untracked().calls);
    let revision = RwSignal::new(0_u64);
    calls.set_change_notifier(Rc::new(move || {
      // Gone with the page: nothing left to read.
      let _ = revision.try_update(|revision| *revision = revision.wrapping_add(1));
    }));
    let handler_calls = calls.clone();
    call.set_handler(move |event| handler_calls.device_event(event));
    Some(Self {
      calls,
      call,
      settings,
      revision,
      access: RwSignal::new(DeviceAccess::NotAsked),
    })
  }

  fn start_check(&self) -> DeviceCheck {
    let feed = DeviceFeed::Page(PageDevices {
      calls: self.calls.clone(),
      revision: self.revision,
    });
    DeviceCheck::start(feed, self.call, self.settings, Some(self.access))
  }
}

/// The first screen: the pre-join screen with the guest's name under the preview.
#[component]
pub fn GuestDeviceSetup(form: GuestForm) -> impl IntoView {
  let i18n = use_i18n();
  let GuestForm {
    devices,
    name,
    errors,
    is_joining,
    submit,
    maybe_sign_in,
  } = form;
  let check = devices.start_check();
  let user = Signal::derive(move || guest_user(&name.get()));
  let maybe_sign_in = StoredValue::new(maybe_sign_in);

  view! {
    <div class="guest-prejoin">
      <PreJoinView
        check=check
        user=user
        title=Signal::derive(move || i18n.t("guest-title"))
        identity=ViewFn::from(move || view! {
          <form id=FORM_ID class="prejoin-identity" novalidate on:submit=move |ev| submit.with_value(|submit| submit(ev))>
            <GuestNameField name=name errors=errors />
            <p class="muted-text small">{move || i18n.t("guest-subtitle")}</p>
          </form>
        })
        status=ViewFn::from(move || {
          maybe_sign_in.get_value().map(|href| view! {
            <p class="muted-text small">
              <a href=href>{move || i18n.t("guest-sign-in")}</a>
            </p>
          })
        })
        actions=ViewFn::from(move || view! {
          <button type="submit" form=FORM_ID class="btn btn-primary" prop:disabled=move || is_joining.get()>
            {move || i18n.t("prejoin-join")}
          </button>
        })
      />
    </div>
  }
}

/// While waiting in the lobby: a small preview, and the video and audio options behind a
/// disclosure.
#[component]
pub fn GuestWaitingDevices(devices: GuestDevices, #[prop(into)] name: Signal<String>) -> impl IntoView {
  let i18n = use_i18n();
  let check = devices.start_check();
  let user = Signal::derive(move || guest_user(&name.get()));
  view! {
    <div class="guest-waiting-devices">
      <DevicePreview check=check user=user />
      <details class="guest-device-settings">
        <summary>{move || i18n.t("guest-devices-settings")}</summary>
        <div class="guest-device-fields">
          <VideoOptions check=check />
          <AudioOptions check=check />
        </div>
      </details>
    </div>
  }
}

/// The guest as their own tile shows them before they have an account: the name they typed.
fn guest_user(name: &str) -> User {
  User {
    id: id_from_raw(0),
    username: String::new(),
    display_name: name.trim().to_owned(),
    presence: PresenceStatus::Online,
    maybe_custom_status: None,
    avatar_hue: GUEST_HUE,
    is_guest: true,
    is_bot: false,
    maybe_organization: None,
    device_setup: DeviceSetup::default(),
    is_deactivated: false,
    is_deleted: false,
    maybe_email: None,
  }
}

/// The browser runs the call media in the page; the core that signs the call's keys starts once
/// the guest knocked, before a join needs it.
#[cfg(target_arch = "wasm32")]
fn page_calls() -> Option<RemoteCalls> {
  match crate::web_calls::browser_support() {
    Ok(()) => Some(RemoteCalls::in_page(crate::web_calls::WebCallHost::new())),
    Err(reason) => {
      tracing::info!(?reason, "no calls in this browser");
      None
    },
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn page_calls() -> Option<RemoteCalls> {
  None
}
