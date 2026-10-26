//! The settings sections Appearance, Preferences, Audio & video and Help & about. Preferences are
//! edited in `Settings::prefs`; see `prefs.rs` for what each value means. The other sections have
//! files of their own (`settings_account`, `settings_notifications`, `settings_security`,
//! `settings_keyboard`).

use leptos::prelude::*;
use teamcraft_client_api::calls::CallStatusSounds;
use teamcraft_client_api::window_chrome::TitleBar;

use crate::auth::tauri;
use crate::components::background_picker::{BackgroundPicker, effect_from_slug, effect_slug};
use crate::components::bundle_banner::AboutSection;
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_and_confirm;
use crate::components::settings_rows::{
  SegmentedRow, SelectRow, SettingRow, SettingsCard, SwitchRow, row_id, slug_options,
};
use crate::components::update_notice::UpdatesSection;
use crate::feedback::{ActionFailure, FailureKind, PendingAction, report_failure};
use crate::i18n::{Locale, use_i18n};
use crate::keys::t_with_shortcuts;
use crate::prefs::{CallPrefs, Density, ImageSize, SidebarSort, TextSize, Zoom};
use crate::reactive::SetIfAlive;
use crate::settings::{SendMode, Theme};
use crate::state::use_app_state;

#[component]
pub fn AppearanceSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let settings = state.settings;
  let prefs = settings.prefs;
  let display = Memo::new(move |_| prefs.get().display);

  view! {
    <SettingsCard heading="settings-theme">
      <ThemeTiles />
    </SettingsCard>
    <SettingsCard heading="settings-messages-look" hint="settings-messages-look-hint">
      <MessagePreview />
      <SegmentedRow
        label="settings-density"
        options=slug_options(&["bubbles", "comfortable", "compact"], "settings-density-")
        value=Signal::derive(move || display.get().density.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(density) = Density::from_slug(&slug) {
            prefs.update(|all| all.display.density = density);
          }
        })
      />
      <SegmentedRow
        label="settings-image-size"
        options=slug_options(&["standard", "large"], "settings-image-size-")
        value=Signal::derive(move || display.get().image_size.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(size) = ImageSize::from_slug(&slug) {
            prefs.update(|all| all.display.image_size = size);
          }
        })
      />
      <TextSizeRow />
      // Desktop app only: browsers zoom with their own Ctrl/Cmd + +/-/0.
      {tauri::is_available().then(|| view! {
        <SettingRow label="settings-zoom" live_hint=Signal::derive(move || t_with_shortcuts(i18n, "settings-zoom-hint"))>
          <select
            id="setting-zoom"
            class="input set-select"
            aria-label=move || i18n.t("settings-zoom")
            on:change=move |ev| {
              if let Ok(percent) = event_target_value(&ev).parse::<u16>() {
                prefs.update(|all| all.display.zoom = Zoom::from_percent(percent));
              }
            }
          >
            {Zoom::STEPS
              .into_iter()
              .map(|percent| {
                view! {
                  <option value=percent.to_string() selected=move || display.get().zoom.percent() == percent>
                    {move || i18n.t_with("settings-zoom-percent", &[("percent", &percent.to_string())])}
                  </option>
                }
              })
              .collect_view()}
          </select>
        </SettingRow>
      })}
    </SettingsCard>
    <SettingsCard heading="settings-messages-show">
      <SwitchRow
        label="settings-join-leave"
        hint="settings-join-leave-hint"
        value=settings.show_join_leave
        on_change=Callback::new(move |value: bool| settings.show_join_leave.set(value))
      />
      <SwitchRow
        label="settings-status-in-messages"
        value=Signal::derive(move || display.get().show_status_in_messages)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.display.show_status_in_messages = value))
      />
      <SelectRow
        label="settings-divider-dwell"
        hint="settings-divider-dwell-hint"
        options=slug_options(&["5", "10", "30", "never"], "settings-divider-dwell-")
        value=Signal::derive(move || {
          display.get().maybe_divider_dwell_secs.map_or_else(|| "never".to_owned(), |secs| secs.to_string())
        })
        on_change=Callback::new(move |slug: String| {
          let maybe_secs = slug.parse::<u16>().ok();
          prefs.update(|all| all.display.maybe_divider_dwell_secs = maybe_secs);
        })
      />
    </SettingsCard>
  }
}

