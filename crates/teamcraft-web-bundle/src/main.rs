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
//! 5. joins the signed-in app's chunks (`session`) into one file, its pack (#1465), which the chunk
//!    loader takes them from: hundreds of files, most of them under 1 kB, were each their own request
//!    (through the service worker) and streaming compile, most of a phone's start;
//! 6. writes `init.js`, which loads the app with the loading screen's progress (`loader.js`) and,
//!    unless the page is a guest's meeting link, fetches the pack at the same time, as it does the
//!    UI's texts in the language the app starts in (#1437); and the preload links with subresource
//!    integrity into `index.html`.
//!
//! ```text
//! teamcraft-web-bundle --input <app.wasm> --staging <dir> --name teamcraft-ui --initializer <loader.js>
//!                      [--wasm-opt <binary> [--wasm-opt-arg <arg>]...]
//! ```
//!
//! `teamcraft-web-bundle worker …` binds and names a module that runs in a worker instead
//! ([`worker`]: the segmentation of the background effects).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256, Sha384};

mod worker;

/// Where the page loads everything from; the app is always served at the root.
const PUBLIC_URL: &str = "/";
/// Marks where the preload links and the start script go in `index.html`.
const HEAD_END: &str = "</head>";
/// The split module every page but a guest's needs at once (teamcraft-ui `areas.rs`): `init.js`
/// fetches its files next to the app's.
const SESSION_MODULE: &str = "session";

fn main() -> Result<()> {
  tracing_subscriber::fmt().with_target(false).without_time().init();
  let mut raw = std::env::args().skip(1).peekable();
  if raw.peek().map(String::as_str) == Some("worker") {
    raw.next();
    return worker::run(worker::WorkerArgs::parse(raw)?);
  }
  let args = Args::parse(raw)?;
  let input = std::fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
  let id = build_id(
    &input,
    &optimizer_settings(args.maybe_wasm_opt.as_deref(), &args.wasm_opt_args)?,
  );
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
  let link_script = write_link_module(&work, &args.staging, &names, &chunks)?;
  if let Some(wasm_opt) = &args.maybe_wasm_opt {
    for file in &wasm_files {
      optimize(wasm_opt, &args.wasm_opt_args, file)?;
    }
  }
  std::fs::remove_dir_all(&work)?;
  let initializer = copy_hashed(&args.initializer, &args.staging)?;
  // After wasm-opt: the pack holds the files as they are served.
  let session_files = module_files(&link_script, SESSION_MODULE)?;
  let pack = write_session_pack(&args.staging, &names, &session_files)?;
  std::fs::write(
    args.staging.join(&names.link_js),
    with_session_pack(&link_script, &pack)?,
  )?;
  write_init(&args.staging, &names, &initializer, &pack)?;
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
  session_pack: String,
  /// File names of chunks start with this.
  chunk_prefix: String,
  id: String,
}

/// The signed-in app's files as one (see [`write_session_pack`]).
struct SessionPack {
  file: String,
  parts: Vec<PackedPart>,
}

/// A file in the pack and where its bytes are.
struct PackedPart {
  file: String,
  offset: usize,
  len: usize,
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
      session_pack: format!("{name}-session-{id}.pack"),
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

/// 16 hex digits of the SHA-256 of the input and of how it is optimized: every file of a build
/// carries it, so a changed build gets new names (the server caches hashed names for good;
/// `teamcraft-server` `web.rs`), also when only wasm-opt or its flags changed.
fn build_id(input: &[u8], optimizer_settings: &str) -> String {
  let mut hasher = Sha256::new();
  hasher.update(input);
  hasher.update(optimizer_settings.as_bytes());
  hex(&hasher.finalize()[..8])
}

/// wasm-opt's version and flags (empty without wasm-opt).
fn optimizer_settings(maybe_wasm_opt: Option<&Path>, wasm_opt_args: &[String]) -> Result<String> {
  let Some(wasm_opt) = maybe_wasm_opt else {
    return Ok(String::new());
  };
  let output = std::process::Command::new(wasm_opt)
    .arg("--version")
    .output()
    .with_context(|| format!("running {}", wasm_opt.display()))?;
  Ok(format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout).trim(),
    wasm_opt_args.join(" ")
  ))
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

