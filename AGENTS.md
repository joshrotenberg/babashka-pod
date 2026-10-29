# babashka-pod

A Rust library for writing babashka pods (`src/`), and pods built on it (`pods/`), in one Cargo
workspace. The README documents the library API and the pod release tooling.

## Checks

Run `cj run check` before every push. It runs these from the repository root and reports each as
EDN; every step must pass:

| Step | Command |
|---|---|
| `:fmt` | `cargo fmt --all -- --check` |
| `:clippy` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `:doc` | `RUSTDOCFLAGS="-D warnings" cargo doc -p babashka-pod --no-deps` |
| `:test` | `REQUIRE_BB=1 cargo test --workspace` (the babashka tests fail instead of skipping when `bb` is missing) |
| `:release-test` | `bb release-test` |
| `:package` | `cargo publish -p babashka-pod --dry-run --allow-dirty` |
| `:em-dashes` | no em dashes in tracked text files |
| `:commits` | commits in `origin/main..HEAD` have conventional-commit subjects and no attribution trailers |

`cj run check --skip-test --skip-package` is the quick form. CI (`.github/workflows/ci.yml`) runs
fmt, clippy, build, test and `bb release-test`.

## Crate and pods

- The library crate `babashka-pod` is the only package published to crates.io. release-plz
  (`release-plz.toml`, `.github/workflows/release-plz.yml`) publishes it on a push to `main` when
  its version is not on crates.io yet, and tags it `v<version>`. A published version cannot be
  deleted, so a version bump merged to `main` is a release.
- Each pod lives in `pods/<name>` as crate `pod-<name>` with `publish = false`. Pods are
  distributed as binaries: a `pod-<name>-v<version>` tag builds them, packages them with
  `bb package`, writes a pod-registry manifest with `bb manifest`, and creates a GitHub release.
  `bb check-version` checks the tag against the pod's `Cargo.toml` version.
- Every pod has a `test.bb` and a `tests/bb.rs` that runs it against the built binary, so
  `cargo test --workspace` covers the pods.
- A gap found in the library while building a pod is filed as its own issue and fixed in the
  library, not worked around in the pod.

## Writing

- Conventional-commit prefixes on commit subjects, PR titles and branch names (`feat/`, `fix/`,
  `chore/`, ...).
- No em dashes in code, comments, docs, commits or PRs.
- State what changed and why. No marketing tone.
- No `Co-Authored-By` or other attribution trailers, and no "Generated with" lines.
