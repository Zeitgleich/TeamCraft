use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use teamcraft_client_api::os_notify::{NotificationTag, OsNotification, OsNotificationKind};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::areas::{self, Place, lazy_area};
use crate::attention::{AttentionReporter, AttentionSignals, is_attentive, last_activity_at};
use crate::auth::tauri;
use crate::backend::Backend;
use crate::calls::CallPhase;
use crate::calls::{BackgroundEffect, CallEvent, CallRequest, CallStatusSounds};
use crate::clock::now_ms;
use crate::components::activity_view::ActivityView;
use crate::components::announcement_banner::AnnouncementBanner;
use crate::components::bookmarks::BookmarksDialog;
use crate::components::bottom_nav::BottomNav;
use crate::components::bundle_banner::{BundleBanner, ReleaseBanner};
use crate::components::calls_unavailable::CallsUnavailableDialog;
use crate::components::calls_view::CallsView;
use crate::components::channel_preview::ChannelPreview;
use crate::components::channel_view::ChannelView;
use crate::components::edge_swipe::EdgeSwipeBack;
use crate::components::file_drop;
use crate::components::files_view::FilesView;
use crate::components::getting_started::GettingStarted;
use crate::components::guest_join::GuestJoinPage;
use crate::components::home::{HomeSidebar, HomeStart, NewMessageDialog};
use crate::components::icons::{Icon, IconName};
use crate::components::incoming_call::IncomingCallDialog;
use crate::components::link_request_prompt::LinkRequestPrompt;
use crate::components::members_panel::MembersPanel;
use crate::components::message_actions::{EditHistoryDialog, ForwardDialog, PinsPanel, SavedView};
use crate::components::notification_prompt::NotificationPrompt;
use crate::components::offline_banner::OfflineBanner;
use crate::components::profile_views::UserProfileDialog;
use crate::components::quick_switcher::QuickSwitcher;
use crate::components::rail::SpaceRail;
use crate::components::security_ui::{
  DeviceApprovalDialog, LinkDeviceDialog, VerifyContactDialog, VerifyKeyDialog,
};
use crate::components::settings_modal::ShortcutsModal;
use crate::components::settings_tabs::apply_call_prefs;
#[cfg(target_arch = "wasm32")]
use crate::components::share_download::ShareDownloadPage;
use crate::components::share_link_dialog::ShareLinkDialog;
use crate::components::sidebar::Sidebar;
use crate::components::status_menu::CustomStatusDialog;
use crate::components::status_menu::StatusMenu;
use crate::components::storage_banner::StorageBanner;
use crate::components::thread_panel::ThreadPanel;
use crate::components::threads_inbox::ThreadsInbox;
use crate::components::top_bar::TopBar;
use crate::components::update_notice::UpdateNotice;
use crate::components::window_controls::{WindowControls, provide_window_chrome};
use crate::history::{Travel, travel};
use crate::i18n::{provide_i18n, use_i18n};
use crate::meetings::parse_guest_link;
use crate::model::TimestampMs;
use crate::model::{ConversationId, SpaceId};
use crate::motion::use_leave_ghost;
use crate::navigation::Step;
use crate::notify_sync::NotificationSync;
use crate::prefs::Zoom;
use crate::reactive::{OwnerLiveness, use_interval};
use crate::sections::badge_count;
use crate::settings::{Settings, Theme};
use crate::state::{AppState, MainView, Overlay, RailEntry, SidePanel, ToastTone};
use crate::toast_motion::{Swipe, use_flip_stack};
use crate::toast_timer::Hold;
use crate::viewport::{CompactDepth, CompactPane, PaneStep};

#[component]
pub fn App() -> impl IntoView {
  let settings = Settings::load();
  let i18n = provide_i18n(settings.locale.get_untracked());
  // The settings signal is the source of truth; mirror it into the translator.
  Effect::new(move |_| i18n.set_locale(settings.locale.get()));
  apply_theme(settings.theme);
  apply_density(settings);
  apply_zoom(settings);
  apply_text_size(settings);
  apply_language_attribute();
  crate::viewport::sync_theme_color(move || settings.theme.track());
  crate::brand::provide_brand(settings);
  provide_window_chrome();
  if tauri::is_available() {
    sync_tray_labels(i18n);
  }
  // Above every screen: sign-in has no top bar, but the window still needs its controls.
  view! {
    <WindowControls />
    {screen(settings)}
  }
}

/// The guest page, the sign-in gate or (development) the demo shell.
fn screen(settings: Settings) -> AnyView {
  // A personal meeting link needs no sign-in: it gets its own page before anything else.
  if let Some(link) = guest_link_from_location() {
    return view! { <GuestJoinPage link=link /> }.into_any();
  }

  // A share link needs no sign-in either: the file's key is in the fragment.
  #[cfg(target_arch = "wasm32")]
  if let Some((id, fragment)) = share_link_from_location() {
    return view! { <ShareDownloadPage id=id fragment=fragment /> }.into_any();
  }

  // What the service worker says about the code that is running (docs/e2ee.md §9.3).
  let bundle = crate::bundle::Bundle(RwSignal::new(crate::bundle::BundleState::Unknown));
  provide_context(bundle);
  let release = crate::bundle::Release(RwSignal::new(crate::bundle::ReleaseUpdate::Current));
  provide_context(release);
  if !tauri::is_available() {
    crate::bundle::watch(bundle.0);
    crate::bundle::watch_releases(release.0);
  }

  #[cfg(feature = "demo")]
  if crate::settings::BackendMode::load() == crate::settings::BackendMode::Mock {
    let backend: Rc<dyn Backend> = Rc::new(crate::mock::MockBackend::new(now_ms()));
    return view! { <Shell backend=backend settings=settings /> }.into_any();
  }
  lazy_area(Place::Screen, move || areas::session(settings))
}

