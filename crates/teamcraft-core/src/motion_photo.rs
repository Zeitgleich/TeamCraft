//! Motion Photos (issue #2181, e2ee.md §4.5): Android cameras put a short MP4 behind the JPEG of
//! the picture. Google's format names the video's length in the picture's XMP (`GCamera:
//! MicroVideoOffset`, or a `Container:Directory` item with `Item:Semantic="MotionPhoto"` and
//! `Item:Length`); Samsung's marks it with `MotionPhoto_Data` and keeps its own trailer after it.
//! [`locate`] finds the video in such a file so the sending phone sends the picture and the clip
//! as two files; it reads untrusted bytes, so every length is checked against the file and the
//! video must be a run of well-formed MP4 boxes that starts with `ftyp`. Anything else is a plain
//! picture.

use std::ops::Range;

/// The XMP header of an APP1 segment.
const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
/// Samsung's marker right before the embedded video.
const SAMSUNG_MARKER: &[u8] = b"MotionPhoto_Data";
/// The JPEG segments read before giving up on finding the XMP: the start of the picture.
const MAX_SEGMENTS: usize = 64;
/// The XMP text read, in bytes (the extended XMP of a Motion Photo is not needed).
const MAX_XMP_BYTES: usize = 64 * 1024;
/// MP4 boxes walked at the top level before the video counts as malformed: a camera's clip has a
/// handful (`ftyp`, `moov`, `mdat`, maybe `free` and `meta`).
const MAX_TOP_LEVEL_BOXES: usize = 64;
/// The smallest MP4 box: size and type.
const BOX_HEADER_BYTES: u64 = 8;

/// Where the picture and the video of a Motion Photo are in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionPhotoParts {
  /// The picture without the video and anything after it: a plain JPEG every app reads.
  pub still: Range<u64>,
  /// The MP4 behind it.
  pub video: Range<u64>,
}

/// The parts of a Motion Photo, or `None` when `file` is a plain picture or anything the phone
/// shouldn't trust as one (lengths outside the file, a video that isn't MP4).
pub fn locate(file: &[u8]) -> Option<MotionPhotoParts> {
  if !file.starts_with(&[0xFF, 0xD8]) {
    return None;
  }
  let maybe_from_xmp = xmp(file)
    .and_then(|xmp| video_length(&xmp))
    .and_then(|length| (file.len() as u64).checked_sub(length));
  let (still_end, start) = match maybe_from_xmp {
    Some(start) => (start, start),
    None => samsung_parts(file)?,
  };
  let end = mp4_end(file, start)?;
  // The picture before the video must at least hold the JPEG's markers.
  (still_end >= 4).then_some(MotionPhotoParts {
    still: 0..still_end,
    video: start..end,
  })
}

/// The XMP of the picture's first APP1 XMP segment, as text.
fn xmp(file: &[u8]) -> Option<String> {
  let mut position = 2usize;
  for _ in 0..MAX_SEGMENTS {
    let marker = file.get(position..position + 2)?;
    if marker[0] != 0xFF {
      return None;
    }
    // Start of scan: the XMP comes before the picture's data.
    if marker[1] == 0xDA || marker[1] == 0xD9 {
      return None;
    }
    let length = usize::from(u16::from_be_bytes([
      *file.get(position + 2)?,
      *file.get(position + 3)?,
    ]));
    if length < 2 {
      return None;
    }
    let body = file.get(position + 4..position + 2 + length)?;
    if marker[1] == 0xE1 && body.starts_with(XMP_HEADER) {
      let text = &body[XMP_HEADER.len()..];
      let text = &text[..text.len().min(MAX_XMP_BYTES)];
      return Some(String::from_utf8_lossy(text).into_owned());
    }
    position += 2 + length;
  }
  None
}

/// The video's length from the XMP: the newer container directory first, then the older offset.
fn video_length(xmp: &str) -> Option<u64> {
  container_video_length(xmp).or_else(|| property(xmp, "GCamera:MicroVideoOffset"))
}

/// `Item:Length` of the directory item whose `Item:Semantic` is `MotionPhoto`.
fn container_video_length(xmp: &str) -> Option<u64> {
  xmp.match_indices("<Container:Item").find_map(|(start, _)| {
    let rest = &xmp[start..];
    let item = &rest[..rest.find('>').unwrap_or(rest.len())];
    (attribute(item, "Item:Semantic")? == "MotionPhoto")
      .then(|| attribute(item, "Item:Length")?.trim().parse().ok())
      .flatten()
  })
}

