//! The files that tell Android and iOS which apps may open this server's links (docs/app-links.md).
//! Both are public, the same for every caller of a host, and only say what the admin settings say.
//!
//! They name places, not the whole site: the API, the sign-in's way back to the bare address, share
//! links (`/f/`, whose key is for the web page) and personal one-time links (`#invite=`) stay with
//! the browser. Keep the paths in step with `teamcraft-client-api::app_link` and the Android filter
//! (apps/android/app/build.gradle.kts).

use serde_json::{Value, json};
use teamcraft_proto::AppLinkSettings;

/// The places an app may claim, as Apple's pattern.
const APP_PATHS: [&str; 5] = ["/home", "/c/*", "/s/*", "/dm/*", "/meeting/*"];
/// What `/.well-known/assetlinks.json` grants: following links, not sharing sign-in credentials.
const ANDROID_RELATION: &str = "delegate_permission/common.handle_all_urls";

/// The body of `/.well-known/assetlinks.json`, or `None` when it would grant nothing.
pub fn asset_links(settings: &AppLinkSettings) -> Option<Value> {
  if !settings.is_enabled
    || settings.android_package_names.is_empty()
    || settings.android_cert_fingerprints.is_empty()
  {
    return None;
  }
  let statements: Vec<Value> = settings
    .android_package_names
    .iter()
    .map(|package_name| {
      json!({
        "relation": [ANDROID_RELATION],
        "target": {
          "namespace": "android_app",
          "package_name": package_name,
          "sha256_cert_fingerprints": settings.android_cert_fingerprints,
        },
      })
    })
    .collect();
  Some(Value::Array(statements))
}

/// The body of `/.well-known/apple-app-site-association`, or `None` without a team to name.
pub fn apple_site_association(settings: &AppLinkSettings) -> Option<Value> {
  if !settings.is_enabled || settings.apple_team_id.is_empty() {
    return None;
  }
  let mut components = vec![
    json!({ "/": "/f/*", "exclude": true }),
    json!({ "#": "*invite=*", "exclude": true }),
  ];
  components.extend(APP_PATHS.iter().map(|path| json!({ "/": path })));
  Some(json!({
    "applinks": {
      "details": [{
        "appIDs": [format!("{}.{}", settings.apple_team_id, settings.apple_bundle_id)],
        "components": components,
      }],
    },
  }))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn android_gets_every_package_with_every_certificate() {
    let links = asset_links(&AppLinkSettings::default()).unwrap();
    let statements = links.as_array().unwrap();
    assert_eq!(statements.len(), 2);
    for statement in statements {
      assert_eq!(statement["relation"][0], ANDROID_RELATION);
      assert_eq!(statement["target"]["namespace"], "android_app");
      assert_eq!(
        statement["target"]["sha256_cert_fingerprints"]
          .as_array()
          .unwrap()
          .len(),
        1
      );
    }
    assert_eq!(statements[0]["target"]["package_name"], "de.zeitgleich.teamcraft");
  }

  #[test]
  fn apple_excludes_what_stays_in_the_browser_before_it_claims_anything() {
    let settings = AppLinkSettings {
      apple_team_id: "ABCDE12345".to_owned(),
      ..AppLinkSettings::default()
    };
    let association = apple_site_association(&settings).unwrap();
    let details = &association["applinks"]["details"][0];
    assert_eq!(details["appIDs"][0], "ABCDE12345.de.zeitgleich.teamcraft");
    // The first component that matches decides.
    let components = details["components"].as_array().unwrap();
    assert_eq!(components[0]["/"], "/f/*");
    assert_eq!(components[0]["exclude"], true);
    assert_eq!(components[1]["#"], "*invite=*");
    assert_eq!(components[1]["exclude"], true);
    assert!(components.iter().all(|component| component["/"] != "/"));
  }

  #[test]
  fn a_file_that_would_grant_nothing_is_not_served() {
    let off = AppLinkSettings {
      is_enabled: false,
      apple_team_id: "ABCDE12345".to_owned(),
      ..AppLinkSettings::default()
    };
    assert_eq!(asset_links(&off), None);
    assert_eq!(apple_site_association(&off), None);
    assert_eq!(apple_site_association(&AppLinkSettings::default()), None);
    let no_certificates = AppLinkSettings {
      android_cert_fingerprints: Vec::new(),
      ..AppLinkSettings::default()
    };
    assert_eq!(asset_links(&no_certificates), None);
  }
}