/// Everything after sign-in: the chat shell on top of a backend.
#[component]
pub fn Shell(backend: Rc<dyn Backend>, settings: Settings) -> impl IntoView {
  let i18n = use_i18n();
  let start = initial_location(backend.as_ref());
  let change_source = backend.clone();
  let state = AppState::new(backend, settings, start);
  provide_context(state);
  crate::startup::first_view();
  crate::viewport::watch_layout(state.layout);
  crate::viewport::track_visual_viewport();
  Effect::new(move |previous: Option<()>| {
    let layout = state.layout.get();
    if previous.is_some() {
      untrack(|| state.layout_changed(layout));
    }
  });
  focus_new_compact_pane(state);
  hold_toasts_while_hidden(state);
  // Backends that load in the background (the desktop core) say so through this hook.
  change_source.set_change_notifier(Rc::new(move || state.backend_changed()));
  // A failed action is never silent (issue #322).
  crate::feedback::set_failure_sink(move |failure| {
    state.show_error(crate::feedback::failure_message(i18n, &failure), failure.detail);
  });
  // Preferences the server enforces (push, do not disturb, mutes) follow the account on the server.
  if let Some(sync) = use_context::<StoredValue<NotificationSync, LocalStorage>>() {
    sync.get_value().start(settings);
  }
  let link_menu = RwSignal::new(None::<crate::external_links::LinkMenuTarget>);
  provide_context(link_menu);
  crate::external_links::start(link_menu);
  if tauri::has_core() {
    crate::os_alerts::start(state, i18n);
  }
  if tauri::is_available() {
    sync_tray(state, i18n);
  } else {
    sync_favicon(state);
  }
  if let Some((_, conversation)) = start {
    state.open_conversation(conversation);
  }
  // The URL's place wins over the start; a notification click (below) wins over the URL.
  crate::history::start(state, i18n);
  if !tauri::is_available() {
    open_conversations_from_notifications(state);
  }
  // `/remind` reminders come due while the app is open.
  let brand = crate::brand::Brand::use_current();
  crate::reactive::use_interval(REMINDER_CHECK_MS, move || {
    for reminder in crate::components::composer_commands::due_reminders(now_ms()) {
      let message = i18n.t_with("reminder-due", &[("text", &reminder.text)]);
      state.show_toast(message.clone());
      let shown = OsNotification {
        tag: NotificationTag::reminder(reminder.due_at),
        kind: OsNotificationKind::Reminder,
        title: brand.0.with_untracked(|brand| brand.display_name.clone()),
        maybe_body: Some(message),
        actions: Vec::new(),
        maybe_reply: None,
      };
      leptos::task::spawn_local(async move { tauri::show_notification(&shown).await });
    }
  });
  // Real calls (desktop shell or the page's call module); the demo's simulated ones need no wiring.
  // Untracked: the shell must not render again on every backend change.
  if untrack(|| state.read(|backend| backend.has_calls() && !backend.has_simulated_calls())) {
    connect_calls(state);
  }
  watch_activity(state);
  // Call defaults follow the settings (and set the initial state of the call session).
  let call_prefs = Memo::new(move |_| settings.prefs.get().calls);
  Effect::new(move |_| apply_call_prefs(state.call, &call_prefs.get()));

  let listener = window_event_listener(ev::keydown, move |ev| handle_global_shortcut(&ev, state));
  // Removed with the component: a late event must not reach its disposed signals.
  on_cleanup(move || listener.remove());
  file_drop::block_stray_file_drops();
  let banners = NodeRef::<leptos::html::Div>::new();
  track_banner_height(banners);
  let toast_stack = NodeRef::<leptos::html::Div>::new();
  use_flip_stack(toast_stack);
  let app_body = NodeRef::<leptos::html::Div>::new();
  let edge_swipe = EdgeSwipeBack::new(state, app_body);
  // Which way the phone layout moved last, for the direction the panes slide in (#368). After an
  // edge swipe the parent is on screen already. Through memos, so a navigation that sets the view
  // and then the panel counts once; a rerun without a move keeps the step.
  let depth = Memo::new(move |_| state.compact_depth());
  let pane = Memo::new(move |_| state.compact_pane());
  let pane_step = Memo::new(move |previous: Option<&(CompactDepth, CompactPane, PaneStep)>| {
    let (depth, pane) = (depth.get(), pane.get());
    let step = match previous {
      None => PaneStep::Lateral,
      Some(_) if edge_swipe.is_committed() => PaneStep::Lateral,
      Some(&(before, before_pane, step)) if before == depth && before_pane == pane => step,
      Some(&(before, _, _)) => PaneStep::between(before, depth),
    };
    (depth, pane, step)
  });

  // The main column and the side panel mount their view again only when it changes: a navigation
  // that sets the same view again (the router after a click, a notification of the open
  // conversation) would otherwise mount it anew and play its entrance from the start.
  let main_view = Memo::new(move |_| state.view.get());
  let side_panel = Memo::new(move |_| state.panel.get());
  let open_conversation = Memo::new(move |_| match main_view.get() {
    MainView::Conversation(id) => Some(id),
    _ => None,
  });

  view! {
    <a class="skip-link" href="#main-content">
      {move || i18n.t("skip-to-content")}
    </a>
    <div
      class="app"
      data-pane=move || state.compact_pane().slug()
      data-nav=move || pane_step.get().2.slug()
      data-rail=move || if state.rail.get() == RailEntry::Chats { "home" } else { "space" }
      class:has-bottom-nav=move || has_bottom_nav(state)
    >
      // One grid row for all banners, so the top bar and the body keep their own rows.
      <div class="app-banners" node_ref=banners>
        <BundleBanner />
        <ReleaseBanner />
        <OfflineBanner />
        <StorageBanner />
        <NotificationPrompt />
        <AnnouncementBanner />
        <UpdateNotice />
      </div>
      <TopBar />
      <div
        class="app-body"
        node_ref=app_body
        on:pointerdown=move |ev| edge_swipe.pointer_down(&ev)
        on:pointermove=move |ev| edge_swipe.pointer_move(&ev)
        on:pointerup=move |ev| edge_swipe.pointer_up(&ev)
        on:pointercancel=move |_| edge_swipe.pointer_cancel()
      >
        // The administration console takes the whole window, like a system console.
        <Show when=move || !matches!(state.view.get(), MainView::Admin(_)) fallback=|| ()>
          <SpaceRail />
          <Show when=move || state.rail.get() == RailEntry::Chats fallback=|| view! { <Sidebar /> }>
            <HomeSidebar />
          </Show>
        </Show>
        <main id="main-content" class="main" tabindex="-1">
          {move || match main_view.get() {
            MainView::Conversation(id) => view! { <ChannelView conversation=id /> }.into_any(),
            MainView::Threads => view! { <ThreadsInbox /> }.into_any(),
            MainView::Admin(section) => lazy_area(Place::Pane, move || areas::admin_console(section)),
            MainView::Search => lazy_area(Place::Pane, || areas::search(())),
            MainView::Meetings => lazy_area(Place::Pane, || areas::meetings(())),
            MainView::Saved => view! { <SavedView /> }.into_any(),
            MainView::Files => view! { <FilesView /> }.into_any(),
            MainView::Activity => view! { <ActivityView /> }.into_any(),
            MainView::Calls => view! { <CallsView /> }.into_any(),
            MainView::Directory => lazy_area(Place::Pane, || areas::directory(())),
            MainView::GettingStarted => view! { <GettingStarted /> }.into_any(),
            MainView::Home => view! { <HomeStart /> }.into_any(),
            // Only the compact layout shows a space without a channel; its list is the sidebar.
            MainView::Space => ().into_any(),
            MainView::ChannelPreview(space, channel) => view! { <ChannelPreview space=space channel=channel /> }.into_any(),
          }}
        </main>
        <Show when=move || matches!(state.view.get(), MainView::Conversation(_)) fallback=|| ()>
          {move || match side_panel.get() {
            SidePanel::Closed => ().into_any(),
            SidePanel::Thread { conversation, thread } => {
              view! { <ThreadPanel conversation=conversation thread=thread /> }.into_any()
            },
            SidePanel::Pins => match open_conversation.get() {
              Some(id) => view! { <PinsPanel conversation=id /> }.into_any(),
              None => ().into_any(),
            },
            SidePanel::Members => match open_conversation.get() {
              Some(id) => view! { <MembersPanel conversation=id /> }.into_any(),
              None => ().into_any(),
            },
          }}
        </Show>
      </div>
      <Show when=move || has_bottom_nav(state) fallback=|| ()>
        <BottomNav />
      </Show>
      // On phones the account menu is a sheet over the bottom navigation's "You".
      <Show when=move || state.is_compact() && state.overlay.get() == Overlay::StatusMenu fallback=|| ()>
        <StatusMenu />
      </Show>
      <crate::components::link_menu::LinkMenu />
      {move || match state.call.phase.get() {
        CallPhase::PreJoin => {
          state.call.maybe_request.get().map(|request| lazy_area(Place::Screen, move || areas::call_prejoin(request)))
        },
        CallPhase::Active if !state.call.is_minimized.get() => {
          state.call.maybe_request.get().map(|request| lazy_area(Place::Screen, move || areas::call_screen(request)))
        },
        _ => None,
      }}
      <IncomingCallDialog />
      <LinkRequestPrompt />
      // Always in the page: screen readers only announce changes in a live region that already
      // existed when the toast arrived.
      <div
        class="toasts"
        role="status"
        node_ref=toast_stack
        on:pointerenter=move |_| state.hold_toasts(Hold::Pointer, true)
        on:pointerleave=move |_| state.hold_toasts(Hold::Pointer, false)
        on:focusin=move |_| state.hold_toasts(Hold::Focus, true)
        on:focusout=move |ev: ev::FocusEvent| {
          if !focus_stays_inside(&ev) {
            state.hold_toasts(Hold::Focus, false);
          }
        }
      >
        <For each=move || state.toasts.get() key=|toast| toast.id let:toast>
          {
            let id = toast.id;
            let node = NodeRef::<leptos::html::Div>::new();
            use_leave_ghost(node);
            let swipe = Swipe::new();
            let is_error = matches!(toast.tone, ToastTone::Error { .. });
            view! {
              <div
                class=if is_error { "toast toast-error" } else { "toast" }
                role=is_error.then_some("alert")
                node_ref=node
                on:pointerdown=move |ev| swipe.start(&ev)
                on:pointermove=move |ev| swipe.track(&ev)
                on:pointerup=move |ev| swipe.end(&ev, move || state.dismiss_toast(id))
                on:pointercancel=move |ev| swipe.cancel(&ev)
              >
                <div class="toast-body">
                  <span>{toast.text}</span>
                  {match toast.tone {
                    ToastTone::Info => None,
                    ToastTone::Error { detail } => Some(view! {
                      <details class="toast-details">
                        <summary>{move || i18n.t("error-details")}</summary>
                        <code class="toast-detail-text">{detail}</code>
                      </details>
                    }),
                  }}
                </div>
                {toast.maybe_undo.map(|undo| view! {
                  <button class="btn btn-small" on:click=move |_| state.undo(id, undo)>
                    {move || i18n.t("undo")}
                  </button>
                })}
                <button class="icon-btn icon-btn-small" aria-label=move || i18n.t("close") on:click=move |_| state.dismiss_toast(id)>
                  <Icon name=IconName::Close />
                </button>
              </div>
            }
          }
        </For>
      </div>
      {move || match state.overlay.get() {
        Overlay::QuickSwitcher => view! { <QuickSwitcher /> }.into_any(),
        Overlay::CustomStatus => view! { <CustomStatusDialog /> }.into_any(),
        Overlay::Settings => lazy_area(Place::Overlay, || areas::settings(())),
        Overlay::Shortcuts => view! { <ShortcutsModal /> }.into_any(),
        Overlay::DeviceApproval => view! { <DeviceApprovalDialog /> }.into_any(),
        Overlay::LinkDevice => view! { <LinkDeviceDialog /> }.into_any(),
        Overlay::VerifyContact(user) => view! { <VerifyContactDialog user=user /> }.into_any(),
        Overlay::VerifyKey(conversation) => view! { <VerifyKeyDialog conversation=conversation /> }.into_any(),
        Overlay::ScheduleMeeting => lazy_area(Place::Overlay, || areas::schedule_meeting(())),
        Overlay::EditMeeting(meeting) => lazy_area(Place::Overlay, move || areas::edit_meeting(meeting)),
        Overlay::MeetingCreated => lazy_area(Place::Overlay, || areas::meeting_created(())),
        Overlay::ShareFile(conversation, message, file) => {
          view! { <ShareLinkDialog conversation=conversation message=message file=file /> }.into_any()
        },
        Overlay::Forward(from, message) => view! { <ForwardDialog from=from message=message /> }.into_any(),
        Overlay::Lightbox(conversation, start) => lazy_area(Place::Overlay, move || areas::lightbox(conversation, start)),
        Overlay::FilePreview(conversation, key) => lazy_area(Place::Overlay, move || areas::file_preview(conversation, key)),
        Overlay::NewSpace => lazy_area(Place::Overlay, || areas::new_space(())),
        Overlay::NewMessage => view! { <NewMessageDialog /> }.into_any(),
        Overlay::BrowseSpaces(maybe_highlight) => lazy_area(Place::Overlay, move || areas::browse_spaces(maybe_highlight)),
        Overlay::InvitePeople(space) => lazy_area(Place::Overlay, move || areas::invite_people(space)),
        Overlay::AddToChannel(conversation) => lazy_area(Place::Overlay, move || areas::add_to_channel(conversation)),
        Overlay::AddToCall(conversation) => lazy_area(Place::Overlay, move || areas::add_to_call(conversation)),
        Overlay::NewChannel(space) => lazy_area(Place::Overlay, move || areas::new_channel(space)),
        Overlay::BrowseChannels(space) => lazy_area(Place::Overlay, move || areas::browse_channels(space)),
        Overlay::ChannelSettings(id) => lazy_area(Place::Overlay, move || areas::channel_settings(id)),
        Overlay::SpaceSettings(space) => lazy_area(Place::Overlay, move || areas::space_settings(space)),
        Overlay::EditHistory(conversation, message) => view! { <EditHistoryDialog conversation=conversation message=message /> }.into_any(),
        Overlay::Bookmarks(conversation) => view! { <BookmarksDialog conversation=conversation /> }.into_any(),
        Overlay::UserProfile(user) => view! { <UserProfileDialog user=user /> }.into_any(),
        Overlay::CallsUnavailable => view! { <CallsUnavailableDialog /> }.into_any(),
        Overlay::None | Overlay::StatusMenu => ().into_any(),
      }}
    </div>
  }
}