/// A numeric XMP property, written as an attribute (`name="12"`) or an element
/// (`<name>12</name>`).
fn property(xmp: &str, name: &str) -> Option<u64> {
  let value = attribute(xmp, name).or_else(|| {
    let open = format!("<{name}>");
    let start = xmp.find(&open)? + open.len();
    let end = start + xmp[start..].find('<')?;
    Some(&xmp[start..end])
  })?;
  value.trim().parse().ok()
}

/// The value of attribute `name` (`name="value"`) in `text`.
fn attribute<'a>(text: &'a str, name: &str) -> Option<&'a str> {
  let key = format!("{name}=\"");
  let start = text.find(&key)? + key.len();
  let end = start + text[start..].find('"')?;
  Some(&text[start..end])
}

/// Where the picture ends and the video starts in Samsung's format: at and right after the last
/// `MotionPhoto_Data` marker.
fn samsung_parts(file: &[u8]) -> Option<(u64, u64)> {
  let position = file
    .windows(SAMSUNG_MARKER.len())
    .rposition(|window| window == SAMSUNG_MARKER)?;
  Some((position as u64, (position + SAMSUNG_MARKER.len()) as u64))
}

/// Where the MP4 that starts at `start` ends: after its last top-level box, the first being
/// `ftyp`. `None` when it isn't one.
fn mp4_end(file: &[u8], start: u64) -> Option<u64> {
  let file_len = file.len() as u64;
  let mut position = start;
  let mut boxes = 0usize;
  while position + BOX_HEADER_BYTES <= file_len && boxes < MAX_TOP_LEVEL_BOXES {
    let header =
      file.get(usize::try_from(position).ok()?..usize::try_from(position + BOX_HEADER_BYTES).ok()?)?;
    let kind = &header[4..8];
    if boxes == 0 && kind != b"ftyp" {
      return None;
    }
    if !kind
      .iter()
      .all(|byte| byte.is_ascii_alphanumeric() || *byte == b' ')
    {
      break;
    }
    let size = match u32::from_be_bytes([header[0], header[1], header[2], header[3]]) {
      // To the end of the file.
      0 => file_len - position,
      // A 64-bit size follows the type.
      1 => {
        let at = usize::try_from(position + BOX_HEADER_BYTES).ok()?;
        let large = file.get(at..at + 8)?;
        u64::from_be_bytes(large.try_into().ok()?)
      },
      size => u64::from(size),
    };
    let Some(end) = position
      .checked_add(size)
      .filter(|end| size >= BOX_HEADER_BYTES && *end <= file_len)
    else {
      break;
    };
    position = end;
    boxes += 1;
  }
  (boxes > 0 && position > start).then_some(position)
}

#[cfg(test)]
mod tests {
  use super::*;

  const STILL_BODY: &[u8] = &[0xFF, 0xDA, 0x00, 0x04, 1, 2, 0xFF, 0xD9];

  mod google {
    use super::*;

    #[test]
    fn finds_the_video_from_the_container_directory() {
      let video = mp4(&[b"moov", b"mdat"]);
      let xmp = format!(
        r#"<x:xmpmeta><rdf:Description GCamera:MotionPhoto="1"><Container:Directory><rdf:Seq><rdf:li><Container:Item Item:Mime="image/jpeg" Item:Semantic="Primary" Item:Length="0" Item:Padding="0"/></rdf:li><rdf:li><Container:Item Item:Mime="video/mp4" Item:Semantic="MotionPhoto" Item:Length="{}"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description></x:xmpmeta>"#,
        video.len()
      );
      let (file, still_len) = photo(&xmp, &video);
      let parts = locate(&file).expect("a motion photo");
      assert_eq!(parts.still, 0..still_len);
      assert_eq!(&file[range(&parts.video)], video.as_slice());
    }

    #[test]
    fn finds_the_video_from_the_older_offset_attribute_or_element() {
      let video = mp4(&[b"moov"]);
      for xmp in [
        format!(
          r#"<rdf:Description GCamera:MicroVideo="1" GCamera:MicroVideoOffset="{}"/>"#,
          video.len()
        ),
        format!(
          "<GCamera:MicroVideoOffset>{}</GCamera:MicroVideoOffset>",
          video.len()
        ),
      ] {
        let (file, _) = photo(&xmp, &video);
        let parts = locate(&file).expect("a motion photo");
        assert_eq!(&file[range(&parts.video)], video.as_slice());
      }
    }
  }

  mod samsung {
    use super::*;

