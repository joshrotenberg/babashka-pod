#!/usr/bin/env bb
;; Exercises the tree-sitter pod. Run from the repository root:
;;
;;   cargo build -p pod-treesitter
;;   bb pods/treesitter/test.bb [socket]
;;
;; Prints one "name: value" line per check; tests/bb.rs asserts on them.

(require '[babashka.pods :as pods]
         '[clojure.string :as str])

(def transport (if (= "socket" (first *command-line-args*)) :socket :stdio))
(def pod-path (or (System/getenv "TREESITTER_POD") "target/debug/pod-treesitter"))
(pods/load-pod [pod-path] (cond-> {} (= :socket transport) (assoc :transport :socket)))
(require '[pod.joshrotenberg.treesitter :as ts])

(defn check [label v] (println (str label ":") (pr-str v)))
(def fixtures "pods/treesitter/tests/fixtures")

(println "transport:" transport)
(check "languages" (ts/languages))
(check "language-of" (mapv ts/language-of ["src/core.clj" "lib.rs" "README.md"]))

(check "sexp" (ts/parse :rust "fn f() {}" {:as :sexp}))
(let [tree (ts/parse :python "x = 1\n")]
  (check "tree" [(:type tree) (-> tree :children first :type) (-> tree :children first :end)]))

(check "query" (mapv (juxt :capture :text :start)
                     (ts/query :go "package p\nfunc A() {}\nfunc B() {}\n"
                               "(function_declaration name: (identifier) @fn)")))

(check "outline" (mapv (juxt :name :kind :start-line :end-line :signature)
                       (ts/outline :clojure (slurp (str fixtures "/sample.clj")))))
(check "outline-file" (mapv (juxt :name :kind :parent)
                            (filter :parent (ts/outline-file (str fixtures "/sample.rs")))))
(check "outline-file lang" (count (ts/outline-file (str fixtures "/sample.ts") :typescript)))

(check "unknown language" (try (ts/outline :cobol "x")
                               (catch clojure.lang.ExceptionInfo e
                                 [(first (str/split (ex-message e) #";")) (:language (ex-data e))])))
(check "bad query" (try (ts/query :rust "fn f() {}" "(nope)")
                        (catch clojure.lang.ExceptionInfo e (ex-message e))))
(check "no language" (try (ts/outline-file "README.md")
                          (catch clojure.lang.ExceptionInfo e (first (str/split (ex-message e) #";")))))
(check "doc" (:doc (meta #'ts/outline)))
(println "done")
