//! Embeds the Fluent resources: every `i18n/<language>/*.ftl`, so a new area file needs no list
//! (#614). Writes `$OUT_DIR/i18n_sources.rs` with `fn sources(tag)`, the files of one language by
//! name, sorted (src/i18n.rs builds the bundles from them). Comments are for the people editing the
//! files: the embedded copies leave them out, so they don't add to the download.

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() -> Result<(), Box<dyn Error>> {
  let i18n = Path::new(&env::var("CARGO_MANIFEST_DIR")?).join("i18n");
  let out_dir = PathBuf::from(env::var("OUT_DIR")?);
  // A directory: Cargo looks at everything inside it, so new files count too.
  println!("cargo::rerun-if-changed=i18n");
  let mut code = String::from(
    "/// The Fluent files of the language `tag` (`(file name, source)`, by name); empty for others.\n\
     pub(crate) fn sources(tag: &str) -> &'static [(&'static str, &'static str)] {\n  match tag {\n",
  );
  for language in languages(&i18n)? {
    writeln!(code, "    {:?} => &[", language.tag)?;
    let embedded_dir = out_dir.join("i18n").join(&language.tag);
    fs::create_dir_all(&embedded_dir)?;
    for file in language.files {
      let name = file_name(&file);
      let embedded = embedded_dir.join(&name);
      fs::write(&embedded, without_comments(&fs::read_to_string(&file)?))?;
      let path = embedded.to_str().ok_or("a non-UTF-8 path")?;
      writeln!(code, "      ({name:?}, include_str!({path:?})),")?;
    }
    writeln!(code, "    ],")?;
  }
  code.push_str("    _ => &[],\n  }\n}\n");
  fs::write(out_dir.join("i18n_sources.rs"), code)?;
  Ok(())
}

/// One directory of `i18n/`: the language's tag (its name) and its `.ftl` files.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Language {
  tag: String,
  files: Vec<PathBuf>,
}

/// The language directories with their `.ftl` files, both by name.
fn languages(i18n: &Path) -> Result<Vec<Language>, Box<dyn Error>> {
  let mut languages = Vec::new();
  for entry in fs::read_dir(i18n)? {
    let directory = entry?.path();
    if !directory.is_dir() {
      continue;
    }
    let mut files = Vec::new();
    for file in fs::read_dir(&directory)? {
      let file = file?.path();
      if file.extension().is_some_and(|extension| extension == "ftl") {
        files.push(file);
      }
    }
    files.sort();
    languages.push(Language {
      tag: file_name(&directory),
      files,
    });
  }
  languages.sort();
  Ok(languages)
}

/// The Fluent source without its comment lines. A line that starts with `#` is always a comment
/// (a message's continuation lines are indented), and comments don't change any message.
fn without_comments(source: &str) -> String {
  source
    .lines()
    .filter(|line| !line.starts_with('#'))
    .flat_map(|line| [line, "\n"])
    .collect()
}

fn file_name(path: &Path) -> String {
  path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default()
}
