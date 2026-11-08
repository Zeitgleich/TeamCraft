//! Small drawings for empty lists that are a first impression, such as Chats without any direct
//! messages yet (issue #301). Line art in the app's colours, drawn from the theme's variables in the
//! stylesheet (`.illustration`), so it follows light, dark and the colour themes. Decorative only:
//! the text next to it says what it means.

use leptos::prelude::*;

/// Which drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Art {
  /// Two speech bubbles: conversations to come.
  Conversations,
  /// A speech bubble with a check mark: nothing left unread.
  AllRead,
}

#[component]
pub fn Illustration(art: Art) -> impl IntoView {
  let drawing = match art {
    Art::Conversations => view! {
      <rect class="illustration-back" x="10" y="8" width="64" height="42" rx="14" />
      <path class="illustration-back" d="M22 46 L18 60 L34 48 Z" />
      <rect class="illustration-front" x="44" y="30" width="66" height="44" rx="15" />
      <path class="illustration-front" d="M96 70 L102 84 L86 72 Z" />
      <circle class="illustration-mark" cx="63" cy="52" r="4" />
      <circle class="illustration-mark" cx="77" cy="52" r="4" />
      <circle class="illustration-mark" cx="91" cy="52" r="4" />
    }
    .into_any(),
    Art::AllRead => view! {
      <rect class="illustration-back" x="16" y="14" width="88" height="56" rx="18" />
      <path class="illustration-back" d="M34 66 L28 82 L50 68 Z" />
      <path class="illustration-check" d="M44 42 L56 54 L78 30" />
    }
    .into_any(),
  };
  view! {
    <svg
      class="illustration"
      viewBox="0 0 120 90"
      width="120"
      height="90"
      aria-hidden="true"
      focusable="false"
    >
      {drawing}
    </svg>
  }
}
