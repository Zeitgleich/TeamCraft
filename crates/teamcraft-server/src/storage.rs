//! Presigned S3 URLs for attachments. The server signs, clients move the bytes; the server never
//! sees the data or the keys (e2ee.md §4.5). The server itself only stores public objects this
//! way: the brand images admins upload (docs/branding.md §2.1).

use std::time::Duration;

use reqwest::StatusCode;
use rusty_s3::actions::CreateMultipartUpload;
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
  #[error("the stored object is larger than expected")]
  TooLarge,
  #[error("the storage's answer could not be read: {0}")]
  Answer(String),
  /// The storage refused a request of an upload in parts (4xx, or an error in a 200 answer): the
  /// upload is gone (`NoSuchUpload`), a part is missing (`InvalidPart`) or too small. Carries the
  /// S3 error code.
  #[error("the storage refused the upload in parts: {0}")]
  MultipartRefused(String),
}

/// Signs requests against one bucket.
pub struct BlobStore {
  bucket: Bucket,
  credentials: Credentials,
  expiry: Duration,
  multipart_expiry: Duration,
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
      multipart_expiry: Duration::from_secs(config.multipart_expiry_secs),
      max_attachment_bytes: config.max_attachment_bytes,
      http: reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?,
    })
  }

  pub fn expiry(&self) -> Duration {
    self.expiry
  }

  /// How long an unfinished upload in parts waits for its next part.
  pub fn multipart_expiry(&self) -> Duration {
    self.multipart_expiry
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

  /// Starts an upload in parts to `key` and returns the storage's upload ID.
  pub async fn create_multipart(&self, key: &str) -> Result<String, StorageError> {
    let url = self
      .bucket
      .create_multipart_upload(Some(&self.credentials), key)
      .sign(self.expiry);
    let response = self.http.post(url).send().await?;
    let body = success_body(response).await?;
    CreateMultipartUpload::parse_response(&body)
      .map(|created| created.upload_id().to_owned())
      .map_err(|error| StorageError::Answer(error.to_string()))
  }

  /// A URL that accepts part `part_number` of an upload in parts, of exactly `content_length`
  /// bytes, until it expires. The storage answers with the part's `ETag`.
  pub fn presign_upload_part(
    &self,
    key: &str,
    upload_id: &str,
    part_number: u16,
    content_length: u64,
  ) -> Url {
    let mut action = self
      .bucket
      .upload_part(Some(&self.credentials), key, part_number, upload_id);
    action
      .headers_mut()
      .insert("content-length", content_length.to_string());
    action.sign(self.expiry)
  }

  /// Joins the uploaded parts, given by their `ETag`s in part order (1, 2, …), into the object.
  pub async fn complete_multipart(
    &self,
    key: &str,
    upload_id: &str,
    etags: &[String],
  ) -> Result<(), StorageError> {
    let action = self.bucket.complete_multipart_upload(
      Some(&self.credentials),
      key,
      upload_id,
      etags.iter().map(String::as_str),
    );
    let url = action.sign(self.expiry);
    let response = self.http.post(url).body(action.body()).send().await?;
    let body = success_body(response).await?;
    // S3 may report a failed completion in a 200 answer.
    if body.contains("<Error>") {
      return Err(refusal(&body));
    }
    Ok(())
  }

  /// Aborts an upload in parts and frees its parts; one the storage doesn't know counts as aborted.
  pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<(), StorageError> {
    let url = self
      .bucket
      .abort_multipart_upload(Some(&self.credentials), key, upload_id)
      .sign(self.expiry);
    let response = self.http.delete(url).send().await?;
    match response.status() {
      StatusCode::NOT_FOUND => Ok(()),
      status if status.is_success() => Ok(()),
      status => Err(StorageError::Status(status)),
    }
  }

  pub fn presign_download(&self, key: &str) -> Url {
    self
      .bucket
      .get_object(Some(&self.credentials), key)
      .sign(self.expiry)
  }

  /// Stores an object the server itself writes (brand images).
  pub async fn put_object(&self, key: &str, bytes: Vec<u8>, content_type: &str) -> Result<(), StorageError> {
    let url = self.presign_typed_upload(key, bytes.len() as u64, content_type);
    let response = self
      .http
      .put(url)
      .header(reqwest::header::CONTENT_TYPE, content_type)
      .body(bytes)
      .send()
      .await?;
    match response.status() {
      status if status.is_success() => Ok(()),
      status => Err(StorageError::Status(status)),
    }
  }

  /// Reads an object the server wrote, if it exists and is at most `max_bytes` long.
  pub async fn get_object(&self, key: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, StorageError> {
    let response = self.http.get(self.presign_download(key)).send().await?;
    match response.status() {
      StatusCode::NOT_FOUND => Ok(None),
      status if status.is_success() => {
        if response.content_length().is_some_and(|length| length > max_bytes) {
          return Err(StorageError::TooLarge);
        }
        let bytes = response.bytes().await?;
        if bytes.len() as u64 > max_bytes {
          return Err(StorageError::TooLarge);
        }
        Ok(Some(bytes.to_vec()))
      },
      status => Err(StorageError::Status(status)),
    }
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

/// The body of a successful answer; a refusal (4xx) is [`StorageError::MultipartRefused`].
async fn success_body(response: reqwest::Response) -> Result<String, StorageError> {
  let status = response.status();
  let body = response.text().await?;
  if status.is_success() {
    Ok(body)
  } else if status.is_client_error() {
    Err(refusal(&body))
  } else {
    Err(StorageError::Status(status))
  }
}

/// The refusal an S3 error document describes, by its error code.
fn refusal(body: &str) -> StorageError {
  let code = body
    .split_once("<Code>")
    .and_then(|(_, rest)| rest.split_once("</Code>"))
    .map_or("unknown", |(code, _)| code);
  StorageError::MultipartRefused(code.to_owned())
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
      multipart_expiry_secs: 3600,
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
  fn part_urls_name_the_upload_and_part_and_sign_the_length() {
    let url = store().presign_upload_part("attachments/42", "upload-1", 3, 5_242_880);
    let query = url.query().unwrap();
    assert!(query.contains("partNumber=3"), "{query}");
    assert!(query.contains("uploadId=upload-1"), "{query}");
    assert!(
      query.contains("X-Amz-SignedHeaders=content-length%3Bhost"),
      "{query}"
    );
  }

  #[test]
  fn refusals_carry_the_storage_error_code() {
    let refused = "<Error><Code>NoSuchUpload</Code><Message>gone</Message></Error>";
    assert!(matches!(
      refusal(refused),
      StorageError::MultipartRefused(code) if code == "NoSuchUpload"
    ));
    assert!(matches!(
      refusal("not xml"),
      StorageError::MultipartRefused(code) if code == "unknown"
    ));
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
      multipart_expiry_secs: 60,
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
      multipart_expiry_secs: 60,
    };
    assert!(BlobStore::new(&config("not a url")).is_err());
    assert!(BlobStore::new(&config("ftp://example.com")).is_err());
  }
}
