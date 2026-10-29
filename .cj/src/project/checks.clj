(ns project.checks
  "Helpers for this repository's cj check recipe: running a command with the
  tail of its output, the em dash scan and the commit message rules. Adapted
  from clajent's own check (https://github.com/joshrotenberg/clajent)."
  (:require [clajent.proc :as proc]
            [clojure.string :as str]))

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
         keep-tail #(let [s (or % "")] (subs s (max 0 (- (count s) tail))))
         found (when failures (vec (distinct (re-seq failures (str out "\n" err)))))]
     (cond-> {:argv argv
              :exit (get-in r [:process :exit-code])
              :ok? (= :completed (:status r))
              :out (keep-tail out)
              :err (keep-tail (str err (some-> (:execution-error r) str)))
              :truncated? (boolean (or (get-in r [:stdout :truncated?]) (get-in r [:stderr :truncated?])))}
       (seq found) (assoc :failures found)))))

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
  "Tracked text files containing an em dash, as {:ok? :hits [{:file :line :text}]}."
  [root]
  (let [files (some-> (apply git root "ls-files" "--" text-files) str/split-lines)
        hits (vec (for [f files
                        :when (seq f)
                        [n line] (map-indexed vector (str/split-lines (slurp (str root "/" f))))
                        :when (str/includes? line em-dash)]
                    {:file f :line (inc n) :text (str/trim line)}))]
    {:ok? (empty? hits) :hits hits}))

(def conventional-subject
  "A conventional-commit subject: type, optional scope, optional !, colon, text."
  #"^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([^)]+\))?!?: \S")

(def attribution
  "Lines that attribute a commit to a tool or a co-author."
  #"(?i)^\s*(co-authored-by|generated-by|claude-session):|generated with")

(defn commit-messages
  "Commits in base..HEAD (default origin/main): each subject has a
  conventional-commit prefix, and no line is an attribution trailer or says
  \"Generated with\". Merge commits are not checked. Returns
  {:ok? :base :checked :problems}, or a skipped result when base does not exist."
  ([root] (commit-messages root "origin/main"))
  ([root base]
   (if-not (git root "rev-parse" "--verify" "--quiet" base)
     {:ok? true :skipped? true :reason (str base " does not name a commit")}
     (let [shas (some-> (git root "rev-list" "--no-merges" (str base "..HEAD")) str/split-lines)
           cs (for [sha shas :when (seq sha)]
                {:commit (subs sha 0 7)
                 :subject (git root "log" "-1" "--format=%s" sha)
                 :body (or (git root "log" "-1" "--format=%b" sha) "")})
           problems (vec (concat
                          (for [{:keys [commit subject]} cs
                                :when (not (re-find conventional-subject subject))]
                            {:commit commit :subject subject :problem :subject})
                          (for [{:keys [commit subject body]} cs
                                line (str/split-lines body)
                                :when (re-find attribution line)]
                            {:commit commit :subject subject :problem :attribution :line line})))]
       {:ok? (empty? problems) :base base :checked (count cs) :problems problems}))))
