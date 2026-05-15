//! `wasm-harness` — benchmark and test WebAssembly under a chosen engine.
//! Supports browser JS shells (V8 via `d8`, SpiderMonkey via `js`/`sm`) and
//! native wasm runtimes (`wasmtime`). Takes a wasm file and runs it under
//! the chosen engine; for JS shells a minimal WASI snapshot_preview1
//! polyfill is bundled, for `wasmtime` the runtime's native WASI is used.
//! Designed to slot into `.cargo/config.toml` as a runner for
//! `wasm32-wasip1` / `wasm32-wasip1-threads` targets, but the binary works
//! standalone too — `wasm-harness <file.wasm>`.
//!
//! Wire it up in `.cargo/config.toml`:
//!
//! ```toml
//! [target.wasm32-wasip1]
//! runner = ["wasm-harness"]
//!
//! [target.wasm32-wasip1-threads]
//! runner = ["wasm-harness", "--engine", "wasmtime"]
//! ```
//!
//! Install with `cargo install wasm-harness` so the binary lands on
//! `$PATH`.
//!
//! Cargo invokes the runner as `wasm-harness [flags] <wasm> [program
//! args]`. The runner spawns the selected engine, forwards stdout/stderr,
//! and propagates the exit code.
//!
//! ### Engine selection
//!
//! In order of precedence: `--engine`, then `$WASM_HARNESS_ENGINE`, then an
//! automatic search of `$PATH` and `~/.jsvu/bin/` for known engine
//! binaries: `wasmtime`, `d8`, `v8`, `sm`, `spidermonkey`, `js`.
//!
//! Both `--engine` and `$WASM_HARNESS_ENGINE` accept either a path (used
//! verbatim) or a short engine name (resolved against the same search
//! dirs).
//!
//! ### Environment forwarding
//!
//! The wasm program runs in the engine's sandbox and inherits no host env
//! by default. The runner forwards a whitelist to the wasm program:
//!
//! * `CRITERION_*`, `RUST_*`, `RAYON_*`, `NO_COLOR`
//! * `WASM_HARNESS_ENV_X` is forwarded as `X` (opt-in escape hatch)
//!
//! Pass `--inherit-env` to forward *every* host env var instead of the
//! whitelist (handy for non-Rust crates with their own env conventions).
//!
//! wasi's libstd reports `available_parallelism() == 1`, so threading
//! libraries that probe core count auto-default to single-threaded. Set
//! e.g. `RAYON_NUM_THREADS=N` in the host shell; the whitelist will
//! forward it.

use anyhow::{Context, Result};
use clap::Parser;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DRIVER_JS: &str = include_str!("../js/driver.js");
const DRIVER_FILE_NAME: &str = "wasm-harness-driver.js";

/// Engine binary names we know about, in preference order. `js` is last
/// because it's an ambiguous name on many systems.
const KNOWN_ENGINES: &[&str] = &[
    "wasmtime",     // Bytecode Alliance native wasm runtime
    "d8",           // V8 upstream
    "v8",           // jsvu alias for V8
    "sm",           // jsvu alias for SpiderMonkey
    "spidermonkey", // distro-packaged SpiderMonkey
    "js",           // SpiderMonkey upstream
];

/// Run a wasm32-wasip1 binary under a chosen engine (wasmtime, d8,
/// SpiderMonkey, ...).
///
/// Forwards the wasm program's stdout/stderr/exit-code and propagates
/// configured host environment variables.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// Engine to use. Accepts a path or a short engine name
    /// (wasmtime, v8, sm, d8, spidermonkey, js). Overrides
    /// $WASM_HARNESS_ENGINE.
    #[arg(long, value_name = "ENGINE", env = "WASM_HARNESS_ENGINE")]
    engine: Option<String>,

    /// Extra flag to pass to the engine itself (e.g. `--liftoff-only` for
    /// d8, `-W threads=y` for wasmtime). Repeatable. Hyphen-prefixed
    /// values are accepted as-is.
    #[arg(long = "engine-flag", value_name = "FLAG", allow_hyphen_values = true)]
    engine_flags: Vec<String>,

    /// Forward every host env var to the wasm program instead of the
    /// default whitelist.
    #[arg(long)]
    inherit_env: bool,

    /// Wasm artifact to execute.
    wasm: PathBuf,

    /// Arguments forwarded to the wasm program (libtest filter, criterion
    /// flags, etc.). Hyphenated values are accepted as-is.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    program_args: Vec<String>,
}

