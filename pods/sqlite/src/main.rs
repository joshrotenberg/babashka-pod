//! A babashka pod for SQLite, built on `babashka-pod` and rusqlite with
//! bundled SQLite (FTS5, JSON functions and R*Tree compiled in), using the
//! transit+json format.
//!
//! See `pods/sqlite/README.md` for the API.

use babashka_pod::serde_json::{self, Map, Number, Value, json};
use babashka_pod::transit::Bytes;
use babashka_pod::{Emitter, Format, Handle, Handles, Namespace, Pod, PodError};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, Row, Statement};

const NS: &str = "pod.joshrotenberg.sqlite";

/// Statements kept by rusqlite's cache per connection.
const STATEMENT_CACHE: usize = 64;

/// A statement handle. rusqlite statements borrow their connection, so the
/// handle records the connection and SQL and runs through the connection's
/// prepared-statement cache, which keeps the compiled statement.
#[derive(Clone)]
struct Prepared {
    conn: Handle,
    sql: String,
}

/// Arguments of execute!, query and query-stream.
struct Call {
    conn: Handle,
    sql: String,
    params: Value,
}

#[derive(Clone)]
struct Db {
    conns: Handles<Connection>,
    stmts: Handles<Prepared>,
}

fn sql_error(sql: &str) -> impl Fn(rusqlite::Error) -> PodError + '_ {
    move |e| PodError::new(e.to_string()).with_data(json!({ "sql": sql }))
}

fn usage(var: &str, expected: &str) -> PodError {
    PodError::new(format!("{NS}/{var} expects {expected}"))
}

/// A bb value as an SQLite value. Maps and vectors are stored as JSON text,
/// so SQLite's JSON functions can query them.
fn to_sql(v: &Value) -> Result<SqlValue, PodError> {
    if let Some(bytes) = Bytes::from_json(v) {
        return Ok(SqlValue::Blob(bytes.0));
    }
    Ok(match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => SqlValue::Integer(i),
            (None, _) if n.is_u64() => {
                return Err(PodError::new(format!(
                    "integer {n} is out of SQLite's range"
                )));
            }
            (None, Some(f)) => SqlValue::Real(f),
            (None, None) => return Err(PodError::new(format!("unsupported number {n}"))),
        },
        Value::String(s) => SqlValue::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => SqlValue::Text(v.to_string()),
    })
}

fn from_sql(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => serde_json::to_value(Bytes(b.to_vec())).unwrap_or(Value::Null),
    }
}

/// Binds a vector (positional) or a map (named: `:name`, or `name` given as
/// a keyword) of parameters.
fn bind(stmt: &mut Statement<'_>, params: &Value, sql: &str) -> Result<(), PodError> {
    let expected = stmt.parameter_count();
    match params {
        Value::Null if expected == 0 => {}
        Value::Array(items) => {
            if items.len() != expected {
                let noun = if expected == 1 {
                    "parameter"
                } else {
                    "parameters"
                };
                return Err(PodError::new(format!(
                    "statement takes {expected} {noun}, got {}",
                    items.len()
                ))
                .with_data(json!({ "sql": sql })));
            }
            for (i, v) in items.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, to_sql(v)?)
                    .map_err(sql_error(sql))?;
            }
        }
        Value::Object(map) if Bytes::from_json(params).is_none() => {
            for (k, v) in map {
                let name = if k.starts_with([':', '@', '$']) {
                    k.clone()
                } else {
                    format!(":{k}")
                };
                let index = stmt
                    .parameter_index(&name)
                    .map_err(sql_error(sql))?
                    .ok_or_else(|| {
                        PodError::new(format!("statement has no parameter {name}"))
                            .with_data(json!({ "sql": sql }))
                    })?;
                stmt.raw_bind_parameter(index, to_sql(v)?)
                    .map_err(sql_error(sql))?;
            }
        }
        _ => {
            return Err(PodError::new(format!(
                "statement takes {expected} parameters; pass them as a vector or a map"
            ))
            .with_data(json!({ "sql": sql })));
        }
    }
    Ok(())
}

fn row_value(row: &Row<'_>, names: &[String]) -> Result<Value, rusqlite::Error> {
    let mut map = Map::new();
    for (i, name) in names.iter().enumerate() {
        map.insert(name.clone(), from_sql(row.get_ref(i)?));
    }
    Ok(Value::Object(map))
}

fn column_names(stmt: &Statement<'_>) -> Vec<String> {
    stmt.column_names().into_iter().map(String::from).collect()
}

impl Db {
    fn new() -> Self {
        Db {
            conns: Handles::new("connection"),
            stmts: Handles::new("statement"),
        }
    }

