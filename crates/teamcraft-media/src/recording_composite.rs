//! The picture of a call recording with video (issue #2250, e2ee.md §8.3): the tiles the
//! recording device shows, composited into one frame of a fixed size, as the call screen arranges
//! them.
//!
//! - **Order and layout:** screen shares first (the share layout: the first share large, the rest
//!   in a column beside it), then, in the active speaker layout, the one in focus large with the
//!   others in a row below, else a gallery grid. At most [`MAX_TILES`] tiles: the call screen pages
//!   beyond that, and a recording keeps the first page.
//! - **Left out:** a tile whose device isn't in the recording (it declined consent or hasn't
//!   answered, #2115) is never sampled: the cell is blanked and carries the label the app drew
//!   ("Not recorded", in its language: [`LabelMask`]).
//! - Pictures keep their aspect ratio inside their cell (bars around them); a tile without a picture
//!   yet is an empty cell.
//!
//! Plain Rust over I420 frames, no I/O: tested on the host, the same on every platform.

use crate::video::{VideoError, VideoFrame, VideoResolution};

/// The most tiles a recording shows: the call screen's first page of a gallery.
pub const MAX_TILES: usize = 16;
/// Pixels between cells (even: chroma planes are half the size).
const GAP: usize = 4;
/// The frame around and between cells: black.
const BACKGROUND_LUMA: u8 = 16;
/// A cell without a picture: dark grey.
const EMPTY_LUMA: u8 = 40;
/// A blanked cell (left out of the recording): a lighter grey, so it reads as deliberate.
const LEFT_OUT_LUMA: u8 = 56;
/// The label's text: near white.
const LABEL_LUMA: u8 = 235;
const NEUTRAL_CHROMA: u8 = 128;
/// The label takes at most this share of its cell's width ...
const LABEL_MAX_WIDTH_PERCENT: usize = 80;
/// ... and of its height.
const LABEL_MAX_HEIGHT_PERCENT: usize = 30;

/// How the call screen arranges the tiles (the user's choice there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SceneLayout {
  #[default]
  Gallery,
  ActiveSpeaker,
}

/// One tile to draw.
#[derive(Debug, Clone, Copy)]
pub struct SceneTile<'a> {
  pub maybe_picture: Option<&'a VideoFrame>,
  pub is_screen: bool,
  /// The active speaker: large in [`SceneLayout::ActiveSpeaker`].
  pub is_focused: bool,
  /// Not in the recording: blanked, with the label.
  pub is_left_out: bool,
}

/// The label of a blanked tile, as the app drew it in its language and font: one coverage byte
/// per pixel (0 transparent, 255 the text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelMask {
  width: usize,
  height: usize,
  coverage: Vec<u8>,
}

/// Draws scenes into a frame of one size, reusing its buffer.
pub struct Compositor {
  size: VideoResolution,
  data: Vec<u8>,
}

/// A cell of the frame, in luma pixels; every value even.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
  pub x: usize,
  pub y: usize,
  pub width: usize,
  pub height: usize,
}

impl LabelMask {
  /// `None` when the size doesn't match the bytes or is empty.
  pub fn new(width: usize, height: usize, coverage: Vec<u8>) -> Option<Self> {
    (width > 0 && height > 0 && width.checked_mul(height) == Some(coverage.len())).then_some(Self {
      width,
      height,
      coverage,
    })
  }
}

impl Compositor {
  pub fn new(size: VideoResolution) -> Result<Self, VideoError> {
    let data = vec![0; size.i420_bytes()?];
    Ok(Self { size, data })
  }

