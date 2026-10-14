#!/usr/bin/env bb
;; Times pod-sqlite against go-sqlite3 on the same machine. Run from the
;; repository root with both pod binaries:
;;
;;   cargo build --release -p pod-sqlite
;;   GO_SQLITE3_POD=/path/to/pod-babashka-go-sqlite3 bb pods/sqlite/bench.bb
;;
;; Each scenario runs 3 times on a fresh file database; the best time is shown.

(require '[babashka.pods :as pods]
         '[babashka.fs :as fs])

(pods/load-pod [(or (System/getenv "SQLITE_POD") "target/release/pod-sqlite")])
(pods/load-pod [(System/getenv "GO_SQLITE3_POD")])
(require '[pod.joshrotenberg.sqlite :as rs]
         '[pod.babashka.go-sqlite3 :as go])

(def n 5000)
(def schema "create table t (id integer primary key, name text, score real)")
(defn rows [] (mapv (fn [i] [(str "name-" i) (* i 1.5)]) (range n)))

(defn best-ms [f]
  (apply min (for [_ (range 3)]
               (let [path (str (fs/create-temp-file {:suffix ".db"}))
                     _ (fs/delete path)
                     ctx (f :setup path)
                     t0 (System/nanoTime)]
                 (f :run ctx)
                 (let [ms (/ (- (System/nanoTime) t0) 1e6)]
                   (f :teardown ctx)
                   (fs/delete-if-exists path)
                   ms)))))

(defn rs-db [path]
  (let [db (rs/open path)] (rs/execute-batch! db schema) db))

(defn go-db [path]
  (go/execute! path [schema]) path)

(defn multi-insert [rows]
  (into [(str "insert into t (name, score) values "
              (clojure.string/join "," (repeat (count rows) "(?, ?)")))]
        (apply concat rows)))

(defn rs-filled [path]
  (let [db (rs-db path)] (rs/execute-many! db "insert into t (name, score) values (?, ?)" (rows)) db))

(defn go-filled [path]
  (go-db path) (go/execute! path (multi-insert (rows))) path)

(def scenarios
  [[(str n " inserts, one call each")
    (fn [op x] (case op :setup (rs-db x)
                 :run (doseq [r (rows)] (rs/execute! x "insert into t (name, score) values (?, ?)" r))
                 :teardown (rs/close! x)))
    (fn [op x] (case op :setup (go-db x)
                 :run (doseq [r (rows)] (go/execute! x (into ["insert into t (name, score) values (?, ?)"] r)))
                 :teardown nil))]
   [(str n " inserts, one call each, in one transaction")
    (fn [op x] (case op :setup (rs-db x)
                 :run (rs/with-transaction x
                        (doseq [r (rows)] (rs/execute! x "insert into t (name, score) values (?, ?)" r)))
                 :teardown (rs/close! x)))
    nil]
   [(str n " inserts, one call")
    (fn [op x] (case op :setup (rs-db x)
                 :run (rs/execute-many! x "insert into t (name, score) values (?, ?)" (rows))
                 :teardown (rs/close! x)))
    (fn [op x] (case op :setup (go-db x)
                 :run (go/execute! x (multi-insert (rows)))
                 :teardown nil))]
   [(str "query " n " rows")
    (fn [op x] (case op :setup (rs-filled x)
                 :run (count (rs/query x "select * from t"))
                 :teardown (rs/close! x)))
    (fn [op x] (case op :setup (go-filled x)
                 :run (count (go/query x ["select * from t"]))
                 :teardown nil))]
   ["1000 point queries"
    (fn [op x] (case op :setup (rs-filled x)
                 :run (dotimes [i 1000] (rs/query x "select name from t where id = ?" [(inc i)]))
                 :teardown (rs/close! x)))
    (fn [op x] (case op :setup (go-filled x)
                 :run (dotimes [i 1000] (go/query x ["select name from t where id = ?" (inc i)]))
                 :teardown nil))]])

(println "| Scenario | pod-sqlite | go-sqlite3 |")
(println "|---|---|---|")
(doseq [[label rs-f go-f] scenarios]
  (println (format "| %s | %.0f ms | %s |" label (best-ms rs-f)
                   (if go-f (format "%.0f ms" (best-ms go-f)) "n/a: no connection held across calls"))))
