//! Runs `pods/treesitter/test.bb` against the pod binary with a real
//! babashka, over both transports. Skipped when `bb` is not on PATH, unless
//! `REQUIRE_BB` is set (CI sets it). Expects the default features (every
//! language).

#![cfg(all(
    feature = "clojure",
    feature = "go",
    feature = "javascript",
    feature = "python",
    feature = "rust",
    feature = "typescript"
))]

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
        .arg("pods/treesitter/test.bb")
        .args(args)
        .env("TREESITTER_POD", env!("CARGO_BIN_EXE_pod-treesitter"))
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
        "languages: [:clojure :go :javascript :python :rust :tsx :typescript]",
        "language-of: [:clojure :rust nil]",
        r#"sexp: "(source_file (function_item name: (identifier) parameters: (parameters) body: (block)))""#,
        r#"tree: ["module" "expression_statement" [1 5]]"#,
        r#"query: [["fn" "A" [2 5]] ["fn" "B" [3 5]]]"#,
        r#"outline: [["example.core" :module 1 3 "(ns example.core)"] ["limit" :variable 5 5 "(def limit)"]"#,
        r#"["area" :function 17 19 "(defn area ([w]) ([w h]))"]"#,
        r#"["describe" :method 26 27 "(defmethod describe :circle [shape])"]"#,
        r#"outline-file: [["area" :method "Area"] ["area" :method "Area for Shape"] ["fmt" :method "fmt::Display for Point"] ["helper" :function "inner"]]"#,
        "outline-file lang: 13",
        r#"unknown language: ["unknown language cobol" "cobol"]"#,
        r#"bad query: "rust query: unknown node type at row 1, column 1: \"nope\"""#,
        r#"no language: "no language for README.md""#,
        r#"doc: "The definitions in source, without bodies, in source order"#,
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
