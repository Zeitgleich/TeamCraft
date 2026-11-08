# Vendored: tract-nnef

| Item         | Value                                                                          |
|--------------|--------------------------------------------------------------------------------|
| Upstream     | https://github.com/sonos/tract, directory `nnef` (crate `tract-nnef` on crates.io) |
| Version      | 0.23.9, upstream commit `ee878dcb69f7385fc48611b056568effcb2b32cf`             |
| License      | MIT OR Apache-2.0 (`LICENSE`, `LICENSE-MIT`, `LICENSE-APACHE`, from the package) |
| Used by      | `teamcraft-media` (segmentation, the browser's worker module), via `[patch.crates-io]` |
| Why vendored | A feature that leaves the `tract_core` registry's operators out (#1336)       |

## Why

The browser's background effects run the person segmentation in a wasm module of its own
(`teamcraft-web-segmentation`), downloaded when someone first turns an effect on. It reads the
model as NNEF (#1295). tract-nnef's `tract_core` registry registers the loaders and dumpers of every
operator tract-core has, and with them their implementations: FFT and STFT (rustfft), LSTM, GRU,
scans, quantized convolutions and matrix products, top-k, scatter, grid sampling and more. The
model needs none of them: of `tract_core` its NNEF file uses only `tract_core_hard_swish` and
`tract_core_cast`. The registry can't be replaced from outside: its operator modules are private,
and `Nnef::default()` (with the full registry) is reachable from every reader anyway (the
submodel loader in `framework.rs`, the `patch` model transform, registered through `inventory`).

With the feature the module (`build-segmentation.sh`'s release build) is 4,293,005 instead of
5,515,207 bytes, 1,230,189 instead of 1,588,652 gzip -9 (−358 kB, −23 %).

## Changes against upstream 0.23.9

The first commit adding this directory is the unmodified crates.io package; `git log -p --
crates/vendor/tract-nnef` shows every change since.

1. `Cargo.toml`: new feature `minimal_core_registry` (off by default), and an empty `[workspace]`
   table so the copy builds and tests on its own (our workspace excludes `crates/vendor`).
2. `src/ops/core.rs`: with `minimal_core_registry`, `register` registers the element-wise and binary
   operators it registers directly and the cast (`cast.rs`), and leaves out the operator modules
   (`broadcast` … `trilu`, also `complex`): they move into `register_operators`, which the feature
   leaves out with them.
3. Added this file.

Without the feature nothing changes. `teamcraft-media`'s `segmentation-nnef` turns it on, so the
apps' ONNX-to-NNEF conversion (`onnx_to_nnef`) writes with the same registry; the conversion of the
real model still gives the pinned file (`tests/suite/segmentation_model.rs`), and a tract upgrade
that writes another core operator for it fails there and in `the_nnef_archive_is_the_same_every_time`.

## Upstream

Not proposed yet. An upstream version would rather split the registry by cargo features (e.g.
`fft`, `rnn`, `quant`, default on); the crates that depend on tract-nnef with
`default-features = false` (tract-onnx, tract-onnx-opl, tract-extra) would then have to ask for them.

## Updating

Drop this directory and the `[patch.crates-io]` entry once tract offers a way to leave the core
operators out, or re-apply change 2 to the new version (the module list in `src/ops/core.rs`).