/// Light, dark and system as small pictures of the app, a radio group.
#[component]
fn ThemeTiles() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let theme = state.settings.theme;
  view! {
    <div class="set-themes" role="radiogroup" id=row_id("settings-theme") aria-labelledby=format!("{}-heading", row_id("settings-theme"))>
      {Theme::ALL
        .into_iter()
        .map(|candidate| {
          view! {
            <label class="set-theme" class:is-chosen=move || theme.get() == candidate>
              <input
                type="radio"
                name="theme"
                class="visually-hidden"
                prop:checked=move || theme.get() == candidate
                on:change=move |_| theme.set(candidate)
              />
              <span class=format!("set-theme-preview is-{}", candidate.slug()) aria-hidden="true">
                <span class="set-theme-rail"></span>
                <span class="set-theme-body">
                  <span class="set-theme-line is-strong"></span>
                  <span class="set-theme-line"></span>
                  <span class="set-theme-line is-short"></span>
                  <span class="set-theme-bubble"></span>
                </span>
              </span>
              <span class="set-theme-name">
                <span class="set-theme-check" aria-hidden="true"><Icon name=IconName::Check /></span>
                {move || i18n.t(&format!("settings-theme-{}", candidate.slug()))}
              </span>
            </label>
          }
        })
        .collect_view()}
    </div>
  }
}

/// Two messages as the conversation would show them with the chosen layout and image size (one
/// from someone else, one of the user's own, so bubbles make sense), to see the choices before
/// going back. The text size scales the whole app, this preview too.
#[component]
fn MessagePreview() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let display = Memo::new(move |_| state.settings.prefs.get().display);
  view! {
    <div
      class="set-preview"
      data-density=move || display.get().density.slug()
      class:is-large-image=move || display.get().image_size == ImageSize::Large
      aria-hidden="true"
    >
      <div class="set-preview-msg">
        <span class="set-preview-avatar"></span>
        <div class="set-preview-body">
          <div class="set-preview-head">
            <strong>{move || i18n.t("settings-preview-name")}</strong>
            <span class="set-preview-time">"9:41"</span>
          </div>
          <p class="set-preview-text">{move || i18n.t("settings-preview-text")}</p>
          <span class="set-preview-image"></span>
        </div>
      </div>
      <div class="set-preview-msg is-own">
        <div class="set-preview-body">
          <div class="set-preview-head">
            <strong>{move || i18n.t("settings-preview-own-name")}</strong>
            <span class="set-preview-time">"9:42"</span>
          </div>
          <p class="set-preview-text">{move || i18n.t("settings-preview-own-text")}</p>
        </div>
      </div>
    </div>
  }
}

/// The text size as a slider over [`TextSize::STEPS`] with its value; the app, the preview above
/// included, follows it while dragging (`--text-scale`, issue #370).
#[component]
fn TextSizeRow() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let size = Memo::new(move |_| prefs.get().display.text_size);
  let max_position = (TextSize::STEPS.len() - 1).to_string();
  let set_position = move |ev: leptos::ev::Event| {
    if let Ok(position) = event_target_value(&ev).parse::<usize>() {
      let chosen = text_size_at(position);
      if chosen != size.get_untracked() {
        prefs.update(|all| all.display.text_size = chosen);
      }
    }
  };
  let percent = move || {
    i18n.t_with(
      "settings-zoom-percent",
      &[("percent", &size.get().percent().to_string())],
    )
  };
  view! {
    <SettingRow label="settings-text-size" hint="settings-text-size-hint">
      <div class="set-slider">
        <span class="set-slider-end is-small" aria-hidden="true">"A"</span>
        <input
          type="range"
          min="0"
          max=max_position
          step="1"
          aria-label=move || i18n.t("settings-text-size")
          aria-valuetext=percent
          prop:value=move || slider_position(size.get()).to_string()
          on:input=set_position
        />
        <span class="set-slider-end is-large" aria-hidden="true">"A"</span>
        <output class="set-slider-value">{percent}</output>
      </div>
    </SettingRow>
  }
}

/// The slider position of a text size.
fn slider_position(size: TextSize) -> usize {
  TextSize::STEPS
    .iter()
    .position(|step| *step == size.percent())
    .unwrap_or_default()
}

