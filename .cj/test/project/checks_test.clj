(ns project.checks-test
  "Tests for project.checks, run by the :checks-test step of `cj run check`.
  Em dashes are spelled as escapes so the em dash scan does not match this file."
  (:require [babashka.fs :as fs]
            [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [project.checks :as checks]))

(def ^:private em-dash checks/em-dash)

(deftest conventional-subjects
  (testing "match"
    (doseq [s ["feat: add a pod"
               "fix(handles): remove returns the value"
               "feat!: drop the old API"
               "chore(deps)!: bump serde"
               "revert: feat: add a pod"]]
      (is (re-find checks/conventional-subject s) s)))
  (testing "do not match"
    (doseq [s ["Add a pod"
               "feat:add a pod"
               "feat: "
               "feature: add a pod"
               "Feat: add a pod"
               "feat(): add a pod"
               "fix (handles): remove"
               "wip"]]
      (is (nil? (re-find checks/conventional-subject s)) s))))

(deftest attribution-lines
  (testing "match"
    (doseq [line ["Co-Authored-By: Someone <someone@example.com>"
                  "co-authored-by: someone"
                  "  Co-authored-by: someone"
                  "Generated-by: a tool"
                  "Claude-Session: 1234"
                  "Generated with Claude Code"
                  "\uD83E\uDD16 Generated with [Claude Code](https://claude.com/claude-code)"
                  "_Generated with a tool_"]]
      (is (re-find checks/attribution line) line)))
  (testing "do not match"
    (doseq [line ["the parser is generated with nom"
                  "The lexer tables are"
                  "generated with build.rs from the grammar."
                  "Co-authored by the team"
                  "Reviewed-by: someone"
                  "See the Co-Authored-By: trailer rule"]]
      (is (nil? (re-find checks/attribution line)) line))))

(deftest commit-problems
  (testing "clean commits"
    (is (= [] (checks/commit-problems
               [{:commit "a1" :subject "feat: add a pod"
                 :body "The parser is generated with nom.\n\nSee #12."}
                {:commit "a2" :subject "fix: nothing in the body" :body ""}]))))
  (testing "each problem"
    (let [dashed (str "fix: a " em-dash " b")]
      (is (= [{:commit "b1" :subject "Add a pod" :problem :subject}
              {:commit "c1" :subject "fix: y" :problem :attribution
               :line "Co-Authored-By: Someone <someone@example.com>"}
              {:commit "d1" :subject dashed :problem :em-dash :line dashed}
              {:commit "e1" :subject "docs: z" :problem :em-dash
               :line (str "one " em-dash " two")}]
             (checks/commit-problems
              [{:commit "b1" :subject "Add a pod" :body ""}
               {:commit "c1" :subject "fix: y"
                :body "Body.\n\nCo-Authored-By: Someone <someone@example.com>"}
               {:commit "d1" :subject dashed :body ""}
               {:commit "e1" :subject "docs: z" :body (str "Body.\n\none " em-dash " two")}]))))))

(deftest cargo-failure-lines
  (let [out (str/join "\n" ["   Compiling babashka-pod v0.1.0 (/repo)"
                            "test handles::remove ... ok"
                            "test handles::insert ... FAILED"
                            "thread 'handles::insert' panicked at src/handles.rs:10:5:"
                            "error[E0308]: mismatched types"
                            "error: could not compile `babashka-pod` (lib) due to 1 previous error"
                            "warning: unused variable: `x`"
                            "Diff in /repo/src/lib.rs at line 3:"
                            "test result: FAILED. 1 passed; 1 failed; 0 ignored"
                            "    error: an indented line is not a diagnostic"])]
    (is (= ["test handles::insert ... FAILED"
            "thread 'handles::insert' panicked at src/handles.rs:10:5:"
            "error[E0308]: mismatched types"
            "error: could not compile `babashka-pod` (lib) due to 1 previous error"
            "warning: unused variable: `x`"
            "Diff in /repo/src/lib.rs at line 3:"]
           (re-seq checks/cargo-failures out)))))

(deftest clojure-test-failure-lines
  (let [out (str/join "\n" ["Testing release-test"
                            ""
                            "FAIL in (reads-headers) (release_test.clj:30)"
                            "ERROR in (checks-headers) (release_test.clj:40)"
                            "Ran 3 tests containing 10 assertions."
                            "1 failures, 1 errors."])]
    (is (= ["FAIL in (reads-headers) (release_test.clj:30)"
            "ERROR in (checks-headers) (release_test.clj:40)"]
           (re-seq checks/clojure-test-failures out)))))

(deftest em-dash-scan
  (let [dir (str (fs/create-temp-dir))]
    (try
      (is (some? (checks/git dir "init" "-q")))
      (spit (str dir "/clean.md") "no dashes here\n")
      (spit (str dir "/dash.rs") (str "// one\n// two " em-dash " three\n"))
      (spit (str dir "/caf\u00e9.md") (str "a " em-dash " b\n"))
      (spit (str dir "/gone.md") (str em-dash "\n"))
      (spit (str dir "/notes.txt") (str em-dash "\n"))
      (is (some? (checks/git dir "add" ".")))
      (fs/delete (str dir "/gone.md"))
      (testing "reads quoted names, skips files deleted from the work tree and other extensions"
        (is (= {:ok? false
                :hits [{:file "caf\u00e9.md" :line 1 :text (str "a " em-dash " b")}
                       {:file "dash.rs" :line 2 :text (str "// two " em-dash " three")}]}
               (checks/em-dashes dir))))
      (finally (fs/delete-tree dir)))))
