//! QR codes as one SVG path, for the device link code (e2ee.md §5) and the recovery kit (§6.5).

use qrcode::{Color, QrCode};

/// Light modules around the code, so scanners find its edges.
const QUIET_ZONE: usize = 3;

/// A QR code of `data` as an SVG path of its dark modules, in a square of the returned size (the
/// quiet zone included). `None` when the data doesn't fit a QR code.
pub fn svg_path(data: &str) -> Option<QrPath> {
  let code = QrCode::new(data.as_bytes()).ok()?;
  let width = code.width();
  let path = code
    .to_colors()
    .into_iter()
    .enumerate()
    .filter(|(_, color)| *color == Color::Dark)
    .map(|(index, _)| {
      format!(
        "M{} {}h1v1h-1z",
        index % width + QUIET_ZONE,
        index / width + QUIET_ZONE
      )
    })
    .collect();
  Some(QrPath {
    size: width + 2 * QUIET_ZONE,
    path,
  })
}

/// A drawn QR code: the side of its square and the path of its dark modules.
pub struct QrPath {
  pub size: usize,
  pub path: String,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_code_has_its_quiet_zone_and_dark_modules() {
    let drawn = svg_path("TCRECOVERY1:00000000000000000000000000000000").unwrap();
    // A QR code of version v has 17 + 4v modules; the quiet zone comes on both sides.
    let modules = drawn.size - 2 * QUIET_ZONE;
    assert!(modules >= 21 && (modules - 17).is_multiple_of(4), "{modules}");
    assert!(
      drawn
        .path
        .starts_with(&format!("M{QUIET_ZONE} {QUIET_ZONE}h1v1h-1z"))
    );
  }
}