/// The text size at a slider position; positions past the end are the largest.
fn text_size_at(position: usize) -> TextSize {
  let last = TextSize::STEPS.len() - 1;
  TextSize::from_percent(TextSize::STEPS[position.min(last)])
}

#[component]
pub fn PreferencesSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let settings = state.settings;
  let prefs = settings.prefs;
  let language_options: Vec<(String, String)> = Locale::ALL
    .into_iter()
    .map(|locale| (locale.tag().to_owned(), format!("language-{}", locale.tag())))
    .collect();

  view! {
    <SettingsCard heading="settings-language" hint="settings-language-translate">
      <SelectRow
        label="settings-app-language"
        hint="settings-language-hint"
        options=language_options
        value=Signal::derive(move || settings.locale.get().tag().to_owned())
        on_change=Callback::new(move |tag: String| {
          if let Some(locale) = Locale::from_tag(&tag) {
            settings.locale.set(locale);
          }
        })
      />
    </SettingsCard>
    <SettingsCard heading="settings-composer">
      <SettingRow label="settings-send-mode" is_stacked=true>
        <div class="set-choices" role="radiogroup" aria-label=move || i18n.t("settings-send-mode")>
          {SendMode::ALL
            .into_iter()
            .map(|mode| {
              view! {
                <label class="set-choice">
                  <input
                    type="radio"
                    name="send-mode"
                    prop:checked=move || settings.send_mode.get() == mode
                    on:change=move |_| settings.send_mode.set(mode)
                  />
                  <span>{move || t_with_shortcuts(i18n, &format!("settings-send-{}", mode.slug()))}</span>
                </label>
              }
            })
            .collect_view()}
        </div>
      </SettingRow>
      <SwitchRow
        label="settings-convert-emoticons"
        value=Signal::derive(move || prefs.get().composer.convert_emoticons)
        on_change=Callback::new(move |on: bool| prefs.update(|all| all.composer.convert_emoticons = on))
      />
      <SwitchRow
        label="settings-show-formatting"
        value=Signal::derive(move || prefs.get().composer.show_formatting)
        on_change=Callback::new(move |on: bool| prefs.update(|all| all.composer.show_formatting = on))
      />
    </SettingsCard>
    <SettingsCard heading="settings-tab-sidebar">
      <SwitchRow
        label="settings-unreads-group"
        value=settings.show_unreads_group
        on_change=Callback::new(move |value: bool| settings.show_unreads_group.set(value))
      />
      <SwitchRow
        label="settings-sidebar-grouped"
        hint="settings-sidebar-grouped-hint"
        value=Signal::derive(move || prefs.get().sidebar.is_grouped_by_space)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.sidebar.is_grouped_by_space = value))
      />
      <SwitchRow
        label="settings-reopen-last"
        hint="settings-reopen-last-hint"
        value=Signal::derive(move || prefs.get().sidebar.reopen_last_conversation)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.sidebar.reopen_last_conversation = value))
      />
      <SelectRow
        label="settings-sidebar-sort"
        options=slug_options(&["default", "alphabetical"], "settings-sort-")
        value=Signal::derive(move || prefs.get().sidebar.sort.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(sort) = SidebarSort::from_slug(&slug) {
            prefs.update(|all| all.sidebar.sort = sort);
          }
        })
      />
    </SettingsCard>
    {tauri::is_available().then(|| view! { <DesktopCard /> })}
  }
}

