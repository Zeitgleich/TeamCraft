use leptos::prelude::*;

use crate::markdown::{self, Block, Inline};

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
      Block::Code { maybe_language, code } => view! {
        <pre class="md-code" data-language=maybe_language.unwrap_or_default()>
          <code>{code}</code>
        </pre>
      }
      .into_any(),
    })
    .collect_view()
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