  /// The frame for `tiles` (in the call screen's order) at `timestamp_us`.
  pub fn compose(
    &mut self,
    tiles: &[SceneTile<'_>],
    layout: SceneLayout,
    maybe_label: Option<&LabelMask>,
    timestamp_us: u64,
  ) -> Result<VideoFrame, VideoError> {
    let (width, height) = (usize::from(self.size.width), usize::from(self.size.height));
    self.fill(
      Cell {
        x: 0,
        y: 0,
        width,
        height,
      },
      BACKGROUND_LUMA,
    );
    let ordered = in_screen_order(tiles);
    let cells = arrange(&ordered, layout, width, height);
    for (tile, cell) in ordered.iter().zip(cells) {
      self.draw_tile(tile, cell, maybe_label);
    }
    VideoFrame::new(self.size, timestamp_us, self.data.clone())
  }

  fn draw_tile(&mut self, tile: &SceneTile<'_>, cell: Cell, maybe_label: Option<&LabelMask>) {
    if tile.is_left_out {
      // Never sampled: nothing of the picture reaches the file.
      self.fill(cell, LEFT_OUT_LUMA);
      if let Some(label) = maybe_label {
        self.draw_label(label, cell);
      }
      return;
    }
    self.fill(cell, EMPTY_LUMA);
    if let Some(picture) = tile.maybe_picture {
      let fitted = fit(picture.resolution(), cell);
      self.draw_picture(picture, fitted);
    }
  }

  fn fill(&mut self, cell: Cell, luma: u8) {
    let width = usize::from(self.size.width);
    let height = usize::from(self.size.height);
    let (y_plane, chroma) = self.data.split_at_mut(width * height);
    let (u_plane, v_plane) = chroma.split_at_mut(width * height / 4);
    fill_plane(y_plane, width, cell, luma);
    let half = half_cell(cell);
    fill_plane(u_plane, width / 2, half, NEUTRAL_CHROMA);
    fill_plane(v_plane, width / 2, half, NEUTRAL_CHROMA);
  }

  fn draw_picture(&mut self, picture: &VideoFrame, cell: Cell) {
    let width = usize::from(self.size.width);
    let height = usize::from(self.size.height);
    let source = picture.resolution();
    let (source_width, source_height) = (usize::from(source.width), usize::from(source.height));
    let (y, u, v) = picture.planes();
    let (y_plane, chroma) = self.data.split_at_mut(width * height);
    let (u_plane, v_plane) = chroma.split_at_mut(width * height / 4);
    scale_into(y, source_width, source_height, y_plane, width, cell);
    let half = half_cell(cell);
    scale_into(u, source_width / 2, source_height / 2, u_plane, width / 2, half);
    scale_into(v, source_width / 2, source_height / 2, v_plane, width / 2, half);
  }

  /// The label, centred in `cell`, scaled down to fit it (never up).
  fn draw_label(&mut self, label: &LabelMask, cell: Cell) {
    let max_width = cell.width * LABEL_MAX_WIDTH_PERCENT / 100;
    let max_height = cell.height * LABEL_MAX_HEIGHT_PERCENT / 100;
    // Scale by the tighter of the two, in thousandths.
    let scale = (max_width * 1000 / label.width)
      .min(max_height * 1000 / label.height)
      .min(1000);
    let (drawn_width, drawn_height) = (label.width * scale / 1000, label.height * scale / 1000);
    if drawn_width == 0 || drawn_height == 0 {
      return;
    }
    let (left, top) = (
      cell.x + (cell.width - drawn_width) / 2,
      cell.y + (cell.height - drawn_height) / 2,
    );
    let stride = usize::from(self.size.width);
    for row in 0..drawn_height {
      let source_row = row * label.height / drawn_height;
      for column in 0..drawn_width {
        let source_column = column * label.width / drawn_width;
        let coverage = u32::from(label.coverage[source_row * label.width + source_column]);
        let at = (top + row) * stride + left + column;
        let under = u32::from(self.data[at]);
        let over = u32::from(LABEL_LUMA);
        self.data[at] = ((under * (255 - coverage) + over * coverage) / 255) as u8;
      }
    }
  }
}

/// The tiles as the call screen orders them: screen shares first, then the one in focus, then the
/// rest as given; at most [`MAX_TILES`].
fn in_screen_order<'a>(tiles: &[SceneTile<'a>]) -> Vec<SceneTile<'a>> {
  let mut ordered: Vec<SceneTile<'a>> = tiles.iter().filter(|tile| tile.is_screen).copied().collect();
  ordered.extend(tiles.iter().filter(|tile| !tile.is_screen && tile.is_focused));
  ordered.extend(tiles.iter().filter(|tile| !tile.is_screen && !tile.is_focused));
  ordered.truncate(MAX_TILES);
  ordered
}

