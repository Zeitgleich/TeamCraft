//! Profiles and avatars: title, department, phone, time zone and working hours, and the picture.
//!
//! The server owns them (they are directory data, not message content). The hub loads a profile the
//! first time a screen asks for it and keeps it for the session; avatars are fetched from their
//! presigned URL (the page's CSP only lets images come from `blob:` and `data:`) and shown from a
//! `blob:` URL. In the desktop app the shell moves the bytes to and from object storage: the
//! webview's CSP only lets it reach the shell.
//!
//! Avatars are also kept in the persistent media cache (#749, [`crate::persisted_media`]): the
//! cached picture shows as soon as a screen asks for it, before the profile has loaded; once the
//! profile names a newer version (`avatar_version`) it is downloaded in the background, and when
//! that fails (the server or its storage is down) the cached one stays instead of the initials.

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{
  AvatarContentType, AvatarResponse, CreateAvatarUploadRequest, CreateAvatarUploadResponse, ProfileResponse,
  UpdateProfileRequest, UserId,
};

use crate::auth::ApiMethod;
use crate::auth::fetch::fetch;
use crate::auth::tauri;
use crate::components::file_views::blob_url;
use crate::persisted_media::{self, CachedPicture, Freshness, MediaKey};
use crate::reactive::UpdateIfAlive;
use crate::rest::{RestClient, RestError, check};
use crate::space_images::revoke;