/// Which kind of engine we resolved to. Determines whether we go through
/// the bundled JS driver or invoke the runtime directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineKind {
    JsShell,
    Wasmtime,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let (engine_path, kind) = resolve_engine(cli.engine.as_deref())?;

    let mut cmd = Command::new(&engine_path);
    match kind {
        EngineKind::JsShell => build_js_shell_command(&mut cmd, &cli)?,
        EngineKind::Wasmtime => build_wasmtime_command(&mut cmd, &cli),
    }

    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    let status = cmd
        .status()
        .with_context(|| format!("failed to spawn engine at {}", engine_path.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Wire up a JS-shell invocation: write out the driver next to the wasm,
/// then `<shell> [flags] <driver.js> -- <wasm> --driver-path=... [args]
/// [--env=K=V ...]`.
fn build_js_shell_command(cmd: &mut Command, cli: &Cli) -> Result<()> {
    let driver_js = write_driver_script(&cli.wasm)?;
    for flag in &cli.engine_flags {
        cmd.arg(flag);
    }
    cmd.arg(&driver_js);
    cmd.arg("--");
    cmd.arg(&cli.wasm);
    let mut driver_path_arg = OsString::from("--driver-path=");
    driver_path_arg.push(&driver_js);
    cmd.arg(driver_path_arg);
    cmd.args(&cli.program_args);

    for (key, value) in env::vars_os() {
        if let Some(injection) = host_env_to_js_arg(&key, &value, cli.inherit_env) {
            cmd.arg(injection);
        }
    }
    Ok(())
}

/// Wire up a wasmtime invocation: `wasmtime run [engine-flags]
/// [--env K=V ...] -- <wasm> [program args]`. Env forwarding uses
/// wasmtime's native `--env` flag rather than the JS driver's
/// `--env=K=V` script-arg convention.
///
/// The `--` goes *before* the wasm path so wasmtime stops parsing
/// flags. Without it, criterion's `--sample-size`, libtest's
/// `--test-threads`, etc. get interpreted as wasmtime options and the
/// run fails with "unexpected argument found".
fn build_wasmtime_command(cmd: &mut Command, cli: &Cli) {
    cmd.arg("run");
    for flag in &cli.engine_flags {
        cmd.arg(flag);
    }
    for (key, value) in env::vars_os() {
        if let Some((k, v)) = forwarded_env_pair(&key, &value, cli.inherit_env) {
            cmd.arg("--env");
            let mut kv = OsString::from(k);
            kv.push("=");
            kv.push(v);
            cmd.arg(kv);
        }
    }
    cmd.arg("--");
    cmd.arg(&cli.wasm);
    cmd.args(&cli.program_args);
}

/// Render an environment variable to the `--env=KEY=VALUE` form the JS
/// driver expects, if the variable is being forwarded.
fn host_env_to_js_arg(key: &OsStr, value: &OsStr, inherit_env: bool) -> Option<OsString> {
    let (dest_key, value) = forwarded_env_pair(key, value, inherit_env)?;
    let mut arg = OsString::from("--env=");
    arg.push(dest_key);
    arg.push("=");
    arg.push(value);
    Some(arg)
}

/// Decide whether a host env var should be forwarded, and under what
/// destination name. Returns the destination `(key, value)` or `None` to
/// drop. Shared between JS-shell and wasmtime invocations so both engines
/// see the same env.
fn forwarded_env_pair<'a>(
    key: &'a OsStr,
    value: &'a OsStr,
    inherit_env: bool,
) -> Option<(&'a str, &'a OsStr)> {
    let key_str = key.to_str()?;
    let dest_key = if inherit_env {
        Some(key_str)
    } else {
        forwarded_name(key_str)
    }?;
    Some((dest_key, value))
}

/// Map a host env var name to its destination name in the wasm program, or
/// `None` to drop it. `WASM_HARNESS_ENV_FOO` is rewritten to `FOO`;
/// whitelisted names pass through unchanged.
fn forwarded_name(name: &str) -> Option<&str> {
    if let Some(suffix) = name.strip_prefix("WASM_HARNESS_ENV_") {
        if !suffix.is_empty() {
            return Some(suffix);
        }
    }
    if name == "NO_COLOR"
        || name.starts_with("CRITERION_")
        || name.starts_with("RUST_")
        || name.starts_with("RAYON_")
    {
        return Some(name);
    }
    None
}

/// Write the bundled driver next to the wasm artifact. We use a fixed file
/// name; cargo serializes runner invocations per build, so concurrent
/// writes to the same path don't happen in normal use.
fn write_driver_script(wasm: &Path) -> Result<PathBuf> {
    let dir = wasm
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dst = dir.join(DRIVER_FILE_NAME);
    std::fs::write(&dst, DRIVER_JS)
        .with_context(|| format!("writing driver script to {}", dst.display()))?;
    Ok(dst)
}

/// Resolve the engine to execute. Priority: explicit `--engine` /
/// `$WASM_HARNESS_ENGINE` (clap handles that merge), then a fallback
/// search of `$PATH` and `~/.jsvu/bin/` for known engine names.
fn resolve_engine(requested: Option<&str>) -> Result<(PathBuf, EngineKind)> {
    let search_dirs = engine_search_dirs();
    if let Some(req) = requested {
        let path = resolve_named_or_path(req, &search_dirs)
            .with_context(|| format!("resolving engine {req:?}"))?;
        let kind = classify_engine(&path);
        return Ok((path, kind));
    }
    for name in KNOWN_ENGINES {
        if let Some(p) = find_in(&search_dirs, name) {
            let kind = classify_engine(&p);
            return Ok((p, kind));
        }
    }
    anyhow::bail!(
        "no engine found on $PATH or in ~/.jsvu/bin/. Install wasmtime, \
         or install jsvu (https://github.com/GoogleChromeLabs/jsvu) and \
         run `jsvu`, or pass --engine <path-or-name>."
    )
}

/// Classify a resolved engine path by its file stem. Anything we don't
/// explicitly recognise as `wasmtime` is treated as a JS shell — that's
/// the historical behavior and keeps unknown-but-jsvu-like binaries
/// usable.
fn classify_engine(path: &Path) -> EngineKind {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if stem.eq_ignore_ascii_case("wasmtime") {
        EngineKind::Wasmtime
    } else {
        EngineKind::JsShell
    }
}

/// Resolve a user-supplied engine reference. Path-like values are used
/// verbatim; bare short names are looked up in the search dirs.
fn resolve_named_or_path(raw: &str, search_dirs: &[PathBuf]) -> Result<PathBuf> {
    if looks_like_path(raw) {
        let p = PathBuf::from(raw);
        if !is_executable(&p) {
            anyhow::bail!("{} is not an executable file", p.display());
        }
        return Ok(p);
    }
    find_in(search_dirs, raw).ok_or_else(|| {
        anyhow::anyhow!(
            "{raw:?} not found on $PATH or in ~/.jsvu/bin/. \
             Known engine names: {}.",
            KNOWN_ENGINES.join(", ")
        )
    })
}

fn looks_like_path(s: &str) -> bool {
    s.contains(std::path::MAIN_SEPARATOR) || s.contains('/') || s.starts_with('.')
}

fn engine_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|p| env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(jsvu) = jsvu_bin_dir() {
        if !dirs.contains(&jsvu) {
            dirs.push(jsvu);
        }
    }
    dirs
}

