//! Sealing and opening cache entries.
//!
//! An entry's plaintext is `format (1) || name || version || content type || written (u64 ms) ||
//! bytes`, each of the three strings prefixed with its length (u32, big-endian). It is sealed by
//! `teamcraft-crypto::local_store` with the media cache's own keys
//! ([`LocalStoreKeys::derive_media_cache`]) under the lookup ID of the key's name, so the entry
//! can't be moved to another key unnoticed. The name inside is checked again on opening; the time
//! it was written bounds its age ([`super::CachePolicy::max_age`]).

use rand_core::OsRng;
use teamcraft_crypto::local_store::{LOOKUP_ID_LEN, LocalStoreKeys, LookupId};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use zeroize::Zeroizing;

use super::{CachedMedia, MediaCacheError, MediaKey};

/// Lookup namespace of the media cache, apart from the store's records (`store::sealed`).
const NAMESPACE: &str = "media_cache";
const FORMAT: u8 = 1;
/// Longest name, version or content type an entry carries; they are short identifiers.
const MAX_FIELD_BYTES: usize = 1024;

/// Seals and opens entries with keys derived from the local database key.
pub struct MediaSealer {
  keys: LocalStoreKeys,
}

impl MediaSealer {
  pub fn new(database_key: &SymmetricKey) -> Self {
    Self {
      keys: LocalStoreKeys::derive_media_cache(database_key),
    }
  }

  /// The pseudorandom ID the entry of `key` is stored under (a file or record name).
  pub fn lookup_id(&self, key: &MediaKey) -> LookupId {
    self.keys.lookup_id(NAMESPACE, key.name().as_bytes())
  }

  /// The sealed entry for `media` under `key`, written at `written_ms` (Unix milliseconds).
  pub fn seal(
    &self,
    key: &MediaKey,
    media: &CachedMedia,
    written_ms: u64,
  ) -> Result<Vec<u8>, MediaCacheError> {
    let name = key.name();
    let fields = [
      name.as_bytes(),
      media.version.as_bytes(),
      media.content_type.as_bytes(),
    ];
    if fields.iter().any(|field| field.len() > MAX_FIELD_BYTES) {
      return Err(MediaCacheError::BadEntry);
    }
    let mut plaintext = Zeroizing::new(Vec::with_capacity(
      1 + fields.iter().map(|field| 4 + field.len()).sum::<usize>() + 8 + media.bytes.len(),
    ));
    plaintext.push(FORMAT);
    for field in fields {
      // Invariant: fields are at most MAX_FIELD_BYTES long, far below u32::MAX.
      plaintext.extend_from_slice(&u32::try_from(field.len()).unwrap_or(u32::MAX).to_be_bytes());
      plaintext.extend_from_slice(field);
    }
    plaintext.extend_from_slice(&written_ms.to_be_bytes());
    plaintext.extend_from_slice(media.bytes.expose_secret());
    self
      .keys
      .seal(&self.lookup_id(key), &plaintext, &mut OsRng)
      .map_err(|_| MediaCacheError::BadEntry)
  }

  /// The entry of `key` in `sealed` and when it was written; [`MediaCacheError::BadEntry`] when it
  /// was tampered with, was sealed with another key or for another name.
  pub fn open(&self, key: &MediaKey, sealed: &[u8]) -> Result<(CachedMedia, u64), MediaCacheError> {
    let plaintext = self
      .keys
      .open(&self.lookup_id(key), sealed)
      .map_err(|_| MediaCacheError::BadEntry)?;
    let (&format, mut rest) = plaintext
      .expose_secret()
      .split_first()
      .ok_or(MediaCacheError::BadEntry)?;
    if format != FORMAT {
      return Err(MediaCacheError::BadEntry);
    }
    let name = take_field(&mut rest)?;
    if name != key.name() {
      return Err(MediaCacheError::BadEntry);
    }
    let version = take_field(&mut rest)?;
    let content_type = take_field(&mut rest)?;
    let (written, rest) = rest.split_first_chunk::<8>().ok_or(MediaCacheError::BadEntry)?;
    let media = CachedMedia {
      version,
      content_type,
      bytes: SecretBytes::new(rest.to_vec()),
    };
    Ok((media, u64::from_be_bytes(*written)))
  }
}

/// The next length-prefixed string of `rest`, which is advanced past it.
fn take_field(rest: &mut &[u8]) -> Result<String, MediaCacheError> {
  let (length, after) = rest.split_first_chunk::<4>().ok_or(MediaCacheError::BadEntry)?;
  let length = usize::try_from(u32::from_be_bytes(*length)).map_err(|_| MediaCacheError::BadEntry)?;
  if length > MAX_FIELD_BYTES || length > after.len() {
    return Err(MediaCacheError::BadEntry);
  }
  let (field, after) = after.split_at(length);
  *rest = after;
  String::from_utf8(field.to_vec()).map_err(|_| MediaCacheError::BadEntry)
}