/// A hidden tab or minimized window doesn't eat the countdown of a toast (#367).
fn hold_toasts_while_hidden(state: AppState) {
  let is_visible = crate::reactive::use_page_visible();
  Effect::new(move |_| state.hold_toasts(Hold::Hidden, !is_visible.get()));
  end_timed_toasts_when_the_page_goes(state);
}

/// A page that is closed, navigated away, frozen or about to be discarded does what its Undo toasts
/// hold back now, or a deletion would never be sent. Capturing on the window runs this before the
/// browser core writes its store on the same events (`web_core.rs`), so the action is in it. The
/// page listens once; a new shell (after signing in again) takes it over.
fn end_timed_toasts_when_the_page_goes(state: AppState) {
  thread_local! {
    static SHELL: Cell<Option<AppState>> = const { Cell::new(None) };
    static IS_LISTENING: Cell<bool> = const { Cell::new(false) };
  }
  SHELL.with(|shell| shell.set(Some(state)));
  if IS_LISTENING.with(|is_listening| is_listening.replace(true)) {
    return;
  }
  let Some(window) = web_sys::window() else {
    return;
  };
  let listener = Closure::<dyn FnMut(web_sys::Event)>::new(|_: web_sys::Event| {
    // async-ok: a thread-local, not a signal; a gone shell's state does nothing (`try_*`).
    if let Some(state) = SHELL.with(Cell::get) {
      state.end_timed_toasts();
    }
  });
  for kind in ["pagehide", "freeze"] {
    if window
      .add_event_listener_with_callback_and_bool(kind, listener.as_ref().unchecked_ref(), true)
      .is_err()
    {
      tracing::debug!(kind, "could not watch the page lifecycle");
    }
  }
  // Lives as long as the page.
  listener.forget();
}

