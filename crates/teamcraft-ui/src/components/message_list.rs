//! Virtualized message feed. Only the rows near the viewport are in the DOM; the rest is represented
//! by two spacer heights. Row heights are estimated first and corrected by measuring rendered rows.

use std::cell::Cell;
use std::time::Duration;

use leptos::ev;
use leptos::html;
use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::channel_access::ChannelAccess;
use crate::clock::{format_date, now_ms};
use crate::components::date_jump::{DateJumpMenu, DateJumpPopover, DatePill};
use crate::components::empty_state::{EmptyAction, EmptyState};
use crate::components::icons::{Icon, IconName};
use crate::components::message_row::MessageRow;
use crate::components::message_toolbar::{MessageToolbar, ToolbarHost};
use crate::components::skeleton::MessageSkeleton;
use crate::feed_motion::{FeedMotion, Shown, newest_confirmed};
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ConversationKind, Message, as_chat_message};
use crate::reactive::SetIfAlive;
use crate::state::{Overlay, use_app_state};
use crate::unread_divider::{DividerEvent, DwellClock, ReadMarkerGate};

/// Extra pixels rendered above and below the viewport.
const OVERSCAN_PX: f64 = 700.0;
/// Distance from the bottom that still counts as "following the latest message".
const STICK_THRESHOLD_PX: f64 = 8.0;
const ESTIMATED_MESSAGE_HEIGHT: f32 = 68.0;
const ESTIMATED_LOG_HEIGHT: f32 = 22.0;
/// New messages looked at for the first one from somebody else (the divider goes there).
const MAX_NEW_SCAN: usize = 50;
/// How long after a scrolling key its scroll events count as the user's.
const KEY_SCROLL_MS: i64 = 1_000;
/// Measure passes after a jump that keep the target row in place while heights settle.
const ALIGN_PASSES: u8 = 6;
/// How long the floating day stays after scrolling stops.
const FLOATING_DAY_LINGER: Duration = Duration::from_millis(1_000);
/// Space kept above the "New messages" divider when a conversation opens there.
const DIVIDER_CONTEXT_PX: f64 = 48.0;
/// Heights differing by less than this are treated as equal, which ends the measure loop.
const MEASURE_EPSILON: f32 = 0.5;

thread_local! {
  /// ID of the list that is currently mounted; 0 when none is.
  static ACTIVE_LIST: Cell<u64> = const { Cell::new(0) };
  static NEXT_LIST: Cell<u64> = const { Cell::new(1) };
}

/// Runs `callback` on the next animation frame unless the list that asked for it was unmounted in
/// the meantime. Touching signals of a disposed list would panic.
fn defer_while_alive(list_id: u64, callback: impl FnOnce() + 'static) {
  request_animation_frame(move || {
    // async-ok: a thread-local Cell, not a signal.
    if ACTIVE_LIST.get() == list_id {
      callback();
    }
  });
}

/// The slice of rows to render plus the spacer sizes around it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
  pub first: usize,
  pub end: usize,
  pub pad_top: f64,
  pub pad_bottom: f64,
  pub total: f64,
  /// First row that intersects the viewport (not the overscan); anchors scroll corrections.
  pub first_visible: usize,
}

/// Computes the visible window from row heights. Linear in the row count, which stays well below a
/// millisecond for the 50,000-line demo channel.
pub fn compute_window(heights: &[f32], scroll_top: f64, viewport: f64) -> Window {
  let from = (scroll_top - OVERSCAN_PX).max(0.0);
  let to = scroll_top + viewport + OVERSCAN_PX;
  let mut offset = 0.0_f64;
  let (mut first, mut first_visible, mut pad_top) = (None, None, 0.0);
  let mut end = heights.len();
  let mut end_offset = None;
  for (index, height) in heights.iter().enumerate() {
    let bottom = offset + f64::from(*height);
    if first.is_none() && bottom > from {
      first = Some(index);
      pad_top = offset;
    }
    if first_visible.is_none() && bottom > scroll_top {
      first_visible = Some(index);
    }
    if end_offset.is_none() && offset >= to {
      end = index;
      end_offset = Some(offset);
    }
    offset = bottom;
  }
  let total = offset;
  let first = first.unwrap_or(heights.len());
  Window {
    first,
    end: end.max(first),
    pad_top,
    pad_bottom: (total - end_offset.unwrap_or(total)).max(0.0),
    total,
    first_visible: first_visible.unwrap_or(heights.len().saturating_sub(1)),
  }
}

