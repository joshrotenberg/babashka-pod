#!/usr/bin/env bb
;; Exercises the SQLite pod. Run from the repository root:
;;
;;   cargo build -p pod-sqlite
;;   bb pods/sqlite/test.bb [socket]
;;
;; Prints one "name: value" line per check; tests/bb.rs asserts on them.

(require '[babashka.pods :as pods]
         '[babashka.fs :as fs])

(def transport (if (= "socket" (first *command-line-args*)) :socket :stdio))
(def pod-path (or (System/getenv "SQLITE_POD") "target/debug/pod-sqlite"))
(pods/load-pod [pod-path] (cond-> {} (= :socket transport) (assoc :transport :socket)))
(require '[pod.joshrotenberg.sqlite :as sql])

(defn check [label v] (println (str label ":") (pr-str v)))

(println "transport:" transport)
(def db (sql/open ":memory:"))
(check "handle" db)

(sql/execute-batch! db "create table users (id integer primary key, name text not null, email text unique, meta text, avatar blob);
                        create table logs (id integer primary key, msg text)")
(check "insert" (sql/execute! db "insert into users (name, email) values (?, ?)" ["ada" "ada@example.com"]))
(check "named params" (sql/execute! db "insert into users (name, email) values (:name, :email)" {:name "bob" :email "bob@example.com"}))
(check "query" (sql/query db "select id, name from users order by id"))
(check "query params" (sql/query db "select name from users where id = ?" [2]))

;; Transactions: commit, then a rollback through with-transaction.
(sql/with-transaction db
  (sql/execute! db "insert into logs (msg) values (?)" ["committed"]))
(try
  (sql/with-transaction db
    (sql/execute! db "insert into logs (msg) values (?)" ["rolled back"])
    (throw (ex-info "abort" {})))
  (catch clojure.lang.ExceptionInfo _))
(check "transaction" (map :msg (sql/query db "select msg from logs")))

;; Bulk insert in one call.
(check "execute-many" (sql/execute-many! db "insert into logs (msg) values (?)" (mapv (fn [i] [(str "bulk " i)]) (range 1000))))
(check "count" (sql/query db "select count(*) as n from logs"))

;; Errors from SQLite become ex-info with the SQL in ex-data.
(try (sql/execute! db "insert into users (name, email) values (?, ?)" ["dup" "ada@example.com"])
     (catch clojure.lang.ExceptionInfo e (check "constraint error" [(ex-message e) (:sql (ex-data e))])))
(try (sql/query db "select * from nope")
     (catch clojure.lang.ExceptionInfo e (check "missing table" (ex-message e))))
(try (sql/query db "select ?" [])
     (catch clojure.lang.ExceptionInfo e (check "param count" (ex-message e))))

;; Maps and vectors bind as JSON text, for SQLite's JSON functions.
(sql/execute! db "update users set meta = ? where name = ?" [{:roles ["admin" "dev"] :level 3} "ada"])
(check "json" (sql/query db "select json_extract(meta, '$.level') as level, json_array_length(meta, '$.roles') as roles from users where name = 'ada'"))

;; Blobs travel as byte arrays.
(sql/execute! db "update users set avatar = ? where name = ?" [(byte-array [0 1 2 255]) "bob"])
(let [avatar (:avatar (first (sql/query db "select avatar from users where name = 'bob'")))]
  (check "blob" [(bytes? avatar) (vec avatar)]))

;; FTS5.
(sql/execute-batch! db "create virtual table docs using fts5(title, body)")
(sql/execute-many! db "insert into docs (title, body) values (?, ?)"
                   [["pods" "babashka pods are programs that expose namespaces"]
                    ["sqlite" "an embedded database with full text search"]
                    ["rust" "a systems language for fast native pods"]])
(check "fts5" (map :title (sql/query db "select title from docs where docs match ? order by rank" ["pods"])))

;; R*Tree.
(sql/execute-batch! db "create virtual table boxes using rtree(id, minx, maxx, miny, maxy)")
(sql/execute-many! db "insert into boxes values (?, ?, ?, ?, ?)" [[1 0 10 0 10] [2 20 30 20 30]])
(check "rtree" (sql/query db "select id from boxes where minx <= 5 and maxx >= 5 and miny <= 5 and maxy >= 5"))

;; Prepared statements.
(let [stmt (sql/prepare db "select name from users where id = ?")]
  (check "prepared" [(sql/query stmt [1]) (sql/query stmt [2])])
  (sql/finalize! stmt)
  (try (sql/query stmt [1])
       (catch clojure.lang.ExceptionInfo e (check "finalized" (ex-message e)))))

;; Streaming rows.
(let [seen (atom 0)]
  (sql/query-stream db "select id from logs" (fn [_] (swap! seen inc)))
  (check "stream" @seen))

;; CSV as a table.
(let [csv (str (fs/create-temp-file {:suffix ".csv"}))]
  (spit csv "task,seconds\nlint,0.2\ntest,0.6\njvm,4.1\n")
  (sql/attach-csv! db "timings" csv)
  (check "csv" (sql/query db "select task, cast(seconds as real) as s from timings where s > 0.5 order by s"))
  (fs/delete csv))

;; A file database, reopened.
(let [path (str (fs/create-temp-file {:suffix ".db"}))
      a (sql/open path)]
  (sql/execute-batch! a "create table kv (k text primary key, v text); insert into kv values ('x', 'persisted')")
  (sql/close! a)
  (let [b (sql/open path)]
    (check "reopened" (sql/query b "select v from kv"))
    (sql/close! b))
  (try (sql/query a "select 1")
       (catch clojure.lang.ExceptionInfo e (check "closed" (ex-message e))))
  (fs/delete path))

(check "doc" (:doc (meta #'sql/query)))
(sql/close! db)
(println "done")
