(ns check
  "Run this repository's pre-push gates and return a pass/fail report. Changes nothing.

  The same gates as .github/workflows/ci.yml, plus the doc build, the tests for
  this recipe's helpers and the writing rules, from the repository root:

  - :fmt           cargo fmt --all -- --check
  - :clippy        cargo clippy --workspace --all-targets -- -D warnings
  - :build         cargo build --workspace, which catches features enabled
                   only through dev-dependencies
  - :test          cargo test --workspace, with REQUIRE_BB=1 so the bb tests
                   fail instead of skipping when bb is missing
  - :doc           cargo doc --workspace --no-deps with warnings as errors
                   (skipped with --skip-doc)
  - :release-test  bb release-test
  - :checks-test   the tests in .cj/test/project/checks_test.clj for the
                   helpers in .cj/src/project/checks.clj
  - :em-dashes     no em dashes in tracked text files
  - :commits       commits in origin/main..HEAD have conventional-commit
                   subjects, no attribution lines and no em dashes (skipped
                   when origin/main does not exist)

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

(defn -main [{:keys [skip-doc]}]
  (let [root (checks/repo-root (System/getProperty "user.dir"))
        cargo (fn [argv & [env]] #(checks/command root argv {:env (or env {}) :failures checks/cargo-failures}))
        ;; [key thunk] pairs in run order. An array-map past 8 entries becomes a
        ;; hash map on assoc, so the order is kept in a vector.
        steps (cond-> [[:fmt (cargo ["cargo" "fmt" "--all" "--" "--check"])]
                       [:clippy (cargo ["cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"])]
                       [:build (cargo ["cargo" "build" "--workspace"])]
                       [:test (cargo ["cargo" "test" "--workspace"] {"REQUIRE_BB" "1"})]]
                (not skip-doc) (conj [:doc (cargo ["cargo" "doc" "--workspace" "--no-deps"] {"RUSTDOCFLAGS" "-D warnings"})])
                true (into [[:release-test #(checks/command root ["bb" "release-test"] {:failures checks/clojure-test-failures})]
                            [:checks-test #(checks/clojure-tests root ".cj/test/project/checks_test.clj" 'project.checks-test)]
                            [:em-dashes #(checks/em-dashes root)]
                            [:commits #(checks/commit-messages root)]]))
        results (apply array-map (mapcat (fn [[k f]] [k (f)]) steps))
        failed (vec (for [[k r] results :when (not (:ok? r))] k))
        skipped (cond-> [] skip-doc (conj :doc)
                  (get-in results [:commits :skipped?]) (conj :commits))]
    (cond-> {:ok? (empty? failed) :root root :steps results :failed failed}
      (seq skipped) (assoc :skipped skipped))))
