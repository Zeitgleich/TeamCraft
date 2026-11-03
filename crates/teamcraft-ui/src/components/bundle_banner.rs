//! A banner when the service worker could not verify the app code, and the About section with what
//! is running (docs/e2ee.md §9.3).

use leptos::prelude::*;

use crate::bundle::{Bundle, BundleState, Release, ReleaseUpdate, apply_update};
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;

/// Shown above everything when the worker rejected a bundle or the release build is unsigned.
#[component]
pub fn BundleBanner() -> impl IntoView {
  let i18n = use_i18n();
  let bundle = use_context::<Bundle>();
  move || {
    let state = bundle?.0.get();
    state.needs_warning().then(|| {
      let key = match &state {
        BundleState::Rejected { .. } => "bundle-rejected",
        _ => "bundle-unsigned",
      };
      let detail = match &state {
        BundleState::Rejected { reason } => reason.clone(),
        _ => String::new(),
      };
      view! {
        <div class="bundle-banner" role="alert">
          <Icon name=IconName::Alert />
          <span>{i18n.t(key)}</span>
          <span class="small mono" translate="no">{detail}</span>
        </div>
      }
    })
  }
}

/// "A new version is ready, reload" (browser): a new release never replaces the running page on its
/// own, so the user decides when (docs/e2ee.md §9.3). While the new release takes over the button
/// waits; if it doesn't, the banner says so and the button tries again (#1045).
#[component]
pub fn ReleaseBanner() -> impl IntoView {
  let i18n = use_i18n();
  let release = use_context::<Release>();
  let switch = PendingAction::new();
  move || {
    let text = match release?.0.get() {
      ReleaseUpdate::Current => return None,
      ReleaseUpdate::Ready => i18n.t("release-ready"),
      ReleaseUpdate::TooOld { minimum_version } => {
        i18n.t_with("release-too-old", &[("version", &minimum_version)])
      },
    };
    Some(view! {
      <div class="update-banner" role="status">
        <Icon name=IconName::Download />
        <span>{text}</span>
        <PendingButton pending=switch class="btn btn-primary btn-small" on_click=move |_| switch.spawn(apply_update())>
          {move || i18n.t("release-reload")}
        </PendingButton>
        {move || switch.maybe_failure().map(|_| view! {
          <span class="small update-banner-error" role="alert">{i18n.t("release-switch-failed")}</span>
        })}
      </div>
    })
  }
}

/// Version, the release key and the limits of the web client's integrity check.
#[component]
pub fn AboutSection() -> impl IntoView {
  let i18n = use_i18n();
  let bundle = use_context::<Bundle>();
  let status = move || match bundle.map(|bundle| bundle.0.get()).unwrap_or_default() {
    BundleState::Verified {
      version,
      key_fingerprint,
    } => i18n.t_with(
      "about-bundle-verified",
      &[("version", &version), ("key", &key_fingerprint)],
    ),
    BundleState::Unsigned => i18n.t("about-bundle-unsigned"),
    BundleState::Unsupported => i18n.t("about-bundle-unsupported"),
    BundleState::Rejected { reason } => i18n.t_with("about-bundle-rejected", &[("reason", &reason)]),
    BundleState::Unknown => i18n.t("about-bundle-desktop"),
  };
  view! {
    <h3 class="settings-heading">{move || i18n.t("about-title")}</h3>
    <p class="small" translate="no">{format!("TeamCraft {}", env!("CARGO_PKG_VERSION"))}</p>
    <p class="small" role="status">{status}</p>
    <p class="muted-text small">{move || i18n.t("about-trust-note")}</p>
    <h4 class="settings-heading">{move || i18n.t("about-notices")}</h4>
    <ul class="small muted-text" translate="no">
      <li>"jpeg-encoder: This software is based in part on the work of the Independent JPEG Group."</li>
      <li>"OpenH264 Video Codec provided by Cisco Systems, Inc. Downloaded from Cisco at runtime, not shipped."</li>
      <li>"libbz2-rs-sys: Copyright (C) 1996-2010 Julian R Seward; Rust port by the Trifecta Tech Foundation."</li>
    </ul>
  }
}