#[component]
pub fn MessageList(
  conversation: ConversationId,
  /// Show an integration channel as plain chat messages instead of log lines.
  #[prop(default = false)]
  is_chat: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let list_id = NEXT_LIST.get();
  NEXT_LIST.set(list_id + 1);
  ACTIVE_LIST.set(list_id);
  on_cleanup(move || {
    if ACTIVE_LIST.get() == list_id {
      ACTIVE_LIST.set(0);
    }
  });
  let container = NodeRef::<html::Div>::new();
  let rows = NodeRef::<html::Div>::new();
  let toolbar = ToolbarHost::provide(container);

  let is_log = state
    .read(|backend| backend.conversation(conversation))
    .is_some_and(|conv| conv.kind == ConversationKind::Integration)
    && !is_chat;
  let estimate = if is_log {
    ESTIMATED_LOG_HEIGHT
  } else {
    ESTIMATED_MESSAGE_HEIGHT
  };

  let count = Memo::new(move |_| state.read(|backend| backend.message_count(conversation)));
  let motion = StoredValue::new_local(FeedMotion::new(newest_confirmed_of(state, conversation)));
  // A conversation that opened empty (still loading): its first messages are history, not news.
  Effect::new(move |_| {
    if count.get() > 0
      && let Some(newest) = newest_confirmed_of(state, conversation)
    {
      motion.update_value(|motion| motion.set_baseline_if_unset(newest));
    }
  });
  let heights = StoredValue::new_local(vec![estimate; count.get_untracked()]);
  let scroll_top = RwSignal::new(0.0_f64);
  let viewport = RwSignal::new(800.0_f64);
  // Bumped whenever `heights` changed, so the window is recomputed.
  let layout_rev = RwSignal::new(0_u64);
  // Opened with unread messages: start at the "New messages" divider instead of the bottom.
  let maybe_marker = state
    .unread_marker
    .get_untracked()
    .filter(|marker| marker.conversation == conversation)
    .map(|marker| marker.index);
  // A jump to a message (search, link, activity) asked before this feed existed wins over both.
  let maybe_jump = state
    .jump_to_index
    .get_untracked()
    .filter(|(target, _)| *target == conversation)
    .map(|(_, index)| index);
  // Otherwise return to where the reader was when they left this conversation scrolled up.
  let maybe_saved_anchor = maybe_marker
    .is_none()
    .then(|| {
      state
        .scroll_anchors
        .with_value(|anchors| anchors.get(&conversation).copied())
    })
    .flatten();
  let stick_to_bottom =
    RwSignal::new(maybe_jump.is_none() && maybe_marker.is_none() && maybe_saved_anchor.is_none());
  let last_top = StoredValue::new(0.0_f64);
  // Scrollbar drags and scrolling keys; wheel and touch leave the bottom on their own.
  let intent = StoredValue::new(ScrollIntent::default());
  // Row to keep at the top while heights settle after a jump: (index, measure passes left).
  let pending_align = StoredValue::new(maybe_jump.map(|index| (index, ALIGN_PASSES)));
  // The message count when the reader left the bottom (or where the unread ones began): what is
  // newer is announced on the "Jump to latest" bar.
  let away_from = RwSignal::new(maybe_marker);

  let window = Memo::new(move |_| {
    layout_rev.track();
    let count = count.get();
    heights.update_value(|heights| heights.resize(count, estimate));
    heights.with_value(|heights| compute_window(heights, scroll_top.get(), viewport.get()))
  });

  // After a programmatic scroll: the window follows, and the next scroll event compares against it.
  let note_position = move |element: &web_sys::Element| {
    let top = exact_scroll_top(element);
    scroll_top.set(top);
    last_top.set_value(top);
  };

  // Jump to a message (date, search, link): it goes near the top, with a little context above.
  Effect::new(move |_| {
    let Some((target, index)) = state.jump_to_index.get() else {
      return;
    };
    if target != conversation {
      return;
    }
    let top: f32 = heights.with_value(|heights| heights.iter().take(index).sum());
    if let Some(element) = container.get_untracked() {
      pending_align.set_value(Some((index, ALIGN_PASSES)));
      stick_to_bottom.set(false);
      element.set_scroll_top((f64::from(top) - DIVIDER_CONTEXT_PX).max(0.0) as i32);
      note_position(&element);
      layout_rev.update(|rev| *rev += 1);
    }
    state.jump_to_index.set(None);
  });

  let scroll_to_bottom = move || {
    // At the bottom, a jump's target no longer needs to stay at the top.
    pending_align.set_value(None);
    if let Some(element) = container.get_untracked() {
      element.set_scroll_top(element.scroll_height());
      note_position(&element);
    }
  };

  // Measure rendered rows, store their real heights and keep the viewport anchored.
  let measure = move || {
    let (Some(rows_element), Some(container_element)) = (rows.get_untracked(), container.get_untracked())
    else {
      return;
    };
    let anchor = window.get_untracked().first_visible;
    let mut changed = false;
    let mut shift_above_anchor = 0.0_f64;
    let children = rows_element.children();
    heights.update_value(|heights| {
      for position in 0..children.length() {
        let Some(child) = children.item(position) else {
          continue;
        };
        let Some(index) = child
          .get_attribute("data-idx")
          .and_then(|value| value.parse::<usize>().ok())
        else {
          continue;
        };
        // The exact height: `offset_height` rounds, and the rounding adds up in the corrections.
        let measured = child.get_bounding_client_rect().height() as f32;
        // A row whose message is still loading is empty; keep its estimate so it stays in the window.
        if measured <= 0.0 {
          continue;
        }
        if let Some(slot) = heights.get_mut(index)
          && (*slot - measured).abs() > MEASURE_EPSILON
        {
          if index < anchor {
            shift_above_anchor += f64::from(measured - *slot);
          }
          *slot = measured;
          changed = true;
        }
      }
    });
    if !changed {
      return;
    }
    toolbar.reposition();
    if stick_to_bottom.get_untracked() {
      layout_rev.update(|rev| *rev += 1);
      defer_while_alive(list_id, scroll_to_bottom);
    } else {
      // Relative, so the fraction of the position survives (#642): rows above the one at the top
      // changed height, the rows on screen stay where they are.
      if shift_above_anchor != 0.0 {
        container_element.scroll_by_with_x_and_y(0.0, shift_above_anchor);
      }
      note_position(&container_element);
      layout_rev.update(|rev| *rev += 1);
    }
  };
  // After a jump the target row's real height and the rows above it are only known once rendered:
  // keep it at the top for the next few measure passes.
  let align_row = move || {
    let Some((index, passes_left)) = pending_align.get_value() else {
      return;
    };
    pending_align.set_value((passes_left > 1).then_some((index, passes_left - 1)));
    let (Some(container_element), Some(rows_element)) = (container.get_untracked(), rows.get_untracked())
    else {
      return;
    };
    let Ok(Some(row)) = rows_element.query_selector(&format!("[data-idx=\"{index}\"]")) else {
      return;
    };
    let offset = row.get_bounding_client_rect().top() - container_element.get_bounding_client_rect().top();
    let delta = offset - DIVIDER_CONTEXT_PX;
    if delta.abs() > 1.0 {
      container_element.scroll_by_with_x_and_y(0.0, delta);
      note_position(&container_element);
    }
  };
  let schedule_measure = move || {
    defer_while_alive(list_id, move || {
      measure();
      align_row();
    })
  };

  // Re-measure whenever the rendered range changes.
  Effect::new(move |_| {
    window.track();
    schedule_measure();
  });
  // The side panel changes the width, hence the wrapping and heights.
  Effect::new(move |_| {
    state.panel.track();
    schedule_measure();
  });
  // New content while following: stay at the bottom.
  Effect::new(move |_| {
    count.track();
    if stick_to_bottom.get_untracked() {
      defer_while_alive(list_id, scroll_to_bottom);
    }
  });
  // The composer asks for the latest message after sending.
  Effect::new(move |previous: Option<u64>| {
    let latest = state.jump_to_latest.get();
    if previous.is_some_and(|previous| previous != latest) {
      stick_to_bottom.set(true);
      defer_while_alive(list_id, scroll_to_bottom);
    }
    latest
  });
  // The jump bar takes space from the feed, so the viewport size changes when it appears.
  Effect::new(move |_| {
    stick_to_bottom.track();
    defer_while_alive(list_id, move || {
      if let Some(element) = container.get_untracked() {
        viewport.set(f64::from(element.client_height()));
      }
    });
  });
  // Initial viewport size and position: the unread divider when the unread messages don't fit,
  // the latest message otherwise.
  Effect::new(move |_| {
    if let Some(element) = container.get() {
      let height = f64::from(element.client_height());
      viewport.set(height);
      let maybe_divider_top = maybe_marker.and_then(|index| {
        let (top, total) = heights.with_value(|heights| {
          let top: f32 = heights.iter().take(index).sum();
          (f64::from(top), f64::from(heights.iter().sum::<f32>()))
        });
        (total - top > height).then_some(top)
      });
      let maybe_saved_top = maybe_jump.or(maybe_saved_anchor).map(|index| {
        let top: f32 = heights.with_value(|heights| heights.iter().take(index).sum());
        f64::from(top) + DIVIDER_CONTEXT_PX
      });
      let maybe_start = if maybe_jump.is_some() {
        maybe_saved_top
      } else {
        maybe_divider_top.or(maybe_saved_top)
      };
      match maybe_start {
        Some(top) => {
          let top = (top - DIVIDER_CONTEXT_PX).max(0.0);
          element.set_scroll_top(top as i32);
          note_position(&element);
        },
        None => {
          stick_to_bottom.set(true);
          defer_while_alive(list_id, scroll_to_bottom);
        },
      }
    }
  });
  // Remember where the reader left the bottom; back at the bottom, everything counts as seen.
  Effect::new(move |_| {
    if stick_to_bottom.get() {
      away_from.set(None);
    } else if away_from.get_untracked().is_none() {
      away_from.set(Some(count.get_untracked()));
    }
  });
  on_cleanup(move || {
    let (Some(is_following), Some(window)) =
      (stick_to_bottom.try_get_untracked(), window.try_get_untracked())
    else {
      return;
    };
    state.scroll_anchors.update_value(|anchors| {
      if is_following {
        anchors.remove(&conversation);
      } else {
        anchors.insert(conversation, window.first_visible);
      }
    });
  });
  // While reading older messages, the day of the top message floats over the feed.
  let floating_day = Memo::new(move |_| {
    if stick_to_bottom.get() || is_log {
      return None;
    }
    let first = window.get().first_visible;
    let locale = i18n.locale().tag();
    state
      .read(|backend| backend.messages(conversation, first, 1))
      .first()
      .map(|message| format_date(message.sent_at, locale))
  });
  // The day pills open one menu per feed; the floating one fades out while the feed rests.
  let date_menu = DateJumpMenu(RwSignal::new(None));
  provide_context(date_menu);
  let is_scrolling = RwSignal::new(false);
  let scroll_generation = StoredValue::new(0_u32);
  let new_below = move || away_from.get().map_or(0, |from| count.get().saturating_sub(from));
  // Rows grow after they were measured (reactions, edits, late-loading content). While following,
  // stay at the bottom so the newest line is never cut off. Corrected in the same frame, before it
  // is painted: a frame later the rows would visibly jump and jump back (#642).
  observe_row_growth(list_id, rows, move || {
    if stick_to_bottom.get_untracked() {
      scroll_to_bottom();
    }
    measure();
    align_row();
  });
  let listener = window_event_listener(ev::resize, move |_| {
    if let Some(element) = container.get_untracked() {
      viewport.set(f64::from(element.client_height()));
      schedule_measure();
    }
  });
  // Removed with the component: a late event must not reach its disposed signals.
  on_cleanup(move || listener.remove());

  let on_scroll = move |_| {
    let Some(element) = container.get_untracked() else {
      return;
    };
    let top = exact_scroll_top(&element);
    let bottom_gap = f64::from(element.scroll_height()) - top - f64::from(element.client_height());
    scroll_top.set(top);
    viewport.set(f64::from(element.client_height()));
    // Fades the floating day out 1 s after the last scroll event (opacity only).
    is_scrolling.set(true);
    let generation = scroll_generation.try_update_value(|generation| {
      *generation += 1;
      *generation
    });
    set_timeout(
      move || {
        if scroll_generation.try_get_value() == generation {
          is_scrolling.set_if_alive(false);
        }
      },
      FLOATING_DAY_LINGER,
    );
    let is_user_scrolling = intent.with_value(|intent| intent.is_user_scrolling(now_ms()));
    let maybe_follow = follow_after_scroll(bottom_gap, top, last_top.get_value(), is_user_scrolling);
    if let Some(is_following) = maybe_follow
      && stick_to_bottom.get_untracked() != is_following
    {
      stick_to_bottom.set(is_following);
    }
    last_top.set_value(top);
  };

  // The user's own scrolling up leaves the bottom at once, before the scroll event: a row that
  // grows meanwhile must not pin the list back down against them (#642). It also ends the
  // alignment after a jump: the user took over.
  let leave_bottom = move || {
    let can_scroll_up = container
      .get_untracked()
      .is_some_and(|element| exact_scroll_top(&element) > 0.0);
    pending_align.set_value(None);
    if can_scroll_up && stick_to_bottom.get_untracked() {
      stick_to_bottom.set(false);
    }
  };
  listen_to_user_scrolls(list_id, container, intent, leave_bottom);

  let indices = move || {
    let window = window.get();
    (window.first..window.end).collect::<Vec<usize>>()
  };

  track_unread_divider(conversation, count, stick_to_bottom);
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    // Esc marks everything read and removes the "New messages" divider (#321), unless it is used
    // for something else (an editor, a menu, closing the side panel or a dialog).
    if ev.key() == "Escape" {
      if is_escape_free(&ev) {
        state.mark_conversation_read(conversation);
      }
      return;
    }
    if is_scroll_key(&ev) {
      intent.update_value(|intent| intent.key_pressed(now_ms()));
    }
    handle_list_keys(&ev, container, rows);
  };
  // Bubbles show no avatars in a conversation of two (#576).
  let is_dm = move || {
    state
      .read(|backend| backend.conversation(conversation))
      .is_some_and(|conv| conv.kind == ConversationKind::Direct)
  };
  // Offered to people who may add members only.
  let access = ChannelAccess::new(state, conversation);
  let is_alone_in_channel = move || {
    state
      .read(|backend| backend.conversation(conversation))
      .is_some_and(|conv| {
        matches!(conv.kind, ConversationKind::Public | ConversationKind::Private)
          && conv.member_ids.len() <= 1
      })
      && access.can_add_members()
  };

  view! {
    <div class="message-list-wrap">
      {move || {
        floating_day.get().map(|label| {
          view! {
            <div class="floating-day" class:is-idle=move || !is_scrolling.get() && date_menu.0.get().is_none()>
              <DatePill label=label />
            </div>
          }
        })
      }}
      <DateJumpPopover conversation=conversation menu=date_menu />
      <div
        class="message-list"
        class:log-view=is_log
        class:is-dm=is_dm
        role="log"
        tabindex="0"
        aria-label=move || i18n.t("messages-label")
        node_ref=container
        on:scroll=on_scroll
        on:pointerdown=move |ev: web_sys::PointerEvent| {
          // A press on the list itself, not on a row: its scrollbar.
          let is_on_list = ev.target().zip(container.get_untracked()).is_some_and(|(target, element)| {
            target.dyn_ref::<web_sys::Element>().is_some_and(|target| target.is_same_node(Some(&element)))
          });
          if is_on_list {
            intent.update_value(|intent| intent.is_dragging = true);
          }
        }
        on:keydown=on_keydown
      >
        <Show when=move || count.get() == 0 fallback=|| ()>
          // Alone in a new channel: the obvious next step is bringing the team in.
          {move || {
            let maybe_action = is_alone_in_channel().then_some(EmptyAction {
              label_key: "channel-add-people",
              on_click: Callback::new(move |()| state.overlay.set(Overlay::AddToChannel(conversation))),
            });
            view! { <EmptyState icon=IconName::Smile title_key="channel-empty-title" body_key="channel-empty" maybe_action=maybe_action /> }
          }}
        </Show>
        <div class="spacer" style:height=move || format!("{}px", window.get().pad_top)></div>
        <div class="rows" node_ref=rows>
          <For each=indices key=|index| *index let:index>
            <RowSlot conversation=conversation index=index is_chat=is_chat motion=motion />
          </For>
        </div>
        <div class="spacer" style:height=move || format!("{}px", window.get().pad_bottom)></div>
        <MessageToolbar host=toolbar />
      </div>
      <Show when=move || !stick_to_bottom.get() fallback=|| ()>
        <button
          class="jump-latest"
          on:click=move |_| {
            stick_to_bottom.set(true);
            scroll_to_bottom();
          }
        >
          <Icon name=IconName::ArrowDown />
          <span>
            {move || match new_below() {
              0 => i18n.t("jump-to-latest"),
              count => i18n.t_count("jump-to-latest-new", count),
            }}
          </span>
        </button>
      </Show>
    </div>
  }
}

