//! The web client's app bundle (#334): turns the linked `teamcraft-ui` wasm into the files the page
//! loads, split into chunks that load when first used.
//!
//! Trunk builds the rest of the page (styles, the service worker, the call module) but can't split
//! a wasm module: splitting needs the linked module with its relocations, before wasm-bindgen, and
//! wasm-bindgen must then keep LLD's exports for the chunks. So `scripts/build-app.sh` (a Trunk
//! hook) builds the app with `-Clink-args=--emit-relocs` and runs this tool, which
//!
//! 1. splits the module at its `wasm_split` points (`wasm_split_cli_support`, as cargo-leptos does),
//! 2. runs wasm-bindgen on the main part (`--target web --keep-lld-exports`),
//! 3. optimizes every part with wasm-opt (release),
//! 4. names every file after the build (`<name>-<id>…`, so the server caches them for good),
//! 5. writes `init.js`, which loads the app with the loading screen's progress (`loader.js`), and the
//!    preload links with subresource integrity into `index.html`.
//!
//! ```text
//! teamcraft-web-bundle --input <app.wasm> --staging <dir> --name teamcraft-ui --initializer <loader.js>
//!                      [--wasm-opt <binary> [--wasm-opt-arg <arg>]...]
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256, Sha384};

/// Where the page loads everything from; the app is always served at the root.
const PUBLIC_URL: &str = "/";
/// Marks where the preload links and the start script go in `index.html`.
const HEAD_END: &str = "</head>";

fn main() -> Result<()> {
  tracing_subscriber::fmt().with_target(false).without_time().init();
  let args = Args::parse(std::env::args().skip(1))?;
  let input = std::fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
  let id = build_id(&input);
  let work = args.staging.join(format!(".split-{id}"));
  std::fs::create_dir_all(&work)?;
  let names = Names::new(&args.name, &id);

  let chunks = split(&input, &work, &names)?;
  bind(&work.join("main.wasm"), &args.staging, &names)?;
  let mut wasm_files = vec![args.staging.join(&names.main_wasm)];
  for chunk in &chunks {
    std::fs::rename(work.join(&chunk.original), args.staging.join(&chunk.named))?;
    wasm_files.push(args.staging.join(&chunk.named));
  }
  write_link_module(&work, &args.staging, &names, &chunks)?;
  if let Some(wasm_opt) = &args.maybe_wasm_opt {
    for file in &wasm_files {
      optimize(wasm_opt, &args.wasm_opt_args, file)?;
    }
  }
  std::fs::remove_dir_all(&work)?;
  let initializer = copy_hashed(&args.initializer, &args.staging)?;
  write_init(&args.staging, &names, &initializer)?;
  report(&args.staging, &wasm_files)?;
  Ok(())
}

/// The command line.
struct Args {
  input: PathBuf,
  staging: PathBuf,
  name: String,
  initializer: PathBuf,
  maybe_wasm_opt: Option<PathBuf>,
  wasm_opt_args: Vec<String>,
}

/// The files of one build.
struct Names {
  main_js: String,
  main_wasm: String,
  link_js: String,
  /// File names of chunks start with this.
  chunk_prefix: String,
  id: String,
}

/// One split-off part: the name wasm-split gave it and the one it is served under.
struct Chunk {
  original: String,
  named: String,
}

impl Args {
  fn parse(mut raw: impl Iterator<Item = String>) -> Result<Self> {
    let (mut input, mut staging, mut name, mut initializer, mut maybe_wasm_opt) =
      (None, None, None, None, None);
    let mut wasm_opt_args = Vec::new();
    while let Some(flag) = raw.next() {
      let mut value = || raw.next().with_context(|| format!("{flag} needs a value"));
      match flag.as_str() {
        "--input" => input = Some(PathBuf::from(value()?)),
        "--staging" => staging = Some(PathBuf::from(value()?)),
        "--name" => name = Some(value()?),
        "--initializer" => initializer = Some(PathBuf::from(value()?)),
        "--wasm-opt" => maybe_wasm_opt = Some(PathBuf::from(value()?)),
        "--wasm-opt-arg" => wasm_opt_args.push(value()?),
        other => bail!("unknown argument {other}"),
      }
    }
    Ok(Self {
      input: input.context("--input is missing")?,
      staging: staging.context("--staging is missing")?,
      name: name.context("--name is missing")?,
      initializer: initializer.context("--initializer is missing")?,
      maybe_wasm_opt,
      wasm_opt_args,
    })
  }
}

