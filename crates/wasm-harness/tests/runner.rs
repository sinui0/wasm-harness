//! End-to-end smoke test: build the example crate's test binary for
//! `wasm32-wasip1`, run it through the runner, and check the output.
//!
//! Skipped automatically when no JS shell is available — the test runner
//! itself emits a `println!` note and returns success rather than failing,
//! so it's safe to run unconditionally in CI environments that may or may
//! not have d8/sm installed.
//!
//! Override the shell via `$JS_SHELL` (or `$D8`).

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn js_shell_available() -> bool {
    if std::env::var_os("JS_SHELL").is_some() || std::env::var_os("D8").is_some() {
        return true;
    }
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    for dir in std::env::split_paths(&path) {
        for name in ["d8", "v8", "js", "sm", "spidermonkey"] {
            if dir.join(name).is_file() {
                return true;
            }
        }
    }
    false
}

fn wasm32_wasip1_installed() -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-wasip1"))
        .unwrap_or(false)
}

#[test]
fn example_tests_run_under_js_shell() {
    if !js_shell_available() {
        println!("skipping: no JS shell on PATH and $JS_SHELL/$D8 unset");
        return;
    }
    if !wasm32_wasip1_installed() {
        println!("skipping: wasm32-wasip1 target not installed");
        return;
    }

    let root = workspace_root();

    // Build the example tests for wasi.
    let build = Command::new("cargo")
        .current_dir(&root)
        .args([
            "test",
            "-p",
            "example",
            "--test",
            "smoke",
            "--target",
            "wasm32-wasip1",
            "--release",
            "--no-run",
            "--message-format=json",
        ])
        .stderr(Stdio::inherit())
        .output()
        .expect("cargo test --no-run should succeed");
    assert!(build.status.success(), "cargo --no-run failed");

    // Find the produced .wasm executable from the json messages.
    let stdout = String::from_utf8_lossy(&build.stdout);
    let mut wasm_path: Option<String> = None;
    for line in stdout.lines() {
        if !line.contains("\"executable\"") {
            continue;
        }
        if let Some(start) = line.find("\"executable\":\"") {
            let rest = &line[start + "\"executable\":\"".len()..];
            if let Some(end) = rest.find('"') {
                let candidate = &rest[..end];
                if candidate.ends_with(".wasm") {
                    wasm_path = Some(candidate.to_string());
                    break;
                }
            }
        }
    }
    let wasm = wasm_path.unwrap_or_else(|| {
        panic!(
            "no .wasm artifact in cargo output. status={}, stdout snippet:\n{}",
            build.status,
            stdout.lines().take(20).collect::<Vec<_>>().join("\n")
        )
    });

    // Locate the runner binary the same way Cargo would.
    let runner = PathBuf::from(env!("CARGO_BIN_EXE_wasm-harness"));

    let out = Command::new(&runner)
        .arg(&wasm)
        .output()
        .expect("runner should spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "runner exited non-zero\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("test result: ok"),
        "missing libtest summary in:\n{stdout}"
    );
    assert!(
        stdout.contains("test_fib_10 ... ok"),
        "missing per-test line in:\n{stdout}"
    );
}
