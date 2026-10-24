//! Virtualized message feed. Only the rows near the viewport are in the DOM; the rest is represented
//! by two spacer heights. Row heights are estimated first and corrected by measuring rendered rows.

use std::cell::Cell;

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::clock::format_date;
use crate::components::empty_state::{EmptyAction, EmptyState};
use crate::components::icons::{Icon, IconName};
use crate::components::message_row::MessageRow;
use crate::components::message_toolbar::{MessageToolbar, ToolbarHost};
use crate::components::skeleton::MessageSkeleton;
use crate::feed_motion::{FeedMotion, Shown, newest_confirmed};
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ConversationKind, Message, as_chat_message};
use crate::state::{Overlay, use_app_state};

/// Extra pixels rendered above and below the viewport.
const OVERSCAN_PX: f64 = 700.0;
/// Distance from the bottom that still counts as "following the latest message".
const STICK_THRESHOLD_PX: f64 = 8.0;
const ESTIMATED_MESSAGE_HEIGHT: f32 = 68.0;
const ESTIMATED_LOG_HEIGHT: f32 = 22.0;
/// Measure passes after a jump that keep the target row in place while heights settle.
const ALIGN_PASSES: u8 = 6;
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
      scroll_top.set(f64::from(element.scroll_top()));
      last_top.set_value(f64::from(element.scroll_top()));
      layout_rev.update(|rev| *rev += 1);
    }
    state.jump_to_index.set(None);
  });

  let scroll_to_bottom = move || {
    if let Some(element) = container.get_untracked() {
      element.set_scroll_top(element.scroll_height());
      scroll_top.set(f64::from(element.scroll_top()));
      last_top.set_value(f64::from(element.scroll_top()));
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
        let Some(measured) = child
          .dyn_ref::<web_sys::HtmlElement>()
          .map(|el| el.offset_height() as f32)
        else {
          continue;
        };
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
      let corrected = container_element.scroll_top() as f64 + shift_above_anchor;
      container_element.set_scroll_top(corrected as i32);
      scroll_top.set(f64::from(container_element.scroll_top()));
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
      let corrected = f64::from(container_element.scroll_top()) + delta;
      container_element.set_scroll_top(corrected.max(0.0) as i32);
      scroll_top.set(f64::from(container_element.scroll_top()));
      last_top.set_value(f64::from(container_element.scroll_top()));
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
          scroll_top.set(f64::from(element.scroll_top()));
          last_top.set_value(f64::from(element.scroll_top()));
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
  let new_below = move || away_from.get().map_or(0, |from| count.get().saturating_sub(from));
  // Rows grow after they were measured (reactions, edits, late-loading content). While following,
  // stay at the bottom so the newest line is never cut off.
  observe_row_growth(list_id, rows, move || {
    if stick_to_bottom.get_untracked() {
      scroll_to_bottom();
    }
    schedule_measure();
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
    let top = f64::from(element.scroll_top());
    let bottom_gap = f64::from(element.scroll_height()) - top - f64::from(element.client_height());
    scroll_top.set(top);
    viewport.set(f64::from(element.client_height()));
    // Only an upward move counts as the user leaving the bottom; programmatic scrolls to the bottom
    // can fire late scroll events while row heights are still settling.
    if bottom_gap <= STICK_THRESHOLD_PX {
      stick_to_bottom.set(true);
    } else if top < last_top.get_value() - 1.0 {
      stick_to_bottom.set(false);
    }
    last_top.set_value(top);
  };

  let indices = move || {
    let window = window.get();
    (window.first..window.end).collect::<Vec<usize>>()
  };

  let on_keydown = move |ev: web_sys::KeyboardEvent| handle_list_keys(&ev, container, rows);
  let is_alone_in_channel = move || {
    state
      .read(|backend| backend.conversation(conversation))
      .is_some_and(|conv| {
        matches!(conv.kind, ConversationKind::Public | ConversationKind::Private)
          && conv.member_ids.len() <= 1
      })
  };

  view! {
    <div class="message-list-wrap">
      {move || floating_day.get().map(|label| view! { <div class="floating-day" aria-hidden="true">{label}</div> })}
      <div
        class="message-list"
        class:log-view=is_log
        role="log"
        tabindex="0"
        aria-label=move || i18n.t("messages-label")
        node_ref=container
        on:scroll=on_scroll
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

/// Calls `on_change` (on the next frame) whenever the rendered rows change size.
fn observe_row_growth(list_id: u64, rows: NodeRef<html::Div>, on_change: impl Fn() + Copy + 'static) {
  let callback = Closure::<dyn FnMut()>::new(move || defer_while_alive(list_id, on_change));
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
            .try_update_value(|motion| shown.with_value(|shown| motion.row_motion(shown.as_ref(), &message)))
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

  #[test]
  fn huge_list_renders_few_rows() {
    let heights = vec![22.0_f32; 50_000];
    let window = compute_window(&heights, 500_000.0, 800.0);
    assert!(window.end - window.first < 150);
  }
}