/// The loader of the chunks, which wasm-split wrote for the original chunk names; returns it.
fn write_link_module(work: &Path, staging: &Path, names: &Names, chunks: &[Chunk]) -> Result<String> {
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
  std::fs::write(staging.join(&names.link_js), &script)?;
  Ok(script)
}

/// The files a split module loads, its own and the shared chunks it needs, read from the loader
/// wasm-split writes: each chunk is `const __chunk_<n> = makeLoad(…fetch(new URL("./<file>"…), [deps])`,
/// each module `export const __wasm_split_load_<module> = …` of the same shape.
fn module_files(link_script: &str, module: &str) -> Result<Vec<String>> {
  let mut loaders = BTreeMap::new();
  for part in loader_definitions(link_script) {
    let Some((name, rest)) = part.split_once(" = ") else {
      continue;
    };
    let Some((_, after_url)) = rest.split_once("new URL(\"./") else {
      continue;
    };
    let Some((file, _)) = after_url.split_once('"') else {
      continue;
    };
    let deps = rest
      .rsplit_once(", [")
      .and_then(|(_, list)| list.split_once(']'))
      .map(|(list, _)| {
        list
          .split(',')
          .map(str::trim)
          .filter(|dep| !dep.is_empty())
          .map(str::to_owned)
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();
    loaders.insert(name.trim().to_owned(), (file.to_owned(), deps));
  }
  let loader = format!("__wasm_split_load_{module}");
  let (file, deps) = loaders
    .get(&loader)
    .with_context(|| format!("the chunk loader has no module {module}"))?;
  let mut files = vec![file.clone()];
  for dep in deps {
    let (file, _) = loaders
      .get(dep)
      .with_context(|| format!("the chunk loader has no {dep}"))?;
    files.push(file.clone());
  }
  Ok(files)
}

/// The top-level loader definitions of the chunk loader, each from its name (`__chunk_<n>`,
/// `__wasm_split_load_<module>`) to the next one.
fn loader_definitions(link_script: &str) -> Vec<&str> {
  const STARTS: [&str; 2] = ["\nconst ", "\nexport const "];
  let mut starts = STARTS
    .iter()
    .flat_map(|start| {
      link_script
        .match_indices(start)
        .map(|(at, matched)| at + matched.len())
    })
    .filter(|&at| link_script[at..].starts_with("__"))
    .collect::<Vec<_>>();
  starts.sort_unstable();
  starts
    .iter()
    .enumerate()
    .map(|(index, &at)| &link_script[at..starts.get(index + 1).copied().unwrap_or(link_script.len())])
    .collect()
}

/// Joins the files of the signed-in app into one, which `init.js` fetches next to the app. The split
/// leaves that module with hundreds of shared chunks (one per combination of modules that share
/// code), most of them under 1 kB: as files, each is a request (through the service worker, in a
/// signed release), a streaming compile and an instantiation of its own (#1465). The files stay as
/// they are next to the pack: a guest's meeting loads its parts of them one by one, and a page
/// whose pack didn't arrive falls back on them.
fn write_session_pack(staging: &Path, names: &Names, session_files: &[String]) -> Result<SessionPack> {
  let mut bytes = Vec::new();
  let mut parts = Vec::with_capacity(session_files.len());
  for file in session_files {
    let part = std::fs::read(staging.join(file)).with_context(|| format!("reading {file}"))?;
    parts.push(PackedPart {
      file: file.clone(),
      offset: bytes.len(),
      len: part.len(),
    });
    bytes.extend_from_slice(&part);
  }
  std::fs::write(staging.join(&names.session_pack), bytes)?;
  Ok(SessionPack {
    file: names.session_pack.clone(),
    parts,
  })
}

/// The chunk loader with the pack's files taken from the pack: each of them says where it is, every
/// other file is still fetched by itself (`fetchPart` in [`PACK_LOADER`]).
fn with_session_pack(link_script: &str, pack: &SessionPack) -> Result<String> {
  let mut script = link_script.to_owned();
  for part in &pack.parts {
    let from = format!("fetch(new URL(\"./{}\", import.meta.url))", part.file);
    if !script.contains(&from) {
      bail!("the chunk loader doesn't fetch {}", part.file);
    }
    script = script.replace(
      &from,
      &format!(
        "fetchPart(new URL(\"./{}\", import.meta.url), {}, {})",
        part.file, part.offset, part.len
      ),
    );
  }
  if !script.contains(INSTANTIATE_STREAMING) {
    bail!("the chunk loader doesn't instantiate its parts the way this tool expects");
  }
  let script = script.replace(INSTANTIATE_STREAMING, "instantiatePart(src, imports)");
  let (first_line, rest) = script
    .split_once('\n')
    .context("the chunk loader has no import line")?;
  Ok(format!(
    "{first_line}\n{}{rest}",
    PACK_LOADER.replace("__PACK_PARTS__", &pack.parts.len().to_string())
  ))
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
fn write_init(staging: &Path, names: &Names, initializer: &str, pack: &SessionPack) -> Result<()> {
  let wasm_size = std::fs::metadata(staging.join(&names.main_wasm))?.len();
  let init = INIT_TEMPLATE
    .replace("__SESSION_PACK__", &format!("{PUBLIC_URL}{}", pack.file))
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
    r#"<link rel="preload" href="{PUBLIC_URL}{}" crossorigin="anonymous" as="fetch" type="application/wasm">"#,
    names.main_wasm
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

/// How wasm-split's loader instantiates a part (`wasm_split_cli_support`, `js.rs`).
const INSTANTIATE_STREAMING: &str = "WebAssembly.instantiateStreaming(src, imports)";

/// Goes into the chunk loader, after its import line: how it gets and instantiates its parts.
const PACK_LOADER: &str = r#"
// The signed-in app's parts come out of one file, the pack `init.js` fetches next to the app
// (#1465): hundreds of tiny modules, each its own request and compile, were most of a phone's start.
// A part that isn't in the pack, or a pack that didn't arrive, is fetched on its own as before.
// Parts this small compile faster right here than they wait for a turn of the event loop.
const MAX_COMPILED_AT_ONCE = 64 * 1024;
let partsToTake = __PACK_PARTS__;
function fetchPart(url, offset, length) {
  const pack = globalThis.teamcraftSessionPack;
  if (!pack) return fetch(url);
  // The last part asked for lets the pack's bytes go once the modules are compiled.
  partsToTake -= 1;
  if (partsToTake === 0) globalThis.teamcraftSessionPack = undefined;
  return pack.then((bytes) => new Uint8Array(bytes, offset, length), () => fetch(url));
}
async function instantiatePart(src, imports) {
  const part = await src;
  if (part instanceof Response) return WebAssembly.instantiateStreaming(part, imports);
  if (part.byteLength <= MAX_COMPILED_AT_ONCE) {
    try {
      return new WebAssembly.Instance(new WebAssembly.Module(part), imports);
    } catch (error) {
      // A browser may refuse to compile on the main thread above its own limit.
      if (!(error instanceof RangeError)) throw error;
    }
  }
  return WebAssembly.instantiate(part, imports);
}
"#;

/// Starts the app like Trunk's initializer does: the wasm is fetched with progress for the loading
/// screen (`loader.js` default export: `onStart`, `onProgress`, `onComplete`, `onSuccess`,
/// `onFailure`).
const INIT_TEMPLATE: &str = r#"import init from '__MAIN_JS__';
import initializer from '__INITIALIZER__';

// Local timing of the start (#1465, teamcraft-ui start_timing.rs): marks only when asked for with
// `localStorage` `teamcraft.startTiming`; the app reads them at its first view. Nothing is sent.
let isTiming = false;
try {
  isTiming = localStorage.getItem('teamcraft.startTiming') === '1';
} catch (error) {
  // Storage blocked: no timing.
}
const mark = (name) => isTiming && performance.mark(`teamcraft:${name}`);

// The signed-in app is a chunk of its own (areas.rs): its files, in one pack (#1465), are fetched at
// once, next to the app, unless this is a guest's meeting link (`/meeting/<id>#invite=…`, meetings.rs
// `parse_guest_link`) or a share link's download page (`/f/<id>#…`, share_link.rs), which never need
// it. The chunk loader takes them from it; without it, it fetches them one by one.
const sessionPack = '__SESSION_PACK__';
const isGuestLink = (/^\/meeting\/[^/]+\/?$/.test(location.pathname) && /(^#|&)invite=[^&]/.test(location.hash))
  || (/^\/f\/[A-Za-z0-9_-]+\/?$/.test(location.pathname) && location.hash.length > 1);
if (!isGuestLink) {
  mark('session-pack:start');
  globalThis.teamcraftSessionPack = fetch(sessionPack).then(async (response) => {
    // A file the server doesn't have comes back as the app's page (its fallback), with 200.
    if (!response.ok || (response.headers.get('content-type') || '').startsWith('text/html')) {
      throw new Error(`${sessionPack}: HTTP ${response.status}`);
    }
    const bytes = await response.arrayBuffer();
    mark('session-pack:end');
    return bytes;
  });
  // Nobody may be waiting yet; the chunk loader falls back on the files.
  globalThis.teamcraftSessionPack.catch(() => {});
}

// The UI's texts (teamcraft-ui i18n.rs): the app carries none, so the language it starts in is
// fetched next to it, and the app waits for them before anything mounts; it never shows a message's
// key. `teamcraftTranslationFiles` names each language's file of this build (scripts/finish-loader.sh
// puts it at the top of this file). The choice is the app's (settings.rs `stored_locale`): the saved
// setting, else the browser's language, else English.
const translationFiles = globalThis.teamcraftTranslationFiles || {};
globalThis.teamcraftTranslations = startTranslations();

function startLanguage() {
  let saved = null;
  try {
    saved = localStorage.getItem('teamcraft.locale');
  } catch (error) {
    // Storage blocked: the browser's language.
  }
  for (const tag of [saved, navigator.language]) {
    const primary = String(tag || '').split(/[-_]/)[0].toLowerCase();
    if (translationFiles[primary]) return primary;
  }
  return 'en';
}

async function startTranslations() {
  const tag = startLanguage();
  const file = translationFiles[tag];
  if (!file) throw new Error(`this build has no texts for ${tag}`);
  mark('texts-fetch:start');
  const response = await fetch(file);
  // A file the server doesn't have comes back as the app's page (its fallback), with 200.
  if (!response.ok || (response.headers.get('content-type') || '').startsWith('text/html')) {
    throw new Error(`${file}: HTTP ${response.status}`);
  }
  const text = await response.text();
  mark('texts-fetch:end');
  return { tag, text };
}

const hooks = initializer();
hooks.onStart?.();
const total = __WASM_SIZE__;
// No integrity here: the browser would hold the body back until it is checked, which stops the
// progress bar and streaming compilation. It protects nothing, the same server sends index.html;
// the signed release check is the service worker's.
mark('wasm-fetch:start');
const response = fetch('__MAIN_WASM__').then((response) => {
  const reader = response.body.getReader();
  let current = 0;
  const stream = new ReadableStream({
    async pull(controller) {
      const { done, value } = await reader.read();
      if (done) {
        mark('wasm-fetch:end');
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
// Without its texts the app doesn't mount: that is a failure to load too.
const started = init({ module_or_path: response }).then((value) => {
  mark('wasm:ready');
  return value;
});
Promise.all([started, globalThis.teamcraftTranslations]).then(
  ([value]) => {
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
    assert_eq!(build_id(b"a", "").len(), 16);
    assert_ne!(
      build_id(b"a", "wasm-opt 131\n-Oz"),
      build_id(b"a", "wasm-opt 132\n-Oz")
    );
  }

  #[test]
  fn a_module_needs_its_own_file_and_its_shared_chunks() {
    let script = r#"
const __chunk_1 = makeLoad(() => {
    const src = fetch(new URL("./app-chunk-1-id.wasm", import.meta.url));
}
, []);
const __chunk_2 = makeLoad(() => {
    const src = fetch(new URL("./app-chunk-2-id.wasm", import.meta.url));
}
, []);
export const __wasm_split_load_session = wrapAsyncCb(makeLoad(() => {
    const src = fetch(new URL("./app-split-session-id.wasm", import.meta.url));
}
, [__chunk_2]));
export const __wasm_split_load_admin = wrapAsyncCb(makeLoad(() => {
    const src = fetch(new URL("./app-split-admin-id.wasm", import.meta.url));
}
, [__chunk_1, __chunk_2]));
"#;
    assert_eq!(
      module_files(script, "session").unwrap(),
      ["app-split-session-id.wasm", "app-chunk-2-id.wasm"]
    );
    assert_eq!(module_files(script, "admin").unwrap().len(), 3);
    assert!(module_files(script, "missing").is_err());
  }

  fn pack_of(parts: &[(&str, usize, usize)]) -> SessionPack {
    SessionPack {
      file: "app-session-id.pack".to_owned(),
      parts: parts
        .iter()
        .map(|&(file, offset, len)| PackedPart {
          file: file.to_owned(),
          offset,
          len,
        })
        .collect(),
    }
  }

  const LINK_SCRIPT: &str = r#"import { initSync } from "./app-id.js";
const __chunk_1 = makeLoad(() => {
    const src = fetch(new URL("./app-chunk-1-id.wasm", import.meta.url));
    src.catch(() => {});
    return async (imports) => (WebAssembly.instantiateStreaming(src, imports));
}
, []);
export const __wasm_split_load_admin = wrapAsyncCb(makeLoad(() => {
    const src = fetch(new URL("./app-split-admin-id.wasm", import.meta.url));
    src.catch(() => {});
    return async (imports) => (WebAssembly.instantiateStreaming(src, imports));
}
, []));
"#;

  #[test]
  fn the_pack_holds_each_file_after_the_other() {
    let dir = std::env::temp_dir().join(format!("web-bundle-pack-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.wasm"), b"aaa").unwrap();
    std::fs::write(dir.join("b.wasm"), b"bb").unwrap();
    let names = Names::new("app", "0123456789abcdef");
    let pack = write_session_pack(&dir, &names, &["a.wasm".to_owned(), "b.wasm".to_owned()]).unwrap();
    assert_eq!(pack.file, "app-session-0123456789abcdef.pack");
    assert_eq!(std::fs::read(dir.join(&pack.file)).unwrap(), b"aaabb");
    assert_eq!((pack.parts[1].offset, pack.parts[1].len), (3, 2));
    assert!(write_session_pack(&dir, &names, &["missing.wasm".to_owned()]).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn the_loader_takes_the_packed_files_from_the_pack_and_fetches_the_others() {
    let script = with_session_pack(LINK_SCRIPT, &pack_of(&[("app-chunk-1-id.wasm", 7, 412)])).unwrap();
    assert!(script.contains(r#"fetchPart(new URL("./app-chunk-1-id.wasm", import.meta.url), 7, 412)"#));
    assert!(script.contains(r#"const src = fetch(new URL("./app-split-admin-id.wasm", import.meta.url));"#));
    assert!(!script.contains("(WebAssembly.instantiateStreaming(src, imports))"));
    assert_eq!(script.matches("(instantiatePart(src, imports))").count(), 2);
    // Right after the import, which must stay the module's first statement.
    assert!(script.starts_with("import { initSync } from \"./app-id.js\";\n\n// The signed-in app's parts"));
    assert!(script.contains("let partsToTake = 1;"));
  }

  #[test]
  fn a_loader_that_does_not_look_as_expected_stops_the_build() {
    assert!(with_session_pack(LINK_SCRIPT, &pack_of(&[("not-there.wasm", 0, 1)])).is_err());
    let other_way = LINK_SCRIPT.replace(INSTANTIATE_STREAMING, "WebAssembly.instantiate(src, imports)");
    assert!(with_session_pack(&other_way, &pack_of(&[("app-chunk-1-id.wasm", 0, 1)])).is_err());
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
