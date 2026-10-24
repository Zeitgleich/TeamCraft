//! Profiles and avatars: title, department, phone, time zone and working hours, and the picture.
//!
//! The server owns them (they are directory data, not message content). The hub loads a profile the
//! first time a screen asks for it and keeps it for the session; avatars are fetched from their
//! presigned URL (the page's CSP only lets images come from `blob:` and `data:`) and shown from a
//! `blob:` URL.

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{
  AvatarContentType, AvatarResponse, CreateAvatarUploadRequest, CreateAvatarUploadResponse, ProfileResponse,
  UpdateProfileRequest, UserId,
};

use crate::auth::ApiMethod;
use crate::auth::fetch::fetch;
use crate::components::file_views::blob_url;
use crate::reactive::UpdateIfAlive;
use crate::rest::{RestClient, RestError};

/// Largest avatar the picker accepts (the server has its own limit).
pub const MAX_AVATAR_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct ProfileHub {
  client: StoredValue<RestClient, LocalStorage>,
  profiles: RwSignal<HashMap<UserId, ProfileResponse>>,
  avatars: RwSignal<HashMap<UserId, String>>,
  asked_profiles: StoredValue<HashSet<UserId>>,
  asked_avatars: StoredValue<HashSet<UserId>>,
}

impl ProfileHub {
  pub fn new(client: RestClient) -> Self {
    Self {
      client: StoredValue::new_local(client),
      profiles: RwSignal::new(HashMap::new()),
      avatars: RwSignal::new(HashMap::new()),
      asked_profiles: StoredValue::new(HashSet::new()),
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

  /// A `blob:` URL of the user's picture, if they have one and it has loaded. Reactive.
  pub fn avatar_url(&self, user: UserId) -> Option<String> {
    let url = self.avatars.with(|all| all.get(&user).cloned());
    if url.is_some() {
      return url;
    }
    // Only people known to have a picture are asked for it.
    let has_avatar = self.profile(user).is_some_and(|profile| profile.has_avatar);
    if has_avatar && self.asked_avatars.try_update_value(|asked| asked.insert(user)) == Some(true) {
      self.load_avatar(user);
    }
    None
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
    let profile: ProfileResponse = client
      .post("/api/v1/me/avatar/complete", &serde_json::json!({}))
      .await?;
    self.forget_avatar(me);
    self.profiles.update_if_alive(|all| {
      all.insert(profile.user_id, profile);
    });
    Ok(())
  }

  pub async fn remove_avatar(&self, me: UserId) -> Result<(), RestError> {
    self
      .client
      .get_value()
      .send(ApiMethod::Delete, "/api/v1/me/avatar", None)
      .await?;
    self.forget_avatar(me);
    self.profiles.update_if_alive(|all| {
      if let Some(profile) = all.get_mut(&me) {
        profile.has_avatar = false;
      }
    });
    Ok(())
  }

  fn forget_avatar(&self, user: UserId) {
    self.avatars.update(|all| {
      all.remove(&user);
    });
    self.asked_avatars.update_value(|asked| {
      asked.remove(&user);
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

  fn load_avatar(&self, user: UserId) {
    let hub = *self;
    spawn_local(async move {
      let path = format!("/api/v1/users/{user}/avatar");
      let Ok(location) = hub.client.get_value().get::<AvatarResponse>(&path).await else {
        return;
      };
      match fetch("GET", &location.download_url, &[], None).await {
        Ok(image) if image.status < 300 => {
          if let Some(url) = blob_url(&image.body, location.content_type.as_str()) {
            hub.avatars.update_if_alive(|all| {
              all.insert(user, url);
            });
          }
        },
        Ok(_) | Err(_) => tracing::debug!("avatar not downloaded"),
      }
    });
  }
}

pub fn use_profiles() -> Option<ProfileHub> {
  use_context::<ProfileHub>()
}

/// The image type of a picked file, if the server accepts it.
pub fn avatar_type(media_type: &str) -> Option<AvatarContentType> {
  match media_type {
    "image/png" => Some(AvatarContentType::Png),
    "image/jpeg" => Some(AvatarContentType::Jpeg),
    "image/webp" => Some(AvatarContentType::Webp),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_the_servers_image_types_are_avatars() {
    assert_eq!(avatar_type("image/png"), Some(AvatarContentType::Png));
    assert_eq!(avatar_type("image/gif"), None);
    assert_eq!(avatar_type("text/html"), None);
  }
}