impl Names {
  fn new(name: &str, id: &str) -> Self {
    Self {
      main_js: format!("{name}-{id}.js"),
      main_wasm: format!("{name}-{id}_bg.wasm"),
      link_js: format!("{name}-split-{id}.js"),
      chunk_prefix: format!("{name}-"),
      id: id.to_owned(),
    }
  }

  fn chunk(&self, original_stem: &str) -> String {
    format!(
      "{}{}-{}.wasm",
      self.chunk_prefix,
      original_stem.replace('_', "-"),
      self.id
    )
  }
}

/// 16 hex digits of the input's SHA-256: every file of a build carries it, so a changed build gets
/// new names (the server caches hashed names for good; `teamcraft-server` `web.rs`).
fn build_id(input: &[u8]) -> String {
  hex(&Sha256::digest(input)[..8])
}

/// Splits the module; the main part is `work/main.wasm`, the chunks `work/<stem>.wasm`.
fn split(input: &[u8], work: &Path, names: &Names) -> Result<Vec<Chunk>> {
  let main_out = work.join("main.wasm");
  let link_name = format!("./{}", names.link_js);
  let main_module = format!("./{}", names.main_js);
  let mut options = wasm_split_cli_support::Options::new(input);
  options.output_dir = work;
  options.main_out_path = &main_out;
  options.link_name = &link_name;
  options.main_module = &main_module;
  options.target.web();
  let split = wasm_split_cli_support::transform(options).map_err(|error| anyhow::anyhow!("{error:?}"))?;
  let mut chunks = Vec::new();
  for path in split.split_modules {
    let file_name = path
      .file_name()
      .and_then(|name| name.to_str())
      .context("a chunk without a file name")?
      .to_owned();
    let stem = file_name.trim_end_matches(".wasm");
    chunks.push(Chunk {
      named: names.chunk(stem),
      original: file_name,
    });
  }
  Ok(chunks)
}

/// wasm-bindgen on the main part, with the exports the chunks link against.
fn bind(main: &Path, staging: &Path, names: &Names) -> Result<()> {
  let stem = names.main_js.trim_end_matches(".js");
  wasm_bindgen_cli_support::Bindgen::new()
    .input_path(main)
    .out_name(stem)
    .web(true)?
    .keep_lld_exports(true)
    .remove_name_section(true)
    .remove_producers_section(true)
    .typescript(false)
    .generate(staging)
    .context("wasm-bindgen")?;
  Ok(())
}

/// The loader of the chunks, which wasm-split wrote for the original chunk names.
fn write_link_module(work: &Path, staging: &Path, names: &Names, chunks: &[Chunk]) -> Result<()> {
  let mut script = std::fs::read_to_string(work.join(&names.link_js))?;
  for chunk in chunks {
    let (from, to) = (
      format!("\"./{}\"", chunk.original),
      format!("\"./{}\"", chunk.named),
    );
    if !script.contains(&from) {
      bail!("the chunk loader doesn't name {}", chunk.original);
    }
    script = script.replace(&from, &to);
  }
  std::fs::write(staging.join(&names.link_js), script)?;
  Ok(())
}

fn optimize(wasm_opt: &Path, args: &[String], file: &Path) -> Result<()> {
  let status = std::process::Command::new(wasm_opt)
    .args(args)
    .arg(file)
    .arg("-o")
    .arg(file)
    .status()
    .with_context(|| format!("running {}", wasm_opt.display()))?;
  if !status.success() {
    bail!("wasm-opt failed on {}", file.display());
  }
  Ok(())
}

/// Copies `source` into the staging directory as `<stem>-<hash>.<ext>`; returns the new name.
fn copy_hashed(source: &Path, staging: &Path) -> Result<String> {
  let bytes = std::fs::read(source).with_context(|| format!("reading {}", source.display()))?;
  let stem = source
    .file_stem()
    .and_then(|stem| stem.to_str())
    .context("initializer name")?;
  let extension = source.extension().and_then(|ext| ext.to_str()).unwrap_or("js");
  let name = format!("{stem}-{}.{extension}", hex(&Sha256::digest(&bytes)[..8]));
  std::fs::write(staging.join(&name), bytes)?;
  Ok(name)
}

