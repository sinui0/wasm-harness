# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial public release. Benchmark and test WebAssembly inside browser
  JavaScript engines — V8 via `d8`, SpiderMonkey via `js` / `sm`. Wasm is
  built for `wasm32-wasip1` / `wasm32-wasip1-threads` and executed via a
  bundled minimal WASI snapshot_preview1 polyfill. Usable directly
  (`wasm-harness <file.wasm>`) or as a cargo runner. Supports criterion
  benches, libtest tests, and real threading (d8 only) via Worker-per-thread.

[Unreleased]: https://github.com/sinui0/wasm-harness/commits/main
