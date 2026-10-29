(ns cj.project.check
  "Run the pre-push checks for babashka-pod and return a pass/fail report. Changes nothing.

  Steps, in order, from the repository root:

  - :fmt           cargo fmt --all -- --check
  - :clippy        cargo clippy --workspace --all-targets -- -D warnings
  - :doc           cargo doc -p babashka-pod --no-deps with RUSTDOCFLAGS=-D warnings
  - :test          cargo test --workspace with REQUIRE_BB=1 (skipped with --skip-test)
  - :release-test  bb release-test
  - :package       cargo publish -p babashka-pod --dry-run --allow-dirty
                   (skipped with --skip-package)
  - :em-dashes     no em dashes in tracked Markdown, Rust, Clojure, EDN, TOML
                   and YAML files
  - :commits       commits in origin/main..HEAD have conventional-commit
                   subjects and no attribution trailers or \"Generated with\"
                   lines (skipped when origin/main does not exist)

  Every step runs even when an earlier one fails. Returns

    {:ok? false
     :steps {:fmt {:ok? true ...} :test {:ok? false :out \"...tail...\"} ...}
     :failed [:test]}

  Command steps keep the tail of their output, and :failures lists the names
  of failing Rust tests. Skipped steps are listed in :skipped."
  {:cj/args [{:name :skip-test :type :boolean :doc "Skip cargo test"}
             {:name :skip-package :type :boolean :doc "Skip the cargo publish dry run"}]
   :cj/examples ["cj run check" "cj run check --skip-test --skip-package"]}
  (:require [project.checks :as checks]))

(defn -main [{:keys [skip-test skip-package]}]
  (let [root (checks/repo-root (System/getProperty "user.dir"))
        cmd (fn [argv & [env]] #(checks/command root argv {:env (or env {})}))
        steps (cond-> (array-map
                       :fmt (cmd ["cargo" "fmt" "--all" "--" "--check"])
                       :clippy (cmd ["cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"])
                       :doc (cmd ["cargo" "doc" "-p" "babashka-pod" "--no-deps"] {"RUSTDOCFLAGS" "-D warnings"}))
                (not skip-test) (assoc :test (cmd ["cargo" "test" "--workspace"] {"REQUIRE_BB" "1"}))
                true (assoc :release-test (cmd ["bb" "release-test"]))
                (not skip-package) (assoc :package (cmd ["cargo" "publish" "-p" "babashka-pod" "--dry-run" "--allow-dirty"]))
                true (assoc :em-dashes #(checks/em-dashes root)
                            :commits #(checks/commit-messages root)))
        results (into (array-map) (for [[k f] steps] [k (f)]))
        failed (vec (for [[k r] results :when (not (:ok? r))] k))
        skipped (cond-> []
                  skip-test (conj :test)
                  skip-package (conj :package)
                  (get-in results [:commits :skipped?]) (conj :commits))]
    (cond-> {:ok? (empty? failed) :root root :steps results :failed failed}
      (seq skipped) (assoc :skipped skipped))))