fn jsvu_bin_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(|h| PathBuf::from(h).join(".jsvu").join("bin"))
}

fn find_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    for dir in dirs {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_whitelisted_names_unchanged() {
        assert_eq!(forwarded_name("CRITERION_HOME"), Some("CRITERION_HOME"));
        assert_eq!(forwarded_name("RUST_BACKTRACE"), Some("RUST_BACKTRACE"));
        assert_eq!(
            forwarded_name("RAYON_NUM_THREADS"),
            Some("RAYON_NUM_THREADS")
        );
        assert_eq!(forwarded_name("NO_COLOR"), Some("NO_COLOR"));
    }

    #[test]
    fn rewrites_explicit_opt_in_prefix() {
        assert_eq!(forwarded_name("WASM_HARNESS_ENV_FOO"), Some("FOO"));
        assert_eq!(forwarded_name("WASM_HARNESS_ENV_FOO_BAR"), Some("FOO_BAR"));
    }

    #[test]
    fn drops_unrelated_names() {
        assert_eq!(forwarded_name("PATH"), None);
        assert_eq!(forwarded_name("HOME"), None);
        assert_eq!(forwarded_name("CRITERION"), None);
        assert_eq!(forwarded_name("WASM_HARNESS_ENV_"), None);
    }

    #[test]
    fn looks_like_path_recognises_path_separators() {
        assert!(looks_like_path("/usr/bin/d8"));
        assert!(looks_like_path("./d8"));
        assert!(looks_like_path("../d8"));
        assert!(!looks_like_path("d8"));
        assert!(!looks_like_path("v8"));
        assert!(!looks_like_path("spidermonkey"));
    }

    #[test]
    fn cli_parses_runner_flags_separately_from_program_args() {
        let cli = Cli::try_parse_from([
            "wasm-harness",
            "--engine",
            "wasmtime",
            "--engine-flag",
            "-W",
            "--engine-flag",
            "threads=y",
            "--inherit-env",
            "/tmp/bench.wasm",
            "--bench",
            "fib",
            "--sample-size",
            "10",
        ])
        .expect("clap should accept these args");

        assert_eq!(cli.engine.as_deref(), Some("wasmtime"));
        assert_eq!(cli.engine_flags, vec!["-W", "threads=y"]);
        assert!(cli.inherit_env);
        assert_eq!(cli.wasm, PathBuf::from("/tmp/bench.wasm"));
        assert_eq!(
            cli.program_args,
            vec!["--bench", "fib", "--sample-size", "10"]
        );
    }

    #[test]
    fn host_env_to_js_arg_inherit_passes_unknowns() {
        let key = OsString::from("PATH");
        let value = OsString::from("/usr/bin");
        let arg = host_env_to_js_arg(&key, &value, true).unwrap();
        assert_eq!(arg.to_string_lossy(), "--env=PATH=/usr/bin");
    }

    #[test]
    fn host_env_to_js_arg_default_drops_unknowns() {
        let key = OsString::from("PATH");
        let value = OsString::from("/usr/bin");
        assert!(host_env_to_js_arg(&key, &value, false).is_none());
    }

    #[test]
    fn classify_engine_recognises_wasmtime() {
        assert_eq!(
            classify_engine(Path::new("/usr/bin/wasmtime")),
            EngineKind::Wasmtime
        );
        assert_eq!(
            classify_engine(Path::new("/home/x/.cargo/bin/wasmtime")),
            EngineKind::Wasmtime
        );
        assert_eq!(
            classify_engine(Path::new("/usr/bin/d8")),
            EngineKind::JsShell
        );
        assert_eq!(classify_engine(Path::new("sm")), EngineKind::JsShell);
    }

    /// The clap docs recommend running `Command::debug_assert()` in a test
    /// so config-time mistakes (duplicate args, broken value parsers, etc.)
    /// fail the test suite rather than only surfacing at runtime.
    #[test]
    fn cli_definition_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
