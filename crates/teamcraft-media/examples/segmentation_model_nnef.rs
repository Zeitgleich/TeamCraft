//! Converts the segmentation model the apps bundle (ONNX, `models/fetch.sh`) to NNEF, tract's own
//! format, for the browser (#1295): its segmentation worker reads NNEF without tract's ONNX parser,
//! which was most of that module. `models/fetch-nnef.sh` runs this; see there.
//!
//! ```sh
//! cargo run --release -p teamcraft-media --no-default-features --features segmentation \
//!   --example segmentation_model_nnef -- <model.onnx> <model.nnef> [--pin]
//! ```
//!
//! The result must be the pinned file (`models/selfie_segmentation_landscape.nnef.sha256`), which
//! the worker checks; anything else is refused and not written. With `--pin` the result is written
//! whatever its hash, for `fetch-nnef.sh --pin` to pin it: after a tract upgrade that changes the
//! archive (check the masks first: `tests/suite/segmentation_model.rs` compares them with the ONNX
//! model's).

use std::process::ExitCode;

use teamcraft_media::effects::{SegmentationModel, check_segmentation_model, onnx_to_nnef};

fn main() -> ExitCode {
  let arguments: Vec<String> = std::env::args().skip(1).collect();
  let (is_pinning, paths): (Vec<&String>, Vec<&String>) =
    arguments.iter().partition(|argument| *argument == "--pin");
  let [onnx_path, nnef_path] = paths.as_slice() else {
    report("usage: segmentation_model_nnef <model.onnx> <model.nnef> [--pin]");
    return ExitCode::FAILURE;
  };
  match convert(onnx_path, nnef_path, !is_pinning.is_empty()) {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      report(&error);
      ExitCode::FAILURE
    },
  }
}

fn convert(onnx_path: &str, nnef_path: &str, is_pinning: bool) -> Result<(), String> {
  let onnx = std::fs::read(onnx_path).map_err(|error| format!("reading {onnx_path}: {error}"))?;
  let nnef =
    onnx_to_nnef(&onnx, SegmentationModel::MEDIAPIPE_LANDSCAPE).map_err(|error| error.to_string())?;
  if !is_pinning {
    check_segmentation_model(&nnef).map_err(|error| {
      format!("{error}: not the pinned NNEF file (a tract upgrade changes it; see --pin)")
    })?;
  }
  std::fs::write(nnef_path, &nnef).map_err(|error| format!("writing {nnef_path}: {error}"))?;
  Ok(())
}

/// The tool's one line of output: an example has no logging set up.
#[allow(clippy::print_stderr)]
fn report(message: &str) {
  eprintln!("segmentation_model_nnef: {message}");
}
