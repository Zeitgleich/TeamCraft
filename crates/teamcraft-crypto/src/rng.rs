//! Bridges the crate's RNG interface (`rand_core` 0.6, shared with ed25519-dalek 2 and openmls) to
//! dependencies that take a `rand_core` 0.10 RNG (x25519-dalek 3).
//!
//! Not a generator of its own: every byte comes unchanged from the caller's RNG, so swapping the
//! trait version changes no output.

use std::convert::Infallible;

use rand_core::CryptoRngCore;
use rand_core_010::{TryCryptoRng, TryRng};

/// Lends a caller's cryptographic RNG to an API that takes a `rand_core` 0.10 `CryptoRng`.
pub(crate) struct CompatRng<'a, R: ?Sized>(pub(crate) &'a mut R);

impl<R: CryptoRngCore + ?Sized> TryRng for CompatRng<'_, R> {
  type Error = Infallible;

  fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
    Ok(self.0.next_u32())
  }

  fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
    Ok(self.0.next_u64())
  }

  fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
    // Like x25519-dalek 2, which called the same infallible `fill_bytes` (the OS RNG panics on
    // failure rather than returning weak bytes).
    self.0.fill_bytes(dst);
    Ok(())
  }
}

// The wrapped RNG is a `CryptoRngCore`, so its cryptographic guarantee carries over.
impl<R: CryptoRngCore + ?Sized> TryCryptoRng for CompatRng<'_, R> {}

#[cfg(test)]
mod tests {
  use rand_core::RngCore;
  use rand_core_010::Rng as _;

  use super::*;

  #[test]
  fn passes_the_callers_bytes_through_unchanged() {
    let mut direct = CountingRng(0);
    let mut expected = [0u8; 40];
    direct.fill_bytes(&mut expected);

    let mut wrapped = CountingRng(0);
    let mut actual = [0u8; 40];
    CompatRng(&mut wrapped).fill_bytes(&mut actual);
    assert_eq!(actual, expected);
    assert_eq!(CompatRng(&mut wrapped).next_u64(), direct.next_u64());
    assert_eq!(CompatRng(&mut wrapped).next_u32(), direct.next_u32());
  }

  /// Deterministic byte counter standing in for the OS RNG. Test-only: not a CSPRNG.
  struct CountingRng(u8);

  impl RngCore for CountingRng {
    fn next_u32(&mut self) -> u32 {
      rand_core::impls::next_u32_via_fill(self)
    }

    fn next_u64(&mut self) -> u64 {
      rand_core::impls::next_u64_via_fill(self)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
      for byte in dest {
        *byte = self.0;
        self.0 = self.0.wrapping_add(1);
      }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
      self.fill_bytes(dest);
      Ok(())
    }
  }

  impl rand_core::CryptoRng for CountingRng {}
}