/// Moves or removes the "New messages" divider as the user reads (#321, `unread_divider`):
/// staying at the bottom of the focused window for the dwell time removes it, and messages that
/// arrive while the window is blurred or hidden move it to the first one from somebody else.
/// Leaving the conversation is handled where it happens (`AppState::open_conversation`), so a
/// remount of this list (layout change, thread pane) keeps it. One timer, only while the user sits
/// at the bottom with a divider shown; it is cancelled when that ends or the list goes away. The
/// read marker doesn't wait for the divider: see [`mark_read_while_watched`].
fn track_unread_divider(conversation: ConversationId, count: Memo<usize>, is_at_bottom: RwSignal<bool>) {
  let state = use_app_state();
  let is_focused = RwSignal::new(is_window_focused());
  mark_read_while_watched(conversation, is_at_bottom, is_focused);
  // Whether the user was at the bottom (had seen the divider) when they looked away.
  let was_at_bottom_when_away = StoredValue::new(false);
  let on_focus_change = move || {
    let focused = is_window_focused();
    if !focused && is_focused.try_get_untracked() == Some(true) {
      was_at_bottom_when_away.try_set_value(is_at_bottom.try_get_untracked().unwrap_or(false));
    }
    is_focused.try_set(focused);
  };
  let listeners = [
    window_event_listener(ev::focus, move |_| on_focus_change()),
    window_event_listener(ev::blur, move |_| on_focus_change()),
    window_event_listener(ev::visibilitychange, move |_| on_focus_change()),
  ];
  // Removed with the list: dropping a handle keeps the listener, so every list shown left three
  // listeners behind with all they hold (docs/performance.md, "Desktop on Linux: memory").
  on_cleanup(move || listeners.into_iter().for_each(|listener| listener.remove()));

  // News while the user looks elsewhere.
  Effect::new(move |previous: Option<usize>| {
    let total = count.get();
    if let Some(previous) = previous
      && total > previous
      && !is_focused.get_untracked()
    {
      let me = state.me().id;
      let len = (total - previous).min(MAX_NEW_SCAN);
      let maybe_first_foreign = state
        .read(|backend| backend.messages(conversation, previous, len))
        .iter()
        .position(|message| message.author != me);
      state.divider_event(
        conversation,
        DividerEvent::ArrivedWhileAway {
          first_new: previous + maybe_first_foreign.unwrap_or(0),
          is_own: maybe_first_foreign.is_none(),
          has_seen_divider: was_at_bottom_when_away.get_value(),
        },
      );
    }
    total
  });

  let maybe_dwell = Memo::new(move |_| {
    state
      .settings
      .prefs
      .with(|prefs| prefs.display.maybe_divider_dwell_secs)
      .map(|secs| Duration::from_secs(u64::from(secs)))
  });
  let clock = StoredValue::new(DwellClock::default());
  let timer = StoredValue::new(None::<TimeoutHandle>);
  let cancel_timer = move || {
    if let Some(Some(handle)) = timer.try_update_value(Option::take) {
      handle.clear();
    }
  };
  on_cleanup(cancel_timer);
  Effect::new(move |_| {
    let has_divider = state
      .unread_marker
      .get()
      .is_some_and(|marker| marker.conversation == conversation);
    let is_reading = has_divider && is_at_bottom.get() && is_focused.get();
    let maybe_dwell = maybe_dwell.get();
    cancel_timer();
    let now = now_ms();
    clock.update_value(|clock| clock.update(is_reading && maybe_dwell.is_some(), now));
    let Some(dwell) = maybe_dwell else { return };
    let Some(remaining) = clock.with_value(|clock| clock.remaining(now, dwell)) else {
      return;
    };
    let fire = move || {
      timer.try_set_value(None);
      let is_due = clock
        .try_with_value(|clock| clock.remaining(now_ms(), dwell))
        .flatten()
        .is_some_and(|left| left.is_zero());
      if is_due {
        state.divider_event(conversation, DividerEvent::DwelledAtBottom);
      }
    };
    match set_timeout_with_handle(fire, remaining) {
      Ok(handle) => timer.set_value(Some(handle)),
      Err(error) => tracing::debug!(?error, "no timer for the divider"),
    }
  });
}

