//! Runs `pods/fswatcher/test.bb` against the pod binary with a real babashka,
//! over both transports, and checks that the pod process is gone once bb has
//! exited. Skipped when `bb` is not on PATH, unless `REQUIRE_BB` is set (CI
//! sets it).

use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// bb returns once the pod has exited after `shutdown`; the runtime's grace
/// period is two seconds, so a clean run finishes well inside this.
const MAX_RUN: Duration = Duration::from_secs(10);

fn run(args: &[&str]) -> Option<String> {
    if Command::new("bb").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("REQUIRE_BB").is_none(),
            "bb not found on PATH and REQUIRE_BB is set"
        );
        eprintln!("bb not found on PATH, skipping");
        return None;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let started = Instant::now();
    let output = Command::new("bb")
        .arg("pods/fswatcher/test.bb")
        .args(args)
        .env("FSWATCHER_POD", env!("CARGO_BIN_EXE_pod-fswatcher"))
        .current_dir(root)
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "bb failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed < MAX_RUN, "bb took {elapsed:?} to finish");
    Some(stdout)
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The pod processes bb reported must be gone once bb has exited.
fn check_no_orphans(stdout: &str) {
    let pids: Vec<&str> = stdout
        .lines()
        .find_map(|l| l.strip_prefix("pod pids: "))
        .expect("pod pids line")
        .split_whitespace()
        .collect();
    assert!(!pids.is_empty(), "bb reported no pod process");
    let deadline = Instant::now() + Duration::from_secs(2);
    while pids.iter().any(|p| alive(p)) {
        assert!(
            Instant::now() < deadline,
            "pod process still running after bb exited: {pids:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn check(stdout: &str) {
    for expected in [
        "watcher: #:watcher{:id 1}",
        "second watcher: #:watcher{:id 2}",
        r#"event: {:path "<dir>/a.txt", :type :create}"#,
        "unwatch: #:watcher{:id 1}",
        "unwatch again: #:watcher{:id 1}",
        r#"deduped a.txt events: [{:path "<dir>/a.txt", :type :create}]"#,
        "events after unwatch: []",
        "second watcher saw b.txt: true",
        r#"missing path: "cannot watch <dir>/nope: "#,
        r#"bad unwatch: "pod.babashka.fswatcher/unwatch expects a watcher returned by watch""#,
        r#"doc: "Watches path and calls (callback event) for each change. event is a map""#,
        "done",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
    check_no_orphans(stdout);
}

#[test]
fn stdio_transport() {
    if let Some(out) = run(&[]) {
        assert!(out.contains("transport: :stdio"));
        check(&out);
    }
}

#[test]
fn socket_transport() {
    if let Some(out) = run(&["socket"]) {
        assert!(out.contains("transport: :socket"));
        check(&out);
    }
}
