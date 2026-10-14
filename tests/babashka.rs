//! Runs the example bb scripts against the example pods with a real
//! babashka, over both transports. Skipped when `bb` is not on PATH, unless
//! `REQUIRE_BB` is set (CI sets it).

use std::path::PathBuf;
use std::process::Command;

fn example_binary(name: &str) -> PathBuf {
    // cargo test builds examples into target/<profile>/examples, next to the
    // deps directory holding this test binary.
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap();
    profile_dir
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Runs `examples/<script>` with the `<pod>` example binary passed in the
/// `<env>` variable.
fn run_script(script: &str, pod: &str, env: &str, args: &[&str]) -> Option<String> {
    let bb_found = Command::new("bb").arg("--version").output().is_ok();
    if !bb_found {
        // CI sets REQUIRE_BB so a missing bb fails the run instead of
        // passing it silently.
        assert!(
            std::env::var_os("REQUIRE_BB").is_none(),
            "bb not found on PATH and REQUIRE_BB is set"
        );
        eprintln!("bb not found on PATH, skipping");
        return None;
    }
    let pod = example_binary(pod);
    assert!(pod.exists(), "example pod not built at {}", pod.display());
    let output = Command::new("bb")
        .arg(format!("examples/{script}"))
        .args(args)
        .env(env, &pod)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "bb failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(stdout)
}

fn run_example(args: &[&str]) -> Option<String> {
    run_script("example.bb", "example_pod", "EXAMPLE_POD", args)
}

fn run_transit(args: &[&str]) -> Option<String> {
    run_script("transit.bb", "transit_pod", "TRANSIT_POD", args)
}

fn check(stdout: &str) {
    for expected in [
        "add: 6.5",
        "greet: {:greeting hello, babashka, :length 8}",
        "doc: Adds any number of numbers.",
        "divide: 3",
        "divide error: divide by zero {:a 1, :b 0}",
        "count-up: [0 1 2 3 4]",
        "stream doc: Streams the integers from 0 below n from the pod, calling f with each.",
        "stream error: invalid arguments for pod.joshrotenberg.example/count-up*",
        "no callback: the last argument to pod.joshrotenberg.example/count-up must be a callback fn",
        "bad args: invalid arguments for pod.joshrotenberg.example/add",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}

#[test]
fn stdio_transport() {
    if let Some(out) = run_example(&[]) {
        assert!(out.contains("transport: :stdio"));
        check(&out);
    }
}

#[test]
fn socket_transport() {
    if let Some(out) = run_example(&["socket"]) {
        assert!(out.contains("transport: :socket"));
        check(&out);
    }
}

fn check_transit(stdout: &str) {
    for expected in [
        "tag-set is a set of keywords: true",
        "kw-info: {:kw :my.ns/thing, :name \"thing\", :ns \"my.ns\"}",
        "plus-days: #inst \"2026-10-15T00:00:00.000-00:00\" java.util.Date",
        "point: {:x 3, :y 4}",
        "meta-of: {:source \"repl\"}",
        "meta-of plain: nil",
        "fail: failed on purpose {:kind :boom}",
        "ticks: [:tick/0 :tick/1 :tick/2]",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}

#[test]
fn transit_stdio_transport() {
    if let Some(out) = run_transit(&[]) {
        assert!(out.contains("transport: :stdio"));
        check_transit(&out);
    }
}

#[test]
fn transit_socket_transport() {
    if let Some(out) = run_transit(&["socket"]) {
        assert!(out.contains("transport: :socket"));
        check_transit(&out);
    }
}
