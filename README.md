# wasm-harness

Benchmark and test WebAssembly under a chosen engine: the JavaScript
engines that ship in browsers — V8 (Chrome/Edge/Node) via `d8`, and
SpiderMonkey (Firefox) via `js`/`sm` — or the native `wasmtime` runtime.
Plug it into `cargo test` / `cargo bench` for a WASI target and any
libtest binary or bench harness (criterion, etc.) runs unmodified.

## Quickstart

```bash
# 1. Get an engine. Either install wasmtime (https://wasmtime.dev/),
#    or grab a JS shell via jsvu:
npm install -g jsvu && jsvu

# 2. Install wasm-harness.
cargo install wasm-harness

# 3. Add a runner to your project's .cargo/config.toml:
cat >> .cargo/config.toml <<'EOF'
[target.wasm32-wasip1]
runner = ["wasm-harness"]

[target.wasm32-wasip1-threads]
runner = ["wasm-harness"]
EOF

# 4. Bench / test as usual.
cargo test  --target wasm32-wasip1
cargo bench --target wasm32-wasip1
```

No source-level changes to the crate under test. wasm is built for
`wasm32-wasip1` / `wasm32-wasip1-threads` and executed under the chosen
engine. For JS shells a minimal WASI snapshot_preview1 polyfill is
bundled; `wasmtime` uses its native WASI implementation.

The binary also works standalone:

```bash
wasm-harness --engine wasmtime path/to/bench.wasm
```

## Engine selection

`--engine` and `$WASM_HARNESS_ENGINE` accept either a **path** or a
**short name** (`d8`, `v8`, `sm`, `spidermonkey`, `js`, `wasmtime`),
resolved against `$PATH` and `~/.jsvu/bin/`. Precedence: `--engine` >
`$WASM_HARNESS_ENGINE` > auto-search. Auto-search prefers V8 when
installed (for threading support), falling back to other JS shells and
finally wasmtime.

Pin per-target in `.cargo/config.toml`:

```toml
[target.wasm32-wasip1]
runner = ["wasm-harness", "--engine", "wasmtime"]
```

## Runner flags

```text
--engine <ENGINE>     Engine path or short name. Also reads
                      $WASM_HARNESS_ENGINE.
--engine-flag <FLAG>  Extra flag for the engine itself (e.g.
                      `--liftoff-only` for d8, `-W threads=y` for
                      wasmtime). Repeatable.
--inherit-env         Forward every host env var instead of the whitelist.
```

`wasm-harness --help` for the full list.

## Environment forwarding

The wasm program runs in the shell's sandbox and inherits no host env by
default. The runner auto-forwards `CRITERION_*`, `RUST_*`, `RAYON_*`, and
`NO_COLOR`. For anything else, set `WASM_HARNESS_ENV_X=value` on the host
(forwarded as `X=value`) or pass `--inherit-env` to forward everything.

### Threading

wasi's libstd reports `available_parallelism() == 1`, so libraries that
auto-detect core count (rayon, tokio, …) run serially. Set the library's
pool-size env var on the host:

```bash
RAYON_NUM_THREADS=8 cargo bench --target wasm32-wasip1-threads
```

## Limitations

- **No persistent filesystem.** Under JS shells, writes through `path_open`
  succeed but vanish and reads return EOF — test fixtures, criterion
  baselines, and log files appear to succeed but leave nothing behind.
  Under wasmtime, the wasm guest sees no preopened directories by default
  (pass `--engine-flag --dir=...` to mount one).
- **Threading.** On `wasm32-wasip1-threads`, real threads work under d8
  (each `thread-spawn` becomes a `Worker` re-instantiating the module
  against shared `env.memory`) and under wasmtime when the runtime is
  invoked with `-W threads=y -S threads=y` (forward those via
  `--engine-flag`). SpiderMonkey's Worker API differs and isn't wired up
  yet; programs fall back to single-threaded there.
- **Clock precision under JS shells is `performance.now()`-bound.**
  Sub-microsecond timing is noisier there than on a native host or under
  wasmtime.