/// The cells of `tiles` (already in screen order) in a frame of `width` x `height`.
pub fn arrange(tiles: &[SceneTile<'_>], layout: SceneLayout, width: usize, height: usize) -> Vec<Cell> {
  let count = tiles.len();
  if count == 0 {
    return Vec::new();
  }
  let has_share = tiles[0].is_screen;
  let is_speaker = layout == SceneLayout::ActiveSpeaker && count > 1;
  match (has_share, is_speaker) {
    (true, _) if count > 1 => stage_with_column(count, width, height),
    (false, true) => stage_with_row(count, width, height),
    _ => grid(count, width, height),
  }
}

/// A large cell with the rest in a column on its right, a quarter of the width.
fn stage_with_column(count: usize, width: usize, height: usize) -> Vec<Cell> {
  let column_width = even(width / 4);
  let stage = cell(GAP, GAP, width - column_width - 3 * GAP, height - 2 * GAP);
  let others = count - 1;
  let row_height = even(((height - GAP) / others.max(4)).saturating_sub(GAP));
  let column_x = width - column_width - GAP;
  let mut cells = vec![stage];
  cells.extend((0..others).map(|index| {
    cell(
      column_x,
      GAP + index * (row_height + GAP),
      column_width,
      row_height,
    )
  }));
  cells
}

/// A large cell with the rest in a row below it, a fifth of the height.
fn stage_with_row(count: usize, width: usize, height: usize) -> Vec<Cell> {
  let row_height = even(height / 5);
  let stage = cell(GAP, GAP, width - 2 * GAP, height - row_height - 3 * GAP);
  let others = count - 1;
  let cell_width = even(((width - GAP) / others.max(5)).saturating_sub(GAP));
  let row_y = height - row_height - GAP;
  let mut cells = vec![stage];
  cells
    .extend((0..others).map(|index| cell(GAP + index * (cell_width + GAP), row_y, cell_width, row_height)));
  cells
}

/// Equal cells in rows: as many columns as the square root rounded up.
fn grid(count: usize, width: usize, height: usize) -> Vec<Cell> {
  let columns = (1..=count)
    .find(|columns| columns * columns >= count)
    .unwrap_or(1);
  let rows = count.div_ceil(columns);
  let cell_width = even((width - GAP) / columns - GAP);
  let cell_height = even((height - GAP) / rows - GAP);
  (0..count)
    .map(|index| {
      let (row, column) = (index / columns, index % columns);
      cell(
        GAP + column * (cell_width + GAP),
        GAP + row * (cell_height + GAP),
        cell_width,
        cell_height,
      )
    })
    .collect()
}

fn cell(x: usize, y: usize, width: usize, height: usize) -> Cell {
  Cell {
    x: even(x),
    y: even(y),
    width: even(width),
    height: even(height),
  }
}

/// The largest part of `cell` with the aspect ratio of `source`, centred.
fn fit(source: VideoResolution, cell: Cell) -> Cell {
  let (source_width, source_height) = (
    usize::from(source.width).max(1),
    usize::from(source.height).max(1),
  );
  let by_width = cell.width * source_height / source_width;
  let (width, height) = if by_width <= cell.height {
    (cell.width, by_width)
  } else {
    (cell.height * source_width / source_height, cell.height)
  };
  let (width, height) = (even(width).max(2), even(height).max(2));
  Cell {
    x: even(cell.x + (cell.width - width) / 2),
    y: even(cell.y + (cell.height - height) / 2),
    width,
    height,
  }
}

fn even(value: usize) -> usize {
  value & !1
}

fn half_cell(cell: Cell) -> Cell {
  Cell {
    x: cell.x / 2,
    y: cell.y / 2,
    width: cell.width / 2,
    height: cell.height / 2,
  }
}

fn fill_plane(plane: &mut [u8], stride: usize, cell: Cell, value: u8) {
  for row in cell.y..cell.y + cell.height {
    plane[row * stride + cell.x..row * stride + cell.x + cell.width].fill(value);
  }
}

/// Scales `source` (`source_width` x `source_height`) into `cell` of `target`, bilinear, in fixed
/// point (16 bits of fraction).
fn scale_into(
  source: &[u8],
  source_width: usize,
  source_height: usize,
  target: &mut [u8],
  stride: usize,
  cell: Cell,
) {
  if source_width == 0 || source_height == 0 || cell.width == 0 || cell.height == 0 {
    return;
  }
  let step_x = (source_width << 16) / cell.width;
  let step_y = (source_height << 16) / cell.height;
  for row in 0..cell.height {
    let source_y = (row * step_y + step_y / 2).saturating_sub(1 << 15);
    let (top, weight_y) = ((source_y >> 16).min(source_height - 1), source_y & 0xffff);
    let bottom = (top + 1).min(source_height - 1);
    let (top_row, bottom_row) = (
      &source[top * source_width..(top + 1) * source_width],
      &source[bottom * source_width..(bottom + 1) * source_width],
    );
    let output = &mut target[(cell.y + row) * stride + cell.x..(cell.y + row) * stride + cell.x + cell.width];
    for (column, pixel) in output.iter_mut().enumerate() {
      let source_x = (column * step_x + step_x / 2).saturating_sub(1 << 15);
      let (left, weight_x) = ((source_x >> 16).min(source_width - 1), source_x & 0xffff);
      let right = (left + 1).min(source_width - 1);
      // u64: the products overflow 32 bits (the browser's usize).
      let (weight_x, weight_y) = (weight_x as u64, weight_y as u64);
      let upper = u64::from(top_row[left]) * (0x10000 - weight_x) + u64::from(top_row[right]) * weight_x;
      let lower =
        u64::from(bottom_row[left]) * (0x10000 - weight_x) + u64::from(bottom_row[right]) * weight_x;
      let value = (upper * (0x10000 - weight_y) + lower * weight_y) >> 32;
      *pixel = value.min(255) as u8;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const SIZE: VideoResolution = VideoResolution {
    width: 1280,
    height: 720,
  };

  mod layout {
    use super::*;

    #[test]
    fn a_gallery_is_a_grid_of_equal_cells_inside_the_frame() {
      for count in 1..=MAX_TILES {
        let tiles = vec![tile(None); count];
        let cells = arrange(&tiles, SceneLayout::Gallery, 1280, 720);
        assert_eq!(cells.len(), count);
        assert!(
          cells
            .iter()
            .all(|cell| cell.width == cells[0].width && cell.height == cells[0].height)
        );
        assert_inside_and_apart(&cells);
      }
    }

    #[test]
    fn a_share_comes_first_and_large() {
      let picture = frame(640, 360, 100);
      let tiles = [
        tile(Some(&picture)),
        SceneTile {
          is_screen: true,
          ..tile(Some(&picture))
        },
        tile(None),
      ];
      let ordered = in_screen_order(&tiles);
      assert!(ordered[0].is_screen);
      let cells = arrange(&ordered, SceneLayout::Gallery, 1280, 720);
      assert!(cells[0].width > 2 * cells[1].width);
      assert_eq!(cells[1].width, cells[2].width);
      assert_inside_and_apart(&cells);
    }

    #[test]
    fn the_active_speaker_is_large_with_the_rest_in_a_row() {
      let tiles = [
        tile(None),
        SceneTile {
          is_focused: true,
          ..tile(None)
        },
        tile(None),
      ];
      let ordered = in_screen_order(&tiles);
      assert!(ordered[0].is_focused);
      let cells = arrange(&ordered, SceneLayout::ActiveSpeaker, 1280, 720);
      assert!(cells[0].height > 3 * cells[1].height);
      assert_eq!(cells[1].y, cells[2].y);
      assert_inside_and_apart(&cells);
    }

    #[test]
    fn more_tiles_than_a_page_keep_the_first_page() {
      let tiles = vec![tile(None); MAX_TILES + 5];
      assert_eq!(in_screen_order(&tiles).len(), MAX_TILES);
    }
  }

  mod drawing {
    use super::*;

    #[test]
    fn a_picture_fills_its_cell_with_bars_for_another_aspect() {
      let mut compositor = Compositor::new(SIZE).unwrap();
      let square = frame(200, 200, 200);
      let frame = compositor
        .compose(&[tile(Some(&square))], SceneLayout::Gallery, None, 7)
        .unwrap();
      assert_eq!(frame.timestamp_us(), 7);
      let (y, u, _) = frame.planes();
      assert_eq!(luma_at(y, 640, 360), 200, "the picture in the middle");
      assert_eq!(luma_at(y, 100, 360), EMPTY_LUMA, "bars beside a square picture");
      assert_eq!(luma_at(y, 0, 0), BACKGROUND_LUMA, "the frame around the cell");
      assert_eq!(u[(180 * 640) + 320], 90, "chroma scaled along");
    }

    #[test]
    fn a_left_out_tile_shows_nothing_of_its_picture_but_the_label() {
      let mut compositor = Compositor::new(SIZE).unwrap();
      let picture = frame(640, 360, 200);
      let label = LabelMask::new(4, 2, vec![255; 8]).unwrap();
      let frame = compositor
        .compose(
          &[SceneTile {
            is_left_out: true,
            ..tile(Some(&picture))
          }],
          SceneLayout::Gallery,
          Some(&label),
          0,
        )
        .unwrap();
      let (y, u, v) = frame.planes();
      assert!(!y.contains(&200), "not one pixel of the picture");
      assert!(u.iter().chain(v).all(|chroma| *chroma == NEUTRAL_CHROMA));
      assert_eq!(luma_at(y, 640, 360), LABEL_LUMA, "the label in the middle");
      assert_eq!(luma_at(y, 100, 100), LEFT_OUT_LUMA);
    }

    #[test]
    fn a_large_label_is_scaled_down_into_its_cell() {
      let mut compositor = Compositor::new(SIZE).unwrap();
      let label = LabelMask::new(4000, 400, vec![255; 4000 * 400]).unwrap();
      let tiles = vec![
        SceneTile {
          is_left_out: true,
          ..tile(None)
        };
        4
      ];
      let frame = compositor
        .compose(&tiles, SceneLayout::Gallery, Some(&label), 0)
        .unwrap();
      let (y, _, _) = frame.planes();
      // The cells' edges stay the blank grey: the label fits inside.
      assert_eq!(luma_at(y, 8, 8), LEFT_OUT_LUMA);
      assert_eq!(luma_at(y, 320, 180), LABEL_LUMA);
    }

    #[test]
    fn labels_must_match_their_size() {
      assert!(LabelMask::new(3, 3, vec![0; 8]).is_none());
      assert!(LabelMask::new(0, 0, Vec::new()).is_none());
    }
  }

  fn assert_inside_and_apart(cells: &[Cell]) {
    for (index, cell) in cells.iter().enumerate() {
      assert!(
        cell.x + cell.width <= 1280 && cell.y + cell.height <= 720,
        "{cell:?}"
      );
      assert!(
        [cell.x, cell.y, cell.width, cell.height]
          .iter()
          .all(|value| value.is_multiple_of(2))
      );
      for other in &cells[index + 1..] {
        let apart = cell.x + cell.width <= other.x
          || other.x + other.width <= cell.x
          || cell.y + cell.height <= other.y
          || other.y + other.height <= cell.y;
        assert!(apart, "{cell:?} overlaps {other:?}");
      }
    }
  }

  fn luma_at(y: &[u8], x: usize, row: usize) -> u8 {
    y[row * 1280 + x]
  }

  fn tile(maybe_picture: Option<&VideoFrame>) -> SceneTile<'_> {
    SceneTile {
      maybe_picture,
      is_screen: false,
      is_focused: false,
      is_left_out: false,
    }
  }

  fn frame(width: u16, height: u16, luma: u8) -> VideoFrame {
    let resolution = VideoResolution { width, height };
    let pixels = usize::from(width) * usize::from(height);
    let mut data = vec![luma; pixels];
    data.extend(std::iter::repeat_n(90, pixels / 2));
    VideoFrame::new(resolution, 0, data).unwrap()
  }
}
