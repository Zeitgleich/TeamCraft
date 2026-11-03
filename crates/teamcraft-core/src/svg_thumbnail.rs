//! Thumbnails of SVG attachments (issue #337), rasterized with `resvg` on native platforms. The
//! browser rasterizes SVG itself (`teamcraft-ui`), which keeps the web bundle within its budget.
//!
//! An SVG is untrusted XML. Before anything is rendered:
//! - no DTD is accepted, so entity expansion ("billion laughs") never happens;
//! - the file size, the element count and the size the tree grows to once `<use>` references are
//!   expanded are bounded, so nested `<use>` fan-out can't exhaust memory or time;
//! - nothing is loaded: `<image>` references (files on this device, data URLs, nested SVGs) are
//!   dropped, there is no text shaping and no system font access;
//! - the picture is drawn into a canvas of at most `max_side` pixels, whatever its `viewBox` says.

use std::collections::HashMap;
use std::io::Cursor;

use image::{ImageFormat, RgbaImage};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::roxmltree::{Document, Node, NodeId, ParsingOptions};
use resvg::usvg::{ImageHrefResolver, Options, Tree};

/// Largest SVG file that gets a thumbnail.
pub const MAX_SVG_BYTES: usize = 4 * 1024 * 1024;
/// Most XML nodes in the file.
const MAX_NODES: u32 = 50_000;
/// Most elements the tree may have once every `<use>` is expanded.
const MAX_EXPANDED_ELEMENTS: u64 = 100_000;

/// A PNG of the SVG whose longer side is `max_side` pixels; `None` for anything that isn't an SVG
/// within the limits above.
pub fn svg_thumbnail_png(bytes: &[u8], max_side: u32) -> Option<Vec<u8>> {
  if bytes.len() > MAX_SVG_BYTES {
    return None;
  }
  let text = std::str::from_utf8(bytes).ok()?;
  let parsing = ParsingOptions {
    allow_dtd: false,
    nodes_limit: MAX_NODES,
    entity_resolver: None,
  };
  let document = Document::parse_with_options(text, parsing).ok()?;
  if expanded_elements(&document)? > MAX_EXPANDED_ELEMENTS {
    return None;
  }
  let tree = Tree::from_xmltree(&document, &offline_options()).ok()?;
  render_png(&tree, max_side)
}

/// Parsing options that load nothing from outside the file.
fn offline_options() -> Options<'static> {
  Options {
    resources_dir: None,
    image_href_resolver: ImageHrefResolver {
      resolve_data: Box::new(|_, _, _| None),
      resolve_string: Box::new(|_, _| None),
    },
    ..Options::default()
  }
}

