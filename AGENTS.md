# Agent instructions

A Rust library for writing babashka pods, plus pods built with it. Read
[README.md](README.md) for the API and the pods.

## Layout

- The library crate is at the repository root (`src/`, `examples/`, `tests/`).
- Each pod is a workspace member under `pods/<name>` with its own README.
- `release/` holds the release tooling (bb tasks in `bb.edn`, tests in
  `release/test/`).

## Checks

Run `cj run check` before pushing; it must report `:ok? true`. It runs the CI
gates (`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo build --workspace`, `cargo test --workspace` with `REQUIRE_BB=1`,
`bb release-test`), `cargo doc` with warnings as errors, the tests for its own
helpers in `.cj/test/`, and the commit subject, attribution and em dash rules
below on tracked files and the branch's commits. Without cj, run the cargo and
bb commands directly.

## Conventions

- Work on a branch and open a draft pull request; never commit to `main`.
- Conventional-commit prefixes (`feat:`, `fix:`, `docs:`, `chore:`, ...) for
  commit subjects and pull request titles.
- No em dashes in code, docs, commits or pull requests.
- No attribution lines: no `Co-Authored-By` or other trailers, no
  "Generated with" footers.
- Do not publish crates, push tags or create releases; those are the
  maintainer's.
