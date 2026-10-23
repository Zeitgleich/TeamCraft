//! The built-in background images, drawn by code: no image files to license, ship or download,
//! and the same pictures on every platform (the browser too). Each is a simple scene in muted
//! colours, softened by a blur as if out of focus behind the person.

use super::{BackgroundImage, EffectsError};

/// Size the presets are drawn at; the compositor scales them to cover the frame.
pub const PRESET_WIDTH: u32 = 1280;
pub const PRESET_HEIGHT: u32 = 720;

/// A built-in background image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresetBackground {
  /// A bright room with a window and a desk.
  Office,
  /// Shelves full of books.
  Bookshelf,
  /// Sky at sunrise over a dark horizon.
  Sunrise,
  /// Tree trunks in green light.
  Forest,
}

/// An RGB canvas in floating point while drawing.
struct Canvas {
  width: usize,
  height: usize,
  pixels: Vec<[f32; 3]>,
}

impl PresetBackground {
  pub const ALL: [Self; 4] = [Self::Office, Self::Bookshelf, Self::Sunrise, Self::Forest];

  /// The image at `width` x `height` (e.g. small for a picker's thumbnails).
  pub fn render(self, width: u32, height: u32) -> Result<BackgroundImage, EffectsError> {
    let mut canvas = Canvas::new(width as usize, height as usize);
    match self {
      Self::Office => draw_office(&mut canvas),
      Self::Bookshelf => draw_bookshelf(&mut canvas),
      Self::Sunrise => draw_sunrise(&mut canvas),
      Self::Forest => draw_forest(&mut canvas),
    }
    // Out of focus: about 0.6 % of the width, so thumbnails look like the full image.
    let radius = (canvas.width / 160).max(1);
    canvas.blur(radius);
    canvas.blur(radius);
    BackgroundImage::new(width, height, canvas.into_rgba())
  }
}

fn draw_office(canvas: &mut Canvas) {
  // Warm grey wall, lighter towards the window.
  canvas.fill(|x, y| mix([0.80, 0.78, 0.74], [0.90, 0.89, 0.86], (1.0 - x) * 0.7 + y * 0.1));
  // Window: bright sky with a frame and a cross bar.
  canvas.rect(0.06, 0.12, 0.40, 0.62, [0.62, 0.66, 0.70]);
  canvas.fill_rect(0.075, 0.14, 0.385, 0.60, |_, y| {
    mix([0.78, 0.88, 0.97], [0.93, 0.96, 0.99], y)
  });
  canvas.rect(0.225, 0.14, 0.235, 0.60, [0.62, 0.66, 0.70]);
  canvas.rect(0.075, 0.36, 0.385, 0.375, [0.62, 0.66, 0.70]);
  // Desk and its shadow, a plant and a lamp.
  canvas.rect(0.0, 0.80, 1.0, 1.0, [0.52, 0.40, 0.30]);
  canvas.rect(0.0, 0.78, 1.0, 0.80, [0.42, 0.32, 0.24]);
  canvas.ellipse(0.80, 0.62, 0.07, 0.15, [0.30, 0.48, 0.32]);
  canvas.rect(0.775, 0.70, 0.825, 0.78, [0.72, 0.70, 0.66]);
  canvas.ellipse(0.60, 0.40, 0.05, 0.05, [0.98, 0.93, 0.80]);
  canvas.rect(0.597, 0.44, 0.603, 0.78, [0.35, 0.35, 0.35]);
}

fn draw_bookshelf(canvas: &mut Canvas) {
  canvas.fill(|_, y| mix([0.30, 0.22, 0.16], [0.22, 0.16, 0.12], y));
  let colours = [
    [0.55, 0.20, 0.18],
    [0.20, 0.32, 0.45],
    [0.72, 0.62, 0.40],
    [0.25, 0.40, 0.30],
    [0.60, 0.45, 0.30],
    [0.40, 0.25, 0.40],
    [0.80, 0.76, 0.66],
  ];
  let shelves = 4;
  for shelf in 0..shelves {
    let top = 0.05 + shelf as f32 * 0.24;
    let bottom = top + 0.20;
    let mut x = 0.02;
    let mut index = shelf * 13;
    while x < 0.98 {
      let width = 0.012 + unit(index, 1) * 0.022;
      let height = 0.6 + unit(index, 2) * 0.4;
      let colour = colours[(hash(index, 3) % colours.len() as u32) as usize];
      let shade = 0.85 + unit(index, 4) * 0.25;
      let right = (x + width).min(0.98);
      canvas.rect(
        x,
        bottom - (bottom - top) * height,
        right,
        bottom,
        colour.map(|c| c * shade),
      );
      x = right + 0.002;
      index += 1;
    }
    // The shelf board.
    canvas.rect(0.0, bottom, 1.0, bottom + 0.025, [0.45, 0.33, 0.24]);
  }
}

