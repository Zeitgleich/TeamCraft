use leptos::prelude::*;

use crate::highlight::{TokenKind, highlight};
use crate::i18n::use_i18n;
use crate::markdown::{self, Block, Inline};
use crate::state::use_app_state;

/// Renders a message body through the safe Markdown subset.
#[component]
pub fn RichText(#[prop(into)] body: String, #[prop(optional, into)] me: String) -> impl IntoView {
  let blocks = markdown::parse(&body);
  let me = StoredValue::new(me);
  blocks
    .into_iter()
    .map(|block| match block {
      Block::Paragraph(lines) => {
        let last = lines.len().saturating_sub(1);
        let rendered: Vec<_> = lines
          .into_iter()
          .enumerate()
          .map(|(index, line)| {
            view! {
              {render_inlines(line, me.get_value())}
              {(index < last).then(|| view! { <br /> })}
            }
          })
          .collect();
        view! { <p class="md-p">{rendered}</p> }.into_any()
      },
      Block::Code { maybe_language, code } => {
        let tokens = highlight(maybe_language.as_deref(), &code);
        view! {
          <pre class="md-code" data-language=maybe_language.unwrap_or_default()>
            <code>
              {tokens
                .into_iter()
                .map(|token| match token.kind {
                  TokenKind::Plain => token.text.into_any(),
                  kind => view! { <span class=token_class(kind)>{token.text}</span> }.into_any(),
                })
                .collect_view()}
            </code>
          </pre>
        }
        .into_any()
      },
    })
    .collect_view()
}

fn token_class(kind: TokenKind) -> &'static str {
  match kind {
    TokenKind::Plain => "",
    TokenKind::Keyword => "tok-keyword",
    TokenKind::Str => "tok-string",
    TokenKind::Comment => "tok-comment",
    TokenKind::Number => "tok-number",
  }
}

fn render_inlines(inlines: Vec<Inline>, me: String) -> Vec<AnyView> {
  inlines
    .into_iter()
    .map(|inline| match inline {
      Inline::Text(text) => text.into_any(),
      Inline::Bold(inner) => view! { <strong>{render_inlines(inner, me.clone())}</strong> }.into_any(),
      Inline::Italic(inner) => view! { <em>{render_inlines(inner, me.clone())}</em> }.into_any(),
      Inline::Strike(inner) => view! { <s>{render_inlines(inner, me.clone())}</s> }.into_any(),
      Inline::Code(code) => view! { <code class="md-inline-code">{code}</code> }.into_any(),
      Inline::Link(url) if url.starts_with("teamcraft:/c/") => {
        view! { <PermalinkChip link=url /> }.into_any()
      },
      Inline::Link(url) => {
        let label = url.clone();
        view! {
          <a href=url target="_blank" rel="noopener noreferrer" class="md-link">
            {label}
          </a>
        }
        .into_any()
      },
      Inline::Mention(name) => {
        let class = if name == me {
          "mention mention-me"
        } else {
          "mention"
        };
        view! { <span class=class translate="no">{format!("@{name}")}</span> }.into_any()
      },
    })
    .collect()
}

/// A permalink to a message: names the conversation and opens it; links the user can't open say so.
#[component]
fn PermalinkChip(link: String) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let target = Memo::new(move |_| state.read(|backend| backend.resolve_permalink(&link)));
  move || match target.get() {
    Some(target) => {
      let name = state
        .read(|backend| backend.conversation(target.conversation))
        .map(|conversation| conversation.name)
        .unwrap_or_default();
      view! {
        <button
          class="md-link permalink-chip"
          on:click=move |_| {
            state.open_conversation(target.conversation);
            if let Some(thread) = target.maybe_thread {
              state.open_thread(target.conversation, thread);
            }
          }
        >
          <span translate="no">{i18n.t_with("permalink-label", &[("name", &name)])}</span>
        </button>
      }
      .into_any()
    },
    None => view! { <span class="permalink-unavailable">{i18n.t("permalink-unavailable")}</span> }.into_any(),
  }
}
