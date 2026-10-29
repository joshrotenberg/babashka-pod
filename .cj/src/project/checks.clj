(ns project.checks
  "Repository checks for babashka-pod that return data. Each takes the absolute
  repository root and only reads. Adapted from clajent's .cj/src/project/checks.clj."
  (:require [clajent.proc :as proc]
            [clojure.java.io :as io]
            [clojure.string :as str]))

(defn command
  "Run argv in cwd and return {:argv :exit :ok? :out :err :truncated?}. :out and
  :err keep the last tail characters of what was captured. Capture keeps the
  first 16 MiB of each stream; :truncated? is true when either stream exceeded it.
  :failures, present when there are any, holds the names of failing Rust tests
  (\"test NAME ... FAILED\" lines) from the whole capture. :env overlays the
  environment."
  ([cwd argv] (command cwd argv {}))
  ([cwd argv {:keys [env tail] :or {env {} tail 4000}}]
   (let [r (proc/run! {:argv argv :cwd cwd :env env
                       :stdout-limit-bytes 16777216 :stderr-limit-bytes 16777216})
         keep-tail #(let [s (or % "")] (subs s (max 0 (- (count s) tail))))
         failures (vec (map second (re-seq #"(?m)^test (\S+) \.\.\. FAILED$"
                                           (str (get-in r [:stdout :text]) "\n" (get-in r [:stderr :text])))))]
     (cond-> {:argv argv
              :exit (get-in r [:process :exit-code])
              :ok? (= :completed (:status r))
              :out (keep-tail (get-in r [:stdout :text]))
              :err (keep-tail (str (get-in r [:stderr :text]) (some-> (:execution-error r) str)))
              :truncated? (boolean (or (get-in r [:stdout :truncated?]) (get-in r [:stderr :truncated?])))}
       (seq failures) (assoc :failures failures)))))

(defn repo-root
  "The top level of the Git work tree containing cwd."
  [cwd]
  (let [r (command cwd ["git" "rev-parse" "--show-toplevel"])]
    (when-not (:ok? r)
      (throw (ex-info "Not inside a Git work tree" {:kind :not-a-repo :cwd cwd})))
    (str/trim (:out r))))

(defn- tracked [root pathspec]
  (let [r (proc/run! {:argv ["git" "ls-files" "-z" "--" pathspec] :cwd root})]
    (when-not (and (= :completed (:status r)) (zero? (get-in r [:process :exit-code] -1)))
      (throw (ex-info "git ls-files failed" {:kind :command-failed :root root :result (select-keys r [:status :process])})))
    (vec (remove str/blank? (str/split (or (get-in r [:stdout :text]) "") #"\u0000")))))

(def em-dash "U+2014, spelled as an escape so this file does not match." "\u2014")

(def text-files
  "Pathspecs the em dash check reads."
  ["*.md" "*.rs" "*.clj" "*.bb" "*.edn" "*.toml" "*.yml"])

(defn em-dashes
  "Lines with an em dash in tracked text files (text-files). A tracked file
  deleted from the work tree is skipped. Returns
  {:ok? :checked :matches [{:file :line}]}, :line 1-based."
  [root]
  (let [files (vec (filter #(.isFile (io/file root %)) (distinct (mapcat #(tracked root %) text-files))))
        matches (vec (for [file files
                           [i line] (map-indexed vector (str/split-lines (slurp (io/file root file))))
                           :when (str/includes? line em-dash)]
                       {:file file :line (inc i)}))]
    {:ok? (empty? matches) :checked (count files) :matches matches}))

(def conventional-subject
  "A conventional-commit subject: type, optional (scope), optional !, colon, text."
  #"^(?:(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([^)\s]+\))?!?: \S|Revert \")")

(def attribution
  "Commit message lines that attribute the work to someone other than its author
  or to a tool."
  #"(?i)^\s*(?:(?:co-authored-by|assisted-by|generated-by)\s*:|\W*generated with\b)")

(defn- commits
  "Non-merge commits in base..HEAD as [{:commit :subject :body}], oldest first,
  or nil when base does not name a commit."
  [root base]
  (when (:ok? (command root ["git" "rev-parse" "--verify" "--quiet" (str base "^{commit}")]))
    (let [r (command root ["git" "log" "--no-merges" "--reverse" "--format=%H%x00%B%x1e" (str base "..HEAD")]
                     {:tail 16777216})]
      (when-not (:ok? r)
        (throw (ex-info "git log failed" {:kind :command-failed :result r})))
      (vec (for [entry (str/split (:out r) #"\u001e")
                 :let [[sha body] (str/split (str/triml entry) #"\u0000" 2)]
                 :when (not (str/blank? sha))]
             {:commit sha :subject (first (str/split-lines (or body ""))) :body (or body "")})))))

(defn commit-messages
  "Commit messages in base..HEAD (default origin/main): each subject has a
  conventional-commit prefix, and no line is an attribution trailer
  (Co-Authored-By and the like) or says \"Generated with\". Returns
  {:ok? :base :checked :problems [{:commit :subject :problem :line}]}, with
  :problem :subject or :attribution, or a skipped result when base does not
  exist."
  ([root] (commit-messages root "origin/main"))
  ([root base]
   (if-let [cs (commits root base)]
     (let [problems (vec (concat
                          (for [{:keys [commit subject]} cs
                                :when (not (re-find conventional-subject subject))]
                            {:commit commit :subject subject :problem :subject})
                          (for [{:keys [commit subject body]} cs
                                line (str/split-lines body)
                                :when (re-find attribution line)]
                            {:commit commit :subject subject :problem :attribution :line line})))]
       {:ok? (empty? problems) :base base :checked (count cs) :problems problems})
     {:ok? true :skipped? true :reason (str base " does not name a commit")})))
