//! Inline SVG icons (24×24, stroke based) so the UI needs no icon font or network access.

use leptos::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconName {
  Hash,
  Lock,
  Star,
  Users,
  UserPlus,
  Search,
  Sliders,
  Smile,
  Paperclip,
  Send,
  Reply,
  Threads,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Close,
  Plus,
  Bookmark,
  Link,
  File,
  Sun,
  Terminal,
  Check,
  CheckDouble,
  Keyboard,
  Help,
  More,
  ArrowDown,
  Bell,
  BellOff,
  Phone,
  Video,
  VideoOff,
  SwitchCamera,
  Mic,
  MicOff,
  Headphones,
  Monitor,
  Hand,
  Grid,
  Spotlight,
  Maximize,
  Minimize,
  Image,
  Camera,
  Volume,
  Shield,
  Clock,
  List,
  ListNumbered,
  Quote,
  Code,
  CodeBlock,
  AtSign,
  Slash,
  Key,
  Layers,
  Alert,
  Smartphone,
  QrCode,
  Download,
  Trash,
  User,
  Eye,
  EyeOff,
  Calendar,
  /// A speech bubble: Chats, where the direct and group messages are.
  Chat,
  /// A pencil: write a new message.
  Compose,
  /// Magnifier with plus and minus: zoom in and out of an image.
  ZoomIn,
  ZoomOut,
  /// An arrow leaving to the right: forward a message.
  Forward,
  /// A circular arrow: turn an image a quarter clockwise.
  Rotate,
  /// A box with an arrow leaving it: open in a new window.
  External,
  /// Two overlapping sheets: copy to the clipboard.
  Copy,
  /// An "i" in a circle: details.
  Info,
  Play,
  Pause,
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
      Self::Chat => "M21 12a8 8 0 0 1-11.6 7.1L3 20.5l1.4-5.4A8 8 0 1 1 21 12z",
      Self::Compose => "M12 20h9 M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z",
      Self::ChevronDown => "M6 9l6 6 6-6",
      Self::ChevronLeft => "M15 6l-6 6 6 6",
      Self::ChevronRight => "M9 6l6 6-6 6",
      Self::ZoomIn => "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16z M21 21l-4.3-4.3 M11 8v6 M8 11h6",
      Self::ZoomOut => "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16z M21 21l-4.3-4.3 M8 11h6",
      Self::Forward => "M15 14l5-5-5-5 M20 9H10a6 6 0 0 0-6 6v3",
      Self::Rotate => "M21 12a9 9 0 1 1-2.6-6.4 M21 3v6h-6",
      Self::External => "M14 4h6v6 M20 4l-9 9 M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5",
      Self::Copy => "M9 9h11v11H9z M5 15H4V4h11v1",
      Self::Info => "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z M12 11v6 M12 7.5h.01",
      Self::Play => "M7 4l13 8-13 8z",
      Self::Pause => "M7 4h3v16H7z M14 4h3v16h-3z",
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
      Self::CheckDouble => "M18 6 7 17l-5-5 M22 10l-7.5 7.5L13 16",
      Self::Keyboard => "M3 6h18v12H3z M7 10h.01M11 10h.01M15 10h.01M7 14h10",
      Self::Help => "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3 M12 17h.01",
      Self::More => "M5 12h.01M12 12h.01M19 12h.01",
      Self::ArrowDown => "M12 5v14 M5 12l7 7 7-7",
      Self::Bell => "M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9 M13.7 21a2 2 0 0 1-3.4 0",
      Self::BellOff => {
        "M8.7 3A6 6 0 0 1 18 8a21.3 21.3 0 0 0 .6 5 M17 17H3s3-2 3-9a4.7 4.7 0 0 1 .3-1.7 M13.7 21a2 2 0 0 1-3.4 0 M2 2l20 20"
      },
      Self::Phone => {
        "M22 16.9v3a2 2 0 0 1-2.2 2 19.8 19.8 0 0 1-8.6-3.1 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.1 4.2 2 2 0 0 1 4.1 2h3a2 2 0 0 1 2 1.7c.1 1 .4 1.9.7 2.8a2 2 0 0 1-.5 2.1L8.1 9.9a16 16 0 0 0 6 6l1.3-1.3a2 2 0 0 1 2.1-.4c.9.3 1.8.6 2.8.7a2 2 0 0 1 1.7 2z"
      },
      Self::Video => "M23 7l-7 5 7 5z M1 5h15a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H1z",
      Self::VideoOff => {
        "M16 16v1a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h2m5.7 0H14a2 2 0 0 1 2 2v3.3l1 1L23 7v10 M1 1l22 22"
      },
      Self::Mic => {
        "M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z M19 10v2a7 7 0 0 1-14 0v-2 M12 19v4 M8 23h8"
      },
      Self::MicOff => {
        "M1 1l22 22 M9 9v3a3 3 0 0 0 5.1 2.1 M15 9.3V4a3 3 0 0 0-5.9-.6 M17 16.9A7 7 0 0 1 5 12v-2 M19 10v2a7 7 0 0 1-.1 1.2 M12 19v4 M8 23h8"
      },
      Self::Headphones => {
        "M3 18v-6a9 9 0 0 1 18 0v6 M21 19a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3z M3 19a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-3a2 2 0 0 0-2-2H3z"
      },
      Self::SwitchCamera => {
        "M20 7h-3a2 2 0 0 1-2-2V4H9v1a2 2 0 0 1-2 2H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2z M9.5 12.5a2.5 2.5 0 0 1 4.5-1.5 M14.5 13.5a2.5 2.5 0 0 1-4.5 1.5"
      },
      Self::Monitor => "M2 4h20v12H2z M8 20h8 M12 16v4",
      Self::Hand => {
        "M18 11V6a2 2 0 0 0-4 0 M14 10V4a2 2 0 0 0-4 0v2 M10 10.5V6a2 2 0 0 0-4 0v8 M18 8a2 2 0 1 1 4 0v6a8 8 0 0 1-8 8h-2c-2.8 0-4.5-.9-5.9-2.4L3.4 16a2 2 0 0 1 3.2-2.4L8 15"
      },
      Self::Grid => "M3 3h7v7H3z M14 3h7v7h-7z M14 14h7v7h-7z M3 14h7v7H3z",
      Self::Spotlight => "M3 3h18v12H3z M3 19h4 M10 19h4 M17 19h4",
      Self::Maximize => {
        "M8 3H5a2 2 0 0 0-2 2v3 M21 8V5a2 2 0 0 0-2-2h-3 M3 16v3a2 2 0 0 0 2 2h3 M16 21h3a2 2 0 0 0 2-2v-3"
      },
      Self::Minimize => {
        "M8 3v3a2 2 0 0 1-2 2H3 M21 8h-3a2 2 0 0 1-2-2V3 M3 16h3a2 2 0 0 1 2 2v3 M16 21v-3a2 2 0 0 1 2-2h3"
      },
      Self::Image => "M3 3h18v18H3z M8.5 10a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3z M21 15l-5-5L5 21",
      Self::Camera => {
        "M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z M12 17a4 4 0 1 0 0-8 4 4 0 0 0 0 8z"
      },
      Self::Volume => "M11 5L6 9H2v6h4l5 4z M15.5 8.5a5 5 0 0 1 0 7 M19 5a10 10 0 0 1 0 14",
      Self::Shield => "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z",
      Self::Clock => "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z M12 7v5l3 3",
      Self::List => "M8 6h13 M8 12h13 M8 18h13 M3 6h.01 M3 12h.01 M3 18h.01",
      Self::ListNumbered => {
        "M10 6h11 M10 12h11 M10 18h11 M4 4h1v5 M4 9h2 M6 19H4c0-1.5 2-2.2 2-3.4a1 1 0 0 0-2-.3"
      },
      Self::Quote => "M5 18h3l2-4V7H4v7h3z M14 18h3l2-4V7h-6v7h3z",
      Self::Code => "M16 18l6-6-6-6 M8 6l-6 6 6 6",
      Self::CodeBlock => "M3 4h18v16H3z M10 9l-3 3 3 3 M14 9l3 3-3 3",
      Self::AtSign => "M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8z M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-3.9 7.9",
      Self::Slash => "M4 4h16v16H4z M15 7l-6 10",
      Self::Key => "M21 2l-2 2m-7.6 7.6a5.5 5.5 0 1 1-7.8 7.8 5.5 5.5 0 0 1 7.8-7.8z M15.5 7.5l3 3L22 7l-3-3",
      Self::Layers => "M12 2l10 5-10 5L2 7z M2 17l10 5 10-5 M2 12l10 5 10-5",
      Self::Alert => {
        "M10.3 3.9L1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z M12 9v4 M12 17h.01"
      },
      Self::Smartphone => "M7 2h10v20H7z M11 18h2",
      Self::QrCode => "M3 3h7v7H3z M14 3h7v7h-7z M3 14h7v7H3z M14 14h3v3h-3z M20 14v3 M14 20h3 M20 20h1",
      Self::Download => "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M7 10l5 5 5-5 M12 15V3",
      Self::Trash => "M3 6h18 M8 6V4h8v2 M19 6l-1 14H6L5 6",
      Self::User => "M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2 M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z",
      Self::UserPlus => {
        "M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2 M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z M19 8v6 M22 11h-6"
      },
      Self::Calendar => "M4 6h16v14H4z M4 10h16 M8 3v4 M16 3v4",
      Self::Eye => "M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z",
      Self::EyeOff => {
        "M17.9 17.9A10.1 10.1 0 0 1 12 20c-7 0-11-8-11-8a18.5 18.5 0 0 1 5.1-5.9 M9.9 4.2A9 9 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.2 3.2 M1 1l22 22"
      },
    }
  }
}

/// Decorative icon; give the surrounding control an accessible name instead.
#[component]
pub fn Icon(
  #[prop(into)] name: Signal<IconName>,
  #[prop(optional, into)] class: &'static str,
) -> impl IntoView {
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
      <path d=move || name.get().path() />
    </svg>
  }
}
