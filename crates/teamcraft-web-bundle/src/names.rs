//! The function names of the app's wasm files, kept out of the download (#2228).
//!
//! The shipped files carry no name section (download size), so a browser names a wasm frame by its
//! index (`wasm-function[17228]`), and every split file has its own index space. With
//! `--names-out <dir>`, the build keeps the names through wasm-bindgen and wasm-opt (`-g`), then
//! takes the name section out of each finished file without touching anything else: the code the
//! page runs is byte for byte the code the names were read from, so indices and offsets match by
//! construction. The names go to `<dir>/teamcraft-ui-names-<build id>.json.gz` ([`NamesMap`]),
//! outside the staging directory: never served, never in the desktop app, never a workflow
//! artifact (the release keeps it privately, docs/release.md, "Debug symbols";
//! `scripts/symbolicate-web.py` reads it).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::Serialize;
use wasmparser::{KnownCustom, Name, Parser, Payload, TypeRef};

/// The custom section holding the names.
const NAME_SECTION: &str = "name";
/// DWARF sections start with this; wasm-opt keeps them with `-g` and skips passes for them.
const DEBUG_SECTION_PREFIX: &str = ".debug";
/// The map's format; a reader refuses others.
const FORMAT: u32 = 1;

/// Every wasm file of one build with the names of its functions.
#[derive(Debug, Serialize)]
pub struct NamesMap {
  pub format: u32,
  /// The build ID every file name of the build carries (`teamcraft-ui-<id>_bg.wasm`).
  pub build: String,
  pub modules: Vec<ModuleNames>,
}

/// One served wasm file.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ModuleNames {
  /// The name it is served under (in the pack too, at the same bytes).
  pub file: String,
  /// Imported functions come first in the index space: the first defined function's index.
  pub imported_functions: u32,
  /// The defined functions in index order (index = `imported_functions` + position).
  pub functions: Vec<DefinedFunction>,
}

/// A defined function: where its body is in the file (what a browser's `0x…` offset points into)
/// and its name, if the module named it.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct DefinedFunction {
  pub start: u64,
  pub end: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub name: Option<String>,
}

impl NamesMap {
  pub fn new(build: &str) -> Self {
    Self {
      format: FORMAT,
      build: build.to_owned(),
      modules: Vec::new(),
    }
  }

  /// Writes the map to `dir`, gzipped (the names of a release build are some 60 MB of JSON,
  /// 3 MB compressed); returns its path.
  pub fn write(&self, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("teamcraft-ui-names-{}.json.gz", self.build));
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&serde_json::to_vec(self)?)?;
    std::fs::write(&path, encoder.finish()?).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
  }
}

/// Takes the DWARF sections out of the file at `path` (before wasm-opt): with `-g`, wasm-opt would
/// keep them and leave out passes that can't update them, and the shipped code would differ.
pub fn drop_debug_info(path: &Path) -> Result<()> {
  let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
  let kept = without_custom_sections(&bytes, |name| name.starts_with(DEBUG_SECTION_PREFIX))?;
  if kept.len() != bytes.len() {
    std::fs::write(path, kept)?;
  }
  Ok(())
}

/// Reads the names of the finished file at `path` and writes it back without its name section
/// (and any DWARF left); `file` is the name it is served under.
pub fn take_names(path: &Path, file: &str) -> Result<ModuleNames> {
  let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
  let shipped = without_custom_sections(&bytes, |name| {
    name == NAME_SECTION || name.starts_with(DEBUG_SECTION_PREFIX)
  })?;
  // Offsets are read from the shipped bytes: what the browser reports offsets in.
  let mut names = module_names(&shipped, file)?;
  name_functions(&mut names, &bytes)?;
  std::fs::write(path, shipped)?;
  Ok(names)
}

