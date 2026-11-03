//! 16-bit PCM mono WAV, the format every transcription model on OpenRouter takes.

/// Bytes of the RIFF/WAVE header in front of the samples.
const HEADER_BYTES: usize = 44;

/// `samples` (mono, `sample_rate_hz`) as a WAV file in memory.
pub fn encode_wav(samples: &[i16], sample_rate_hz: u32) -> Vec<u8> {
  let data_bytes = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX - HEADER_BYTES as u32);
  let mut wav = Vec::with_capacity(HEADER_BYTES + samples.len() * 2);
  wav.extend_from_slice(b"RIFF");
  wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
  wav.extend_from_slice(b"WAVE");
  wav.extend_from_slice(b"fmt ");
  wav.extend_from_slice(&16u32.to_le_bytes());
  // PCM, one channel.
  wav.extend_from_slice(&1u16.to_le_bytes());
  wav.extend_from_slice(&1u16.to_le_bytes());
  wav.extend_from_slice(&sample_rate_hz.to_le_bytes());
  // Bytes per second and per frame, bits per sample.
  wav.extend_from_slice(&(sample_rate_hz * 2).to_le_bytes());
  wav.extend_from_slice(&2u16.to_le_bytes());
  wav.extend_from_slice(&16u16.to_le_bytes());
  wav.extend_from_slice(b"data");
  wav.extend_from_slice(&data_bytes.to_le_bytes());
  for sample in samples {
    wav.extend_from_slice(&sample.to_le_bytes());
  }
  wav
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_header_describes_16_bit_mono_pcm() {
    let wav = encode_wav(&[0, 1, -1, i16::MAX], 16_000);
    assert_eq!(wav.len(), 44 + 8);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 8);
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 32_000);
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16);
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8);
    assert_eq!(&wav[44..48], &[0, 0, 1, 0]);
    assert_eq!(&wav[48..52], &[0xff, 0xff, 0xff, 0x7f]);
  }
}