/// Marks the conversation read whenever something in it is unread while the user watches its newest
/// message: at the bottom of the focused, visible window. Covers messages that arrive while it is
/// open, coming back to the window and scrolling down to the end; opening it marks it read in
/// `AppState::open_conversation`. Each newest message is sent once ([`ReadMarkerGate`]):
/// an `unread_update` the server sent before it applied the new position raises the count again,
/// and that must not queue another read position for the same message. A new message does.
fn mark_read_while_watched(
  conversation: ConversationId,
  is_at_bottom: RwSignal<bool>,
  is_focused: RwSignal<bool>,
) {
  let state = use_app_state();
  // The newest message the server stored while something is unread; looked up only then. A
  // reaction is no row but counts as unread, so the newest row alone would never read it (#641).
  let maybe_unread_through = Memo::new(move |_| {
    let has_unread = state
      .read(|backend| backend.conversation(conversation))
      .is_some_and(|conversation| conversation.has_unread());
    has_unread
      .then(|| {
        state
          .read(|backend| backend.newest_message(conversation))
          .or_else(|| newest_confirmed_of(state, conversation))
      })
      .flatten()
  });
  let gate = StoredValue::new(ReadMarkerGate::default());
  Effect::new(move |_| {
    let (maybe_through, at_bottom, focused) =
      (maybe_unread_through.get(), is_at_bottom.get(), is_focused.get());
    let should_mark = gate
      .try_update_value(|gate| gate.should_mark(maybe_through, at_bottom, focused))
      .unwrap_or(false);
    if should_mark {
      state.write(|backend| backend.mark_read(conversation));
    }
  });
}

