//! Files the user picked in the browser, uploaded without reading them into memory whole: the
//! picked `File` stays a handle that uploads read in slices (one part at a time, issue #153). The
//! browser also keeps a copy in IndexedDB, so a reload continues the upload; where it can't (no
//! space, a private window), the file fails after a reload with "attach it again".

use std::collections::HashSet;

use zeroize::Zeroizing;

/// Takes a picked file for an upload and returns the handle its `UploadSource::Picked` names.
#[cfg(target_arch = "wasm32")]
pub fn register(blob: web_sys::Blob) -> String {
  use rand_core::{OsRng, RngCore};

  let handle = format!("picked-{:016x}{:016x}", OsRng.next_u64(), OsRng.next_u64());
  PICKED.with(|picked| picked.borrow_mut().insert(handle.clone(), blob.clone()));
  let stored = handle.clone();
  wasm_bindgen_futures::spawn_local(async move {
    if let Err(error) = crate::store::browser::picked::put(&stored, &blob).await {
      tracing::info!(%error, "a picked file is not kept for a reload");
    }
  });
  handle
}

/// The bytes `start..end` of a picked file that has `expected_len` bytes. Fails when the file is
/// gone (a reload without a kept copy) or changed.
#[cfg(target_arch = "wasm32")]
pub async fn read(handle: &str, start: u64, end: u64, expected_len: u64) -> Result<Zeroizing<Vec<u8>>, ()> {
  use wasm_bindgen_futures::JsFuture;

  let blob = blob(handle).await.ok_or(())?;
  if blob.size() as u64 != expected_len {
    tracing::warn!("a picked file changed since it was queued");
    return Err(());
  }
  // Invariant: file sizes are far below 2^53, so the offsets are exact as f64.
  let slice = blob
    .slice_with_f64_and_f64(start as f64, end as f64)
    .map_err(|_| ())?;
  let buffer = JsFuture::from(slice.array_buffer()).await.map_err(|_| ())?;
  Ok(Zeroizing::new(js_sys::Uint8Array::new(&buffer).to_vec()))
}

/// Native builds read files from paths; nothing is picked there.
#[cfg(not(target_arch = "wasm32"))]
pub async fn read(
  _handle: &str,
  _start: u64,
  _end: u64,
  _expected_len: u64,
) -> Result<Zeroizing<Vec<u8>>, ()> {
  Err(())
}

/// Drops the picked files no upload needs any more (uploaded, cancelled, or left by a tab that was
/// closed first), here and their kept copies.
#[cfg(target_arch = "wasm32")]
pub async fn forget_all_but(referenced: &HashSet<String>) {
  PICKED.with(|picked| {
    picked
      .borrow_mut()
      .retain(|handle, _| referenced.contains(handle))
  });
  let stored = match crate::store::browser::picked::handles().await {
    Ok(stored) => stored,
    Err(error) => {
      tracing::debug!(%error, "the kept picked files could not be listed");
      return;
    },
  };
  for handle in stored.iter().filter(|handle| !referenced.contains(*handle)) {
    if let Err(error) = crate::store::browser::picked::delete(handle).await {
      tracing::debug!(%error, "a kept picked file could not be deleted");
    }
  }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn forget_all_but(_referenced: &HashSet<String>) {}

#[cfg(target_arch = "wasm32")]
thread_local! {
  static PICKED: std::cell::RefCell<std::collections::HashMap<String, web_sys::Blob>> =
    std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The picked file: from this page, else the copy kept for a reload.
#[cfg(target_arch = "wasm32")]
async fn blob(handle: &str) -> Option<web_sys::Blob> {
  if let Some(blob) = PICKED.with(|picked| picked.borrow().get(handle).cloned()) {
    return Some(blob);
  }
  let kept = crate::store::browser::picked::get(handle)
    .await
    .map_err(|error| tracing::debug!(%error, "a kept picked file could not be read"))
    .ok()
    .flatten()?;
  PICKED.with(|picked| picked.borrow_mut().insert(handle.to_owned(), kept.clone()));
  Some(kept)
}
