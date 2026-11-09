//! A message body as the one line of text notifications and quotes show: the Markdown source, but
//! tables as rows of cells instead of pipes (`teamcraft_client_api::markdown::readable_tables`).
//! Builds without the UI contract (the server's and tools' `teamcraft-core`) show nothing of a
//! message and keep the body as it is.

use std::borrow::Cow;

#[cfg(any(feature = "ui", feature = "service-worker", test))]
pub fn readable_text(body: &str) -> Cow<'_, str> {
  teamcraft_client_api::markdown::readable_tables(body)
}

#[cfg(not(any(feature = "ui", feature = "service-worker", test)))]
pub fn readable_text(body: &str) -> Cow<'_, str> {
  Cow::Borrowed(body)
}
