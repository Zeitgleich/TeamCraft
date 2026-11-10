//! The mobile composers' formatting (`teamcraft_client_api::composer_format`): the toolbar's edits on
//! the Markdown the field holds, the keys that leave a block, and the spans for live styling. Offsets
//! are UTF-16 units, as `NSRange` and Compose's `TextRange` count them.

use teamcraft_client_api::composer_format::{self, Edit, Format, SpanKind, StyleSpan};

/// What a formatting button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ComposerFormat {
  Bold,
  Italic,
  Strike,
  Code,
  CodeBlock,
  Quote,
  Bulleted,
  Numbered,
  Link,
}

/// The text after an edit, with the selection (a caret when both are equal).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ComposerEdit {
  pub text: String,
  pub selection_start: u32,
  pub selection_end: u32,
}

/// How a part of the source is shown while typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ComposerSpanKind {
  /// Syntax characters (`**`, `> `, a fence): shown dimmed.
  Marker,
  Bold,
  Italic,
  Underline,
  Strike,
  Code,
  CodeBlock,
  Quote,
  Heading,
  Link,
  Mention,
}

/// A range of the text and how to show it; later spans are drawn over earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ComposerSpan {
  pub start: u32,
  pub end: u32,
  pub kind: ComposerSpanKind,
}

/// `format` applied to the selection `start..end` of `text`, toggled off if it is on already.
#[uniffi::export]
pub fn composer_apply_format(text: String, start: u32, end: u32, format: ComposerFormat) -> ComposerEdit {
  edit(composer_format::apply_format(&text, start, end, native(format)))
}

/// The formats the selection has, for the buttons' pressed state.
#[uniffi::export]
pub fn composer_active_formats(text: String, start: u32, end: u32) -> Vec<ComposerFormat> {
  composer_format::active_formats(&text, start, end)
    .into_iter()
    .map(foreign)
    .collect()
}

/// What Enter at the caret does in a quote, list or code block; `None`: a plain new line.
#[uniffi::export]
pub fn composer_enter(text: String, start: u32, end: u32) -> Option<ComposerEdit> {
  composer_format::enter(&text, start, end).map(edit)
}

/// What Backspace at the start of a block's text does; `None`: Backspace as always.
#[uniffi::export]
pub fn composer_backspace(text: String, start: u32, end: u32) -> Option<ComposerEdit> {
  composer_format::backspace(&text, start, end).map(edit)
}

/// How to style `text` as it renders; empty for a very long text.
#[uniffi::export]
pub fn composer_style_spans(text: String) -> Vec<ComposerSpan> {
  composer_format::style_spans(&text)
    .into_iter()
    .map(span)
    .collect()
}

fn edit(edit: Edit) -> ComposerEdit {
  ComposerEdit {
    text: edit.text,
    selection_start: edit.selection_start,
    selection_end: edit.selection_end,
  }
}

fn span(span: StyleSpan) -> ComposerSpan {
  ComposerSpan {
    start: span.start,
    end: span.end,
    kind: match span.kind {
      SpanKind::Marker => ComposerSpanKind::Marker,
      SpanKind::Bold => ComposerSpanKind::Bold,
      SpanKind::Italic => ComposerSpanKind::Italic,
      SpanKind::Underline => ComposerSpanKind::Underline,
      SpanKind::Strike => ComposerSpanKind::Strike,
      SpanKind::Code => ComposerSpanKind::Code,
      SpanKind::CodeBlock => ComposerSpanKind::CodeBlock,
      SpanKind::Quote => ComposerSpanKind::Quote,
      SpanKind::Heading => ComposerSpanKind::Heading,
      SpanKind::Link => ComposerSpanKind::Link,
      SpanKind::Mention => ComposerSpanKind::Mention,
    },
  }
}

fn native(format: ComposerFormat) -> Format {
  match format {
    ComposerFormat::Bold => Format::Bold,
    ComposerFormat::Italic => Format::Italic,
    ComposerFormat::Strike => Format::Strike,
    ComposerFormat::Code => Format::Code,
    ComposerFormat::CodeBlock => Format::CodeBlock,
    ComposerFormat::Quote => Format::Quote,
    ComposerFormat::Bulleted => Format::Bulleted,
    ComposerFormat::Numbered => Format::Numbered,
    ComposerFormat::Link => Format::Link,
  }
}

fn foreign(format: Format) -> ComposerFormat {
  match format {
    Format::Bold => ComposerFormat::Bold,
    Format::Italic => ComposerFormat::Italic,
    Format::Strike => ComposerFormat::Strike,
    Format::Code => ComposerFormat::Code,
    Format::CodeBlock => ComposerFormat::CodeBlock,
    Format::Quote => ComposerFormat::Quote,
    Format::Bulleted => ComposerFormat::Bulleted,
    Format::Numbered => ComposerFormat::Numbered,
    Format::Link => ComposerFormat::Link,
  }
}
