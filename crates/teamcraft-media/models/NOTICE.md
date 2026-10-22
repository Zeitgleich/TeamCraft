# Models

| File                                 | Source                                                                                     | License    |
|--------------------------------------|--------------------------------------------------------------------------------------------|------------|
| `selfie_segmentation_landscape.onnx` | MediaPipe Selfie Segmentation (landscape) by Google, ONNX conversion by the ONNX Community | Apache-2.0 |

`fetch.sh` downloads the model from the ONNX Community's Hugging Face repository
`onnx-community/mediapipe_selfie_segmentation_landscape` at a pinned revision and checks its
SHA-256 (`selfie_segmentation_landscape.onnx.sha256`). The model is not kept in git; packaging
fetches it and bundles it with the apps:

| App     | Where the model goes                                                             |
|---------|----------------------------------------------------------------------------------|
| Desktop | Tauri resource `models/` (`apps/desktop/tauri.models.conf.json`, release builds) |
| Android | `app/src/main/assets/models/` (`apps/android/scripts/build.sh`)                  |
| Web     | `models/` next to the web client's files, served by the server as a static file  |

At runtime the desktop looks at `TEAMCRAFT_SEGMENTATION_MODEL` first (an override for
development), then at the bundled resource. Without a model, background effects are off.

Apache-2.0 requires passing on the license and this notice with the model: apps list it in their
about/licenses screen (Copyright Google LLC, Apache License 2.0,
https://www.apache.org/licenses/LICENSE-2.0).