/// Whether an Esc that reached the feed has no other job: nothing handled it yet, it doesn't come
/// from a text field, and there is no dialog or side panel for it to close.
fn is_escape_free(ev: &web_sys::KeyboardEvent) -> bool {
  let state = use_app_state();
  let is_from_text_field = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .is_some_and(|element| {
      element
        .closest("input, textarea, select, [contenteditable]")
        .ok()
        .flatten()
        .is_some()
    });
  !ev.default_prevented()
    && !is_from_text_field
    && state.overlay.get_untracked() == Overlay::None
    && state.panel.get_untracked() == crate::state::SidePanel::Closed
}

/// Who moves the list. Only the user's own scrolling leaves the bottom (#640, #642): programmatic
/// scrolls, late scroll events and rows that change height move it too, but never unpin it, or a
/// message sent right then would land below the visible area.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollIntent {
  /// The scrollbar is held.
  pub is_dragging: bool,
  /// When a scrolling key (Page Up, arrows, Home, space) was pressed last, in ms.
  maybe_key_at: Option<i64>,
}

impl ScrollIntent {
  pub fn key_pressed(&mut self, now: i64) {
    self.maybe_key_at = Some(now);
  }

  /// Whether a scroll event now comes from the user's scrollbar or keys. Wheel and touch don't
  /// need this: they leave the bottom themselves, before the list moves.
  pub fn is_user_scrolling(&self, now: i64) -> bool {
    self.is_dragging
      || self
        .maybe_key_at
        .is_some_and(|at| (0..KEY_SCROLL_MS).contains(&now.saturating_sub(at)))
  }
}