/// A lookup ID as the 64 lowercase hex digits that name its file or record.
pub(super) fn hex_name(id: &LookupId) -> String {
  id.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The lookup ID a file or record is named after ([`hex_name`]), if it is named like an entry.
pub(super) fn lookup_id_of(name: &str) -> Option<LookupId> {
  if name.len() != LOOKUP_ID_LEN * 2 {
    return None;
  }
  let mut id = [0u8; LOOKUP_ID_LEN];
  let (pairs, _) = name.as_bytes().as_chunks::<2>();
  for (byte, pair) in id.iter_mut().zip(pairs) {
    *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
  }
  Some(LookupId(id))
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::UserId;

  use super::super::tests::id;
  use super::*;

  const PICTURE: &[u8] = b"\x89PNG a picture of somebody";

  #[test]
  fn an_entry_round_trips_and_its_bytes_are_not_readable_sealed() {
    let sealer = sealer(1);
    let key = MediaKey::Avatar(id::<UserId>(1));
    let sealed = sealer.seal(&key, &media("v1"), 5).expect("seal");
    assert!(
      !sealed.windows(PICTURE.len()).any(|window| window == PICTURE),
      "the picture is encrypted at rest"
    );
    assert!(
      !sealed.windows(2).any(|window| window == b"v1"),
      "so is its version"
    );
    let (opened, written_ms) = sealer.open(&key, &sealed).expect("open");
    assert_eq!(written_ms, 5);
    assert_eq!(
      (
        opened.version.as_str(),
        opened.content_type.as_str(),
        opened.bytes.expose_secret()
      ),
      ("v1", "image/png", PICTURE)
    );
  }

  #[test]
  fn an_entry_does_not_open_under_another_key_name_or_database_key() {
    let sealer = sealer(1);
    let key = MediaKey::Avatar(id::<UserId>(1));
    let sealed = sealer.seal(&key, &media("v1"), 5).expect("seal");
    let other_user = MediaKey::Avatar(id::<UserId>(2));
    assert!(matches!(
      sealer.open(&other_user, &sealed),
      Err(MediaCacheError::BadEntry)
    ));
    assert!(matches!(
      self::sealer(2).open(&key, &sealed),
      Err(MediaCacheError::BadEntry)
    ));
    let mut tampered = sealed.clone();
    if let Some(last) = tampered.last_mut() {
      *last ^= 1;
    }
    assert!(matches!(
      sealer.open(&key, &tampered),
      Err(MediaCacheError::BadEntry)
    ));
    assert!(matches!(sealer.open(&key, &[]), Err(MediaCacheError::BadEntry)));
  }

  #[test]
  fn lookup_ids_hide_the_key_and_differ_per_database_key() {
    let key = MediaKey::Avatar(id::<UserId>(1));
    let name = hex_name(&sealer(1).lookup_id(&key));
    assert_eq!(name.len(), 64);
    assert!(!name.contains("avatar"));
    assert_ne!(name, hex_name(&sealer(2).lookup_id(&key)));
  }

  #[test]
  fn only_names_of_64_hex_digits_are_entries() {
    let id = sealer(1).lookup_id(&MediaKey::Avatar(super::super::tests::id::<UserId>(1)));
    assert_eq!(lookup_id_of(&hex_name(&id)), Some(id));
    assert!(lookup_id_of(&"0f".repeat(31)).is_none());
    assert!(lookup_id_of(&"zz".repeat(32)).is_none());
    assert!(lookup_id_of("").is_none());
  }

  #[test]
  fn truncated_fields_are_refused() {
    assert!(take_field(&mut &[0u8, 0, 0][..]).is_err());
    assert!(take_field(&mut &[0u8, 0, 0, 5, b'a'][..]).is_err());
    assert!(take_field(&mut &[0xffu8, 0xff, 0xff, 0xff, b'a'][..]).is_err());
    let mut rest = &[0u8, 0, 0, 1, b'a', b'b'][..];
    assert_eq!(take_field(&mut rest).expect("one field"), "a");
    assert_eq!(rest, b"b");
  }

  fn sealer(seed: u8) -> MediaSealer {
    MediaSealer::new(&SymmetricKey::from_bytes([seed; 32]))
  }

  fn media(version: &str) -> CachedMedia {
    CachedMedia {
      version: version.to_owned(),
      content_type: "image/png".to_owned(),
      bytes: SecretBytes::new(PICTURE.to_vec()),
    }
  }
}