fn draw_sunrise(canvas: &mut Canvas) {
  let horizon = 0.66;
  canvas.fill(|_, y| {
    if y < horizon {
      let t = y / horizon;
      if t < 0.6 {
        mix([0.18, 0.26, 0.48], [0.62, 0.52, 0.66], t / 0.6)
      } else {
        mix([0.62, 0.52, 0.66], [0.98, 0.70, 0.45], (t - 0.6) / 0.4)
      }
    } else {
      mix(
        [0.20, 0.16, 0.20],
        [0.10, 0.09, 0.12],
        (y - horizon) / (1.0 - horizon),
      )
    }
  });
  // The sun's glow and disk, half above the horizon.
  canvas.glow(0.70, horizon, 0.30, [1.0, 0.82, 0.55], 0.55);
  canvas.ellipse_clipped(0.70, horizon, 0.05, 0.089, [1.0, 0.92, 0.75], horizon);
  // Hills in front.
  canvas.fill_where(
    |x, y| y > horizon - 0.04 * (1.0 + (x * 7.0).sin()) * 0.6 + 0.02,
    |_, y| mix([0.16, 0.13, 0.18], [0.08, 0.07, 0.10], y),
  );
}

fn draw_forest(canvas: &mut Canvas) {
  canvas.fill(|x, y| {
    mix(
      [0.62, 0.74, 0.52],
      [0.20, 0.34, 0.22],
      y * 0.8 + (x - 0.5).abs() * 0.3,
    )
  });
  // Light from the upper left.
  canvas.glow(0.25, 0.0, 0.6, [0.95, 0.97, 0.80], 0.35);
  // Trunks: far ones pale and thin, near ones dark and wide.
  for layer in 0..3 {
    let (count, width, colour) = match layer {
      0 => (14, 0.012, [0.42, 0.50, 0.36]),
      1 => (9, 0.022, [0.30, 0.32, 0.24]),
      _ => (5, 0.040, [0.18, 0.16, 0.12]),
    };
    for trunk in 0..count {
      let index = layer * 100 + trunk;
      let x = (trunk as f32 + unit(index, 5)) / count as f32;
      let width = width * (0.7 + unit(index, 6) * 0.6);
      canvas.rect(x - width / 2.0, 0.0, x + width / 2.0, 1.0, colour);
    }
  }
  // Ground.
  canvas.fill_where(
    |_, y| y > 0.86,
    |_, y| mix([0.22, 0.30, 0.18], [0.14, 0.18, 0.11], y),
  );
}

impl Canvas {
  fn new(width: usize, height: usize) -> Self {
    Self {
      width,
      height,
      pixels: vec![[0.0; 3]; width * height],
    }
  }

  /// Sets every pixel from its position (`x`, `y` in 0..1).
  fn fill(&mut self, colour: impl Fn(f32, f32) -> [f32; 3]) {
    self.fill_where(|_, _| true, colour);
  }

  fn fill_where(&mut self, inside: impl Fn(f32, f32) -> bool, colour: impl Fn(f32, f32) -> [f32; 3]) {
    let (width, height) = (self.width, self.height);
    for (index, pixel) in self.pixels.iter_mut().enumerate() {
      let x = (index % width) as f32 / width as f32;
      let y = (index / width) as f32 / height as f32;
      if inside(x, y) {
        *pixel = colour(x, y);
      }
    }
  }

  fn rect(&mut self, left: f32, top: f32, right: f32, bottom: f32, colour: [f32; 3]) {
    self.fill_rect(left, top, right, bottom, |_, _| colour);
  }

  fn fill_rect(
    &mut self,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    colour: impl Fn(f32, f32) -> [f32; 3],
  ) {
    self.fill_where(
      |x, y| x >= left && x < right && y >= top && y < bottom,
      |x, y| colour((x - left) / (right - left), (y - top) / (bottom - top)),
    );
  }

  fn ellipse(&mut self, centre_x: f32, centre_y: f32, radius_x: f32, radius_y: f32, colour: [f32; 3]) {
    self.ellipse_clipped(centre_x, centre_y, radius_x, radius_y, colour, 1.0);
  }

  /// An ellipse, only where `y` is above `clip_y`.
  fn ellipse_clipped(
    &mut self,
    centre_x: f32,
    centre_y: f32,
    radius_x: f32,
    radius_y: f32,
    colour: [f32; 3],
    clip_y: f32,
  ) {
    self.fill_where(
      |x, y| {
        let (dx, dy) = ((x - centre_x) / radius_x, (y - centre_y) / radius_y);
        dx * dx + dy * dy <= 1.0 && y <= clip_y
      },
      |_, _| colour,
    );
  }

