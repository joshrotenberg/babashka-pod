(ns check
  "Run this repository's pre-push gates and return a pass/fail report. Changes nothing.

  The same gates as .github/workflows/ci.yml, plus the doc build and the
  writing rules, from the repository root:

  - :fmt           cargo fmt --all -- --check
  - :clippy        cargo clippy --workspace --all-targets -- -D warnings
  - :test          cargo test --workspace, with REQUIRE_BB=1 so the bb tests
                   fail instead of skipping when bb is missing
  - :doc           cargo doc --workspace --no-deps with warnings as errors
                   (skipped with --skip-doc)
  - :release-test  bb release-test
  - :em-dashes     no em dashes in tracked text files
  - :commits       commits in origin/main..HEAD have conventional-commit
                   subjects and no attribution lines (skipped when
                   origin/main does not exist)

  Every step runs even when an earlier one fails. Returns

    {:ok? false
     :steps {:fmt {:ok? true ...} :test {:ok? false :out \"...tail...\" :failures [...]} ...}
     :failed [:test]}

  Command steps keep the tail of their output; :failures lists the failing
  tests (`test ... FAILED`), panics, and clippy, rustdoc and rustfmt
  diagnostics found anywhere in the output."
  {:cj/args [{:name :skip-doc :type :boolean :doc "Skip the cargo doc step"}]
   :cj/examples ["cj run check" "cj run check --skip-doc"]}
  (:require [project.checks :as checks]))

(def cargo-failures
  "Lines that name what failed in cargo, clippy, rustdoc and rustfmt output."
  #"(?m)^(?:test .* \.\.\. FAILED|thread '.*' panicked at .*|error(?:\[\w+\])?: .*|warning: .*|Diff in .*)$")

(defn -main [{:keys [skip-doc]}]
  (let [root (checks/repo-root (System/getProperty "user.dir"))
        cargo (fn [argv & [env]] #(checks/command root argv {:env (or env {}) :failures cargo-failures}))
        steps (cond-> (array-map
                       :fmt (cargo ["cargo" "fmt" "--all" "--" "--check"])
                       :clippy (cargo ["cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"])
                       :test (cargo ["cargo" "test" "--workspace"] {"REQUIRE_BB" "1"}))
                (not skip-doc) (assoc :doc (cargo ["cargo" "doc" "--workspace" "--no-deps"] {"RUSTDOCFLAGS" "-D warnings"}))
                true (assoc :release-test #(checks/command root ["bb" "release-test"] {:failures #"(?m)^(?:FAIL|ERROR) in \(.*$"})
                            :em-dashes #(checks/em-dashes root)
                            :commits #(checks/commit-messages root)))
        results (into (array-map) (for [[k f] steps] [k (f)]))
        failed (vec (for [[k r] results :when (not (:ok? r))] k))
        skipped (cond-> [] skip-doc (conj :doc)
                  (get-in results [:commits :skipped?]) (conj :commits))]
    (cond-> {:ok? (empty? failed) :root root :steps results :failed failed}
      (seq skipped) (assoc :skipped skipped))))