/// What a scroll event means for following the newest message: back at the bottom follows it
/// again, the user's own upward move stops following, anything else changes nothing. Any upward
/// move counts: a slow touchpad scroll moves less than a pixel per event.
pub fn follow_after_scroll(
  bottom_gap: f64,
  top: f64,
  last_top: f64,
  is_user_scrolling: bool,
) -> Option<bool> {
  if bottom_gap <= STICK_THRESHOLD_PX {
    Some(true)
  } else if is_user_scrolling && top < last_top {
    Some(false)
  } else {
    None
  }
}

/// Keys that scroll the list up or down.
fn is_scroll_key(ev: &web_sys::KeyboardEvent) -> bool {
  matches!(
    ev.key().as_str(),
    "PageUp" | "PageDown" | "ArrowUp" | "ArrowDown" | "Home" | "End" | " "
  )
}

/// Wheel and touch input that scrolls the list up calls `on_scroll_up`. Passive listeners: an
/// active one would make the browser wait for it before every scroll step. A released scrollbar
/// ends the drag of `intent`.
fn listen_to_user_scrolls(
  list_id: u64,
  container: NodeRef<html::Div>,
  intent: StoredValue<ScrollIntent>,
  on_scroll_up: impl Fn() + Copy + 'static,
) {
  let touch_y = StoredValue::new(None::<i32>);
  let on_wheel = Closure::<dyn FnMut(web_sys::WheelEvent)>::new(move |ev: web_sys::WheelEvent| {
    // async-ok: a thread-local Cell, not a signal.
    if ACTIVE_LIST.get() == list_id && ev.delta_y() < 0.0 && !ev.ctrl_key() {
      on_scroll_up();
    }
  });
  let on_touch_start = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(move |ev: web_sys::TouchEvent| {
    touch_y.try_set_value(ev.touches().get(0).map(|touch| touch.client_y()));
  });
  // The finger moving down scrolls the list up.
  let on_touch_move = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(move |ev: web_sys::TouchEvent| {
    let (Some(Some(start)), Some(touch)) = (touch_y.try_get_value(), ev.touches().get(0)) else {
      return;
    };
    // async-ok: a thread-local Cell, not a signal.
    if ACTIVE_LIST.get() == list_id && touch.client_y() > start {
      on_scroll_up();
    }
  });
  let listeners = StoredValue::new_local([
    ("wheel", on_wheel.into_js_value()),
    ("touchstart", on_touch_start.into_js_value()),
    ("touchmove", on_touch_move.into_js_value()),
  ]);
  Effect::new(move |_| {
    let Some(element) = container.get() else { return };
    let options = web_sys::AddEventListenerOptions::new();
    options.set_passive(true);
    listeners.with_value(|listeners| {
      for (name, listener) in listeners {
        let added = element.add_event_listener_with_callback_and_add_event_listener_options(
          name,
          listener.unchecked_ref(),
          &options,
        );
        if added.is_err() {
          tracing::debug!(name, "could not listen to scroll input");
        }
      }
    });
  });
  let released = [
    window_event_listener(ev::pointerup, move |_| {
      intent.try_update_value(|intent| intent.is_dragging = false);
    }),
    window_event_listener(ev::pointercancel, move |_| {
      intent.try_update_value(|intent| intent.is_dragging = false);
    }),
  ];
  on_cleanup(move || {
    released.into_iter().for_each(|listener| listener.remove());
    let Some(element) = container.get_untracked() else {
      return;
    };
    listeners.try_with_value(|listeners| {
      for (name, listener) in listeners {
        // Removing a listener that was never added is a no-op; nothing to report.
        drop(element.remove_event_listener_with_callback(name, listener.unchecked_ref()));
      }
    });
  });
}