    /// Parses `[connection sql params?]` or `[statement params?]`.
    fn call(&self, var: &str, args: Vec<Value>) -> Result<Call, PodError> {
        let expected = "[connection sql params?] or [statement params?]";
        let mut args = args.into_iter();
        let first: Handle = args
            .next()
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| usage(var, expected))?;
        let (conn, sql) = if first.handle == self.stmts.kind() {
            let p = self.stmts.with(&first, |p| Ok(p.clone()))?;
            (p.conn, p.sql)
        } else {
            match args.next() {
                Some(Value::String(sql)) => (first, sql),
                _ => return Err(usage(var, expected)),
            }
        };
        let params = args.next().unwrap_or(Value::Null);
        if args.next().is_some() {
            return Err(usage(var, expected));
        }
        Ok(Call { conn, sql, params })
    }

    fn open(&self, path: &str) -> Result<Handle, PodError> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open(path)
        }
        .map_err(|e| PodError::new(e.to_string()).with_data(json!({ "path": path })))?;
        conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
        rusqlite::vtab::csvtab::load_module(&conn).map_err(PodError::from)?;
        Ok(self.conns.insert(conn))
    }

    fn execute(&self, call: Call) -> Result<Value, PodError> {
        let Call { conn, sql, params } = call;
        self.conns.with(&conn, |c| {
            let mut stmt = c.prepare_cached(&sql).map_err(sql_error(&sql))?;
            bind(&mut stmt, &params, &sql)?;
            let changes = stmt.raw_execute().map_err(sql_error(&sql))?;
            Ok(json!({ "changes": changes, "last-insert-id": c.last_insert_rowid() }))
        })
    }

    fn query(&self, call: Call) -> Result<Vec<Value>, PodError> {
        let Call { conn, sql, params } = call;
        self.conns.with(&conn, |c| {
            let mut stmt = c.prepare_cached(&sql).map_err(sql_error(&sql))?;
            bind(&mut stmt, &params, &sql)?;
            let names = column_names(&stmt);
            let mut rows = stmt.raw_query();
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(sql_error(&sql))? {
                out.push(row_value(row, &names).map_err(sql_error(&sql))?);
            }
            Ok(out)
        })
    }

    fn query_stream(&self, call: Call, tx: &Emitter) -> Result<(), PodError> {
        let Call { conn, sql, params } = call;
        self.conns.with(&conn, |c| {
            let mut stmt = c.prepare_cached(&sql).map_err(sql_error(&sql))?;
            bind(&mut stmt, &params, &sql)?;
            let names = column_names(&stmt);
            let mut rows = stmt.raw_query();
            while let Some(row) = rows.next().map_err(sql_error(&sql))? {
                tx.send(&row_value(row, &names).map_err(sql_error(&sql))?)?;
                if tx.is_cancelled() {
                    break;
                }
            }
            Ok(())
        })
    }

    /// One statement over many parameter rows, inside a savepoint, so it is
    /// atomic and works inside or outside a transaction from begin!.
    fn execute_many(&self, conn: &Handle, sql: &str, rows: &[Value]) -> Result<Value, PodError> {
        self.conns.with(conn, |c| {
            let sp = c.savepoint().map_err(sql_error(sql))?;
            let mut changes = 0;
            {
                let mut stmt = sp.prepare_cached(sql).map_err(sql_error(sql))?;
                for params in rows {
                    bind(&mut stmt, params, sql)?;
                    changes += stmt.raw_execute().map_err(sql_error(sql))?;
                }
            }
            sp.commit().map_err(sql_error(sql))?;
            Ok(json!({ "changes": changes }))
        })
    }

    fn batch(&self, conn: &Handle, sql: &str) -> Result<(), PodError> {
        self.conns
            .with(conn, |c| c.execute_batch(sql).map_err(sql_error(sql)))
    }

    fn prepare(&self, conn: Handle, sql: String) -> Result<Handle, PodError> {
        // Compile once now, so a syntax error surfaces here; the compiled
        // statement stays in the connection's cache.
        self.conns.with(&conn, |c| {
            c.prepare_cached(&sql).map(drop).map_err(sql_error(&sql))
        })?;
        Ok(self.stmts.insert(Prepared { conn, sql }))
    }

    fn attach_csv(&self, conn: &Handle, table: &str, path: &str) -> Result<(), PodError> {
        let valid = table.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(PodError::new(format!("invalid table name {table:?}")));
        }
        let sql = format!(
            "CREATE VIRTUAL TABLE temp.\"{table}\" USING csv(filename='{}', header=yes)",
            path.replace('\'', "''")
        );
        self.batch(conn, &sql)
    }
}

