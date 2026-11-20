//! Test support: a seeded link that loses in bursts and whose delay walks, as hotel or train Wi-Fi
//! (#2189's relay `Impairment` in `teamcraft-sfu`'s tests, the same model), for the deterministic
//! simulations of the estimator, the voice guard and the probes.

use std::time::Duration;

// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

/// Every [`BurstyLoss::STEP`] the average loss walks within `loss_percent` and the one-way delay
/// within `delay_ms`, by up to a quarter of each range. Losses come in bursts (Gilbert–Elliott: a
/// bad state of [`BurstyLoss::BURST_PACKETS`] packets on average that loses
/// [`BurstyLoss::BURST_LOSS`] of them).
#[derive(Debug)]
pub struct BurstyLoss {
  loss_percent: (f64, f64),
  delay_ms: (f64, f64),
  rng: u64,
  maybe_stepped_at: Option<Instant>,
  loss: f64,
  delay: Duration,
  is_bad: bool,
}

impl BurstyLoss {
  const STEP: Duration = Duration::from_millis(500);
  const BURST_LOSS: f64 = 0.5;
  const BURST_PACKETS: f64 = 4.0;

  pub fn new(seed: u64, loss_percent: (f64, f64), delay_ms: (f64, f64)) -> Self {
    let mut link = Self {
      loss_percent,
      delay_ms,
      rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
      maybe_stepped_at: None,
      loss: 0.0,
      delay: Duration::ZERO,
      is_bad: false,
    };
    link.loss = (loss_percent.0 + link.uniform() * (loss_percent.1 - loss_percent.0)) / 100.0;
    let delay_ms = delay_ms.0 + link.uniform() * (delay_ms.1 - delay_ms.0);
    link.delay = Duration::from_secs_f64(delay_ms / 1000.0);
    link
  }

  /// 1–3 % loss, 20–60 ms one way: the hotel link of #2189.
  pub fn hotel_wifi(seed: u64) -> Self {
    Self::new(seed, (1.0, 3.0), (20.0, 60.0))
  }

  /// Whether a packet sent at `now` is lost; walks the loss and the delay on.
  pub fn is_lost(&mut self, now: Instant) -> bool {
    let stepped_at = *self.maybe_stepped_at.get_or_insert(now);
    if now.saturating_duration_since(stepped_at) >= Self::STEP {
      self.maybe_stepped_at = Some(now);
      self.step();
    }
    let bad_share = (self.loss / Self::BURST_LOSS).min(0.9);
    let leave_bad = 1.0 / Self::BURST_PACKETS;
    let enter_bad = bad_share * leave_bad / (1.0 - bad_share);
    let roll = self.uniform();
    self.is_bad = if self.is_bad {
      roll >= leave_bad
    } else {
      roll < enter_bad
    };
    self.is_bad && self.uniform() < Self::BURST_LOSS
  }

  /// The one-way delay now.
  pub fn delay(&self) -> Duration {
    self.delay
  }

  fn step(&mut self) {
    let (low, high) = self.loss_percent;
    let loss_step = (self.uniform() - 0.5) / 2.0 * (high - low) / 100.0;
    self.loss = (self.loss + loss_step).clamp(low / 100.0, high / 100.0);
    let (shortest, longest) = self.delay_ms;
    let delay_step = (self.uniform() - 0.5) / 2.0 * (longest - shortest);
    let delay_ms = (self.delay.as_secs_f64() * 1000.0 + delay_step).clamp(shortest, longest);
    self.delay = Duration::from_secs_f64(delay_ms / 1000.0);
  }

  /// Uniform in `[0, 1)` (xorshift).
  fn uniform(&mut self) -> f64 {
    self.rng ^= self.rng << 13;
    self.rng ^= self.rng >> 7;
    self.rng ^= self.rng << 17;
    (self.rng >> 11) as f64 / (1u64 << 53) as f64
  }
}
