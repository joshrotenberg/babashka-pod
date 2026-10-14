//! Runs `examples/example.bb` against the example pod with a real babashka.
//! Skipped when `bb` is not on PATH.

use std::path::PathBuf;
use std::process::Command;

fn example_pod() -> PathBuf {
    // cargo test builds examples into target/<profile>/examples, next to the
    // deps directory holding this test binary.
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap();
    profile_dir
        .join("examples")
        .join(format!("example_pod{}", std::env::consts::EXE_SUFFIX))
}

fn run_example(args: &[&str]) -> Option<String> {
    let bb_found = Command::new("bb").arg("--version").output().is_ok();
    if !bb_found {
        eprintln!("bb not found on PATH, skipping");
        return None;
    }
    let pod = example_pod();
    assert!(pod.exists(), "example pod not built at {}", pod.display());
    let output = Command::new("bb")
        .arg("examples/example.bb")
        .args(args)
        .env("EXAMPLE_POD", &pod)
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

fn check(stdout: &str) {
    for expected in [
        "add: 6.5",
        "greet: {:greeting hello, babashka, :length 8}",
        "doc: Adds any number of numbers.",
        "divide: 3",
        "divide error: divide by zero {:a 1, :b 0}",
        "count-up: [0 1 2 3 4]",
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
