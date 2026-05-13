//! `wasm-harness` — benchmark and test WebAssembly inside browser JavaScript
//! engines (V8 via `d8`, SpiderMonkey via `js`/`sm`). Takes a wasm file and
//! runs it under the chosen JS shell with a minimal WASI snapshot_preview1
//! polyfill. Designed to slot into `.cargo/config.toml` as a runner for
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
//! runner = ["wasm-harness", "--engine", "v8"]
//! ```
//!
//! Install with `cargo install wasm-harness` so the binary lands on
//! `$PATH`.
//!
//! Cargo invokes the runner as `wasm-harness [flags] <wasm> [program
//! args]`. The runner spawns the JS shell with the bundled driver (a
//! minimal WASI snapshot_preview1 polyfill), forwards stdout/stderr, and
//! propagates the exit code.
//!
//! ### Engine selection
//!
//! In order of precedence: `--engine`, then `$JS_SHELL`, then an automatic
//! search of `$PATH` and `~/.jsvu/bin/` for known engine binaries: `d8`,
//! `v8`, `sm`, `spidermonkey`, `js`.
//!
//! Both `--engine` and `$JS_SHELL` accept either a path (used verbatim) or
//! a short engine name (resolved against the same search dirs).
//!
//! ### Environment forwarding
//!
//! The wasm program runs in the shell's sandbox and inherits no host env by
//! default. The runner forwards a whitelist to the wasm program via
//! `--env=KEY=VALUE` script args:
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
    "d8",           // V8 upstream
    "v8",           // jsvu alias for V8
    "sm",           // jsvu alias for SpiderMonkey
    "spidermonkey", // distro-packaged SpiderMonkey
    "js",           // SpiderMonkey upstream
];

/// Run a wasm32-wasip1 binary under a JS shell (d8 / SpiderMonkey).
///
/// Forwards the wasm program's stdout/stderr/exit-code and propagates
/// configured host environment variables through a `--env=KEY=VALUE`
/// channel the bundled JS driver understands.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// JS shell to use. Accepts a path or a short engine name
    /// (v8, sm, d8, spidermonkey, js). Overrides $JS_SHELL.
    #[arg(long, value_name = "SHELL", env = "JS_SHELL")]
    engine: Option<String>,

    /// Extra flag to pass to the JS shell itself (e.g. `--liftoff-only` for
    /// d8). Repeatable. Hyphen-prefixed values are accepted as-is.
    #[arg(long = "shell-flag", value_name = "FLAG", allow_hyphen_values = true)]
    shell_flags: Vec<String>,

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    let driver_js = write_driver_script(&cli.wasm)?;
    let shell = resolve_engine(cli.engine.as_deref())?;

    let mut cmd = Command::new(&shell);
    for flag in &cli.shell_flags {
        cmd.arg(flag);
    }
    cmd.arg(&driver_js);
    cmd.arg("--");
    cmd.arg(&cli.wasm);
    // Tell the driver where its own file lives so worker threads can
    // re-spawn it via `new Worker(driverPath)`.
    let mut driver_path_arg = OsString::from("--driver-path=");
    driver_path_arg.push(&driver_js);
    cmd.arg(driver_path_arg);
    cmd.args(&cli.program_args);

    for (key, value) in env::vars_os() {
        if let Some(injection) = host_env_to_arg(&key, &value, cli.inherit_env) {
            cmd.arg(injection);
        }
    }

    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    let status = cmd
        .status()
        .with_context(|| format!("failed to spawn JS shell at {}", shell.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Render an environment variable to the `--env=KEY=VALUE` form the driver
/// expects, if the variable is being forwarded.
fn host_env_to_arg(key: &OsStr, value: &OsStr, inherit_env: bool) -> Option<OsString> {
    let key_str = key.to_str()?;
    let dest_key = if inherit_env {
        Some(key_str)
    } else {
        forwarded_name(key_str)
    }?;
    let mut arg = OsString::from("--env=");
    arg.push(dest_key);
    arg.push("=");
    arg.push(value);
    Some(arg)
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

/// Resolve the engine to execute. Priority: explicit `--engine` / `$JS_SHELL`
/// (clap handles that merge), then a fallback search of `$PATH` and
/// `~/.jsvu/bin/` for known engine names.
fn resolve_engine(requested: Option<&str>) -> Result<PathBuf> {
    let search_dirs = engine_search_dirs();
    if let Some(req) = requested {
        return resolve_named_or_path(req, &search_dirs)
            .with_context(|| format!("resolving engine {req:?}"));
    }
    for name in KNOWN_ENGINES {
        if let Some(p) = find_in(&search_dirs, name) {
            return Ok(p);
        }
    }
    anyhow::bail!(
        "no JS shell found on $PATH or in ~/.jsvu/bin/. Install jsvu \
         (https://github.com/GoogleChromeLabs/jsvu) and run `jsvu`, or pass \
         --engine <path-or-name>."
    )
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
            "v8",
            "--shell-flag",
            "--liftoff-only",
            "--inherit-env",
            "/tmp/bench.wasm",
            "--bench",
            "fib",
            "--sample-size",
            "10",
        ])
        .expect("clap should accept these args");

        assert_eq!(cli.engine.as_deref(), Some("v8"));
        assert_eq!(cli.shell_flags, vec!["--liftoff-only"]);
        assert!(cli.inherit_env);
        assert_eq!(cli.wasm, PathBuf::from("/tmp/bench.wasm"));
        assert_eq!(
            cli.program_args,
            vec!["--bench", "fib", "--sample-size", "10"]
        );
    }

    #[test]
    fn host_env_to_arg_inherit_passes_unknowns() {
        let key = OsString::from("PATH");
        let value = OsString::from("/usr/bin");
        let arg = host_env_to_arg(&key, &value, true).unwrap();
        assert_eq!(arg.to_string_lossy(), "--env=PATH=/usr/bin");
    }

    #[test]
    fn host_env_to_arg_default_drops_unknowns() {
        let key = OsString::from("PATH");
        let value = OsString::from("/usr/bin");
        assert!(host_env_to_arg(&key, &value, false).is_none());
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