const WITH_TRANSACTION: &str = r#"(defmacro with-transaction
  "Runs body in a transaction on conn. Commits when body returns and rolls
  back, then rethrows, when it throws."
  [conn & body]
  `(let [c# ~conn]
     (begin! c#)
     (try
       (let [result# (do ~@body)]
         (commit! c#)
         result#)
       (catch Throwable e#
         (rollback! c#)
         (throw e#)))))"#;

fn namespace(db: &Db) -> Namespace {
    let d = |db: &Db| db.clone();
    let (
        open,
        close,
        exec,
        many,
        batch,
        query,
        stream,
        prep,
        fin,
        tx_begin,
        tx_commit,
        tx_rollback,
        csv,
    ) = (
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
        d(db),
    );
    Namespace::new(NS)
        .function("open", move |(path,): (String,)| open.open(&path))
        .doc("open", "Opens a database file, or an in-memory database for \":memory:\". Returns a connection handle.")
        .arglists("open", ["[path]"])
        .function("close!", move |(conn,): (Handle,)| close.conns.remove(&conn))
        .doc("close!", "Closes a connection.")
        .arglists("close!", ["[conn]"])
        .function("execute!", move |args: Vec<Value>| exec.execute(exec.call("execute!", args)?))
        .doc("execute!", "Runs a statement. Returns {:changes n :last-insert-id id}. params is a vector (positional) or a map (named).")
        .arglists("execute!", ["[conn sql]", "[conn sql params]", "[stmt]", "[stmt params]"])
        .function("execute-many!", move |(conn, sql, rows): (Handle, String, Vec<Value>)| {
            many.execute_many(&conn, &sql, &rows)
        })
        .doc("execute-many!", "Runs one statement for each params entry in rows, atomically. Returns {:changes n}.")
        .arglists("execute-many!", ["[conn sql rows]"])
        .function("execute-batch!", move |(conn, sql): (Handle, String)| batch.batch(&conn, &sql))
        .doc("execute-batch!", "Runs several statements separated by semicolons, without parameters.")
        .arglists("execute-batch!", ["[conn sql]"])
        .function("query", move |args: Vec<Value>| query.query(query.call("query", args)?))
        .doc("query", "Runs a query and returns its rows as maps with keyword keys.")
        .arglists("query", ["[conn sql]", "[conn sql params]", "[stmt]", "[stmt params]"])
        .stream_fn("query-stream", move |args: Vec<Value>, tx: Emitter| {
            stream.query_stream(stream.call("query-stream", args)?, &tx)
        })
        .doc("query-stream", "Runs a query and calls f with each row as it is read, for results too large to hold at once.")
        .arglists("query-stream", ["[conn sql f]", "[conn sql params f]", "[stmt f]", "[stmt params f]"])
        .function("prepare", move |(conn, sql): (Handle, String)| prep.prepare(conn, sql))
        .doc("prepare", "Compiles a statement and returns a statement handle for execute!, query and query-stream.")
        .arglists("prepare", ["[conn sql]"])
        .function("finalize!", move |(stmt,): (Handle,)| fin.stmts.remove(&stmt))
        .doc("finalize!", "Releases a statement handle.")
        .arglists("finalize!", ["[stmt]"])
        .function("begin!", move |(conn,): (Handle,)| tx_begin.batch(&conn, "BEGIN"))
        .doc("begin!", "Starts a transaction. Prefer with-transaction.")
        .arglists("begin!", ["[conn]"])
        .function("commit!", move |(conn,): (Handle,)| tx_commit.batch(&conn, "COMMIT"))
        .doc("commit!", "Commits the current transaction.")
        .arglists("commit!", ["[conn]"])
        .function("rollback!", move |(conn,): (Handle,)| tx_rollback.batch(&conn, "ROLLBACK"))
        .doc("rollback!", "Rolls back the current transaction.")
        .arglists("rollback!", ["[conn]"])
        .code("with-transaction", WITH_TRANSACTION)
        .function("attach-csv!", move |(conn, table, path): (Handle, String, String)| {
            csv.attach_csv(&conn, &table, &path)
        })
        .doc("attach-csv!", "Exposes a CSV file with a header row as the temporary table `table`. Columns are text.")
        .arglists("attach-csv!", ["[conn table path]"])
}

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("pod-sqlite is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }
    let db = Db::new();
    if let Err(e) = Pod::new()
        .format(Format::TransitJson)
        .namespace(namespace(&db))
        .run()
    {
        eprintln!("pod-sqlite: {e}");
        std::process::exit(1);
    }
}