/// Desktop app only: start at login, close to the tray, start minimized, the title bar. The shell
/// stores them; a refused change is reported and the switch goes back. The shell options are saved
/// as one snapshot, so their rows wait while one is saving: two saves could land out of order.
#[component]
fn DesktopCard() -> impl IntoView {
  let prefs = RwSignal::new(None::<tauri::ShellPrefs>);
  let autostart = RwSignal::new(None::<bool>);
  // Hidden where the system can't start the app at login; shown (and waiting) until that is known.
  let is_autostart_supported = RwSignal::new(true);
  let saving_prefs = PendingAction::new();
  let saving_autostart = PendingAction::new();
  leptos::task::spawn_local(async move {
    prefs.set_if_alive(tauri::shell_prefs().await);
    match tauri::autostart().await {
      Ok(Some(is_on)) => autostart.set_if_alive(Some(is_on)),
      Ok(None) => is_autostart_supported.set_if_alive(false),
      // The switch stays disabled; the toast says why.
      Err(error) => report_failure(ActionFailure::new(FailureKind::Other, error)),
    }
  });
  let save = move |change: fn(&mut tauri::ShellPrefs, bool), value: bool| {
    let Some(before) = prefs.get_untracked() else {
      return;
    };
    let mut next = before;
    change(&mut next, value);
    prefs.set(Some(next));
    saving_prefs.spawn(async move {
      if let Err(error) = tauri::set_shell_prefs(next).await {
        prefs.set_if_alive(Some(before));
        return Err(ActionFailure::new(FailureKind::Other, error));
      }
      Ok(())
    });
  };
  let set_autostart = move |on: bool| {
    autostart.set(Some(on));
    saving_autostart.spawn(async move {
      match tauri::set_autostart(on).await {
        // What the system reports: the user may have refused (the Flatpak portal asks).
        Ok(is_on) => {
          autostart.set_if_alive(Some(is_on));
          Ok(())
        },
        Err(error) => {
          autostart.set_if_alive(Some(!on));
          Err(ActionFailure::new(FailureKind::Other, error))
        },
      }
    });
  };
  let is_busy = Signal::derive(move || prefs.get().is_none() || saving_prefs.is_pending());
  let current = move || prefs.get().unwrap_or_default();

  view! {
    <SettingsCard heading="settings-desktop">
      <Show when=move || is_autostart_supported.get()>
        <SwitchRow
          label="settings-autostart"
          is_disabled=Signal::derive(move || autostart.get().is_none() || saving_autostart.is_pending())
          value=Signal::derive(move || autostart.get().unwrap_or(false))
          on_change=Callback::new(set_autostart)
        />
      </Show>
      <SwitchRow
        label="settings-close-to-tray"
        hint="settings-close-to-tray-hint"
        is_disabled=is_busy
        value=Signal::derive(move || current().is_close_to_tray)
        on_change=Callback::new(move |on: bool| save(|prefs, on| prefs.is_close_to_tray = on, on))
      />
      <SwitchRow
        label="settings-start-minimized"
        is_disabled=is_busy
        value=Signal::derive(move || current().is_start_minimized)
        on_change=Callback::new(move |on: bool| save(|prefs, on| prefs.is_start_minimized = on, on))
      />
      <SwitchRow
        label="settings-system-title-bar"
        hint="settings-system-title-bar-hint"
        is_disabled=is_busy
        value=Signal::derive(move || current().title_bar == TitleBar::System)
        on_change=Callback::new(move |on: bool| {
          save(|prefs, on| prefs.title_bar = if on { TitleBar::System } else { TitleBar::Custom }, on)
        })
      />
    </SettingsCard>
  }
}