  /// Brightens towards `colour` around a point, fading out over `radius` (in widths).
  fn glow(&mut self, centre_x: f32, centre_y: f32, radius: f32, colour: [f32; 3], strength: f32) {
    let aspect = self.height as f32 / self.width as f32;
    let (width, height) = (self.width, self.height);
    for (index, pixel) in self.pixels.iter_mut().enumerate() {
      let x = (index % width) as f32 / width as f32;
      let y = (index / width) as f32 / height as f32;
      let distance = ((x - centre_x).powi(2) + ((y - centre_y) * aspect).powi(2)).sqrt();
      let weight = (1.0 - distance / radius).clamp(0.0, 1.0).powi(2) * strength;
      *pixel = mix(*pixel, colour, weight);
    }
  }

  /// Box blur, horizontal then vertical; twice approximates a Gaussian.
  fn blur(&mut self, radius: usize) {
    self.pixels = blur_lines(&self.pixels, self.width, self.height, radius, true);
    self.pixels = blur_lines(&self.pixels, self.width, self.height, radius, false);
  }

  fn into_rgba(self) -> Vec<u8> {
    self
      .pixels
      .iter()
      .flat_map(|[r, g, b]| [channel(*r), channel(*g), channel(*b), 255])
      .collect()
  }
}

/// One blur pass along rows (`is_horizontal`) or columns, with a running sum per line.
fn blur_lines(
  pixels: &[[f32; 3]],
  width: usize,
  height: usize,
  radius: usize,
  is_horizontal: bool,
) -> Vec<[f32; 3]> {
  let (lines, length) = if is_horizontal {
    (height, width)
  } else {
    (width, height)
  };
  let at = |line: usize, position: usize| {
    if is_horizontal {
      line * width + position
    } else {
      position * width + line
    }
  };
  let mut out = vec![[0.0; 3]; pixels.len()];
  for line in 0..lines {
    for position in 0..length {
      let start = position.saturating_sub(radius);
      let end = (position + radius).min(length - 1);
      let mut sum = [0.0; 3];
      for sample in start..=end {
        let pixel = pixels[at(line, sample)];
        for channel in 0..3 {
          sum[channel] += pixel[channel];
        }
      }
      let count = (end - start + 1) as f32;
      out[at(line, position)] = sum.map(|value| value / count);
    }
  }
  out
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
  let t = t.clamp(0.0, 1.0);
  [0, 1, 2].map(|channel| a[channel] + (b[channel] - a[channel]) * t)
}

fn channel(value: f32) -> u8 {
  // In range after the clamp, so the cast can't truncate.
  (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A deterministic pseudo-random value for (`index`, `salt`), so presets look the same everywhere.
fn hash(index: usize, salt: u32) -> u32 {
  let mut value = (index as u32).wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
  value ^= value >> 16;
  value = value.wrapping_mul(0x7FEB_352D);
  value ^= value >> 15;
  value = value.wrapping_mul(0x846C_A68B);
  value ^ (value >> 16)
}

/// [`hash`] as a value in 0..1.
fn unit(index: usize, salt: u32) -> f32 {
  (hash(index, salt) % 10_000) as f32 / 10_000.0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn presets_render_at_any_size_and_differ() {
    let images: Vec<BackgroundImage> = PresetBackground::ALL
      .iter()
      .map(|preset| preset.render(160, 90).unwrap())
      .collect();
    for image in &images {
      assert_eq!((image.width(), image.height()), (160, 90));
      assert!(
        image
          .rgba()
          .as_chunks::<4>()
          .0
          .iter()
          .all(|pixel| pixel[3] == 255)
      );
    }
    for (index, image) in images.iter().enumerate() {
      for other in &images[index + 1..] {
        assert_ne!(image.rgba(), other.rgba());
      }
    }
  }

  #[test]
  fn presets_are_deterministic() {
    let first = PresetBackground::Bookshelf.render(64, 36).unwrap();
    let second = PresetBackground::Bookshelf.render(64, 36).unwrap();
    assert_eq!(first, second);
  }

  #[test]
  fn sunrise_is_bright_at_the_sun_and_dark_below() {
    let image = PresetBackground::Sunrise.render(320, 180).unwrap();
    let luma = |x: usize, y: usize| {
      let pixel = &image.rgba()[(y * 320 + x) * 4..][..3];
      pixel.iter().map(|value| u32::from(*value)).sum::<u32>()
    };
    assert!(luma(224, 110) > luma(224, 175) + 200);
  }
}
