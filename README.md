# wasm-harness

Benchmark and test WebAssembly inside the JavaScript engines that ship in
browsers — V8 (Chrome/Edge/Node) via `d8`, and SpiderMonkey (Firefox) via
`js`/`sm`. Plug it into `cargo test` / `cargo bench` for a WASI target and
any libtest binary or bench harness (criterion, etc.) runs unmodified.

## Quickstart

```bash
# 1. Get a JS shell. jsvu is the easiest way:
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
`wasm32-wasip1` / `wasm32-wasip1-threads` and executed under the JS shell
with a bundled minimal WASI snapshot_preview1 polyfill.

The binary also works standalone:

```bash
wasm-harness --engine sm path/to/bench.wasm
```

## Engine selection

`--engine` and `$JS_SHELL` accept either a **path** or a **short name**
(`v8`, `sm`, `d8`, `spidermonkey`, `js`), resolved against `$PATH` and
`~/.jsvu/bin/`. Precedence: `--engine` > `$JS_SHELL` > auto-search.

Pin per-target in `.cargo/config.toml`:

```toml
[target.wasm32-wasip1]
runner = ["wasm-harness", "--engine", "sm"]
```

## Runner flags

```text
--engine <SHELL>      JS shell path or short name. Also reads $JS_SHELL.
--shell-flag <FLAG>   Extra flag for the JS shell itself (e.g.
                      `--liftoff-only` for d8). Repeatable.
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

- **No persistent filesystem.** Writes through `path_open` succeed but
  vanish; reads return EOF. Test fixtures, criterion baselines, log files —
  all appear to succeed but leave nothing behind.
- **Threading: d8 only.** On `wasm32-wasip1-threads`, each `thread-spawn`
  becomes a real `Worker` re-instantiating the module against shared
  `env.memory`. SpiderMonkey's Worker API differs and isn't wired up yet;
  programs fall back to single-threaded there.
- **Clock precision is `performance.now()`-bound.** Sub-microsecond timing
  is noisier than on a native host.