/// Whether focus moves to another element of the same container (from Undo to the close button).
fn focus_stays_inside(ev: &ev::FocusEvent) -> bool {
  let maybe_container = ev
    .current_target()
    .and_then(|target| target.dyn_into::<web_sys::Node>().ok());
  let maybe_next = ev
    .related_target()
    .and_then(|target| target.dyn_into::<web_sys::Node>().ok());
  match (maybe_container, maybe_next) {
    (Some(container), Some(next)) => container.contains(Some(&next)),
    _ => false,
  }
}

/// Phones show one pane at a time: when another one comes up, focus moves to its heading, so
/// screen readers announce it and the keyboard starts there (the old pane's focus is gone).
fn focus_new_compact_pane(state: AppState) {
  Effect::new(move |previous: Option<Option<CompactPane>>| {
    let maybe_pane = state.is_compact().then(|| state.compact_pane());
    let has_changed = previous.is_some_and(|before| before.is_some() && before != maybe_pane);
    if let Some(pane) = maybe_pane
      && has_changed
      && untrack(|| state.overlay.get()) == Overlay::None
    {
      request_animation_frame(move || focus_pane_heading(pane));
    }
    maybe_pane
  });
}

fn focus_pane_heading(pane: CompactPane) {
  let selectors = match pane {
    CompactPane::List => ".app-body > .sidebar .space-name",
    CompactPane::Main => "#main-content h1, #main-content .header-title, #main-content",
    CompactPane::Panel => ".app-body > .side-panel h2, .app-body > .side-panel h1, .app-body > .side-panel",
  };
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let Some(target) = document
    .query_selector(selectors)
    .ok()
    .flatten()
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
  else {
    return;
  };
  // Headings aren't focusable by themselves; -1 keeps them out of the Tab order.
  if !target.has_attribute("tabindex") && target.set_attribute("tabindex", "-1").is_err() {
    return;
  }
  if target.focus().is_err() {
    tracing::debug!("could not focus the new pane");
  }
}

/// The compact layout's bottom navigation shows on the top-level pages: the lists, Activity and
/// the welcome page. Deeper pages have a back button instead.
fn has_bottom_nav(state: AppState) -> bool {
  state.is_compact() && state.compact_depth() == CompactDepth::TopLevel
}