/// The exact scroll position. `Element::scroll_top` truncates to whole pixels, which loses the
/// fraction on scaled displays: corrections drift by a pixel each, and a slow upward scroll looks
/// like no move at all (#642).
fn exact_scroll_top(element: &web_sys::Element) -> f64 {
  js_sys::Reflect::get(element, &wasm_bindgen::JsValue::from_str("scrollTop"))
    .ok()
    .and_then(|value| value.as_f64())
    .unwrap_or_else(|| f64::from(element.scroll_top()))
}

/// Whether the window has the focus and the page is visible.
fn is_window_focused() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_some_and(|document| !document.hidden() && document.has_focus().unwrap_or(true))
}

/// Calls `on_change` whenever the rendered rows change size: after layout, before the frame is
/// painted, unless the list that asked for it was unmounted in the meantime.
fn observe_row_growth(list_id: u64, rows: NodeRef<html::Div>, on_change: impl Fn() + Copy + 'static) {
  let callback = Closure::<dyn FnMut()>::new(move || {
    // async-ok: a thread-local Cell, not a signal.
    if ACTIVE_LIST.get() == list_id {
      on_change();
    }
  });
  let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) else {
    tracing::debug!("ResizeObserver unavailable");
    return;
  };
  let handle = StoredValue::new_local((observer, callback));
  Effect::new(move |_| {
    if let Some(element) = rows.get() {
      handle.with_value(|(observer, _)| observer.observe(&element));
    }
  });
  on_cleanup(move || {
    handle.try_with_value(|(observer, _)| observer.disconnect());
  });
}

/// Wrapper that carries the row index for measuring and re-renders only when its own data changes.
#[component]
fn RowSlot(
  conversation: ConversationId,
  index: usize,
  is_chat: bool,
  motion: StoredValue<FeedMotion, LocalStorage>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let has_divider = move || {
    state
      .unread_marker
      .get()
      .is_some_and(|marker| marker.conversation == conversation && marker.index == index)
  };
  let data = Memo::new(move |_| -> Option<(Message, Option<Message>)> {
    let start = index.saturating_sub(1);
    let mut messages = state.read(|backend| backend.messages(conversation, start, 2));
    if is_chat {
      messages.iter_mut().for_each(as_chat_message);
    }
    if index == 0 {
      let message = messages.into_iter().next()?;
      return Some((message, None));
    }
    if messages.len() < 2 {
      return None;
    }
    let message = messages.remove(1);
    Some((message, messages.into_iter().next()))
  });
  // What this row showed last, so a re-render knows what changed (feed_motion.rs).
  let shown = StoredValue::new(None::<Shown>);
  view! {
    <div class="row" data-idx=index.to_string()>
      <Show when=has_divider fallback=|| ()>
        <div class="new-divider" role="separator">
          <span>{move || i18n.t("new-messages")}</span>
        </div>
      </Show>
      {move || match data.get() {
        Some((message, maybe_prev)) => {
          let row_motion = motion
            .try_update_value(|motion| shown.with_value(|shown| motion.row_motion(shown.as_ref(), &message, crate::motion::now())))
            .unwrap_or_default();
          shown.set_value(Some(Shown::of(&message)));
          view! { <MessageRow message=message maybe_prev=maybe_prev motion=row_motion /> }.into_any()
        },
        // Still loading (or decrypting): a placeholder instead of a gap.
        None => view! { <MessageSkeleton /> }.into_any(),
      }}
    </div>
  }
}

