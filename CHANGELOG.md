# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-05-15

### Fixed

- Wasmtime invocations now place the `--` separator *before* the wasm
  path so that arguments forwarded to the wasm program (criterion's
  `--sample-size`, libtest's `--test-threads`, and even cargo's default
  `--bench`) are no longer re-parsed as wasmtime options. Previously
  `cargo bench --target wasm32-wasip1` silently skipped all benches when
  wasmtime was the selected engine.
- Engine auto-detection now prefers V8 (`d8`) over wasmtime, so vanilla
  `cargo bench` / `cargo test` against `wasm32-wasip1[-threads]` picks
  up the JS-shell path with full threading support when V8 is present.
  Wasmtime is still auto-selected when no JS shell is installed.

## [0.2.0] - 2026-05-15

### Added

- `wasmtime` is now supported as an engine alongside the JS shells. When
  `--engine wasmtime` (or a `wasmtime` binary on `$PATH`) is selected, the
  runner invokes wasmtime's native WASI directly and skips the bundled JS
  driver. Env-var forwarding (whitelist / `WASM_HARNESS_ENV_*` /
  `--inherit-env`) and program-arg passthrough work the same as for JS
  shells. wasmtime is now the first auto-detected engine.

### Changed

- **Breaking:** engine-selector env var renamed from `JS_SHELL` to
  `WASM_HARNESS_ENGINE`. The CLI flag is still `--engine`.
- **Breaking:** `--shell-flag` renamed to `--engine-flag`. Forwards to
  whichever engine is in use (e.g. `--liftoff-only` for d8,
  `-W threads=y` for wasmtime).

## [0.1.0] - 2026-05-13

### Added

- Initial public release. Benchmark and test WebAssembly inside browser
  JavaScript engines — V8 via `d8`, SpiderMonkey via `js` / `sm`. Wasm is
  built for `wasm32-wasip1` / `wasm32-wasip1-threads` and executed via a
  bundled minimal WASI snapshot_preview1 polyfill. Usable directly
  (`wasm-harness <file.wasm>`) or as a cargo runner. Supports criterion
  benches, libtest tests, and real threading (d8 only) via Worker-per-thread.

[Unreleased]: https://github.com/sinui0/wasm-harness/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/sinui0/wasm-harness/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sinui0/wasm-harness/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sinui0/wasm-harness/releases/tag/v0.1.0