fn handle_global_shortcut(ev: &web_sys::KeyboardEvent, state: AppState) {
  let modifier = ev.ctrl_key() || ev.meta_key();
  let key = ev.key().to_lowercase();

  if modifier && !ev.alt_key() && !ev.shift_key() {
    if key == "k" {
      ev.prevent_default();
      if state.overlay.get_untracked() == Overlay::QuickSwitcher {
        state.overlay.set(Overlay::None);
      } else {
        state.open_scoped_switcher();
      }
      return;
    }
    let overlay = match key.as_str() {
      "," => Some(Overlay::Settings),
      "/" => Some(Overlay::Shortcuts),
      _ => None,
    };
    if let Some(overlay) = overlay {
      ev.prevent_default();
      if overlay == Overlay::Settings && state.overlay.get_untracked() != Overlay::Settings {
        state.open_settings();
        return;
      }
      state.overlay.update(|current| {
        *current = if *current == overlay {
          Overlay::None
        } else {
          overlay
        }
      });
      return;
    }
  }
  // In a browser, Ctrl/Cmd + +/-/0 stays the browser's own zoom (it reflows the layout properly).
  if modifier
    && !ev.alt_key()
    && tauri::is_available()
    && let Some(change) = zoom_change(&key)
  {
    ev.prevent_default();
    state
      .settings
      .prefs
      .update(|prefs| prefs.display.zoom = change(prefs.display.zoom));
    return;
  }
  // Everything opens the one search of the top bar: Ctrl/Cmd+F scoped to the open conversation
  // (as in Slack and Teams), Ctrl/Cmd+Shift+F unscoped.
  if modifier && !ev.shift_key() && !ev.alt_key() && key == "f" {
    ev.prevent_default();
    state.open_scoped_switcher();
    return;
  }
  if modifier && ev.shift_key() && !ev.alt_key() && key == "f" {
    ev.prevent_default();
    state.open_switcher("");
    return;
  }
  if ev.alt_key() && ev.shift_key() && !modifier {
    let maybe_step = match ev.code().as_str() {
      "ArrowUp" => Some(Step::Previous),
      "ArrowDown" => Some(Step::Next),
      _ => None,
    };
    if let Some(step) = maybe_step {
      ev.prevent_default();
      state.step_unread(step);
    }
    return;
  }
  if ev.alt_key() && !modifier {
    match ev.code().as_str() {
      "ArrowUp" => {
        ev.prevent_default();
        state.step_conversation(-1);
      },
      "ArrowDown" => {
        ev.prevent_default();
        state.step_conversation(1);
      },
      "KeyT" => {
        ev.prevent_default();
        state.open_threads();
      },
      // Browsers do this themselves; the desktop webviews don't. In a text field Alt+←/→ moves
      // the cursor (by word on macOS).
      "ArrowLeft" | "ArrowRight" if tauri::is_available() && !is_text_input_focused() => {
        ev.prevent_default();
        travel(if ev.code() == "ArrowLeft" {
          Travel::Back
        } else {
          Travel::Forward
        });
      },
      _ => {},
    }
    return;
  }
  if key == "escape"
    && state.overlay.get_untracked() == Overlay::None
    && !is_text_input_focused()
    && state.panel.get_untracked() != SidePanel::Closed
  {
    state.panel.set(SidePanel::Closed);
  }
}

/// Ctrl/Cmd with `+` / `=` zooms in, `-` zooms out, `0` resets (as in browsers).
fn zoom_change(key: &str) -> Option<fn(Zoom) -> Zoom> {
  match key {
    "+" | "=" => Some(Zoom::larger),
    "-" | "_" => Some(Zoom::smaller),
    "0" => Some(|_| Zoom::DEFAULT),
    _ => None,
  }
}

fn is_text_input_focused() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .is_some_and(|element| {
      matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
        || element
          .dyn_ref::<web_sys::HtmlElement>()
          .is_some_and(web_sys::HtmlElement::is_content_editable)
    })
}

/// Mirrors the theme setting into `data-theme` on `<html>`; "system" removes it so CSS media queries apply.
fn apply_theme(theme: RwSignal<Theme>) {
  Effect::new(move |_| {
    let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
    else {
      return;
    };
    let theme = theme.get();
    let result = match theme {
      Theme::System => root.remove_attribute("data-theme"),
      Theme::Light | Theme::Dark => root.set_attribute("data-theme", theme.slug()),
    };
    if result.is_err() {
      tracing::debug!("could not apply theme attribute");
    }
  });
}

/// Mirrors the message density and the image size into `data-density` and `data-image-size` on
/// `<html>` for the CSS.
fn apply_density(settings: Settings) {
  Effect::new(move |_| {
    let display = settings.prefs.get().display;
    let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
    else {
      return;
    };
    if root
      .set_attribute("data-density", display.density.slug())
      .is_err()
      || root
        .set_attribute("data-image-size", display.image_size.slug())
        .is_err()
    {
      tracing::debug!("could not apply density attributes");
    }
  });
}

/// Desktop app: scales the webview natively, like browser zoom, so the layout reflows (narrow
/// layouts apply at large sizes). Browsers have their own zoom, which the app leaves alone.
fn apply_zoom(settings: Settings) {
  if !tauri::is_available() {
    return;
  }
  Effect::new(move |_| {
    let percent = settings.prefs.get().display.zoom.percent();
    leptos::task::spawn_local(async move { tauri::set_zoom(percent).await });
  });
}

/// Every platform: the text size setting sets `--text-scale` on the root (CSSOM, the CSP blocks
/// style attributes); the stylesheet's rem type scale follows it.
fn apply_text_size(settings: Settings) {
  // Only a change of the text size touches the root, not every other preference change.
  let text_size = Memo::new(move |_| settings.prefs.with(|prefs| prefs.display.text_size));
  Effect::new(move |_| {
    let scale = text_size.get().css_scale();
    let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
      .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
    else {
      return;
    };
    if root.style().set_property("--text-scale", &scale).is_err() {
      tracing::debug!("could not apply the text size");
    }
  });
}

fn apply_language_attribute() {
  let i18n = use_i18n();
  Effect::new(move |_| {
    let tag = i18n.locale().tag();
    if let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
    {
      drop(root.set_attribute("lang", tag));
      let _ = root.dyn_ref::<web_sys::HtmlElement>();
    }
  });
}

/// First space and conversation to show, if the backend has any.
fn initial_location(backend: &dyn Backend) -> Option<(SpaceId, ConversationId)> {
  let space = backend.spaces().into_iter().next()?;
  let conversation = backend.space_conversations(space.id).into_iter().next()?;
  Some((space.id, conversation.id))
}

/// Keeps the tray icon's badge, tooltip and menu texts in step with the unread messages and the
/// language (desktop app).
/// The tray texts in the UI language from the start, also on the sign-in screen where `sync_tray`
/// is not running yet. The badge is left alone.
fn sync_tray_labels(i18n: crate::i18n::I18n) {
  let brand = crate::brand::Brand::use_current();
  Effect::new(move |_| {
    let name = brand.0.with(|brand| brand.display_name.clone());
    let (show, quit) = (i18n.t("tray-show"), i18n.t("tray-quit"));
    leptos::task::spawn_local(async move { tauri::tray_update(None, &name, &show, &quit).await });
  });
}