/// The newest message the server confirmed, from the end of the conversation.
fn newest_confirmed_of(
  state: crate::state::AppState,
  conversation: ConversationId,
) -> Option<crate::model::MessageId> {
  /// The user's own unsent messages are at the end; a few are enough to look past them.
  const LOOK_BACK: usize = 8;
  state.read(|backend| {
    let count = backend.message_count(conversation);
    let start = count.saturating_sub(LOOK_BACK);
    let tail = backend.messages(conversation, start, count - start);
    newest_confirmed(tail.iter().rev())
  })
}

/// Arrow keys move between rendered rows; Home/End jump to the oldest/newest loaded row.
pub(crate) fn handle_list_keys(
  ev: &web_sys::KeyboardEvent,
  container: NodeRef<html::Div>,
  rows: NodeRef<html::Div>,
) {
  let key = ev.key();
  if !matches!(key.as_str(), "ArrowUp" | "ArrowDown") {
    return;
  }
  let (Some(container_element), Some(rows_element)) = (container.get_untracked(), rows.get_untracked())
  else {
    return;
  };
  let Ok(list) = rows_element.query_selector_all("[data-nav='row']") else {
    return;
  };
  let items: Vec<web_sys::HtmlElement> = (0..list.length())
    .filter_map(|i| list.item(i))
    .filter_map(|node| node.dyn_into().ok())
    .collect();
  if items.is_empty() {
    return;
  }
  ev.prevent_default();
  let active = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|doc| doc.active_element());
  let current = active.and_then(|active| items.iter().position(|item| active.is_same_node(Some(item))));
  let target = match (current, key.as_str()) {
    (None, "ArrowUp") => items.len() - 1,
    (None, _) => 0,
    (Some(position), "ArrowUp") => position.saturating_sub(1),
    (Some(position), _) => (position + 1).min(items.len() - 1),
  };
  if let Some(item) = items.get(target) {
    drop(item.focus());
    item.scroll_into_view();
  }
  let _ = container_element;
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn window_covers_viewport_plus_overscan() {
    let heights = vec![50.0_f32; 1000];
    let window = compute_window(&heights, 10_000.0, 600.0);
    assert_eq!(window.first_visible, 200);
    assert_eq!(window.first, 200 - 14);
    assert!(window.end > 200 + 12 && window.end < 260);
    assert_eq!(window.total, 50_000.0);
    assert_eq!(window.pad_top, window.first as f64 * 50.0);
    assert_eq!(window.pad_bottom, (1000 - window.end) as f64 * 50.0);
  }

  #[test]
  fn window_at_the_end_and_when_empty() {
    let heights = vec![20.0_f32; 100];
    let window = compute_window(&heights, 1400.0, 600.0);
    assert_eq!(window.end, 100);
    assert_eq!(window.pad_bottom, 0.0);
    let empty = compute_window(&[], 0.0, 600.0);
    assert_eq!((empty.first, empty.end, empty.total), (0, 0, 0.0));
  }

  mod following {
    use super::*;

    #[test]
    fn reaching_the_bottom_follows_again() {
      assert_eq!(follow_after_scroll(0.0, 900.0, 950.0, false), Some(true));
      assert_eq!(
        follow_after_scroll(STICK_THRESHOLD_PX, 900.0, 950.0, true),
        Some(true)
      );
    }

    #[test]
    fn only_the_user_scrolling_up_stops_following() {
      // #640: a programmatic scroll or a late scroll event while rows settle moves the list up,
      // and a message sent right then must still scroll into view.
      assert_eq!(follow_after_scroll(60.0, 900.0, 950.0, false), None);
      assert_eq!(follow_after_scroll(60.0, 900.0, 950.0, true), Some(false));
      assert_eq!(
        follow_after_scroll(60.0, 950.0, 900.0, true),
        None,
        "down is no leaving"
      );
    }

    #[test]
    fn a_sub_pixel_step_up_counts() {
      // #642: a slow touchpad scroll; pinning it back down made the list jump.
      assert_eq!(follow_after_scroll(20.0, 999.6, 1_000.0, true), Some(false));
    }

    #[test]
    fn keys_count_for_a_moment_and_the_scrollbar_while_held() {
      let mut intent = ScrollIntent::default();
      assert!(!intent.is_user_scrolling(5_000));
      intent.key_pressed(5_000);
      assert!(intent.is_user_scrolling(5_000 + KEY_SCROLL_MS - 1));
      assert!(!intent.is_user_scrolling(5_000 + KEY_SCROLL_MS));
      assert!(!intent.is_user_scrolling(4_000), "a clock that went back");
      intent.is_dragging = true;
      assert!(intent.is_user_scrolling(60_000));
    }
  }

  #[test]
  fn huge_list_renders_few_rows() {
    let heights = vec![22.0_f32; 50_000];
    let window = compute_window(&heights, 500_000.0, 800.0);
    assert!(window.end - window.first < 150);
  }
}
