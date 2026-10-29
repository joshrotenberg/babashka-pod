#!/usr/bin/env bb
;; Prints a skeleton of the source files under a directory: for each file the
;; pod has a grammar for, its definitions with line ranges and signatures,
;; without bodies. Run from the repository root:
;;
;;   cargo build --release -p pod-treesitter
;;   bb pods/treesitter/examples/skeleton.bb [dir]
;;
;; TREESITTER_POD overrides the pod binary (default
;; target/release/pod-treesitter).

(require '[babashka.pods :as pods]
         '[babashka.fs :as fs]
         '[clojure.string :as str])

(pods/load-pod [(or (System/getenv "TREESITTER_POD") "target/release/pod-treesitter")])
(require '[pod.joshrotenberg.treesitter :as ts])

(def skip #{".git" "target" "node_modules" "dist" ".venv"})

(defn source-files [dir]
  (->> (fs/glob dir "**" {:hidden false})
       (remove (fn [p] (some skip (map str (fs/components (fs/relativize dir p))))))
       (filter fs/regular-file?)
       (filter #(ts/language-of (str %)))
       (map str)
       sort))

(defn lines [{:keys [start-line end-line]}]
  (if (= start-line end-line) (str start-line) (str start-line "-" end-line)))

(defn print-file [path]
  (let [defs (ts/outline-file path)]
    (when (seq defs)
      (println path)
      (doseq [{:keys [kind signature parent] :as d} defs]
        (println (format "  %s%-9s %-10s %s"
                         (if parent "  " "")
                         (lines d)
                         (name kind)
                         signature))))))

(let [dir (or (first *command-line-args*) ".")]
  (run! print-file (source-files dir)))
