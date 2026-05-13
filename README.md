# wasm-harness

Benchmark and test WebAssembly inside the JavaScript engines that ship in
browsers — V8 (Chrome/Edge/Node) via `d8`, and SpiderMonkey (Firefox) via
`js`/`sm`. The binary takes a wasm file and runs it under the chosen engine
with a minimal WASI polyfill; the wasm program itself runs unmodified, so
any libtest binary or bench harness (criterion, etc.) works out of the box.
Plug it into `.cargo/config.toml` as a runner for the wasi target and plain
`cargo test` / `cargo bench` just work.

Under the hood the wasm is built for `wasm32-wasip1` /
`wasm32-wasip1-threads`; that's the only Rust target that currently
produces real-world-ish wasi binaries which work outside a browser
sandbox.

## How it works

```text
cargo test/bench --target wasm32-wasip1
        │
        ▼ (cargo invokes the configured runner)
wasm-harness <wasm>
        │
        ▼ (spawns)
<js-shell> driver.js -- <wasm> <args> --env=K=V ...
        │
        ▼ (instantiates)
WebAssembly.Instance(wasm, { wasi_snapshot_preview1: <polyfill> })
```

The polyfill is a few hundred lines of JS implementing the WASI
snapshot_preview1 subset typical Rust binaries use: clocks, stdout/stderr,
args, environ, random, sched_yield, a single `/` preopen, and a discard
filesystem (writes succeed silently, reads return EOF).

## Quick start

```bash
# Get a JS shell.
npm install -g jsvu && jsvu     # installs ~/.jsvu/bin/{v8,sm,...}

# Build the runner.
cargo build --release -p wasm-harness

# Run the example (auto-discovers ~/.jsvu/bin/v8).
cargo test  -p example --target wasm32-wasip1
cargo bench -p example --target wasm32-wasip1

# Pick a specific engine by short name:
JS_SHELL=sm cargo test -p example --target wasm32-wasip1
```

## Using in your own crate

1. Install the runner: `cargo install wasm-harness` (or `--path
   crates/wasm-harness` if running from this workspace).
2. Drop a `.cargo/config.toml` into your project:

   ```toml
   [target.wasm32-wasip1]
   runner = ["wasm-harness"]

   [target.wasm32-wasip1-threads]
   runner = ["wasm-harness"]
   ```

3. `cargo test --target wasm32-wasip1` and similar. No source-level changes
   to the crate under test are required.

## Engine selection

`--engine` and `$JS_SHELL` accept either a **path** (used verbatim) or a
**short engine name** (`v8`, `sm`, `d8`, `spidermonkey`, `js`), resolved
against `$PATH` and `~/.jsvu/bin/`. Precedence: `--engine` > `$JS_SHELL` >
auto-search of those two dirs for the first known engine.

```bash
# By short name (jsvu users), env or flag — both work:
JS_SHELL=sm cargo test --target wasm32-wasip1
wasm-harness --engine sm bench.wasm

# By absolute path:
JS_SHELL=/opt/v8/d8 cargo test --target wasm32-wasip1
```

Pinning the engine in `.cargo/config.toml` for a specific target:

```toml
[target.wasm32-wasip1]
runner = ["wasm-harness", "--engine", "sm"]
```

## Other runner flags

```text
--shell-flag <FLAG>   Pass a flag to the JS shell itself (e.g.
                      `--liftoff-only` for d8). Repeatable.
--inherit-env         Forward every host env var to the wasm program
                      instead of the default whitelist.
```

Run `wasm-harness --help` for the full list.

## Environment forwarding

The wasm program inherits no host environment by default (it runs in a JS
sandbox). The runner forwards a small whitelist:

* `RUST_*`, `RAYON_*`, `NO_COLOR`, plus anything bench-harness-specific
  like `CRITERION_*`
* anything named `WASM_HARNESS_ENV_X` is forwarded as `X` (opt-in escape hatch)

### Threading libraries

wasi's libstd reports `available_parallelism() == 1`, so anything that
auto-detects core count (rayon's global pool, tokio's default worker count,
etc.) sees a single CPU and runs serially. To get parallelism, set the
library's pool-size env var in the host shell — e.g.

```bash
RAYON_NUM_THREADS=8 cargo bench --target wasm32-wasip1-threads
```

The runner forwards `RAYON_*` and `RUST_*` automatically.

## Limitations

* **No persistent filesystem.** `path_open` returns a discard fd; writes
  succeed but vanish. Reads of those files return EOF. Anything trying to
  persist output to disk (test fixtures, benchmark baselines, log files)
  will appear to succeed but leave nothing behind. Not hard to add a
  real-fs preopen later if needed.
* **Threading: d8 only.** On `wasm32-wasip1-threads`, each `thread-spawn`
  becomes a real `new Worker(driver.js, {type:'classic'})` that
  re-instantiates the wasm module against the shared `env.memory`.
  Cross-thread synchronisation uses the wasm engine's native
  `memory.atomic.wait`/`memory.atomic.notify` on the `SharedArrayBuffer` —
  no futex syscalls implemented in the WASI shim. SpiderMonkey's Worker
  API differs and isn't supported yet: `thread-spawn` returns -1 there,
  and programs fall back to single-threaded. `proc_exit` from any thread
  brings the whole process down (matches WASI semantics); thread-local
  `quit()` is deliberately avoided in workers.
* **Clock precision is `performance.now()`-bound.** Sub-microsecond timing
  may be noisier than on a native host.
* **One memory import only.** The wasm spec already enforces this, but the
  import-section parser bails on unknown import kinds — future wasm
  extensions may need a tweak.

## Releasing

The CD pipeline (`.github/workflows/cd.yml`) publishes to crates.io on any
pushed tag matching `[v]?MAJOR.MINOR.PATCH`. The release flow:

1. Bump `version` in the workspace `Cargo.toml` (`[workspace.package]`).
2. Move the relevant entries in `CHANGELOG.md` from `[Unreleased]` into a
   new `[X.Y.Z] — YYYY-MM-DD` section.
3. Commit on `main`.
4. Tag: `git tag v0.X.Y && git push --tags`.

GitHub Actions runs `cargo publish -p wasm-harness` using the
`CARGO_REGISTRY_TOKEN` repository secret. The `example` crate has
`publish = false` and is left alone.

## Layout

```
.cargo/config.toml                  runner config
crates/wasm-harness/
  src/main.rs                       runner binary
  js/driver.js                      bundled JS driver (WASI polyfill)
crates/example/
  src/lib.rs                        sample crate
  benches/fib.rs                    sample bench (criterion)
  tests/smoke.rs                    sample tests (#[test])
```
