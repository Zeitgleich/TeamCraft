//! Presigned S3 URLs for attachments. The server signs, clients move the bytes; the server never
//! sees the data or the keys (e2ee.md §4.5).

use std::time::Duration;

use reqwest::StatusCode;
use rusty_s3::{Bucket, BucketError, Credentials, S3Action, UrlStyle};
use secrecy::ExposeSecret;
use url::Url;

use crate::config::StorageConfig;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
  #[error("invalid storage endpoint: {0}")]
  Endpoint(String),
  #[error("storage request failed: {0}")]
  Request(#[from] reqwest::Error),
  #[error("storage answered with status {0}")]
  Status(StatusCode),
}

/// Signs requests against one bucket.
pub struct BlobStore {
  bucket: Bucket,
  credentials: Credentials,
  expiry: Duration,
  max_attachment_bytes: u64,
  http: reqwest::Client,
}

impl BlobStore {
  pub fn new(config: &StorageConfig) -> Result<Self, StorageError> {
    let endpoint: Url = config
      .endpoint
      .parse()
      .map_err(|e: url::ParseError| StorageError::Endpoint(e.to_string()))?;
    let style = if config.path_style {
      UrlStyle::Path
    } else {
      UrlStyle::VirtualHost
    };
    let bucket = Bucket::new(endpoint, style, config.bucket.clone(), config.region.clone())
      .map_err(|e: BucketError| StorageError::Endpoint(e.to_string()))?;
    Ok(Self {
      bucket,
      credentials: Credentials::new(config.access_key.clone(), config.secret_key.expose_secret()),
      expiry: Duration::from_secs(config.presign_expiry_secs),
      max_attachment_bytes: config.max_attachment_bytes,
      http: reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?,
    })
  }

  pub fn expiry(&self) -> Duration {
    self.expiry
  }

  pub fn max_attachment_bytes(&self) -> u64 {
    self.max_attachment_bytes
  }

  /// A URL that accepts uploads of exactly `content_length` bytes to `key` until it expires.
  /// The length is part of the signature, so the storage rejects other sizes. A presigned PUT can
  /// be repeated within its lifetime, also after the upload was completed; readers detect a swapped
  /// blob because the file's AEAD and hash are inside the encrypted message (e2ee.md §4.5), and
  /// the lifetime is short (`presign_expiry_secs`).
  pub fn presign_upload(&self, key: &str, content_length: u64) -> Url {
    let mut action = self.bucket.put_object(Some(&self.credentials), key);
    action
      .headers_mut()
      .insert("content-length", content_length.to_string());
    action.sign(self.expiry)
  }

  /// Like [`Self::presign_upload`], and the storage also rejects another `Content-Type`.
  pub fn presign_typed_upload(&self, key: &str, content_length: u64, content_type: &str) -> Url {
    let mut action = self.bucket.put_object(Some(&self.credentials), key);
    action
      .headers_mut()
      .insert("content-length", content_length.to_string());
    action
      .headers_mut()
      .insert("content-type", content_type.to_owned());
    action.sign(self.expiry)
  }

  pub fn presign_download(&self, key: &str) -> Url {
    self
      .bucket
      .get_object(Some(&self.credentials), key)
      .sign(self.expiry)
  }

  /// Deletes an object; a missing object counts as deleted.
  pub async fn delete_object(&self, key: &str) -> Result<(), StorageError> {
    let url = self
      .bucket
      .delete_object(Some(&self.credentials), key)
      .sign(self.expiry);
    let response = self.http.delete(url).send().await?;
    match response.status() {
      StatusCode::NOT_FOUND => Ok(()),
      status if status.is_success() => Ok(()),
      status => Err(StorageError::Status(status)),
    }
  }

  /// The size of the stored object, `None` if it doesn't exist (yet).
  pub async fn object_size(&self, key: &str) -> Result<Option<u64>, StorageError> {
    let url = self
      .bucket
      .head_object(Some(&self.credentials), key)
      .sign(self.expiry);
    let response = self.http.head(url).send().await?;
    match response.status() {
      StatusCode::NOT_FOUND => Ok(None),
      // reqwest's `content_length()` is the (empty) body of a HEAD response; the header has the size.
      status if status.is_success() => Ok(
        response
          .headers()
          .get(reqwest::header::CONTENT_LENGTH)
          .and_then(|value| value.to_str().ok())
          .and_then(|value| value.parse().ok()),
      ),
      status => Err(StorageError::Status(status)),
    }
  }
}

#[cfg(test)]
mod tests {
  use secrecy::SecretString;

  use super::*;

  fn store() -> BlobStore {
    BlobStore::new(&StorageConfig {
      endpoint: "http://storage.example.com:3900".into(),
      region: "garage".into(),
      bucket: "bucket".into(),
      access_key: "GKtest".into(),
      secret_key: SecretString::from("secret-value"),
      path_style: true,
      presign_expiry_secs: 600,
      max_attachment_bytes: 1000,
    })
    .unwrap()
  }

  #[test]
  fn upload_urls_sign_the_content_length_and_expire() {
    let url = store().presign_upload("attachments/42", 1234);
    let query = url.query().unwrap();
    assert_eq!(url.path(), "/bucket/attachments/42");
    assert!(query.contains("X-Amz-Expires=600"), "{query}");
    assert!(query.contains("X-Amz-Signature="), "{query}");
    assert!(
      query.contains("X-Amz-SignedHeaders=content-length%3Bhost"),
      "{query}"
    );
    assert!(!query.contains("secret-value"));
  }

  #[test]
  fn different_lengths_give_different_signatures() {
    let signature = |length| {
      let url = store().presign_upload("k", length);
      url
        .query_pairs()
        .find(|(name, _)| name == "X-Amz-Signature")
        .unwrap()
        .1
        .into_owned()
    };
    assert_ne!(signature(10), signature(11));
  }

  #[test]
  fn download_urls_are_signed_and_ipv6_endpoints_work() {
    let url = store().presign_download("attachments/42");
    assert!(url.query().unwrap().contains("X-Amz-Signature="));

    let v6 = BlobStore::new(&StorageConfig {
      endpoint: "http://[::1]:3900".into(),
      region: "garage".into(),
      bucket: "bucket".into(),
      access_key: "k".into(),
      secret_key: SecretString::from("s"),
      path_style: true,
      presign_expiry_secs: 60,
      max_attachment_bytes: 1,
    })
    .unwrap();
    assert_eq!(v6.presign_download("x").host_str(), Some("[::1]"));
  }

  #[test]
  fn invalid_endpoints_are_rejected() {
    let config = |endpoint: &str| StorageConfig {
      endpoint: endpoint.into(),
      region: "r".into(),
      bucket: "b".into(),
      access_key: "k".into(),
      secret_key: SecretString::from("s"),
      path_style: true,
      presign_expiry_secs: 60,
      max_attachment_bytes: 1,
    };
    assert!(BlobStore::new(&config("not a url")).is_err());
    assert!(BlobStore::new(&config("ftp://example.com")).is_err());
  }
}
