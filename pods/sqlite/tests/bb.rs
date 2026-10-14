//! Runs `pods/sqlite/test.bb` against the pod binary with a real babashka,
//! over both transports. Skipped when `bb` is not on PATH, unless
//! `REQUIRE_BB` is set (CI sets it).

use std::path::Path;
use std::process::Command;

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
    let output = Command::new("bb")
        .arg("pods/sqlite/test.bb")
        .args(args)
        .env("SQLITE_POD", env!("CARGO_BIN_EXE_pod-sqlite"))
        .current_dir(root)
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
        r#"handle: {:handle "connection", :id 1}"#,
        "insert: {:changes 1, :last-insert-id 1}",
        "named params: {:changes 1, :last-insert-id 2}",
        r#"query: [{:id 1, :name "ada"} {:id 2, :name "bob"}]"#,
        r#"query params: [{:name "bob"}]"#,
        r#"transaction: ("committed")"#,
        "execute-many: {:changes 1000}",
        "count: [{:n 1001}]",
        r#"constraint error: ["UNIQUE constraint failed: users.email" "insert into users (name, email) values (?, ?)"]"#,
        r#"missing table: "no such table: nope""#,
        r#"param count: "statement takes 1 parameter, got 0""#,
        "json: [{:level 3, :roles 2}]",
        "blob: [true [0 1 2 -1]]",
        r#"fts5: ("pods" "rust")"#,
        "rtree: [{:id 1}]",
        r#"prepared: [[{:name "ada"}] [{:name "bob"}]]"#,
        r#"finalized: "unknown or closed statement handle 1""#,
        "stream: 1001",
        r#"csv: [{:s 0.6, :task "test"} {:s 4.1, :task "jvm"}]"#,
        r#"reopened: [{:v "persisted"}]"#,
        r#"closed: "unknown or closed connection handle 2""#,
        r#"doc: "Runs a query and returns its rows as maps with keyword keys.""#,
        "done",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
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