/// `init.js` (it starts the app; the server's Content-Security-Policy allows no inline scripts) and
/// the preload links in `index.html`.
fn write_init(staging: &Path, names: &Names, initializer: &str) -> Result<()> {
  let wasm_size = std::fs::metadata(staging.join(&names.main_wasm))?.len();
  let init = INIT_TEMPLATE
    .replace("__MAIN_JS__", &format!("{PUBLIC_URL}{}", names.main_js))
    .replace("__MAIN_WASM__", &format!("{PUBLIC_URL}{}", names.main_wasm))
    .replace("__WASM_SIZE__", &wasm_size.to_string())
    .replace("__INITIALIZER__", &format!("{PUBLIC_URL}{initializer}"));
  std::fs::write(staging.join("init.js"), init)?;

  let index_path = staging.join("index.html");
  let index = std::fs::read_to_string(&index_path)?;
  if !index.contains(HEAD_END) {
    bail!("index.html has no {HEAD_END}");
  }
  let mut links = String::new();
  for script in [&names.main_js, initializer] {
    writeln!(
      links,
      r#"<link rel="modulepreload" href="{PUBLIC_URL}{script}" crossorigin="anonymous" integrity="{}">"#,
      integrity(&staging.join(script))?
    )?;
  }
  writeln!(
    links,
    r#"<link rel="preload" href="{PUBLIC_URL}{}" crossorigin="anonymous" integrity="{}" as="fetch" type="application/wasm">"#,
    names.main_wasm,
    integrity(&staging.join(&names.main_wasm))?
  )?;
  links.push_str("<script type=\"module\" src=\"/init.js\"></script>\n");
  std::fs::write(
    &index_path,
    index.replacen(HEAD_END, &format!("{links}{HEAD_END}"), 1),
  )?;
  Ok(())
}

/// What the build produced, for the log.
fn report(staging: &Path, wasm_files: &[PathBuf]) -> Result<()> {
  let mut sizes = BTreeMap::new();
  for file in wasm_files {
    let name = file.strip_prefix(staging).unwrap_or(file).display().to_string();
    sizes.insert(name, std::fs::metadata(file)?.len());
  }
  for (name, size) in sizes {
    tracing::info!(%name, size, "wasm");
  }
  Ok(())
}

fn integrity(path: &Path) -> Result<String> {
  Ok(format!(
    "sha384-{}",
    BASE64.encode(Sha384::digest(std::fs::read(path)?))
  ))
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().fold(String::new(), |mut text, byte| {
    let _ = write!(text, "{byte:02x}");
    text
  })
}

/// Starts the app like Trunk's initializer does: the wasm is fetched with progress for the loading
/// screen (`loader.js` default export: `onStart`, `onProgress`, `onComplete`, `onSuccess`,
/// `onFailure`).
const INIT_TEMPLATE: &str = r#"import init from '__MAIN_JS__';
import initializer from '__INITIALIZER__';

const hooks = initializer();
hooks.onStart?.();
const total = __WASM_SIZE__;
const response = fetch('__MAIN_WASM__').then((response) => {
  const reader = response.body.getReader();
  let current = 0;
  const stream = new ReadableStream({
    async pull(controller) {
      const { done, value } = await reader.read();
      if (done) {
        hooks.onProgress?.({ current: total, total });
        controller.close();
        return;
      }
      current += value.byteLength;
      hooks.onProgress?.({ current, total });
      controller.enqueue(value);
    },
  });
  return new Response(stream, { headers: response.headers, status: response.status, statusText: response.statusText });
});
init({ module_or_path: response }).then(
  (value) => {
    hooks.onComplete?.();
    hooks.onSuccess?.(value);
  },
  (reason) => {
    hooks.onComplete?.();
    hooks.onFailure?.(reason);
  },
);
"#;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_file_of_a_build_carries_its_id() {
    let names = Names::new("teamcraft-ui", "0123456789abcdef");
    assert_eq!(names.main_js, "teamcraft-ui-0123456789abcdef.js");
    assert_eq!(names.main_wasm, "teamcraft-ui-0123456789abcdef_bg.wasm");
    assert_eq!(names.link_js, "teamcraft-ui-split-0123456789abcdef.js");
    assert_eq!(
      names.chunk("split_admin"),
      "teamcraft-ui-split-admin-0123456789abcdef.wasm"
    );
    assert_eq!(build_id(b"a").len(), 16);
  }

  #[test]
  fn arguments_are_read_and_checked() {
    let raw = [
      "--input",
      "a.wasm",
      "--staging",
      "dist",
      "--name",
      "app",
      "--initializer",
      "loader.js",
    ];
    let args = Args::parse(raw.iter().map(ToString::to_string)).unwrap();
    assert_eq!(args.name, "app");
    assert!(args.maybe_wasm_opt.is_none());
    assert!(Args::parse(["--input"].iter().map(ToString::to_string)).is_err());
    assert!(Args::parse(["--bogus", "x"].iter().map(ToString::to_string)).is_err());
  }
}
