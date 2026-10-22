# teamcraft-media

Media for TeamCraft calls (ARCHITECTURE.md §6): RTP and the datagram framing, jitter buffer,
bandwidth estimation, DAVE end-to-end encryption, the platform-neutral call engine, and on native
platforms Opus, audio devices, echo cancellation and the WebTransport call driver.

| Module                        | What                                                          |
|-------------------------------|---------------------------------------------------------------|
| `rtp`, `datagram`, `fragment` | Wire formats (RTP + audio level, TeamCraft header, fragments) |
| `jitter`, `bwe`               | Jitter buffer, send-side bandwidth estimation                 |
| `dave`, `call_auth`           | DAVE groups, call key bindings (e2ee.md §8)                   |
| `call_engine`                 | Sans-I/O call client used by desktop, web and mobile          |
| `video`                       | Video frame types, simulcast layer plans, codec choice        |
| `audio_frame`, `audio_io`     | Audio frame unit, sources, sinks and device choices           |
| `audio_*`, `call_native`      | Native only (`native` feature): Opus, devices, driver         |
| `call_web`, `web_api`         | Browser only (`web` feature): WebTransport, WebCodecs driver  |

## Features

| Feature  | Default | What                                                                         |
|----------|---------|------------------------------------------------------------------------------|
| `dave`   | yes     | DAVE and the call engine; builds for `wasm32-unknown-unknown`                |
| `native` | no      | Opus, audio devices, the WebTransport driver and the `teamcraft-call` binary |
| `web`    | no      | Browser driver: WebTransport, WebCodecs Opus, AudioWorklet (wasm32)          |

`native` builds libopus with cmake and uses cpal for audio devices, which needs the ALSA headers on
Linux (`libasound2-dev` on Debian/Ubuntu, `alsa-lib-devel` on Fedora).

## Echo cancellation (build-time switch)

Echo cancellation, noise suppression and gain control come from WebRTC's audio processing module
(`webrtc-audio-processing` 2.1, C++). It is switched on with a `cfg` flag instead of a feature, so
`cargo … --all-features` keeps building on machines without the C++ toolchain. Without it,
`AudioProcessor` passes audio through and logs one warning: calls work, just without echo
cancellation.

- **Without (default):** nothing to do.
- **System library:** `RUSTFLAGS="--cfg teamcraft_audio_processing"`. Needs
  `webrtc-audio-processing-2` via pkg-config (Fedora: `webrtc-audio-processing-devel` and
  `abseil-cpp-devel`), libclang and a C++ compiler.
- **Built from source:** `RUSTFLAGS="--cfg teamcraft_audio_processing --cfg
  teamcraft_audio_processing_bundled"`. Needs meson, ninja, libclang, a C++ compiler and
  `rust-objcopy` (part of rustup toolchains, missing from some distributions' Rust packages).

For example, with the system library:

```sh
RUSTFLAGS="--cfg teamcraft_audio_processing" cargo test -p teamcraft-media --features native
```

CI runs a separate `audio-processing` job with the bundled build, and the desktop release workflow
builds packages with it. `RUSTFLAGS` changes invalidate the build cache, so use a separate
`CARGO_TARGET_DIR` when switching often.

## Headless client

```sh
cargo run -p teamcraft-media --features native --bin teamcraft-call -- \
  --sfu https://127.0.0.1:4433/call --token dev:1:1:1 --user 1 --device 1 \
  --cert-hash <hash from the SFU's startup log> --source tone:440 --seconds 10
```

`--source mic --sink speakers` uses the default devices; `--list-devices` logs the device ids for
`--source mic:<id>` and `--sink speakers:<id>`. Apps list devices with
`audio_device::list_input_devices` / `list_output_devices` and switch mid-call with
`NativeCall::set_audio`. The client has no device keys, so its DAVE
keys are not bound to a device identity (it reports `UnauthenticatedKeys`); it is a development and
test tool.

## Browser driver

`call_web::WebCall` runs the same engine in the browser: WebTransport datagrams and a signaling
stream, WebCodecs Opus, AudioWorklet capture and playback, DAVE in WebAssembly. WebTransport and
WebCodecs are declared in `web_api` instead of enabling `web_sys_unstable_apis` workspace-wide.
Needs a browser with WebTransport and WebCodecs audio (Chrome/Edge, Firefox 130+).

The `web_call` example exports it to JavaScript for the SFU's `/engine` development page; build
steps are in `examples/web_call.rs`. With the SFU in dev mode, the page and `teamcraft-call` join
the same call and hear each other.
