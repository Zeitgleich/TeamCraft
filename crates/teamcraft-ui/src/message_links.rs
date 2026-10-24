//! Links in a message body that point to another message of this server (#319): permalinks
//! (`teamcraft:/c/{c}/m/{m}[/t/{t}]`) and the app's own message and thread URLs on this server's
//! host. Each one gets an embedded preview that the viewer's client resolves from its own store;
//! links to other servers stay plain links.

use leptos::prelude::GetUntracked;

use crate::markdown::{self, Block, Inline};
use crate::model::root_message_of;
use crate::route::Route;

/// Previews under one message at most; more links stay plain links.
pub const MAX_PREVIEWS: usize = 3;

/// The permalinks of the message links in `body`, in order, without duplicates and at most
/// [`MAX_PREVIEWS`]. `own_hosts` are the host names (with port) this client talks to.
pub fn message_links_in(body: &str, own_hosts: &[String]) -> Vec<String> {
  let mut links = Vec::new();
  for block in markdown::parse(body) {
    let Block::Paragraph(lines) = block else { continue };
    for line in lines {
      collect_links(&line, own_hosts, &mut links);
    }
  }
  links
}

/// The permalink a link stands for, if it points to a message (or thread) of this server.
pub fn as_permalink(url: &str, own_hosts: &[String]) -> Option<String> {
  let route = match Route::from_permalink(url) {
    Some(route) => route,
    None => Route::parse(own_path(url, own_hosts)?)?,
  };
  match route {
    Route::Message {
      conversation,
      message,
      maybe_thread: None,
    } => Some(format!("teamcraft:/c/{conversation}/m/{message}")),
    Route::Message {
      conversation,
      message,
      maybe_thread: Some(thread),
    } => Some(format!("teamcraft:/c/{conversation}/m/{message}/t/{thread}")),
    // A thread link shows its root message.
    Route::Thread { conversation, thread } => Some(format!(
      "teamcraft:/c/{conversation}/m/{}",
      root_message_of(thread)
    )),
    _ => None,
  }
}

/// The hosts of this client's own server: the page's origin in the browser, and the server the
/// desktop shell talks to.
pub fn own_hosts() -> Vec<String> {
  let mut hosts = Vec::new();
  if let Some(location) = web_sys::window().map(|window| window.location())
    && location
      .protocol()
      .is_ok_and(|protocol| protocol.starts_with("http"))
    && let Ok(host) = location.host()
  {
    hosts.push(host.to_lowercase());
  }
  if let Some(server) = crate::session::use_session().and_then(|session| session.server.get_untracked()) {
    let host = server.host.to_lowercase();
    if !hosts.contains(&host) {
      hosts.push(host);
    }
  }
  hosts
}

fn collect_links(inlines: &[Inline], own_hosts: &[String], links: &mut Vec<String>) {
  for inline in inlines {
    if links.len() >= MAX_PREVIEWS {
      return;
    }
    match inline {
      Inline::Link(url) => {
        if let Some(link) = as_permalink(url, own_hosts)
          && !links.contains(&link)
        {
          links.push(link);
        }
      },
      Inline::Bold(inner) | Inline::Italic(inner) | Inline::Strike(inner) => {
        collect_links(inner, own_hosts, links);
      },
      Inline::Text(_) | Inline::Code(_) | Inline::Mention(_) => {},
    }
  }
}

/// The path of an `https` (or `http`) URL on one of `own_hosts`; `None` for other servers and for
/// URLs with a query or fragment, which the app's routes never have.
fn own_path<'a>(url: &'a str, own_hosts: &[String]) -> Option<&'a str> {
  let rest = url
    .strip_prefix("https://")
    .or_else(|| url.strip_prefix("http://"))?;
  let (host, path) = rest.split_at(rest.find('/')?);
  let is_own = own_hosts.iter().any(|own| own.eq_ignore_ascii_case(host));
  (is_own && !path.contains(['?', '#'])).then_some(path)
}

#[cfg(test)]
mod tests {
  use super::*;

  const HOST: &str = "chat.example.com";

  #[test]
  fn permalinks_and_own_urls_are_message_links() {
    assert_eq!(links("see teamcraft:/c/2/m/3"), ["teamcraft:/c/2/m/3"]);
    assert_eq!(
      links("https://chat.example.com/c/2/m/3/t/4 and HTTPS://x"),
      ["teamcraft:/c/2/m/3/t/4"]
    );
    assert_eq!(
      links("**https://Chat.Example.com/s/1/c/2**"),
      Vec::<String>::new()
    );
  }

  #[test]
  fn thread_links_show_the_root() {
    assert_eq!(links("https://chat.example.com/c/2/t/5"), ["teamcraft:/c/2/m/5"]);
  }

  #[test]
  fn other_servers_and_other_paths_get_no_preview() {
    for body in [
      "https://other.example.org/c/2/m/3",
      "https://chat.example.com.evil.example/c/2/m/3",
      "https://chat.example.com/c/2/m/3?x=1",
      "https://chat.example.com/c/2",
      "https://chat.example.com/c/two/m/3",
      "`teamcraft:/c/2/m/3`",
      "teamcraft:/c/2/x/3",
    ] {
      assert_eq!(links(body), Vec::<String>::new(), "{body}");
    }
  }

  #[test]
  fn duplicates_are_dropped_and_the_count_is_bounded() {
    assert_eq!(
      links("teamcraft:/c/2/m/3 https://chat.example.com/c/2/m/3"),
      ["teamcraft:/c/2/m/3"]
    );
    let many: String = (1..10).map(|id| format!("teamcraft:/c/2/m/{id} ")).collect();
    assert_eq!(links(&many).len(), MAX_PREVIEWS);
  }

  fn links(body: &str) -> Vec<String> {
    message_links_in(body, &[HOST.to_owned()])
  }
}