/// The browser tab's icon gets a red dot while the tray would show a count.
fn sync_favicon(state: AppState) {
  Effect::new(move |previous: Option<bool>| {
    let has_unread = state.read(|backend| {
      let mut list: Vec<_> = backend
        .spaces()
        .into_iter()
        .flat_map(|space| backend.space_conversations(space.id))
        .collect();
      list.extend(backend.direct_conversations());
      badge_count(&list) > 0
    });
    if previous != Some(has_unread) {
      crate::brand::show_unread_in_favicon(has_unread);
    }
    has_unread
  });
}

fn sync_tray(state: AppState, i18n: crate::i18n::I18n) {
  let brand = crate::brand::Brand::use_current();
  Effect::new(move |previous: Option<(u32, String, String)>| {
    let unread = state.read(|backend| {
      let mut list: Vec<_> = backend
        .spaces()
        .into_iter()
        .flat_map(|space| backend.space_conversations(space.id))
        .collect();
      list.extend(backend.direct_conversations());
      badge_count(&list)
    });
    let current = (
      unread,
      i18n.locale().tag().to_owned(),
      brand.0.with(|brand| brand.display_name.clone()),
    );
    if previous.as_ref() != Some(&current) {
      let name = current.2.clone();
      let tooltip = match unread {
        0 => name,
        count => i18n.t_with(
          "tray-tooltip-unread",
          &[("name", &name), ("count", &count.to_string())],
        ),
      };
      let (show, quit) = (i18n.t("tray-show"), i18n.t("tray-quit"));
      leptos::task::spawn_local(
        async move { tauri::tray_update(Some(unread), &tooltip, &show, &quit).await },
      );
    }
    current
  });
}

/// Browser: a click on a push notification names a conversation, either as a message from the service
/// worker (a page is open) or as `?conversation_id=` of the page it opened.
fn open_conversations_from_notifications(state: AppState) {
  const FIELD: &str = "conversation_id";
  let Some(window) = web_sys::window() else { return };
  let open = move |text: &str| {
    if let Ok(conversation) = text.parse::<ConversationId>() {
      state.open_conversation(conversation);
    }
  };
  if let Ok(search) = window.location().search()
    && let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search)
    && let Some(value) = params.get(FIELD)
  {
    open(&value);
  }
  let liveness = OwnerLiveness::of_current_owner();
  let listener = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
    if !liveness.is_alive() {
      return;
    }
    let data = event.data();
    let is_click = js_sys::Reflect::get(&data, &"type".into())
      .ok()
      .and_then(|value| value.as_string())
      .is_some_and(|kind| kind == "open_conversation");
    if is_click
      && let Some(value) = js_sys::Reflect::get(&data, &FIELD.into())
        .ok()
        .and_then(|value| value.as_string())
    {
      open(&value);
    }
  });
  if window
    .navigator()
    .service_worker()
    .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    listener.forget();
  }
}

fn window_has_focus() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_some_and(|document| document.has_focus().unwrap_or(false))
}

/// The meeting and token when the page was opened from a personal meeting link.
fn guest_link_from_location() -> Option<crate::meetings::GuestLink> {
  let location = web_sys::window()?.location();
  parse_guest_link(&location.pathname().ok()?, &location.hash().ok()?)
}

#[cfg(target_arch = "wasm32")]
/// The share ID and fragment when the page was opened from a share link (`/f/<id>#<fragment>`).
fn share_link_from_location() -> Option<(String, String)> {
  let location = web_sys::window()?.location();
  crate::share_link::parse_share_link(&location.pathname().ok()?, &location.hash().ok()?)
}

/// Keeps `--banners-h` on the root element at the height of the banner row: overlays that are fixed
/// below the top bar (call screens, side panels on narrow windows) start below the banners too.
fn track_banner_height(banners: NodeRef<leptos::html::Div>) {
  let update = move || {
    let Some(element) = banners.get_untracked() else {
      return;
    };
    let height = element.get_bounding_client_rect().height();
    let root = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.document_element())
      .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(root) = root
      && root
        .style()
        .set_property("--banners-h", &format!("{height}px"))
        .is_err()
    {
      tracing::debug!("could not set the banner height");
    }
  };
  let callback = Closure::<dyn FnMut()>::new(update);
  let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) else {
    tracing::debug!("ResizeObserver unavailable");
    return;
  };
  let handle = StoredValue::new_local((observer, callback));
  Effect::new(move |_| {
    if let Some(element) = banners.get() {
      handle.with_value(|(observer, _)| observer.observe(&element));
    }
  });
  on_cleanup(move || {
    handle.try_with_value(|(observer, _)| observer.disconnect());
  });
}