#[component]
pub fn AudioVideoSection() -> impl IntoView {
  let state = use_app_state();
  let prefs = state.settings.prefs;
  let calls = Memo::new(move |_| prefs.get().calls);
  let devices = Memo::new(move |_| state.read(|backend| backend.media_devices()));
  // The desktop app lists its devices on request; the lists come in after a change notification.
  state.call.emit(crate::calls::CallEvent::RefreshDevices);
  let device_options = |list: &[crate::calls::MediaDevice]| -> Vec<(String, String)> {
    std::iter::once((String::new(), String::new()))
      .chain(
        list
          .iter()
          .map(|device| (device.id.clone(), device.label.clone())),
      )
      .collect()
  };
  let effect = RwSignal::new(effect_from_slug(&calls.get_untracked().background));
  // Picker changes flow into the preferences.
  Effect::new(move |previous: Option<()>| {
    let slug = effect_slug(effect.get());
    if previous.is_some() {
      prefs.update(|all| all.calls.background = slug);
    }
  });

  view! {
    <SettingsCard heading="calls-devices" hint="calls-devices-hint">
      {move || {
        let devices = devices.get();
        view! {
          <DeviceRow
            label="prejoin-camera"
            options=device_options(&devices.cameras)
            value=Signal::derive(move || calls.get().camera_id)
            on_change=Callback::new(move |id: String| prefs.update(|all| all.calls.camera_id = id))
          />
          <DeviceRow
            label="prejoin-microphone"
            options=device_options(&devices.microphones)
            value=Signal::derive(move || calls.get().microphone_id)
            on_change=Callback::new(move |id: String| prefs.update(|all| all.calls.microphone_id = id))
          />
          <DeviceRow
            label="prejoin-speaker"
            options=device_options(&devices.speakers)
            value=Signal::derive(move || calls.get().speaker_id)
            on_change=Callback::new(move |id: String| prefs.update(|all| all.calls.speaker_id = id))
          />
        }
      }}
    </SettingsCard>
    <SettingsCard heading="calls-joining">
      <SwitchRow
        label="calls-start-muted"
        value=Signal::derive(move || calls.get().start_muted)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.start_muted = value))
      />
      <SwitchRow
        label="calls-start-camera-off"
        value=Signal::derive(move || calls.get().start_camera_off)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.start_camera_off = value))
      />
      <SwitchRow
        label="prejoin-mirror"
        hint="settings-mirror-hint"
        value=Signal::derive(move || calls.get().mirror_self_view)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.mirror_self_view = value))
      />
    </SettingsCard>
    <SettingsCard heading="calls-audio">
      <SwitchRow
        label="calls-noise"
        hint="calls-noise-hint"
        value=Signal::derive(move || calls.get().noise_suppression)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.noise_suppression = value))
      />
      <SwitchRow
        label="calls-echo"
        hint="calls-echo-hint"
        value=Signal::derive(move || calls.get().echo_cancellation)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.echo_cancellation = value))
      />
      <SwitchRow
        label="calls-ptt"
        hint="calls-ptt-hint"
        value=Signal::derive(move || calls.get().push_to_talk)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.push_to_talk = value))
      />
      <SelectRow
        label="calls-status-sounds"
        hint="calls-status-sounds-hint"
        options=[CallStatusSounds::All, CallStatusSounds::OwnOnly, CallStatusSounds::Off]
          .into_iter()
          .map(|sounds| (sounds.slug().to_owned(), format!("calls-status-sounds-{}", sounds.slug().replace('_', "-"))))
          .collect()
        value=Signal::derive(move || calls.get().status_sounds.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          prefs.update(|all| all.calls.status_sounds = CallStatusSounds::from_slug(&slug))
        })
      />
    </SettingsCard>
    <SettingsCard heading="calls-background" hint="calls-background-hint">
      <BackgroundPicker effect=effect />
    </SettingsCard>
  }
}

/// A device choice; the empty value is "system default".
#[component]
fn DeviceRow(
  label: &'static str,
  options: Vec<(String, String)>,
  #[prop(into)] value: Signal<String>,
  on_change: Callback<String>,
) -> impl IntoView {
  let i18n = use_i18n();
  let options = options
    .into_iter()
    .map(|(id, name)| {
      let shown = if id.is_empty() {
        i18n.t("calls-system-default")
      } else {
        name
      };
      (id, shown)
    })
    .collect();
  view! { <SelectRow label=label options=options value=value on_change=on_change is_raw_labels=true /> }
}

/// Applies call preferences to the running call session (start state, effect, mirror, devices).
pub fn apply_call_prefs(call: crate::calls::CallState, calls: &CallPrefs) {
  call.is_mic_on.set(!calls.start_muted);
  call.is_camera_on.set(!calls.start_camera_off);
  call.mirror_self_view.set(calls.mirror_self_view);
  call.effect.set(effect_from_slug(&calls.background));
  call.camera_id.set(calls.camera_id.clone());
  call.microphone_id.set(calls.microphone_id.clone());
  call.speaker_id.set(calls.speaker_id.clone());
  call.share_sound.set(calls.share_sound);
  call.screen_mode.set(calls.screen_mode);
}