/// `bytes` without the custom sections whose name `drop` accepts; every other byte stays where it
/// was relative to its section, in order.
fn without_custom_sections(bytes: &[u8], drop: impl Fn(&str) -> bool) -> Result<Vec<u8>> {
  let mut kept = Vec::with_capacity(bytes.len());
  // A section starts where the one before it ended (its id byte, then its size): sizes may be
  // padded LEB128, so the header is not recomputed but taken as it is.
  let mut section_start = 0;
  for payload in Parser::new(0).parse_all(bytes) {
    let payload = payload.context("reading the wasm module")?;
    if let Payload::Version { range, .. } = &payload {
      kept.extend_from_slice(&bytes[usize_of(range.start)?..usize_of(range.end)?]);
      section_start = usize_of(range.end)?;
      continue;
    }
    let Some((_, range)) = payload.as_section() else {
      continue;
    };
    let section_end = usize_of(range.end)?;
    let is_dropped = matches!(&payload, Payload::CustomSection(section) if drop(section.name()));
    if !is_dropped {
      kept.extend_from_slice(&bytes[section_start..section_end]);
    }
    section_start = section_end;
  }
  if section_start != bytes.len() {
    bail!(
      "the wasm module has {} bytes after its last section",
      bytes.len() - section_start
    );
  }
  Ok(kept)
}

/// The defined functions of `bytes` with their body ranges, unnamed.
fn module_names(bytes: &[u8], file: &str) -> Result<ModuleNames> {
  let mut names = ModuleNames {
    file: file.to_owned(),
    imported_functions: 0,
    functions: Vec::new(),
  };
  for payload in Parser::new(0).parse_all(bytes) {
    match payload.context("reading the wasm module")? {
      Payload::ImportSection(imports) => {
        for import in imports.into_imports() {
          if matches!(import?.ty, TypeRef::Func(_) | TypeRef::FuncExact(_)) {
            names.imported_functions += 1;
          }
        }
      },
      Payload::CodeSectionEntry(body) => {
        let range = body.range();
        names.functions.push(DefinedFunction {
          start: range.start,
          end: range.end,
          name: None,
        });
      },
      _ => {},
    }
  }
  Ok(names)
}

/// Fills in the function names from the name section of `bytes` (the file before it was stripped).
fn name_functions(names: &mut ModuleNames, bytes: &[u8]) -> Result<()> {
  for payload in Parser::new(0).parse_all(bytes) {
    let Payload::CustomSection(section) = payload.context("reading the wasm module")? else {
      continue;
    };
    let KnownCustom::Name(reader) = section.as_known() else {
      continue;
    };
    for subsection in reader {
      let Name::Function(map) = subsection? else {
        continue;
      };
      for naming in map {
        let naming = naming?;
        let Some(position) = naming.index.checked_sub(names.imported_functions) else {
          continue;
        };
        if let Some(function) = usize::try_from(position)
          .ok()
          .and_then(|position| names.functions.get_mut(position))
        {
          function.name = Some(readable(naming.name));
        }
      }
    }
  }
  Ok(())
}

/// A Rust symbol demangled, without its hash and its crates' disambiguators
/// (`teamcraft_ui::app::{closure#0}`); other names as they are.
fn readable(name: &str) -> String {
  // wasm-bindgen appends what it made of a function to its symbol (`_RNv… local_shim`).
  let is_mangled = name.starts_with("_R") || name.starts_with("_ZN");
  let (symbol, maybe_suffix) = match name.split_once(' ') {
    Some((symbol, suffix)) if is_mangled => (symbol, Some(suffix)),
    _ => (name, None),
  };
  let demangled = match rustc_demangle::try_demangle(symbol) {
    Ok(demangled) => format!("{demangled:#}"),
    Err(_) => without_disambiguators(without_hash(symbol)),
  };
  match maybe_suffix {
    Some(suffix) => format!("{demangled} {suffix}"),
    None => demangled,
  }
}

/// LLD writes legacy Rust names demangled, hash included (`core::fmt::write::h0123456789abcdef`).
fn without_hash(name: &str) -> &str {
  match name.rsplit_once("::h") {
    Some((path, hash)) if is_hash(hash) => path,
    _ => name,
  }
}

/// LLD writes v0 Rust names demangled with each crate's disambiguator
/// (`teamcraft_ui[8d907ea81e540f04]::app`), most of the map's size and nothing for a reader.
fn without_disambiguators(name: &str) -> String {
  let mut out = String::with_capacity(name.len());
  let mut rest = name;
  while let Some(open) = rest.find('[') {
    let after = &rest[open + 1..];
    match after.split_once(']') {
      Some((inside, tail)) if is_hash(inside) => {
        out.push_str(&rest[..open]);
        rest = tail;
      },
      _ => {
        out.push_str(&rest[..=open]);
        rest = after;
      },
    }
  }
  out.push_str(rest);
  out
}

