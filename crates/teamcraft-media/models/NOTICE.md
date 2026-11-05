# Models

| File                                 | Source                                                                                     | License    |
|--------------------------------------|--------------------------------------------------------------------------------------------|------------|
| `selfie_segmentation_landscape.onnx` | MediaPipe Selfie Segmentation (landscape) by Google, ONNX conversion by the ONNX Community | Apache-2.0 |
| `selfie_segmentation_landscape.nnef` | The same model converted to NNEF (tract's tar archive) by `fetch-nnef.sh`                  | Apache-2.0 |

`fetch.sh` downloads the model from the ONNX Community's Hugging Face repository
`onnx-community/mediapipe_selfie_segmentation_landscape` at a pinned revision and checks its
SHA-256 (`selfie_segmentation_landscape.onnx.sha256`). `fetch-nnef.sh` runs it, converts the model to
NNEF with tract (the `segmentation_model_nnef` example of teamcraft-media; needs cargo) and checks the
result against its own pin (`selfie_segmentation_landscape.nnef.sha256`): the browser's segmentation
worker reads NNEF without tract's ONNX parser, which made up most of its module (#1295). The
conversion keeps the graph as it is (the same masks) and gives the same file for the same tract
version. Neither file is kept in git; packaging fetches or makes them and bundles them with the apps:

| App     | Where the model goes                                                                                                 |
|---------|----------------------------------------------------------------------------------------------------------------------|
| Desktop | Tauri resource `models/` (`apps/desktop/tauri.models.conf.json`, release builds)                                     |
| Android | `app/src/main/assets/models/` (`apps/android/scripts/build.sh`)                                                      |
| Web     | `models/<name>-<hash>.nnef` next to the web client's files (`crates/teamcraft-ui/scripts/add-segmentation-model.sh`) |

At runtime the desktop looks at `TEAMCRAFT_SEGMENTATION_MODEL` first (an override for
development), then at the bundled resource. Without a model, background effects are off.

Apache-2.0 requires passing on the license and this notice with the model: apps list it in their
about/licenses screen (Copyright Google LLC, Apache License 2.0,
https://www.apache.org/licenses/LICENSE-2.0).
