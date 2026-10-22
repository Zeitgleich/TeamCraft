//! Inline SVG icons (24×24, stroke based) so the UI needs no icon font or network access.

use leptos::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconName {
  Hash,
  Lock,
  Star,
  Users,
  Search,
  Sliders,
  Smile,
  Paperclip,
  Send,
  Reply,
  Threads,
  ChevronDown,
  ChevronRight,
  Close,
  Plus,
  Bookmark,
  Link,
  File,
  Sun,
  Terminal,
  Check,
  Keyboard,
  Help,
  More,
  ArrowDown,
  Bell,
}

impl IconName {
  fn path(self) -> &'static str {
    match self {
      Self::Hash => "M4 9h16M4 15h16M10 3 8 21M16 3l-2 18",
      Self::Lock => "M5 11h14v10H5z M8 11V7a4 4 0 0 1 8 0v4",
      Self::Star => "M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z",
      Self::Users => {
        "M16 20v-1a4 4 0 0 0-4-4H7a4 4 0 0 0-4 4v1 M9.5 11a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z \
         M21 20v-1a4 4 0 0 0-3-3.9 M16 4.1a3.5 3.5 0 0 1 0 6.8"
      },
      Self::Search => "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16z M21 21l-4.3-4.3",
      Self::Sliders => "M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0 M16 4v4 M10 10v4 M18 16v4",
      Self::Smile => "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z M8 14s1.5 2 4 2 4-2 4-2 M9 9h.01 M15 9h.01",
      Self::Paperclip => {
        "M21 11.5l-8.6 8.6a5.5 5.5 0 0 1-7.8-7.8l8.6-8.6a3.7 3.7 0 0 1 5.2 5.2l-8.6 8.6a1.8 1.8 0 0 1-2.6-2.6l7.9-7.9"
      },
      Self::Send => "M22 2 11 13 M22 2l-7 20-4-9-9-4z",
      Self::Reply => "M9 14 4 9l5-5 M4 9h10a6 6 0 0 1 6 6v3",
      Self::Threads => "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z",
      Self::ChevronDown => "M6 9l6 6 6-6",
      Self::ChevronRight => "M9 6l6 6-6 6",
      Self::Close => "M18 6 6 18M6 6l12 12",
      Self::Plus => "M12 5v14M5 12h14",
      Self::Bookmark => "M19 21l-7-5-7 5V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z",
      Self::Link => {
        "M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7 M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7"
      },
      Self::File => "M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z M14 3v6h6",
      Self::Sun => {
        "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10z M12 1v2M12 21v2M4.2 4.2l1.4 1.4M18.4 18.4l1.4 1.4M1 12h2M21 12h2M4.2 19.8l1.4-1.4M18.4 5.6l1.4-1.4"
      },
      Self::Terminal => "M4 17l6-6-6-6 M12 19h8",
      Self::Check => "M20 6 9 17l-5-5",
      Self::Keyboard => "M3 6h18v12H3z M7 10h.01M11 10h.01M15 10h.01M7 14h10",
      Self::Help => "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3 M12 17h.01",
      Self::More => "M5 12h.01M12 12h.01M19 12h.01",
      Self::ArrowDown => "M12 5v14 M5 12l7 7 7-7",
      Self::Bell => "M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9 M13.7 21a2 2 0 0 1-3.4 0",
    }
  }
}

/// Decorative icon; give the surrounding control an accessible name instead.
#[component]
pub fn Icon(name: IconName, #[prop(optional, into)] class: &'static str) -> impl IntoView {
  view! {
    <svg
      class=format!("icon {class}")
      viewBox="0 0 24 24"
      width="18"
      height="18"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d=name.path() />
    </svg>
  }
}