/// The user's call actions go to the call backend (the desktop shell or the page's call module), and
/// calls that ring there show the dialog.
pub(crate) fn connect_calls(state: AppState) {
  let call = state.call;
  let maybe_alerts = use_context::<crate::os_alerts::CallAlertsCell>();
  call.set_handler(move |event| {
    // Answered or declined here: the ring that ends now was not missed.
    if let (
      Some(alerts),
      CallEvent::Join {
        request: CallRequest { conversation, .. },
        ..
      }
      | CallEvent::Decline(conversation),
    ) = (maybe_alerts, &event)
    {
      alerts.handled_here(*conversation);
    }
    state.read(|backend| match event {
      CallEvent::Join { request, media } => backend.join_call(request, media),
      CallEvent::Leave => backend.leave_call(),
      CallEvent::Decline(conversation) => backend.decline_call(conversation),
      CallEvent::Muted(is_muted) => backend.set_call_muted(is_muted),
      CallEvent::Deafened(is_deafened) => backend.set_call_deafened(is_deafened),
      CallEvent::MoveHere => backend.move_call_here(),
      CallEvent::Microphone(device) => backend.select_microphone(Some(device)),
      CallEvent::Speakers(device) => backend.select_speakers(Some(device)),
      CallEvent::TestSpeakers(device) => backend.test_speakers(Some(device)),
      CallEvent::RefreshDevices => backend.refresh_media_devices(),
      CallEvent::Preview(preview) => backend.set_call_preview(preview),
      CallEvent::Camera(is_on) => backend.set_call_camera(is_on),
      CallEvent::CameraDevice(device) => backend.select_camera(Some(device)),
      CallEvent::Effect(effect) => backend.set_background_effect(effect),
      CallEvent::StartScreenShare { has_sound, mode } => backend.start_screen_share(has_sound, mode),
      CallEvent::StopScreenShare => backend.stop_screen_share(),
      CallEvent::StatusSounds(sounds) => backend.set_call_status_sounds(sounds),
    });
  });
  // The status sounds setting reaches the call backend at start and on every change (not on
  // every settings write: the same value again is no change).
  Effect::new(move |previous: Option<CallStatusSounds>| {
    let sounds = state.settings.prefs.with(|prefs| prefs.calls.status_sounds);
    if previous != Some(sounds) {
      call.emit(CallEvent::StatusSounds(sounds));
    }
    sounds
  });
  // The effect picker writes the signal; the media layer applies it to the camera image.
  Effect::new(move |previous: Option<BackgroundEffect>| {
    let effect = call.effect.get();
    if previous.is_some_and(|previous| previous != effect) {
      call.emit(CallEvent::Effect(effect));
    }
    effect
  });
  // The shell ended the call (the other side hung up, the connection dropped): leave the screen.
  // Only the end of the call the screen is for: switching to another one (a call that moved into a
  // meeting) ends the previous call first.
  let was_connected_to = Cell::new(None);
  Effect::new(move |_| {
    let maybe_connected_to = state.read(|backend| backend.active_call().map(|active| active.conversation));
    let maybe_was_connected_to = was_connected_to.replace(maybe_connected_to);
    let is_requested = |conversation| {
      call
        .maybe_request
        .get_untracked()
        .is_some_and(|request| request.conversation == conversation)
    };
    if maybe_connected_to.is_none()
      && maybe_was_connected_to.is_some_and(is_requested)
      && call.phase.get_untracked() == CallPhase::Active
    {
      call.leave();
    }
  });
  // A join that failed (a refused microphone, an unreachable call server) already told the user
  // why: the call screen it opened closes. Once the user's actions are carried out, the camera and
  // screen share buttons show what the media layer did (a camera the browser refused, a share
  // ended from the system's indicator).
  let seen_join_failures = Cell::new(untrack(|| state.read(|backend| backend.call_join_failures())));
  Effect::new(move |_| {
    let (failures, maybe_media) = state.read(|backend| {
      let maybe_media = (!backend.has_pending_call_actions())
        .then(|| backend.active_call())
        .flatten()
        .map(|active| (active.is_camera_on, active.is_sharing_screen));
      (backend.call_join_failures(), maybe_media)
    });
    if call.phase.get_untracked() != CallPhase::Active {
      seen_join_failures.set(failures);
      return;
    }
    if seen_join_failures.replace(failures) < failures {
      call.leave();
    } else if let Some((is_camera_on, is_sharing_screen)) = maybe_media {
      if call.is_camera_on.get_untracked() != is_camera_on {
        call.is_camera_on.set(is_camera_on);
      }
      if call.is_sharing_screen.get_untracked() != is_sharing_screen {
        call.is_sharing_screen.set(is_sharing_screen);
      }
    }
  });
  follow_moved_calls(state);
  Effect::new(move |_| {
    let ringing = state.read(|backend| backend.incoming_calls().first().copied());
    if call.phase.get_untracked() == CallPhase::Idle && call.incoming.get_untracked() != ringing {
      call.incoming.set(ringing);
    }
  });
}

/// Somebody added people to the DM call this device is in, and the server says the call moved into
/// a meeting room (docs/meetings.md, "A call becomes a meeting"). The server's word alone moves
/// nothing: the move shows only once a member of the DM's encryption group announced it, with who
/// moved it and whom they added. A move this device asked for follows at once (after the core's
/// check); any other, the user's own from another device included, waits for a tap in the call
/// screen.
fn follow_moved_calls(state: AppState) {
  let call = state.call;
  let pending = crate::feedback::PendingAction::new();
  Effect::new(move |_| {
    let maybe_offer = state.read(|backend| {
      let active = backend.active_call()?;
      let to = active.maybe_moved_to?;
      let offer = backend.call_move_offer(active.conversation, to)?;
      Some(crate::calls::MoveOffer {
        from: active.conversation,
        to,
        by: offer.by,
        added: offer.added,
        is_requested_here: offer.is_requested_here,
      })
    });
    let is_in_that_call = maybe_offer.as_ref().is_some_and(|offer| {
      call.phase.get_untracked() == CallPhase::Active
        && call
          .maybe_request
          .get_untracked()
          .is_some_and(|request| request.conversation == offer.from)
    });
    let maybe_offer = maybe_offer.filter(|_| is_in_that_call);
    if call.move_offer.get_untracked() == maybe_offer {
      return;
    }
    call.move_offer.set(maybe_offer.clone());
    if let Some(offer) = maybe_offer.filter(|offer| offer.is_requested_here) {
      state.follow_call_move(offer.from, offer.to, pending);
    }
  });
}

/// How often idle time, OS state and attention are checked (events re-check at once).
const ACTIVITY_CHECK_MS: u64 = 5_000;
/// How often `/remind` reminders are checked.
const REMINDER_CHECK_MS: u64 = 15_000;
/// Rounding noise in the OS idle time that is not new input.
const OS_ACTIVITY_TOLERANCE_MS: TimestampMs = 1_000;
const MS_PER_MINUTE: f64 = 60_000.0;
/// While the window is in front, input repeats "active" at most this often: half the shortest
/// away timeout the workspace policy allows (a minute), so a user who only scrolls or reads with
/// the mouse never times out, and the next input after an automatic away ends it at once.
const ACTIVE_REPEAT_MS: TimestampMs = 30_000;

/// What the activity watcher keeps between events.
struct ActivityWatch {
  state: AppState,
  last_input_at: Cell<TimestampMs>,
  /// The newest input anywhere on the computer the OS reported.
  last_os_activity_at: Cell<TimestampMs>,
  /// When this device last told the server it is active.
  last_active_report_at: Cell<TimestampMs>,
  is_idle: Cell<bool>,
  maybe_os: Cell<Option<tauri::OsAttention>>,
  reporter: RefCell<AttentionReporter>,
}

