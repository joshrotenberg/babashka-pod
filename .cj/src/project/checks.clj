(ns project.checks
  "Helpers for this repository's cj check recipe: running a command with the
  tail of its output, running the tests for these helpers, the em dash scan and
  the commit message rules. Adapted from clajent's own check
  (https://github.com/joshrotenberg/clajent). Tested by
  .cj/test/project/checks_test.clj."
  (:require [clajent.proc :as proc]
            [clojure.string :as str]
            [clojure.test :as t]))

(def cargo-failures
  "Lines that name what failed in cargo, clippy, rustdoc and rustfmt output."
  #"(?m)^(?:test .* \.\.\. FAILED|thread '.*' panicked at .*|error(?:\[\w+\])?: .*|warning: .*|Diff in .*)$")

(def clojure-test-failures
  "The FAIL and ERROR lines in clojure.test output."
  #"(?m)^(?:FAIL|ERROR) in \(.*$")

(defn- keep-tail
  "The last n characters of s, or \"\" when s is nil."
  [s n]
  (let [s (or s "")] (subs s (max 0 (- (count s) n)))))

(defn command
  "Run argv in cwd and return {:argv :exit :ok? :out :err :truncated?}. :out and
  :err keep the last tail characters of what was captured (up to 16 MiB per
  stream). :failures, present when there are any, holds every line of the whole
  capture that matches the failures pattern, so failing tests and lints are
  named even when the tail does not reach them. :env overlays the environment."
  ([cwd argv] (command cwd argv {}))
  ([cwd argv {:keys [env tail failures] :or {env {} tail 4000}}]
   (let [r (proc/run! {:argv argv :cwd cwd :env env
                       :stdout-limit-bytes 16777216 :stderr-limit-bytes 16777216})
         out (get-in r [:stdout :text])
         err (get-in r [:stderr :text])
         found (when failures (vec (distinct (re-seq failures (str out "\n" err)))))]
     (cond-> {:argv argv
              :exit (get-in r [:process :exit-code])
              :ok? (= :completed (:status r))
              :out (keep-tail out tail)
              :err (keep-tail (str err (some-> (:execution-error r) str)) tail)
              :truncated? (boolean (or (get-in r [:stdout :truncated?]) (get-in r [:stderr :truncated?])))}
       (seq found) (assoc :failures found)))))

(defn clojure-tests
  "Load the test file at path (relative to root) and run namespace test-ns with
  clojure.test in this process. Returns {:ok? :out :counts}, with :failures
  holding the FAIL and ERROR lines when there are any. :out keeps the last 4000
  characters of the test output, which does not go to stdout. A file that does
  not load is a failure with the exception message under :err."
  [root path test-ns]
  (let [w (java.io.StringWriter.)]
    (try
      (load-file (str root "/" path))
      (let [counts (binding [t/*test-out* w] (t/run-tests test-ns))
            out (str w)
            found (vec (re-seq clojure-test-failures out))]
        (cond-> {:ok? (zero? (+ (:fail counts) (:error counts)))
                 :out (keep-tail out 4000)
                 :counts (select-keys counts [:test :pass :fail :error])}
          (seq found) (assoc :failures found)))
      (catch Exception e
        {:ok? false :out (keep-tail (str w) 4000) :err (ex-message e)}))))

(defn git
  "Run git in root and return its trimmed stdout, or nil when it fails."
  [root & args]
  (let [r (proc/run! {:argv (into ["git"] args) :cwd root})]
    (when (= :completed (:status r)) (str/trim (get-in r [:stdout :text])))))

(defn repo-root
  "The top level of the Git work tree containing cwd."
  [cwd]
  (or (git cwd "rev-parse" "--show-toplevel")
      (throw (ex-info "Not inside a Git work tree" {:kind :invalid-input :cwd cwd}))))

(def em-dash "U+2014, spelled as an escape so this file does not match." "\u2014")

(def text-files
  "Pathspecs the em dash check reads."
  ["*.md" "*.rs" "*.toml" "*.clj" "*.edn" "*.yml" "*.yaml" "*.json"])

(defn em-dashes
  "Tracked text files containing an em dash, as {:ok? :hits [{:file :line :text}]}.
  Paths come from `git ls-files -z`, so names git would quote are read as they
  are; tracked files missing from the work tree are skipped."
  [root]
  (let [files (some-> (apply git root "ls-files" "-z" "--" text-files) (str/split #"\u0000"))
        hits (vec (for [f files
                        :let [file (java.io.File. (str root) (str f))]
                        :when (and (seq f) (.isFile file))
                        [n line] (map-indexed vector (str/split-lines (slurp file)))
                        :when (str/includes? line em-dash)]
                    {:file f :line (inc n) :text (str/trim line)}))]
    {:ok? (empty? hits) :hits hits}))

(def conventional-subject
  "A conventional-commit subject: type, optional scope, optional !, colon, text."
  #"^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([^)]+\))?!?: \S")

(def attribution
  "Lines that attribute a commit to a tool or a co-author: a Co-authored-by,
  Generated-by or Claude-Session trailer in any case, or a line that starts with
  \"Generated with\" after any emoji or punctuation, the form tool footers take.
  \"generated with\" inside a sentence, or starting a wrapped line in lower case,
  does not match."
  #"^\s*(?:(?i:co-authored-by|generated-by|claude-session):|[\W_]*Generated with\b)")

(defn commit-problems
  "The rule violations in commits, a seq of {:commit :subject :body}: a subject
  without a conventional-commit prefix (:subject), an attribution line in the
  body (:attribution), and a subject or body line with an em dash (:em-dash).
  Returns a vector of {:commit :subject :problem}, with :line for line problems."
  [commits]
  (vec (concat
        (for [{:keys [commit subject]} commits
              :when (not (re-find conventional-subject subject))]
          {:commit commit :subject subject :problem :subject})
        (for [{:keys [commit subject body]} commits
              line (str/split-lines body)
              :when (re-find attribution line)]
          {:commit commit :subject subject :problem :attribution :line line})
        (for [{:keys [commit subject body]} commits
              line (cons subject (str/split-lines body))
              :when (str/includes? line em-dash)]
          {:commit commit :subject subject :problem :em-dash :line line}))))

(defn commit-messages
  "Commits in base..HEAD (default origin/main) checked with commit-problems:
  conventional-commit subjects, no attribution lines and no em dashes. Merge
  commits are not checked. Returns {:ok? :base :checked :problems}, or a skipped
  result when base does not exist."
  ([root] (commit-messages root "origin/main"))
  ([root base]
   (if-not (git root "rev-parse" "--verify" "--quiet" base)
     {:ok? true :skipped? true :reason (str base " does not name a commit")}
     (let [shas (some-> (git root "rev-list" "--no-merges" (str base "..HEAD")) str/split-lines)
           cs (for [sha shas :when (seq sha)]
                {:commit (subs sha 0 7)
                 :subject (git root "log" "-1" "--format=%s" sha)
                 :body (or (git root "log" "-1" "--format=%b" sha) "")})
           problems (commit-problems cs)]
       {:ok? (empty? problems) :base base :checked (count cs) :problems problems}))))
