//! The public download page behind a share link (`/f/<id>#<fragment>`, e2ee.md §4.5.1): no
//! account, no sign-in. It asks the server for the link's state, opens the fragment (with the
//! password, if the link has one), downloads the ciphertext and decrypts it in the browser. The
//! fragment stays in the page; only the share ID is sent to the server.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_core::platform;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_crypto::share_link::{OpenedShareLink, ShareLinkError, needs_password, open_fragment};
use teamcraft_proto::ShareInfoResponse;

use crate::auth::AppConfig;
use crate::components::login::AuthFrame;
use crate::i18n::use_i18n;
use crate::object_url::ObjectUrl;
use crate::reactive::SetIfAlive;
use crate::share_link::{MAX_PAGE_DOWNLOAD_BYTES, ShareClient, ShareError, decrypt_to_blob};

/// Lets the "working" state paint before the browser is busy deriving the password key.
const PAINT_DELAY: Duration = Duration::from_millis(40);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
  Loading,
  /// Waiting for the downloader (and the password, for a protected link).
  Ready,
  Working,
  Done,
  Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
  /// The fragment is not a valid link: cut off when copied, or not a share link at all.
  BadLink,
  Wrong(ShareError),
}

#[component]
pub fn ShareDownloadPage(id: String, fragment: String) -> impl IntoView {
  let i18n = use_i18n();
  crate::startup::first_view();
  let client = ShareClient::new(AppConfig::load());
  let is_protected = needs_password(&fragment);
  let phase = RwSignal::new(match is_protected {
    Ok(_) => Phase::Loading,
    Err(_) => Phase::Failed(Failure::BadLink),
  });
  let is_protected = is_protected.unwrap_or(false);
  let info = RwSignal::new(None::<ShareInfoResponse>);
  let password = RwSignal::new(String::new());
  let has_wrong_password = RwSignal::new(false);
  let file_name = RwSignal::new(None::<String>);
  let saved = StoredValue::new_local(None::<ObjectUrl>);
  let id = StoredValue::new(id);
  let fragment = StoredValue::new(fragment);
  let opened = StoredValue::new_local(None::<OpenedShareLink>);

  if phase.get_untracked() == Phase::Loading {
    let client = client.clone();
    spawn_local(async move {
      match client.info(&id.get_value()).await {
        Ok(response) if response.size_bytes > MAX_PAGE_DOWNLOAD_BYTES => {
          phase.set_if_alive(Phase::Failed(Failure::Wrong(ShareError::TooLarge)));
        },
        Ok(response) => {
          // Without a password the name is readable right away.
          if !is_protected {
            match open_fragment(&fragment.get_value(), None) {
              Ok(link) => {
                file_name.set_if_alive(Some(link.file_name.clone()));
                opened.set_value(Some(link));
              },
              Err(_) => {
                phase.set_if_alive(Phase::Failed(Failure::BadLink));
                return;
              },
            }
          }
          info.set_if_alive(Some(response));
          phase.set_if_alive(Phase::Ready);
        },
        Err(error) => phase.set_if_alive(Phase::Failed(Failure::Wrong(error))),
      }
    });
  }

  let start = {
    move |ev: leptos::ev::SubmitEvent| {
      ev.prevent_default();
      if phase.get_untracked() != Phase::Ready || (is_protected && password.get_untracked().is_empty()) {
        return;
      }
      has_wrong_password.set(false);
      phase.set(Phase::Working);
      let client = client.clone();
      spawn_local(async move {
        platform::sleep(PAINT_DELAY).await;
        if opened.with_value(Option::is_none) {
          let secret = SecretBytes::new(password.get_untracked().into_bytes());
          match open_fragment(&fragment.get_value(), Some(&secret)) {
            Ok(link) => {
              file_name.set_if_alive(Some(link.file_name.clone()));
              opened.set_value(Some(link));
            },
            Err(ShareLinkError::WrongPassword) => {
              has_wrong_password.set_if_alive(true);
              phase.set_if_alive(Phase::Ready);
              return;
            },
            Err(_) => {
              phase.set_if_alive(Phase::Failed(Failure::BadLink));
              return;
            },
          }
        }
        let outcome = fetch_and_decrypt(&client, &id.get_value(), opened).await;
        match outcome {
          Ok(blob) => {
            let Some(url) = ObjectUrl::from_blob(&blob) else {
              phase.set_if_alive(Phase::Failed(Failure::Wrong(ShareError::Damaged)));
              return;
            };
            saved.try_set_value(Some(url));
            phase.set_if_alive(Phase::Done);
          },
          Err(error) => phase.set_if_alive(Phase::Failed(Failure::Wrong(error))),
        }
      });
    }
  };

  view! {
    <AuthFrame>
      {move || match phase.get() {
        Phase::Loading => {
          view! {
            <p class="auth-status" role="status">
              <span class="spinner" aria-hidden="true"></span>
              <span>{move || i18n.t("share-page-loading")}</span>
            </p>
          }
          .into_any()
        },
        Phase::Ready | Phase::Working => {
          let is_working = phase.get() == Phase::Working;
          let size = info
            .get()
            .map(|info| crate::admin::format_bytes(info.size_bytes))
            .unwrap_or_default();
          view! {
            <form class="auth-form" on:submit=start.clone()>
              <h1>{move || i18n.t("share-page-title")}</h1>
              <p class="muted-text">{move || i18n.t_with("share-page-size", &[("size", size.as_str())])}</p>
              {move || file_name.get().map(|name| view! { <p class="share-page-name" translate="no">{name}</p> })}
              {is_protected.then(|| view! {
                <label for="share-password">{move || i18n.t("share-page-password")}</label>
                <input
                  id="share-password"
                  class="input"
                  type="password"
                  autocomplete="off"
                  data-autofocus
                  prop:value=move || password.get()
                  on:input=move |ev| password.set(event_target_value(&ev))
                />
                {move || has_wrong_password.get().then(|| view! {
                  <p role="alert">{move || i18n.t("share-page-wrong-password")}</p>
                })}
              })}
              <div class="auth-actions">
                <button type="submit" class="btn btn-primary" prop:disabled=is_working>
                  {move || i18n.t(if is_working { "share-page-working" } else { "share-page-download" })}
                </button>
              </div>
              <p class="muted-text small">{move || i18n.t("share-page-note")}</p>
            </form>
          }
          .into_any()
        },
        Phase::Done => {
          let href = saved.with_value(|url| url.as_ref().map(|url| url.as_str().to_owned())).unwrap_or_default();
          let name = file_name.get().unwrap_or_default();
          view! {
            <h1>{move || i18n.t("share-page-done-title")}</h1>
            <p class="share-page-name" translate="no">{name.clone()}</p>
            <div class="auth-actions">
              <a class="btn btn-primary" href=href download=name>
                {move || i18n.t("share-page-save")}
              </a>
            </div>
          }
          .into_any()
        },
        Phase::Failed(failure) => {
          let (title, text) = match failure {
            Failure::BadLink => ("share-page-failed-title", "share-page-bad-link"),
            Failure::Wrong(ShareError::Unavailable) => ("share-page-failed-title", "share-page-unavailable"),
            Failure::Wrong(ShareError::TooLarge) => ("share-page-failed-title", "share-page-too-large"),
            Failure::Wrong(ShareError::Network) => ("share-page-failed-title", "share-page-network"),
            Failure::Wrong(ShareError::Damaged) => ("share-page-failed-title", "share-page-damaged"),
          };
          view! {
            <h1>{move || i18n.t(title)}</h1>
            <p role="alert">{move || i18n.t(text)}</p>
          }
          .into_any()
        },
      }}
    </AuthFrame>
  }
}

/// Counts the download, fetches the ciphertext and opens it. The count happens once the password
/// has been right, so a mistyped one doesn't use up a limited link.
async fn fetch_and_decrypt(
  client: &ShareClient,
  id: &str,
  opened: StoredValue<Option<OpenedShareLink>, LocalStorage>,
) -> Result<web_sys::Blob, ShareError> {
  let download = client.start_download(id).await?;
  if download.size_bytes > MAX_PAGE_DOWNLOAD_BYTES {
    return Err(ShareError::TooLarge);
  }
  let ciphertext = client.ciphertext(&download.download_url).await?;
  opened
    .try_with_value(|link| link.as_ref().map(|link| decrypt_to_blob(link, &ciphertext)))
    .flatten()
    .unwrap_or(Err(ShareError::Damaged))
}