/// Help & about: versions, debug info for a support request, updates (desktop) and notices.
#[component]
pub fn HelpSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let version = env!("CARGO_PKG_VERSION");
  let copy_debug = move |_| {
    let layout = format!("{:?}", state.layout.get_untracked());
    copy_and_confirm(debug_info(
      version,
      &platform_text(),
      &layout,
      i18n.locale().tag(),
    ));
  };
  view! {
    <SettingsCard heading="settings-help-app">
      <SettingRow label="settings-help-version" live_hint=Signal::derive(move || format!("TeamCraft {version}"))>
        <button type="button" class="btn btn-small" id=row_id("settings-help-copy-debug") on:click=copy_debug>
          <Icon name=IconName::Copy />
          <span>{move || i18n.t("settings-help-copy-debug")}</span>
        </button>
      </SettingRow>
      <p class="set-note">{move || i18n.t("settings-help-copy-debug-hint")}</p>
    </SettingsCard>
    {tauri::is_available().then(|| view! { <div class="set-card" id=row_id("settings-updates")><UpdatesSection /></div> })}
    <div class="set-card set-about" id=row_id("about-title")><AboutSection /></div>
    {demo_tools()}
  }
}

/// What a support request needs to know about this app, as plain text. Nothing personal: no
/// names, IDs or server address.
fn debug_info(version: &str, platform: &str, layout: &str, locale: &str) -> String {
  format!("TeamCraft {version}\nPlatform: {platform}\nLayout: {layout}\nLanguage: {locale}\n")
}

/// The desktop app or the browser's user agent.
fn platform_text() -> String {
  if tauri::is_available() {
    return "desktop app".to_owned();
  }
  web_sys::window()
    .and_then(|window| window.navigator().user_agent().ok())
    .unwrap_or_default()
}

#[cfg(not(feature = "demo"))]
fn demo_tools() -> impl IntoView {}

/// Development builds only: the data source and triggers that stand in for events from the server.
#[cfg(feature = "demo")]
fn demo_tools() -> impl IntoView {
  use crate::calls::IncomingCall;
  use crate::model::id_from_raw;
  use crate::security::DeviceApproval;
  use crate::settings::BackendMode;
  use crate::state::Overlay;

  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <SettingsCard heading="settings-demo-title" hint="settings-demo-hint">
      <SelectRow
        label="settings-backend"
        hint="settings-backend-hint"
        options=[BackendMode::Mock, BackendMode::Server]
          .into_iter()
          .map(|mode| (mode.slug().to_owned(), format!("settings-backend-{}", mode.slug())))
          .collect()
        value=Signal::derive(|| BackendMode::load().slug().to_owned())
        on_change=Callback::new(|slug: String| {
          if let Some(mode) = BackendMode::from_slug(&slug) {
            mode.save();
            if let Some(window) = web_sys::window() {
              drop(window.location().reload());
            }
          }
        })
      />
      <div class="set-card-actions">
        <button
          class="btn btn-small"
          on:click=move |_| {
            state.close_overlay();
            state.call.incoming.set(Some(IncomingCall {
              caller: id_from_raw(2),
              conversation: id_from_raw(30),
              with_video: true,
            }));
          }
        >
          {move || i18n.t("settings-demo-incoming")}
        </button>
        <button
          class="btn btn-small"
          on:click=move |_| {
            state.device_prompt.set(Some(DeviceApproval {
              device: id_from_raw(777),
              device_name: "Firefox on Linux".into(),
              location: "Berlin, Germany".into(),
              requested_at: 0,
              code: "4719".into(),
            }));
            state.overlay.set(Overlay::DeviceApproval);
          }
        >
          {move || i18n.t("settings-demo-login")}
        </button>
        <button class="btn btn-small" on:click=move |_| state.overlay.set(Overlay::LinkDevice)>
          {move || i18n.t("settings-demo-link")}
        </button>
        <button class="btn btn-small" on:click=move |_| state.open_conversation(id_from_raw(32))>
          {move || i18n.t("settings-demo-key-change")}
        </button>
      </div>
    </SettingsCard>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_slider_walks_the_text_size_steps() {
    for size in TextSize::STEPS.map(TextSize::from_percent) {
      assert_eq!(text_size_at(slider_position(size)), size);
    }
    assert_eq!(
      text_size_at(99).percent(),
      *TextSize::STEPS.last().unwrap_or(&100)
    );
  }

  #[test]
  fn debug_info_names_the_build_and_nothing_personal() {
    let info = debug_info("1.0.3", "Linux", "Wide", "de");
    assert!(info.starts_with("TeamCraft 1.0.3\n"));
    assert!(info.contains("Platform: Linux"));
    assert!(info.contains("Language: de"));
  }
}