    #[test]
    fn takes_the_video_after_the_marker_and_leaves_the_trailer() {
      let video = mp4(&[b"moov", b"mdat"]);
      let mut file = jpeg(None);
      let still_len = file.len() as u64;
      file.extend_from_slice(SAMSUNG_MARKER);
      file.extend_from_slice(&video);
      file.extend_from_slice(b"\x00\x00SEFH\x01\x02SEFT");
      let parts = locate(&file).expect("a motion photo");
      assert_eq!(&file[range(&parts.video)], video.as_slice());
      assert_eq!(parts.still, 0..still_len, "the picture without the marker");
    }
  }

  mod hostile {
    use super::*;

    #[test]
    fn a_plain_picture_has_no_motion() {
      assert_eq!(locate(&jpeg(None)), None);
      assert_eq!(locate(&[]), None);
      assert_eq!(locate(b"GIF89a"), None);
    }

    #[test]
    fn lengths_outside_the_file_are_refused() {
      let video = mp4(&[b"moov"]);
      for length in [u64::MAX, 1 << 40, 0] {
        let xmp = format!(r#"GCamera:MicroVideoOffset="{length}""#);
        let (file, _) = photo(&xmp, &video);
        assert_eq!(locate(&file), None, "length {length}");
      }
    }

    #[test]
    fn a_video_that_is_not_mp4_is_refused() {
      let fake = b"\x00\x00\x00\x10moovxxxxxxxx".to_vec();
      let xmp = format!(r#"GCamera:MicroVideoOffset="{}""#, fake.len());
      let (file, _) = photo(&xmp, &fake);
      assert_eq!(locate(&file), None, "no ftyp first");
    }

    #[test]
    fn a_box_running_past_the_end_stops_the_video_there() {
      let mut video = mp4(&[b"moov"]);
      video.extend_from_slice(&[0x7F, 0xFF, 0xFF, 0xFF]);
      video.extend_from_slice(b"mdat");
      let xmp = format!(r#"GCamera:MicroVideoOffset="{}""#, video.len());
      let (file, _) = photo(&xmp, &video);
      let parts = locate(&file).expect("the valid boxes");
      assert_eq!(parts.video.end - parts.video.start, video.len() as u64 - 8);
    }

    #[test]
    fn truncated_segments_and_garbage_never_panic() {
      let (file, _) = photo(r#"GCamera:MicroVideoOffset="20""#, &mp4(&[b"moov"]));
      for cut in 0..file.len() {
        let _ = locate(&file[..cut]);
      }
      let mut noise = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x01];
      noise.extend(std::iter::repeat_n(0xFF, 100));
      assert_eq!(locate(&noise), None);
    }

    #[test]
    fn a_huge_64_bit_box_size_is_bounded_by_the_file() {
      let mut video = b"\x00\x00\x00\x10ftypisom\x00\x00\x00\x00".to_vec();
      video.extend_from_slice(&[0, 0, 0, 1]);
      video.extend_from_slice(b"mdat");
      video.extend_from_slice(&u64::MAX.to_be_bytes());
      let xmp = format!(r#"GCamera:MicroVideoOffset="{}""#, video.len());
      let (file, _) = photo(&xmp, &video);
      let parts = locate(&file).expect("the ftyp box");
      assert_eq!(parts.video.end - parts.video.start, 16);
    }
  }

  /// A JPEG with an optional XMP segment, then the picture data.
  fn jpeg(maybe_xmp: Option<&str>) -> Vec<u8> {
    let mut file = vec![0xFF, 0xD8];
    if let Some(xmp) = maybe_xmp {
      let mut body = XMP_HEADER.to_vec();
      body.extend_from_slice(xmp.as_bytes());
      file.extend_from_slice(&[0xFF, 0xE1]);
      file.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_be_bytes());
      file.extend_from_slice(&body);
    }
    file.extend_from_slice(STILL_BODY);
    file
  }

  /// A Motion Photo: the JPEG and the video behind it, with the JPEG's length.
  fn photo(xmp: &str, video: &[u8]) -> (Vec<u8>, u64) {
    let mut file = jpeg(Some(xmp));
    let still_len = file.len() as u64;
    file.extend_from_slice(video);
    (file, still_len)
  }

  /// An MP4 of an `ftyp` box and empty boxes of these types.
  fn mp4(kinds: &[&[u8; 4]]) -> Vec<u8> {
    let mut video = b"\x00\x00\x00\x10ftypisom\x00\x00\x02\x00".to_vec();
    for kind in kinds {
      video.extend_from_slice(&[0, 0, 0, 12]);
      video.extend_from_slice(*kind);
      video.extend_from_slice(&[1, 2, 3, 4]);
    }
    video
  }

  fn range(range: &Range<u64>) -> Range<usize> {
    range.start as usize..range.end as usize
  }
}
