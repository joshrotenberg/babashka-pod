# pod-sqlite

A [babashka](https://babashka.org) pod for SQLite, built on [babashka-pod](../../README.md) and
[rusqlite](https://github.com/rusqlite/rusqlite) with SQLite compiled in (3.53.2 at the time of
writing, with FTS5, JSON functions and R*Tree). SQLite is compiled from source with the platform's
C compiler, so the pod builds natively on each platform, including macOS arm64.

```bash
cargo build --release -p pod-sqlite
```

```clojure
(require '[babashka.pods :as pods])
(pods/load-pod ["target/release/pod-sqlite"])
(require '[pod.joshrotenberg.sqlite :as sql])

(def db (sql/open "app.db"))            ; or ":memory:"
(sql/execute-batch! db "create table users (id integer primary key, name text, meta text)")
(sql/execute! db "insert into users (name) values (?)" ["ada"])
(sql/query db "select * from users")    ;; => [{:id 1, :name "ada", :meta nil}]
(sql/close! db)
```

## API

All functions are in `pod.joshrotenberg.sqlite`. Connections and statements are handles, maps like
`{:handle "connection", :id 1}`.

| Function | Does |
|---|---|
| `(open path)` | Opens a database file, or an in-memory database for `":memory:"` |
| `(close! conn)` | Closes a connection |
| `(execute! conn sql)`, `(execute! conn sql params)` | Runs a statement; returns `{:changes n, :last-insert-id id}` |
| `(execute-many! conn sql rows)` | Runs one statement per params entry in `rows`, atomically (a savepoint, so it also works inside a transaction); returns `{:changes n}` |
| `(execute-batch! conn sql)` | Runs several statements separated by semicolons, without parameters |
| `(query conn sql)`, `(query conn sql params)` | Returns rows as maps with keyword keys |
| `(query-stream conn sql f)`, `(query-stream conn sql params f)` | Calls `f` with each row as it is read; for results too large to hold at once |
| `(prepare conn sql)` | Returns a statement handle; pass it to `execute!`, `query` or `query-stream` in place of `conn sql` |
| `(finalize! stmt)` | Releases a statement handle |
| `(with-transaction conn & body)` | Macro: commits when body returns, rolls back and rethrows when it throws |
| `(begin! conn)`, `(commit! conn)`, `(rollback! conn)` | Manual transaction control |
| `(attach-csv! conn table path)` | Exposes a CSV file with a header row as the temporary table `table` (columns are text) |

`params` is a vector for positional `?` parameters, or a map for named ones: `{:name "ada"}`
binds `:name`.

Each function has a docstring: `(clojure.repl/doc sql/query)`.

## Values

| SQLite | babashka |
|---|---|
| INTEGER | long |
| REAL | double |
| TEXT | string |
| BLOB | byte array |
| NULL | nil |

Parameters map the other way. Booleans bind as 0 and 1. Maps and vectors bind as JSON text, so
SQLite's JSON functions can query them:

```clojure
(sql/execute! db "update users set meta = ? where id = 1" [{:roles ["admin"] :level 3}])
(sql/query db "select json_extract(meta, '$.level') as level from users")  ;; => [{:level 3}]
```

Errors from SQLite are raised as `ex-info` with the SQL in `ex-data`:

```clojure
(try (sql/query db "select * from nope")
     (catch clojure.lang.ExceptionInfo e [(ex-message e) (ex-data e)]))
;; => ["no such table: nope" {:sql "select * from nope"}]
```

## As a library

The crate is also a library, `pod_sqlite`, so another pod binary can serve this namespace next to
others. `pod_sqlite::namespace()` builds it with its own state; serve it with `Format::TransitJson`:

```rust
babashka_pod::Pod::new()
    .format(babashka_pod::Format::TransitJson)
    .namespace(pod_sqlite::namespace())
    .run()
```

## Releases

Released from this repository with the tag `pod-sqlite-v<version>`; see
[Pods in this repository](../../README.md#pods-in-this-repository).

## Compared with go-sqlite3

[go-sqlite3](https://github.com/babashka/pod-babashka-go-sqlite3) is the SQLite pod in the pod
registry. Its registry manifests list only x86_64 for macOS; the comparison below uses the
`macos-aarch64` asset from its GitHub releases (0.3.13), so both pods run natively.

| | pod-sqlite | go-sqlite3 0.3.13 |
|---|---|---|
| SQLite version | 3.53.2 | 3.45.1 |
| macOS arm64 from the pod registry | not in the registry | x86_64 only |
| Connection | handle, held across calls | database path on every call |
| Transactions across calls | `with-transaction`, `begin!`/`commit!` | no |
| Prepared statements | `prepare` | no |
| Streaming rows | `query-stream` | no |
| Bulk insert | `execute-many!` | one multi-row `insert` |
| BLOB results | byte array | vector of numbers |
| Map or vector parameters | bound as JSON text | error: unsupported type |
| FTS5, R*Tree, JSON functions | yes | yes |
| CSV as a table | `attach-csv!` | no |

Timings from `pods/sqlite/bench.bb` (best of 3, fresh file database each run, 5000 rows of
`(name text, score real)`), on an Apple M4 Pro with babashka 1.13.226 and a release build:

| Scenario | pod-sqlite | go-sqlite3 |
|---|---|---|
| 5000 inserts, one call each | 1231 ms | 1376 ms |
| 5000 inserts, one call each, in one transaction | 192 ms | not possible: no connection held across calls |
| 5000 inserts, one call | 4 ms | 7 ms |
| query 5000 rows | 7 ms | 12 ms |
| 1000 point queries | 40 ms | 98 ms |

The first row is dominated by SQLite committing each autocommit insert to disk. Holding a
transaction open across calls, which needs a persistent connection, removes most of that.

```bash
cargo build --release -p pod-sqlite
GO_SQLITE3_POD=/path/to/pod-babashka-go-sqlite3 bb pods/sqlite/bench.bb
```

## Tests

`tests/bb.rs` runs `test.bb` under babashka over both transports: schema, named and positional
parameters, transactions with commit and rollback, bulk insert, constraint and SQL errors, JSON
functions, blobs, FTS5, R*Tree, prepared statements, streaming, CSV, and reopening a file database.

## Not supported

- SQL functions written in Clojure: a pod cannot call back into babashka during a query.