/// Two reports from the same input: no input for "Away after" marks this device idle (the server
/// shows the user as away), and attention (focused, visible, recent input, OS not idle, screen not
/// locked) tells the server to hold phone pushes back (notifications.md, "Attention").
fn watch_activity(state: AppState) {
  let watch = Rc::new(ActivityWatch {
    state,
    last_input_at: Cell::new(now_ms()),
    last_os_activity_at: Cell::new(0),
    // The gateway's identify says active.
    last_active_report_at: Cell::new(now_ms()),
    is_idle: Cell::new(false),
    maybe_os: Cell::new(None),
    reporter: RefCell::new(AttentionReporter::default()),
  });
  for event in ["pointermove", "pointerdown", "keydown", "touchstart", "wheel"] {
    let watch = watch.clone();
    on_window(event, move || watch.on_input());
  }
  for event in ["focus", "pageshow"] {
    let watch = watch.clone();
    on_window(event, move || watch.on_shown());
  }
  {
    let watch = watch.clone();
    on_window("blur", move || watch.check());
  }
  {
    let watch = watch.clone();
    on_document("visibilitychange", move || watch.on_shown());
  }
  // The window goes away: the server should not wait for the lease to run out.
  {
    let watch = watch.clone();
    on_window("pagehide", move || watch.leave());
  }
  watch.check();
  use_interval(ACTIVITY_CHECK_MS, move || {
    watch.check_idle();
    watch.check();
    let watch = watch.clone();
    leptos::task::spawn_local(async move {
      if let Some(os) = tauri::os_attention().await {
        // async-ok: a Cell the task shares, not a signal.
        watch.maybe_os.set(Some(os));
        watch.on_os_state();
        watch.check();
      }
    });
  });
}

impl ActivityWatch {
  /// Pointer, key, wheel or touch input. Ends this device's idle at once; while the window is in
  /// front it also repeats "active" now and then, which ends an away timeout of the server.
  fn on_input(&self) {
    let now = now_ms();
    self.last_input_at.set(now);
    let is_repeat_due = now.saturating_sub(self.last_active_report_at.get()) >= ACTIVE_REPEAT_MS;
    if self.is_idle.replace(false) || (is_repeat_due && is_in_front()) {
      self.report_active(now);
    }
    self.check();
  }

  /// The window was focused or became visible: the user turned to the app, which counts as input.
  fn on_shown(&self) {
    if is_in_front() {
      self.on_input();
    } else {
      self.check();
    }
  }

  fn report_active(&self, now: TimestampMs) {
    self.last_active_report_at.set(now);
    self.state.read(|backend| backend.set_activity(false));
  }

  /// System-wide activity (input anywhere on the computer, screen lock) feeds presence like input
  /// in the window does, so a user working elsewhere stays online and a locked screen is away.
  fn on_os_state(&self) {
    let now = now_ms();
    let maybe_os = self.maybe_os.get();
    let idle = maybe_os
      .and_then(|os| os.maybe_idle_ms)
      .map(Duration::from_millis);
    let is_locked = maybe_os.is_some_and(|os| os.is_locked);
    let Some(active_at) = last_activity_at(now, self.last_input_at.get(), idle, is_locked) else {
      // Away at once, unless the user turned automatic away off.
      if self
        .state
        .settings
        .prefs
        .get_untracked()
        .privacy
        .away_after_minutes
        .is_some()
        && !self.is_idle.replace(true)
      {
        self.state.read(|backend| backend.set_activity(true));
      }
      return;
    };
    // The reported idle time wobbles by a few milliseconds; only a clear step forward is input.
    let previous_at = self.last_os_activity_at.get();
    if active_at <= previous_at + OS_ACTIVITY_TOLERANCE_MS {
      return;
    }
    self.last_os_activity_at.set(active_at);
    let is_repeat_due = now.saturating_sub(self.last_active_report_at.get()) >= ACTIVE_REPEAT_MS;
    if self.is_idle.replace(false) || is_repeat_due {
      self.report_active(now);
    }
  }

  fn check_idle(&self) {
    let Some(minutes) = self
      .state
      .settings
      .prefs
      .get_untracked()
      .privacy
      .away_after_minutes
    else {
      return;
    };
    let last_activity_at = self.last_input_at.get().max(self.last_os_activity_at.get());
    let idle_for = (now_ms() - last_activity_at) as f64;
    if idle_for >= f64::from(minutes) * MS_PER_MINUTE && !self.is_idle.replace(true) {
      self.state.read(|backend| backend.set_activity(true));
    }
  }

  fn check(&self) {
    let notes = self.state.settings.prefs.get_untracked().notifications;
    // The user (or the instance) doesn't want attention used: tell the server once, then nothing.
    if !notes.uses_desktop_attention {
      self.leave();
      return;
    }
    let maybe_os = self.maybe_os.get();
    let signals = AttentionSignals {
      is_focused: window_has_focus(),
      is_visible: is_document_visible(),
      last_input_at: self.last_input_at.get(),
      maybe_os_idle: maybe_os
        .and_then(|os| os.maybe_idle_ms)
        .map(Duration::from_millis),
      is_screen_locked: maybe_os.is_some_and(|os| os.is_locked),
    };
    let window = Duration::from_secs(u64::from(notes.attention_input_secs));
    self.report(is_attentive(&signals, now_ms(), window));
  }

  fn report(&self, is_attentive: bool) {
    let maybe_due = self.reporter.borrow_mut().next(is_attentive, now_ms());
    self.send(maybe_due);
  }

  fn leave(&self) {
    let maybe_due = self.reporter.borrow_mut().leave();
    self.send(maybe_due);
  }

  fn send(&self, maybe_due: Option<bool>) {
    if let Some(is_attentive) = maybe_due {
      self.state.read(|backend| backend.set_attention(is_attentive));
    }
  }
}

pub(crate) fn on_window(event: &str, callback: impl Fn() + 'static) {
  let Some(window) = web_sys::window() else { return };
  listen_on(&window, event, callback);
}

pub(crate) fn on_document(event: &str, callback: impl Fn() + 'static) {
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  listen_on(&document, event, callback);
}

/// Calls `callback` on `event` while the current reactive owner lives (see [`OwnerLiveness`]).
pub(crate) fn listen_on(target: &web_sys::EventTarget, event: &str, callback: impl Fn() + 'static) {
  let liveness = OwnerLiveness::of_current_owner();
  let listener = Closure::<dyn Fn()>::new(move || {
    if liveness.is_alive() {
      callback();
    }
  });
  if target
    .add_event_listener_with_callback(event, listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page; it does nothing once its owner is gone.
    listener.forget();
  }
}

/// The window is visible and has the focus: input there is the user working in the app.
fn is_in_front() -> bool {
  is_document_visible() && window_has_focus()
}

fn is_document_visible() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_some_and(|document| !document.hidden())
}