fn render_png(tree: &Tree, max_side: u32) -> Option<Vec<u8>> {
  let size = tree.size();
  let longest = size.width().max(size.height());
  if !(longest.is_finite() && longest > 0.0) {
    return None;
  }
  let scale = max_side as f32 / longest;
  // At least one pixel, at most `max_side`: a huge or tiny viewBox only changes the scale.
  let side = |length: f32| ((length * scale).round() as u32).clamp(1, max_side);
  let (width, height) = (side(size.width()), side(size.height()));
  let mut pixmap = Pixmap::new(width, height)?;
  resvg::render(tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
  // tiny-skia keeps premultiplied alpha; PNG wants it straight.
  let pixels = pixmap
    .pixels()
    .iter()
    .flat_map(|pixel| {
      let color = pixel.demultiply();
      [color.red(), color.green(), color.blue(), color.alpha()]
    })
    .collect();
  let image = RgbaImage::from_raw(width, height, pixels)?;
  let mut out = Cursor::new(Vec::new());
  image.write_to(&mut out, ImageFormat::Png).ok()?;
  Some(out.into_inner())
}

/// How many elements the document has once each `<use>` is replaced by what it references; `None`
/// when references form a cycle.
fn expanded_elements(document: &Document) -> Option<u64> {
  let ids: HashMap<&str, Node> = document
    .descendants()
    .filter(Node::is_element)
    .filter_map(|node| node.attribute("id").map(|id| (id, node)))
    .collect();
  let mut sizes = HashMap::new();
  let mut visiting = Vec::new();
  size_of(document.root_element(), &ids, &mut sizes, &mut visiting)
}

/// The expanded size of one element, memoized by node; `visiting` holds the elements being sized,
/// so a reference back to one of them is a cycle.
fn size_of<'a, 'input>(
  node: Node<'a, 'input>,
  ids: &HashMap<&str, Node<'a, 'input>>,
  sizes: &mut HashMap<NodeId, u64>,
  visiting: &mut Vec<NodeId>,
) -> Option<u64> {
  let key = node.id();
  if let Some(size) = sizes.get(&key) {
    return Some(*size);
  }
  if visiting.contains(&key) {
    return None;
  }
  visiting.push(key);
  let mut size: u64 = 1;
  for child in node.children().filter(Node::is_element) {
    size = size.saturating_add(size_of(child, ids, sizes, visiting)?);
  }
  if node.tag_name().name() == "use"
    && let Some(target) = use_target(node).and_then(|id| ids.get(id))
  {
    size = size.saturating_add(size_of(*target, ids, sizes, visiting)?);
  }
  visiting.pop();
  // Beyond the budget the exact number doesn't matter; stop growing it.
  let size = size.min(MAX_EXPANDED_ELEMENTS + 1);
  sizes.insert(key, size);
  Some(size)
}

/// The local ID a `<use>` points at (`href="#id"` or `xlink:href="#id"`).
fn use_target<'a>(node: Node<'a, '_>) -> Option<&'a str> {
  node
    .attributes()
    .find(|attribute| attribute.name() == "href")
    .and_then(|attribute| attribute.value().strip_prefix('#'))
}

#[cfg(test)]
mod tests {
  use super::*;

  const RED_SQUARE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#;

  #[test]
  fn a_plain_svg_becomes_a_png_of_the_requested_size() {
    let png = svg_thumbnail_png(RED_SQUARE.as_bytes(), 640).unwrap();
    let image = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(image.dimensions(), (640, 320));
    assert_eq!(image.get_pixel(10, 10).0, [255, 0, 0, 255]);
  }

  mod hostile {
    use super::*;

    #[test]
    fn scripts_and_foreign_objects_are_ignored() {
      let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" onload="alert(1)">
        <script>alert(document.cookie)</script>
        <foreignObject width="10" height="10"><body xmlns="http://www.w3.org/1999/xhtml"><iframe src="https://example.com"/></body></foreignObject>
        <rect width="10" height="10" fill="red"/></svg>"#;
      let image = image::load_from_memory(&svg_thumbnail_png(svg.as_bytes(), 32).unwrap()).unwrap();
      assert_eq!(image.width(), 32);
    }

    #[test]
    fn external_and_local_image_references_are_not_loaded() {
      // A picture on this device that a hostile SVG points at must not end up in the thumbnail.
      let path = std::env::temp_dir().join(format!("teamcraft-svg-secret-{}.png", std::process::id()));
      let secret = RgbaImage::from_pixel(4, 4, image::Rgba([0, 255, 0, 255]));
      secret.save(&path).unwrap();
      let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10">
          <image width="10" height="10" xlink:href="{}"/>
          <image width="10" height="10" href="file://{}"/>
          <image width="10" height="10" href="https://example.com/track.png"/>
          <image width="10" height="10" href="data:image/svg+xml;base64,PHN2Zy8+"/></svg>"#,
        path.display(),
        path.display()
      );
      let image = image::load_from_memory(&svg_thumbnail_png(svg.as_bytes(), 10).unwrap())
        .unwrap()
        .to_rgba8();
      assert!(image.pixels().all(|pixel| pixel.0[3] == 0), "nothing was drawn");
      std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn entity_expansion_is_refused() {
      let svg = r#"<?xml version="1.0"?>
        <!DOCTYPE svg [
          <!ENTITY a "aaaaaaaaaa">
          <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
          <!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
          <!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;">
          <!ENTITY e "&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;">
          <!ENTITY f "&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;">
        ]>
        <svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>&f;</text></svg>"#;
      assert_eq!(svg_thumbnail_png(svg.as_bytes(), 32), None);
    }

    #[test]
    fn giant_dimensions_render_at_the_thumbnail_size() {
      let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1000000000" height="1" viewBox="0 0 1000000000 1"><rect width="1000000000" height="1" fill="red"/></svg>"#;
      let image = image::load_from_memory(&svg_thumbnail_png(svg.as_bytes(), 640).unwrap()).unwrap();
      assert_eq!((image.width(), image.height()), (640, 1));
    }

    #[test]
    fn use_cycles_and_fan_out_are_refused() {
      let cycle = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
        <g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g></svg>"##;
      assert_eq!(svg_thumbnail_png(cycle.as_bytes(), 32), None);

      // Ten levels of ten references each: 10^10 elements once expanded, from a tiny file.
      let mut fan_out = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><rect id="l0" width="1" height="1"/>"#,
      );
      for level in 1..=10 {
        fan_out.push_str(&format!(r#"<g id="l{level}">"#));
        for _ in 0..10 {
          fan_out.push_str(&format!(r##"<use href="#l{}"/>"##, level - 1));
        }
        fan_out.push_str("</g>");
      }
      fan_out.push_str(r##"</defs><use href="#l10"/></svg>"##);
      assert!(fan_out.len() < 10_000);
      assert_eq!(svg_thumbnail_png(fan_out.as_bytes(), 32), None);
    }

    #[test]
    fn oversized_and_malformed_files_are_refused() {
      let mut big = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg">"#);
      big.push_str(&"<!-- padding -->".repeat(MAX_SVG_BYTES / 16 + 1));
      big.push_str("</svg>");
      assert_eq!(svg_thumbnail_png(big.as_bytes(), 32), None);
      assert_eq!(svg_thumbnail_png(b"<svg", 32), None);
      assert_eq!(svg_thumbnail_png(&[0xff, 0xfe, 0x00], 32), None);
      assert_eq!(svg_thumbnail_png(b"", 32), None);
      let many = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg">{}</svg>"#,
        "<g/>".repeat(MAX_NODES as usize + 1)
      );
      assert_eq!(svg_thumbnail_png(many.as_bytes(), 32), None);
    }
  }
}
