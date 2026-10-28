//! A wasm module that runs in a module worker: the person segmentation of the browser's background
//! effects (#439, `teamcraft-web-segmentation`). `scripts/build-segmentation.sh` builds it with
//! its own compiler flags (WebAssembly SIMD), which Trunk can't set for one module alone, and runs
//! this, which
//!
//! 1. runs wasm-bindgen on it (`--target web`),
//! 2. optimizes it with wasm-opt (release),
//! 3. names the module after the build (`<name>-<id>.js`, `<name>-<id>_bg.wasm`, as the app's),
//! 4. writes the worker's script with the module's name in place of `__SEGMENTATION_MODULE__`,
//!    named after its content (`<script-name>-<hash>.js`), which `finish-calls.sh` names in
//!    `index.html`.
//!
//! ```text
//! teamcraft-web-bundle worker --input <module.wasm> --staging <dir> --name <name>
//!                             --script <worker.js> --script-name <name>
//!                             [--wasm-opt <binary> [--wasm-opt-arg <arg>]...]
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::{build_id, hex, optimize, optimizer_settings, report};

/// Where the worker's script names the module.
const MODULE_PLACEHOLDER: &str = "__SEGMENTATION_MODULE__";
/// How the module is bound (part of its build id).
const BINDGEN_SETTINGS: &str = "wasm-bindgen --target web, default module path";

pub struct WorkerArgs {
  input: PathBuf,
  staging: PathBuf,
  name: String,
  script: PathBuf,
  script_name: String,
  maybe_wasm_opt: Option<PathBuf>,
  wasm_opt_args: Vec<String>,
}

impl WorkerArgs {
  pub fn parse(mut raw: impl Iterator<Item = String>) -> Result<Self> {
    let (mut input, mut staging, mut name, mut script, mut script_name, mut maybe_wasm_opt) =
      (None, None, None, None, None, None);
    let mut wasm_opt_args = Vec::new();
    while let Some(flag) = raw.next() {
      let mut value = || raw.next().with_context(|| format!("{flag} needs a value"));
      match flag.as_str() {
        "--input" => input = Some(PathBuf::from(value()?)),
        "--staging" => staging = Some(PathBuf::from(value()?)),
        "--name" => name = Some(value()?),
        "--script" => script = Some(PathBuf::from(value()?)),
        "--script-name" => script_name = Some(value()?),
        "--wasm-opt" => maybe_wasm_opt = Some(PathBuf::from(value()?)),
        "--wasm-opt-arg" => wasm_opt_args.push(value()?),
        other => bail!("unknown argument {other}"),
      }
    }
    Ok(Self {
      input: input.context("--input is missing")?,
      staging: staging.context("--staging is missing")?,
      name: name.context("--name is missing")?,
      script: script.context("--script is missing")?,
      script_name: script_name.context("--script-name is missing")?,
      maybe_wasm_opt,
      wasm_opt_args,
    })
  }
}

pub fn run(args: WorkerArgs) -> Result<()> {
  let input = std::fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
  // The glue's settings count too: the same module bound otherwise is another file.
  let settings = format!(
    "{BINDGEN_SETTINGS}\n{}",
    optimizer_settings(args.maybe_wasm_opt.as_deref(), &args.wasm_opt_args)?
  );
  let id = build_id(&input, &settings);
  let stem = format!("{}-{id}", args.name);
  wasm_bindgen_cli_support::Bindgen::new()
    .input_path(&args.input)
    .out_name(&stem)
    .web(true)?
    // The worker's script calls `init()` without a path: the module finds its wasm next to itself.
    .omit_default_module_path(false)
    .remove_name_section(true)
    .remove_producers_section(true)
    .typescript(false)
    .generate(&args.staging)
    .context("wasm-bindgen")?;
  let wasm = args.staging.join(format!("{stem}_bg.wasm"));
  if let Some(wasm_opt) = &args.maybe_wasm_opt {
    optimize(wasm_opt, &args.wasm_opt_args, &wasm)?;
  }
  let source =
    std::fs::read_to_string(&args.script).with_context(|| format!("reading {}", args.script.display()))?;
  let script = worker_script(&source, &format!("./{stem}.js"))?;
  let script_file = format!(
    "{}-{}.js",
    args.script_name,
    hex(&Sha256::digest(script.as_bytes())[..8])
  );
  std::fs::write(args.staging.join(&script_file), script)?;
  tracing::info!(script = %script_file, "worker");
  report(&args.staging, &[wasm])
}

/// The worker's script with the module's path filled in.
fn worker_script(source: &str, module: &str) -> Result<String> {
  if !source.contains(MODULE_PLACEHOLDER) {
    bail!("the worker's script doesn't name {MODULE_PLACEHOLDER}");
  }
  Ok(source.replace(MODULE_PLACEHOLDER, module))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_script_imports_the_module_of_its_build() {
    let script = worker_script("import init from '__SEGMENTATION_MODULE__';", "./seg-0123.js").unwrap();
    assert_eq!(script, "import init from './seg-0123.js';");
    assert!(worker_script("import init from './elsewhere.js';", "./seg.js").is_err());
  }

  #[test]
  fn arguments_are_read_and_checked() {
    let raw = [
      "--input",
      "a.wasm",
      "--staging",
      "dist",
      "--name",
      "seg",
      "--script",
      "worker.js",
      "--script-name",
      "seg-worker",
      "--wasm-opt-arg",
      "-O3",
    ];
    let args = WorkerArgs::parse(raw.iter().map(ToString::to_string)).unwrap();
    assert_eq!(
      (args.name.as_str(), args.script_name.as_str()),
      ("seg", "seg-worker")
    );
    assert_eq!(args.wasm_opt_args, ["-O3"]);
    assert!(
      WorkerArgs::parse(raw[..8].iter().map(ToString::to_string)).is_err(),
      "no script name"
    );
  }
}