/// Largest avatar the picker accepts (the server has its own limit).
pub const MAX_AVATAR_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct ProfileHub {
  client: StoredValue<RestClient, LocalStorage>,
  profiles: RwSignal<HashMap<UserId, ProfileResponse>>,
  /// The `blob:` URL of each avatar shown, with the version it was made from (empty: unknown).
  avatars: RwSignal<HashMap<UserId, ShownAvatar>>,
  asked_profiles: StoredValue<HashSet<UserId>>,
  /// Users whose cached avatar was looked up this session.
  asked_cached: StoredValue<HashSet<UserId>>,
  /// Downloads started this session, per user and announced version.
  asked_avatars: StoredValue<HashSet<(UserId, Option<String>)>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShownAvatar {
  url: String,
  version: String,
}

impl ProfileHub {
  pub fn new(client: RestClient) -> Self {
    Self {
      client: StoredValue::new_local(client),
      profiles: RwSignal::new(HashMap::new()),
      avatars: RwSignal::new(HashMap::new()),
      asked_profiles: StoredValue::new(HashSet::new()),
      asked_cached: StoredValue::new(HashSet::new()),
      asked_avatars: StoredValue::new(HashSet::new()),
    }
  }

  /// The profile of a user, once loaded; asks for it the first time. Reactive.
  pub fn profile(&self, user: UserId) -> Option<ProfileResponse> {
    let known = self.profiles.with(|all| all.get(&user).cloned());
    if known.is_none() && self.asked_profiles.try_update_value(|asked| asked.insert(user)) == Some(true) {
      self.load_profile(user);
    }
    known
  }

  /// A `blob:` URL of the user's picture, if they have one and it has loaded (or is cached).
  /// Reactive.
  pub fn avatar_url(&self, user: UserId) -> Option<String> {
    let shown = self.avatars.with(|all| all.get(&user).cloned());
    if self.asked_cached.try_update_value(|asked| asked.insert(user)) == Some(true) {
      self.show_cached_avatar(user);
    }
    let Some(profile) = self.profile(user) else {
      // Not loaded (yet, or the server is out of reach): the cached picture is the best there is.
      return shown.map(|shown| shown.url);
    };
    if !profile.has_avatar {
      if shown.is_some() {
        // Removed on the server: the initials again, and the cached copy goes too.
        let hub = *self;
        spawn_local(async move { hub.forget_avatar(user) });
        persisted_media::remove(MediaKey::Avatar(user));
      }
      return None;
    }
    // Only people known to have a picture are asked for it, once per version and session.
    let version = profile.maybe_avatar_version;
    let is_current = shown
      .as_ref()
      .is_some_and(|shown| version.as_deref() == Some(shown.version.as_str()));
    if !is_current
      && self
        .asked_avatars
        .try_update_value(|asked| asked.insert((user, version.clone())))
        == Some(true)
    {
      self.load_avatar(user, version);
    }
    shown.map(|shown| shown.url)
  }

  /// Saves the signed-in user's profile (fields left out are cleared).
  pub async fn save(&self, request: UpdateProfileRequest) -> Result<(), RestError> {
    let client = self.client.get_value();
    let saved: ProfileResponse = client.put("/api/v1/me/profile", &request).await?;
    self.profiles.update_if_alive(|all| {
      all.insert(saved.user_id, saved);
    });
    Ok(())
  }

  /// Announces the image, uploads it to the presigned URL and makes it the avatar.
  pub async fn upload_avatar(
    &self,
    me: UserId,
    content_type: AvatarContentType,
    bytes: Vec<u8>,
  ) -> Result<(), RestError> {
    let profile = if tauri::is_available() {
      let answer = check(tauri::avatar_upload(&content_type, &bytes).await?)?;
      serde_json::from_str(&answer.body).map_err(|error| RestError::Unreadable(error.to_string()))?
    } else {
      self.upload_avatar_from_page(content_type, bytes).await?
    };
    self.forget_avatar(me);
    self.profiles.update_if_alive(|all| {
      all.insert(profile.user_id, profile);
    });
    Ok(())
  }

  /// The browser's way: the page reaches object storage itself.
  async fn upload_avatar_from_page(
    &self,
    content_type: AvatarContentType,
    bytes: Vec<u8>,
  ) -> Result<ProfileResponse, RestError> {
    let client = self.client.get_value();
    let announce = CreateAvatarUploadRequest {
      content_type: content_type.clone(),
      size_bytes: bytes.len() as u64,
    };
    let upload: CreateAvatarUploadResponse = client.post("/api/v1/me/avatar", &announce).await?;
    let headers = [("Content-Type".to_owned(), content_type.as_str().to_owned())];
    let stored = fetch("PUT", &upload.upload_url, &headers, Some(bytes))
      .await
      .map_err(RestError::Unreadable)?;
    if stored.status >= 300 {
      return Err(RestError::Refused {
        status: stored.status,
        code: "avatar_upload_failed".to_owned(),
      });
    }
    client
      .post("/api/v1/me/avatar/complete", &serde_json::json!({}))
      .await
  }

  pub async fn remove_avatar(&self, me: UserId) -> Result<(), RestError> {
    self
      .client
      .get_value()
      .send(ApiMethod::Delete, "/api/v1/me/avatar", None)
      .await?;
    self.forget_avatar(me);
    persisted_media::remove(MediaKey::Avatar(me));
    self.profiles.update_if_alive(|all| {
      if let Some(profile) = all.get_mut(&me) {
        profile.has_avatar = false;
        profile.maybe_avatar_version = None;
      }
    });
    Ok(())
  }

  fn forget_avatar(&self, user: UserId) {
    let mut maybe_removed = None;
    self
      .avatars
      .update_if_alive(|all| maybe_removed = all.remove(&user));
    if let Some(removed) = maybe_removed {
      revoke(&removed.url);
    }
    self.asked_avatars.try_update_value(|asked| {
      asked.retain(|(asked_user, _)| *asked_user != user);
    });
  }

  fn load_profile(&self, user: UserId) {
    let hub = *self;
    spawn_local(async move {
      let path = format!("/api/v1/users/{user}/profile");
      match hub.client.get_value().get::<ProfileResponse>(&path).await {
        Ok(profile) => hub.profiles.update_if_alive(|all| {
          all.insert(user, profile);
        }),
        Err(error) => tracing::debug!(%error, "profile not loaded"),
      }
    });
  }

  /// Shows the cached picture of `user` unless a downloaded one is already there.
  fn show_cached_avatar(&self, user: UserId) {
    let hub = *self;
    spawn_local(async move {
      if let Some(cached) = persisted_media::get(MediaKey::Avatar(user)).await {
        hub.show_avatar(user, &cached, ShowIf::NothingShown);
      }
    });
  }

  /// Downloads the picture of `user` in `maybe_version` (`None`: the server names none), unless the
  /// cache already has that version. A picture that doesn't load leaves what is shown: the cached
  /// one, or the initials. Nothing for the user to act on.
  fn load_avatar(&self, user: UserId, maybe_version: Option<String>) {
    let hub = *self;
    let key = MediaKey::Avatar(user);
    spawn_local(async move {
      if maybe_version.is_some()
        && let Some(cached) = persisted_media::get(key).await
        && cached.freshness(maybe_version.as_deref()) == Freshness::Current
      {
        hub.show_avatar(user, &cached, ShowIf::Always);
        return;
      }
      let Some((bytes, content_type)) = hub.download_avatar(user).await else {
        tracing::debug!("avatar not downloaded");
        return;
      };
      let picture = CachedPicture {
        version: maybe_version.unwrap_or_default(),
        content_type,
        bytes,
      };
      hub.show_avatar(user, &picture, ShowIf::Always);
      persisted_media::put(key, picture);
    });
  }

  fn show_avatar(&self, user: UserId, picture: &CachedPicture, condition: ShowIf) {
    let Some(url) = blob_url(&picture.bytes, &picture.content_type) else {
      return;
    };
    let shown = ShownAvatar {
      url: url.clone(),
      version: picture.version.clone(),
    };
    let mut maybe_replaced = None;
    let mut is_kept = false;
    self.avatars.update_if_alive(|all| {
      if condition == ShowIf::NothingShown && all.contains_key(&user) {
        return;
      }
      is_kept = true;
      maybe_replaced = all.insert(user, shown);
    });
    if !is_kept {
      revoke(&url);
    }
    if let Some(replaced) = maybe_replaced {
      revoke(&replaced.url);
    }
  }

  /// The picture's bytes and type: through the desktop shell, or the page's own fetch.
  async fn download_avatar(&self, user: UserId) -> Option<(Vec<u8>, String)> {
    if tauri::is_available() {
      let image = tauri::avatar_download(user).await.ok()?;
      return (image.status < 300).then_some((image.body, image.maybe_content_type?));
    }
    let path = format!("/api/v1/users/{user}/avatar");
    let location = self
      .client
      .try_get_value()?
      .get::<AvatarResponse>(&path)
      .await
      .ok()?;
    let image = fetch("GET", &location.download_url, &[], None).await.ok()?;
    (image.status < 300).then(|| (image.body, location.content_type.as_str().to_owned()))
  }
}

/// When a picture replaces the one shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShowIf {
  /// A downloaded or current picture: always.
  Always,
  /// The cached picture at start: only while nothing else is shown.
  NothingShown,
}

pub fn use_profiles() -> Option<ProfileHub> {
  use_context::<ProfileHub>()
}
