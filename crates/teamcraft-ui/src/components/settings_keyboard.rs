//! The keyboard shortcuts, grouped by area: Settings > Keyboard (searchable) and the quick
//! reference dialog (Ctrl/Cmd + /) read the same table.

use leptos::prelude::*;

use crate::components::composer_toolbar::{FORMAT_SHORTCUTS, action_label};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::settings_rows::row_id;
use crate::i18n::{I18n, use_i18n};
use crate::keys::{ENTER, ESCAPE, Key, Modifier, QUICK_SWITCHER, Shortcut, ShortcutList, shortcut_text};
use crate::state::use_app_state;

/// One area of the app and its shortcuts: (keys, i18n key of what they do).
pub struct ShortcutGroup {
  pub heading_key: &'static str,
  pub rows: Vec<(Vec<Shortcut>, &'static str)>,
}

/// Settings > Keyboard: every shortcut, filtered by what it does or its keys.
#[component]
pub fn KeyboardSection() -> impl IntoView {
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let groups = move || {
    let needle = query.get().trim().to_lowercase();
    shortcut_groups()
      .into_iter()
      .filter_map(|group| filtered(group, &needle, i18n))
      .collect::<Vec<_>>()
  };
  view! {
    <div class="set-row is-field" id=row_id("shortcuts-title")>
      <input
        type="search"
        class="input"
        enterkeyhint="search"
        aria-label=move || i18n.t("settings-keyboard-filter")
        placeholder=move || i18n.t("settings-keyboard-filter")
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
      />
    </div>
    {move || {
      let shown = groups();
      if shown.is_empty() {
        return view! { <p class="set-empty">{move || i18n.t("settings-search-none")}</p> }.into_any();
      }
      shown
        .into_iter()
        .map(|group| {
          let heading = group.heading_key;
          view! {
            <section class="set-card">
              <h3 class="set-card-title">{move || i18n.t(heading)}</h3>
              <ShortcutTable rows=group.rows />
            </section>
          }
        })
        .collect_view()
        .into_any()
    }}
  }
}

/// Reference of the keyboard shortcuts, opened with Ctrl/Cmd + / from anywhere.
#[component]
pub fn ShortcutsModal() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <Modal labelled_by="shortcuts-modal-title" class="modal-small">
      <header class="modal-head">
        <h2 id="shortcuts-modal-title">{move || i18n.t("shortcuts-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") data-autofocus on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        {shortcut_groups()
          .into_iter()
          .map(|group| {
            let heading = group.heading_key;
            view! {
              <h3 class="shortcut-section">{move || i18n.t(heading)}</h3>
              <ShortcutTable rows=group.rows />
            }
          })
          .collect_view()}
      </div>
    </Modal>
  }
}

#[component]
fn ShortcutTable(rows: Vec<(Vec<Shortcut>, &'static str)>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <table class="shortcut-table">
      <tbody>
        {rows
          .into_iter()
          .map(|(keys, key)| {
            view! {
              <tr>
                <th scope="row">{move || i18n.t(key)}</th>
                <td><ShortcutList shortcuts=keys /></td>
              </tr>
            }
          })
          .collect_view()}
      </tbody>
    </table>
  }
}

/// All shortcuts by area, in the order they are listed.
pub fn shortcut_groups() -> Vec<ShortcutGroup> {
  vec![
    ShortcutGroup {
      heading_key: "shortcuts-general",
      rows: vec![
        (
          vec![
            QUICK_SWITCHER,
            Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Char("F")),
          ],
          "shortcut-switcher",
        ),
        (vec![Shortcut::primary("F")], "shortcut-search-here"),
        (vec![Shortcut::primary(",")], "shortcut-settings"),
        (vec![Shortcut::primary("/")], "shortcut-shortcuts"),
        (vec![ESCAPE], "shortcut-close"),
        (
          vec![
            Shortcut::primary("+"),
            Shortcut::primary("-"),
            Shortcut::primary("0"),
          ],
          "shortcut-zoom",
        ),
      ],
    },
    ShortcutGroup {
      heading_key: "shortcuts-navigation",
      rows: vec![
        (
          vec![
            Shortcut::new(&[Modifier::Alt], Key::Up),
            Shortcut::new(&[Modifier::Alt], Key::Down),
          ],
          "shortcut-prev-next",
        ),
        (
          vec![
            Shortcut::new(&[Modifier::Alt, Modifier::Shift], Key::Up),
            Shortcut::new(&[Modifier::Alt, Modifier::Shift], Key::Down),
          ],
          "shortcut-next-unread",
        ),
        (
          vec![Shortcut::new(&[Modifier::Alt], Key::Char("T"))],
          "shortcut-threads",
        ),
        (
          vec![
            Shortcut::new(&[Modifier::Alt], Key::Left),
            Shortcut::new(&[Modifier::Alt], Key::Right),
          ],
          "shortcut-back-forward",
        ),
        (
          vec![
            Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Up),
            Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Down),
          ],
          "shortcut-move-favorite",
        ),
      ],
    },
    ShortcutGroup {
      heading_key: "shortcuts-messages",
      rows: vec![
        (
          vec![Shortcut::new(&[], Key::Up), Shortcut::new(&[], Key::Down)],
          "shortcut-messages",
        ),
        (
          vec![ENTER, Shortcut::new(&[], Key::Char("T"))],
          "shortcut-open-thread",
        ),
        (vec![Shortcut::new(&[], Key::Char("R"))], "shortcut-quote-message"),
        (vec![Shortcut::new(&[], Key::Char("E"))], "shortcut-react-message"),
        (
          vec![Shortcut::new(&[Modifier::Shift], Key::Char("F10"))],
          "shortcut-message-menu",
        ),
        (vec![Shortcut::new(&[], Key::Up)], "shortcut-edit-last"),
      ],
    },
    ShortcutGroup {
      heading_key: "shortcuts-formatting",
      rows: FORMAT_SHORTCUTS
        .into_iter()
        .map(|(action, shortcut)| (vec![shortcut], action_label(action)))
        .collect(),
    },
  ]
}

/// The group with only the rows matching `needle` (lowercase) by what they do, their keys or the
/// group's name; `None` when nothing is left.
fn filtered(group: ShortcutGroup, needle: &str, i18n: I18n) -> Option<ShortcutGroup> {
  if needle.is_empty() || i18n.t(group.heading_key).to_lowercase().contains(needle) {
    return Some(group);
  }
  let locale = i18n.locale();
  let rows: Vec<_> = group
    .rows
    .into_iter()
    .filter(|(keys, label)| {
      i18n.t(label).to_lowercase().contains(needle)
        || keys
          .iter()
          .any(|shortcut| shortcut_text(shortcut, locale).to_lowercase().contains(needle))
    })
    .collect();
  (!rows.is_empty()).then_some(ShortcutGroup {
    heading_key: group.heading_key,
    rows,
  })
}
