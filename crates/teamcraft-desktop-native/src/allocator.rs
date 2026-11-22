//! Gives the C allocator's free memory back to the system (Linux with glibc). glibc only returns
//! the top of its heap on its own; pages freed in the middle stay with the process until
//! `malloc_trim` is asked to release them. In the shell that was 18 MiB of 85 after a catch-up and
//! 40 channel switches (docs/performance.md, "Desktop on Linux: where the memory is"; #1708).

/// Releases the free pages of every glibc arena. Returns whether any memory went back. Takes a few
/// milliseconds on a heap of tens of MiB and holds each arena's lock meanwhile: call it off the
/// UI thread, and not on a hot path.
#[allow(unsafe_code)]
pub fn trim_allocator() -> bool {
  // SAFETY: `malloc_trim` takes no pointers and only works on glibc's own heap state under its
  // arena locks; it is thread-safe and may be called at any time (glibc manual, "Memory
  // Allocation Tunables"). A pad of 0 keeps nothing beyond what is in use.
  unsafe { libc::malloc_trim(0) == 1 }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn trimming_after_freeing_is_harmless() {
    let blocks: Vec<Vec<u8>> = (0..256).map(|_| vec![1; 64 * 1024]).collect();
    drop(blocks);
    // Whether anything went back depends on the heap; it must not fail or disturb allocation.
    let _was_released = trim_allocator();
    assert_eq!(vec![2_u8; 1024].len(), 1024);
  }
}
