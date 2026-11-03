//! Links the operating system hands to the apps: any web page or app can fire one. The parser must
//! never panic, and what it accepts must survive its own round trip: a link that names an instance
//! reads back as the same place through both of its spellings, and is never navigated to without
//! the server in use being that instance.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_client_api::app_link::{AppLinkStep, next_step, parse_app_link};

fuzz_target!(|text: &str| {
  let Ok(link) = parse_app_link(text) else {
    return;
  };
  assert!(!link.path().contains(".."), "a traversal got through");
  if let Some(scheme) = link.scheme_link() {
    // Same place; the scheme form reads a server on this device as http, whatever it said before.
    assert_eq!(parse_app_link(&scheme).map(|read| read.path()), Ok(link.path()));
  }
  if let Some(url) = link.web_url() {
    assert_eq!(parse_app_link(&url).as_ref(), Ok(&link));
  }
  if link.maybe_origin().is_some() {
    assert_ne!(next_step(&link, None, false), AppLinkStep::Navigate);
  }
});