fn is_hash(text: &str) -> bool {
  text.len() == 16 && text.chars().all(|c| c.is_ascii_hexdigit())
}

fn usize_of(offset: u64) -> Result<usize> {
  usize::try_from(offset).context("a wasm offset beyond the address space")
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Two imported functions, three defined ones, a name section naming an import and two of the
  /// defined ones, a DWARF section and a producers section.
  fn module() -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    // Type section: one type, () -> ().
    section(&mut module, 1, &[1, 0x60, 0, 0]);
    // Import section: two functions of type 0 and a memory.
    let mut imports = vec![3];
    for (field, kind) in [(&b"f"[..], 0u8), (b"g", 0), (b"m", 2)] {
      imports.extend_from_slice(&[1, b'e', u8::try_from(field.len()).unwrap()]);
      imports.extend_from_slice(field);
      imports.push(kind);
      imports.extend_from_slice(if kind == 0 { &[0] } else { &[0, 1] });
    }
    section(&mut module, 2, &imports);
    // Function section: three functions of type 0.
    section(&mut module, 3, &[3, 0, 0, 0]);
    // Code section: bodies of 2, 3 and 4 bytes (no locals, nops, end).
    section(
      &mut module,
      10,
      &[3, 2, 0, 0x0b, 3, 0, 0x01, 0x0b, 4, 0, 0x01, 0x01, 0x0b],
    );
    custom(&mut module, ".debug_info", &[1, 2, 3]);
    let mut function_names = vec![3];
    for (index, name) in [
      (0u8, "imported"),
      (2, "teamcraft_ui::app::h0123456789abcdef"),
      (4, "_ZN4core3fmt5write17h0123456789abcdefE"),
    ] {
      function_names.extend_from_slice(&[index, u8::try_from(name.len()).unwrap()]);
      function_names.extend_from_slice(name.as_bytes());
    }
    let mut names = vec![1, u8::try_from(function_names.len()).unwrap()];
    names.extend_from_slice(&function_names);
    custom(&mut module, NAME_SECTION, &names);
    custom(&mut module, "producers", &[0]);
    module
  }

  fn section(module: &mut Vec<u8>, id: u8, content: &[u8]) {
    module.push(id);
    // Padded LEB128 (5 bytes), as wasm-opt writes some sizes.
    let len = u32::try_from(content.len()).unwrap();
    for shift in [0, 7, 14, 21] {
      module.push(u8::try_from((len >> shift) & 0x7f).unwrap() | 0x80);
    }
    module.push(u8::try_from(len >> 28).unwrap());
    module.extend_from_slice(content);
  }

  fn custom(module: &mut Vec<u8>, name: &str, data: &[u8]) {
    let mut content = vec![u8::try_from(name.len()).unwrap()];
    content.extend_from_slice(name.as_bytes());
    content.extend_from_slice(data);
    section(module, 0, &content);
  }

  fn custom_section_names(bytes: &[u8]) -> Vec<String> {
    Parser::new(0)
      .parse_all(bytes)
      .filter_map(|payload| match payload.unwrap() {
        Payload::CustomSection(section) => Some(section.name().to_owned()),
        _ => None,
      })
      .collect()
  }

  #[test]
  fn the_names_leave_the_file_and_the_code_stays_where_it_was() {
    let original = module();
    let dir = std::env::temp_dir().join(format!("web-bundle-names-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("app_bg.wasm");
    std::fs::write(&path, &original).unwrap();

    let names = take_names(&path, "app-0123456789abcdef_bg.wasm").unwrap();
    let shipped = std::fs::read(&path).unwrap();
    assert_eq!(custom_section_names(&shipped), ["producers"]);
    wasmparser::Validator::new().validate_all(&shipped).unwrap();
    assert_eq!(names.imported_functions, 2);
    assert_eq!(
      names
        .functions
        .iter()
        .map(|function| function.name.as_deref())
        .collect::<Vec<_>>(),
      [Some("teamcraft_ui::app"), None, Some("core::fmt::write")]
    );
    // Every body's bytes are the same in the shipped file as in the one with names: offsets the
    // browser reports for the shipped file find the named function.
    let original_names = module_names(&original, "x").unwrap();
    for (shipped_body, original_body) in names.functions.iter().zip(&original_names.functions) {
      assert_eq!(
        shipped[usize_of(shipped_body.start).unwrap()..usize_of(shipped_body.end).unwrap()],
        original[usize_of(original_body.start).unwrap()..usize_of(original_body.end).unwrap()]
      );
    }
    let ranges = |functions: &[DefinedFunction]| {
      functions
        .iter()
        .map(|function| (function.start, function.end))
        .collect::<Vec<_>>()
    };
    // The custom sections that went came after the code: the offsets are the same, too.
    assert_eq!(ranges(&names.functions), ranges(&original_names.functions));
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn only_dwarf_goes_before_the_optimizer() {
    let dir = std::env::temp_dir().join(format!("web-bundle-dwarf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("app_bg.wasm");
    std::fs::write(&path, module()).unwrap();
    drop_debug_info(&path).unwrap();
    assert_eq!(
      custom_section_names(&std::fs::read(&path).unwrap()),
      [NAME_SECTION, "producers"]
    );
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn a_file_without_names_has_unnamed_functions() {
    let stripped = without_custom_sections(&module(), |name| name == NAME_SECTION).unwrap();
    let mut names = module_names(&stripped, "x").unwrap();
    name_functions(&mut names, &stripped).unwrap();
    assert_eq!(names.functions.len(), 3);
    assert!(names.functions.iter().all(|function| function.name.is_none()));
  }

  #[test]
  fn truncated_or_foreign_files_are_refused() {
    let module = module();
    assert!(without_custom_sections(&module[..module.len() - 3], |_| false).is_err());
    assert!(without_custom_sections(b"not wasm", |_| false).is_err());
    assert!(module_names(b"", "x").is_err());
  }

  #[test]
  fn names_are_readable() {
    assert_eq!(
      readable("teamcraft_ui::app::h0123456789abcdef"),
      "teamcraft_ui::app"
    );
    assert_eq!(
      readable("_ZN4core3fmt5write17h0123456789abcdefE"),
      "core::fmt::write"
    );
    assert_eq!(readable("__wbindgen_malloc"), "__wbindgen_malloc");
    assert_eq!(readable("a::hxyz"), "a::hxyz");
    assert_eq!(
      readable("<alloc[42d2b33bd937efdb]::vec::Vec<u8>>::push"),
      "<alloc::vec::Vec<u8>>::push"
    );
    assert_eq!(readable("[u8; 4]::len"), "[u8; 4]::len");
    assert_eq!(
      readable("<[u32; 120usize] as core[37ee0ce0c4cd57d5]::ops::Index<usize>>::index"),
      "<[u32; 120usize] as core::ops::Index<usize>>::index"
    );
    assert_eq!(
      readable("_RNvCs1234_7mycrate3foo local_shim"),
      "mycrate::foo local_shim"
    );
  }

  #[test]
  fn the_map_is_written_outside_the_staging_directory_by_build() {
    let dir = std::env::temp_dir().join(format!("web-bundle-map-{}", std::process::id()));
    let mut map = NamesMap::new("0123456789abcdef");
    map.modules.push(ModuleNames {
      file: "app-0123456789abcdef_bg.wasm".to_owned(),
      imported_functions: 1,
      functions: vec![DefinedFunction {
        start: 10,
        end: 20,
        name: Some("teamcraft_ui::app".to_owned()),
      }],
    });
    let path = map.write(&dir).unwrap();
    assert!(path.ends_with("teamcraft-ui-names-0123456789abcdef.json.gz"));
    let json: serde_json::Value =
      serde_json::from_reader(flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap())).unwrap();
    assert_eq!(json["format"], 1);
    assert_eq!(json["modules"][0]["functions"][0]["name"], "teamcraft_ui::app");
    std::fs::remove_dir_all(&dir).unwrap();
  }
}
