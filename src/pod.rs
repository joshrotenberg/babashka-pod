use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde::de::{self, Deserialize, DeserializeOwned, Deserializer, SeqAccess, Visitor};

use crate::bencode::{self, Value};
use crate::error::{Error, PodError};

/// How long in-flight calls get to finish after shutdown, by default.
const DEFAULT_GRACE: Duration = Duration::from_secs(2);

type Handler = Arc<dyn Fn(&str, Emitter) -> Result<Option<String>, PodError> + Send + Sync>;

/// Returns true when the process was started by babashka as a pod.
///
/// babashka sets `BABASHKA_POD=true` in the pod's environment. A binary that is
/// also a regular CLI can use this to decide which mode to run in.
pub fn is_pod_mode() -> bool {
    std::env::var("BABASHKA_POD").is_ok_and(|v| v == "true")
}

/// Arguments for a function that takes none.
///
/// Deserializes only from an empty argument vector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoArgs;

impl<'de> Deserialize<'de> for NoArgs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = NoArgs;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an empty argument vector")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<NoArgs, A::Error> {
                match seq.next_element::<de::IgnoredAny>()? {
                    None => Ok(NoArgs),
                    Some(_) => Err(de::Error::invalid_length(1, &self)),
                }
            }
        }
        d.deserialize_seq(V)
    }
}

enum VarKind {
    Handler(Handler),
    Code(String),
}

struct Var {
    name: String,
    meta: Option<String>,
    kind: VarKind,
}

/// A namespace exposed by the pod, and the vars in it.
pub struct Namespace {
    name: String,
    deferred: bool,
    vars: Vec<Var>,
}

impl Namespace {
    /// Creates an empty namespace, e.g. `pod.example.math`.
    pub fn new(name: impl Into<String>) -> Self {
        Namespace {
            name: name.into(),
            deferred: false,
            vars: Vec::new(),
        }
    }

    /// Marks the namespace as deferred.
    ///
    /// Deferred namespaces are only announced in `describe`. Their vars are
    /// sent when the client first requires the namespace (the `load-ns` op).
    pub fn deferred(mut self) -> Self {
        self.deferred = true;
        self
    }

    /// Registers a function that returns a single value.
    ///
    /// The client's argument vector is deserialized into `A`, so a tuple such
    /// as `(i64, i64)` matches two positional arguments. Use `(T,)` for one
    /// argument, [`NoArgs`] for none, and `Vec<serde_json::Value>` for any
    /// number. The return value is serialized as JSON.
    pub fn function<A, R, F>(self, name: impl Into<String>, f: F) -> Self
    where
        A: DeserializeOwned,
        R: Serialize,
        F: Fn(A) -> Result<R, PodError> + Send + Sync + 'static,
    {
        let name = name.into();
        let qualified = format!("{}/{}", self.name, name);
        let handler: Handler = Arc::new(move |args, _tx| {
            let args = decode_args(&qualified, args)?;
            let value = f(args)?;
            serde_json::to_string(&value)
                .map(Some)
                .map_err(PodError::from)
        });
        self.push(name, VarKind::Handler(handler))
    }

    /// Registers a function that sends any number of values.
    ///
    /// The handler gets an [`Emitter`] for sending values and output. The call
    /// is complete when the handler returns: `Ok` sends `done`, `Err` sends an
    /// error. Pair it with client-side [`code`](Self::code) that calls
    /// `babashka.pods/invoke` with `:handlers` to receive each value.
    pub fn stream<A, F>(self, name: impl Into<String>, f: F) -> Self
    where
        A: DeserializeOwned,
        F: Fn(A, Emitter) -> Result<(), PodError> + Send + Sync + 'static,
    {
        let name = name.into();
        let qualified = format!("{}/{}", self.name, name);
        let handler: Handler = Arc::new(move |args, tx| {
            let args = decode_args(&qualified, args)?;
            f(args, tx)?;
            Ok(None)
        });
        self.push(name, VarKind::Handler(handler))
    }

    /// Registers a var whose definition is Clojure code evaluated by the client.
    ///
    /// The code runs in this namespace, so it can call the pod's other vars.
    pub fn code(self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.push(name.into(), VarKind::Code(source.into()))
    }

    /// Attaches metadata to a registered var, as an EDN map string such as
    /// `{:doc "Adds two numbers" :arglists ([a b])}`.
    ///
    /// The client ignores metadata on [`code`](Self::code) vars; put a
    /// docstring in the code instead.
    ///
    /// # Panics
    ///
    /// Panics if no var with this name has been registered yet.
    pub fn meta(mut self, name: &str, edn: impl Into<String>) -> Self {
        let var = self
            .vars
            .iter_mut()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("meta for unknown var {}/{name}", self.name));
        var.meta = Some(edn.into());
        self
    }

    fn push(mut self, name: String, kind: VarKind) -> Self {
        assert!(
            !self.vars.iter().any(|v| v.name == name),
            "var {}/{name} registered twice",
            self.name
        );
        self.vars.push(Var {
            name,
            meta: None,
            kind,
        });
        self
    }

    fn describe_vars(&self) -> Value {
        Value::List(
            self.vars
                .iter()
                .map(|var| {
                    let mut pairs = vec![("name", Value::from(var.name.as_str()))];
                    if let VarKind::Code(code) = &var.kind {
                        pairs.push(("code", Value::from(code.as_str())));
                    }
                    if let Some(meta) = &var.meta {
                        pairs.push(("meta", Value::from(meta.as_str())));
                    }
                    Value::dict(pairs)
                })
                .collect(),
        )
    }
}

fn decode_args<A: DeserializeOwned>(var: &str, args: &str) -> Result<A, PodError> {
    serde_json::from_str(args).map_err(|e| {
        PodError::new(format!("invalid arguments for {var}: {e}"))
            .with_data(serde_json::json!({ "var": var, "args": args }))
    })
}

/// A babashka pod: a set of namespaces plus the runtime that serves them.
pub struct Pod {
    namespaces: Vec<Namespace>,
    grace: Duration,
}

impl Default for Pod {
    fn default() -> Self {
        Self::new()
    }
}

impl Pod {
    /// Creates a pod with no namespaces. Payloads are JSON.
    pub fn new() -> Self {
        Pod {
            namespaces: Vec::new(),
            grace: DEFAULT_GRACE,
        }
    }

    /// Adds a namespace.
    pub fn namespace(mut self, ns: Namespace) -> Self {
        self.namespaces.push(ns);
        self
    }

    /// Sets how long in-flight calls may run after shutdown before the
    /// runtime returns anyway. Defaults to two seconds.
    ///
    /// babashka waits for the pod process to exit with no timeout once it
    /// sends `shutdown`, so this bounds how long a script's exit can hang.
    pub fn shutdown_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Serves the pod over the transport babashka asked for.
    ///
    /// Uses a localhost socket when `BABASHKA_POD_TRANSPORT=socket` and
    /// stdin/stdout otherwise. Returns when the client sends `shutdown`, closes
    /// the connection, or stops reading.
    ///
    /// In stdin/stdout mode, stdout carries the protocol: anything else printed
    /// to stdout corrupts it. Log to stderr, or use [`Emitter::out`].
    pub fn run(self) -> Result<(), Error> {
        match std::env::var("BABASHKA_POD_TRANSPORT").as_deref() {
            Ok("socket") => self.run_socket(),
            _ => self.run_with(io::stdin(), io::stdout()),
        }
    }

    fn run_socket(self) -> Result<(), Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let _port_file = PortFile::create(port)?;
        let (stream, _) = listener.accept()?;
        let writer = stream.try_clone()?;
        self.run_with(stream, writer)
    }

    /// Serves the pod over an arbitrary reader and writer.
    ///
    /// Returns `Ok` when the reader reaches end of input, the client sends
    /// `shutdown`, or writing fails because the client went away. Returns an
    /// error only when the input is not valid bencode or reading fails.
    pub fn run_with<R, W>(self, reader: R, writer: W) -> Result<(), Error>
    where
        R: Read,
        W: Write + Send + 'static,
    {
        let shared = Arc::new(Shared::new(Box::new(writer)));
        let handlers = self.handlers();
        let mut reader = BufReader::new(reader);

        let result = loop {
            let msg = match bencode::read_value(&mut reader) {
                Ok(Some(msg)) => msg,
                Ok(None) => break Ok(()),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    break Err(Error::Protocol(e.to_string()));
                }
                Err(e) => break Err(Error::Io(e)),
            };
            match msg.get_str("op") {
                Some("describe") => shared.write(&self.describe()),
                Some("invoke") => invoke(&shared, &handlers, &msg),
                Some("load-ns") => shared.write(&self.load_ns(&msg)),
                Some("shutdown") => break Ok(()),
                op => {
                    let op = op.unwrap_or("<missing>");
                    eprintln!("babashka-pod: ignoring unknown op {op}");
                    if let Some(id) = msg.get_str("id") {
                        shared.write(&error_reply(
                            id,
                            &PodError::new(format!("unknown op: {op}")),
                        ));
                    }
                }
            }
            if shared.is_closed() {
                break Ok(());
            }
        };

        shared.cancel();
        shared.wait_idle(self.grace);
        result
    }

    fn handlers(&self) -> HashMap<String, Handler> {
        let mut map = HashMap::new();
        for ns in &self.namespaces {
            for var in &ns.vars {
                if let VarKind::Handler(h) = &var.kind {
                    map.insert(format!("{}/{}", ns.name, var.name), h.clone());
                }
            }
        }
        map
    }

    fn describe(&self) -> Value {
        let namespaces = self
            .namespaces
            .iter()
            .map(|ns| {
                if ns.deferred {
                    Value::dict([
                        ("name", Value::from(ns.name.as_str())),
                        ("defer", Value::from("true")),
                    ])
                } else {
                    Value::dict([
                        ("name", Value::from(ns.name.as_str())),
                        ("vars", ns.describe_vars()),
                    ])
                }
            })
            .collect();
        Value::dict([
            ("format", Value::from("json")),
            ("namespaces", Value::List(namespaces)),
            (
                "ops",
                Value::dict([("shutdown", Value::dict::<_, &str>([]))]),
            ),
        ])
    }

    fn load_ns(&self, msg: &Value) -> Value {
        let id = msg.get_str("id").unwrap_or_default();
        let name = msg.get_str("ns").unwrap_or_default();
        match self.namespaces.iter().find(|ns| ns.name == name) {
            Some(ns) => Value::dict([
                ("id", Value::from(id)),
                ("name", Value::from(name)),
                ("vars", ns.describe_vars()),
            ]),
            None => error_reply(id, &PodError::new(format!("unknown namespace: {name}"))),
        }
    }
}

fn invoke(shared: &Arc<Shared>, handlers: &HashMap<String, Handler>, msg: &Value) {
    let id = msg.get_str("id").unwrap_or_default().to_string();
    let var = msg.get_str("var").unwrap_or_default().to_string();
    let args = msg.get_str("args").unwrap_or("[]").to_string();

    let Some(handler) = handlers.get(&var).cloned() else {
        let err = PodError::new(format!("unknown var: {var}"))
            .with_data(serde_json::json!({ "var": var }));
        shared.write(&error_reply(&id, &err));
        return;
    };

    shared.begin();
    let tx = Emitter::new(id.clone(), shared.clone());
    let spawned = thread::Builder::new()
        .name(format!("pod:{var}"))
        .spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| handler(&args, tx.clone())));
            let reply = match outcome {
                Ok(Ok(Some(value))) => Value::dict([
                    ("id", Value::from(tx.id.as_str())),
                    ("value", Value::from(value)),
                    ("status", Value::List(vec!["done".into()])),
                ]),
                Ok(Ok(None)) => Value::dict([
                    ("id", Value::from(tx.id.as_str())),
                    ("status", Value::List(vec!["done".into()])),
                ]),
                Ok(Err(e)) => error_reply(&tx.id, &e),
                Err(panic) => {
                    let detail = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    error_reply(&tx.id, &PodError::new(format!("{var} panicked: {detail}")))
                }
            };
            tx.finish(&reply);
            tx.shared.end();
        });
    if let Err(e) = spawned {
        shared.end();
        let err = PodError::new(format!(
            "could not start a thread for {}: {e}",
            msg.get_str("var").unwrap_or_default()
        ));
        shared.write(&error_reply(&id, &err));
    }
}

fn error_reply(id: &str, err: &PodError) -> Value {
    let data = err
        .data()
        .map(|d| d.to_string())
        .unwrap_or_else(|| "{}".to_string());
    Value::dict([
        ("id", Value::from(id)),
        ("status", Value::List(vec!["done".into(), "error".into()])),
        ("ex-message", Value::from(err.message())),
        ("ex-data", Value::from(data)),
    ])
}

/// Sends values and output for one in-flight call.
///
/// Handed to [`stream`](Namespace::stream) handlers. It is cheap to clone and
/// can be moved to other threads; sends from any clone are ordered and never
/// interleave with other calls' replies.
#[derive(Clone)]
pub struct Emitter {
    id: String,
    finished: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

impl Emitter {
    fn new(id: String, shared: Arc<Shared>) -> Self {
        Emitter {
            id,
            finished: Arc::new(AtomicBool::new(false)),
            shared,
        }
    }

    /// Sends one value to the client's `:success` handler.
    ///
    /// Fails once the call has finished or the client has gone away.
    pub fn send<T: Serialize + ?Sized>(&self, value: &T) -> Result<(), PodError> {
        let json = serde_json::to_string(value)?;
        self.emit(Value::dict([
            ("id", Value::from(self.id.as_str())),
            ("value", Value::from(json)),
        ]))
    }

    /// Prints text to the calling script's `*out*`.
    pub fn out(&self, text: &str) -> Result<(), PodError> {
        self.emit(Value::dict([
            ("id", Value::from(self.id.as_str())),
            ("out", Value::from(text)),
        ]))
    }

    /// Prints text to the calling script's `*err*`.
    pub fn err(&self, text: &str) -> Result<(), PodError> {
        self.emit(Value::dict([
            ("id", Value::from(self.id.as_str())),
            ("err", Value::from(text)),
        ]))
    }

    /// True once the pod is shutting down. Long-running handlers should check
    /// this and return.
    pub fn is_cancelled(&self) -> bool {
        self.shared.state().cancelled
    }

    /// Blocks until the pod starts shutting down or `timeout` passes.
    /// Returns true if it is shutting down.
    ///
    /// Use it in place of `thread::sleep` in polling loops, or with a long
    /// timeout to keep a call open while other threads send values.
    pub fn wait_cancelled(&self, timeout: Duration) -> bool {
        let state = self.shared.state();
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |s| !s.cancelled)
            .unwrap_or_else(|e| e.into_inner());
        state.cancelled
    }

    fn emit(&self, msg: Value) -> Result<(), PodError> {
        let mut writer = self.shared.writer();
        if self.finished.load(Ordering::SeqCst) {
            return Err(PodError::new("call already finished"));
        }
        if !self.shared.write_locked(&mut writer, &msg) {
            return Err(PodError::new("client disconnected"));
        }
        Ok(())
    }

    fn finish(&self, reply: &Value) {
        let mut writer = self.shared.writer();
        self.finished.store(true, Ordering::SeqCst);
        self.shared.write_locked(&mut writer, reply);
    }
}

struct State {
    cancelled: bool,
    inflight: usize,
}

struct Shared {
    writer: Mutex<Box<dyn Write + Send>>,
    closed: AtomicBool,
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn new(writer: Box<dyn Write + Send>) -> Self {
        Shared {
            writer: Mutex::new(writer),
            closed: AtomicBool::new(false),
            state: Mutex::new(State {
                cancelled: false,
                inflight: 0,
            }),
            changed: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn writer(&self) -> MutexGuard<'_, Box<dyn Write + Send>> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self, msg: &Value) {
        let mut writer = self.writer();
        self.write_locked(&mut writer, msg);
    }

    /// Writes one whole message. A failed write means the client is gone, so
    /// it marks the connection closed and cancels in-flight calls.
    fn write_locked(&self, writer: &mut Box<dyn Write + Send>, msg: &Value) -> bool {
        if self.is_closed() {
            return false;
        }
        let bytes = msg.to_bytes();
        match writer.write_all(&bytes).and_then(|_| writer.flush()) {
            Ok(()) => true,
            Err(_) => {
                self.closed.store(true, Ordering::SeqCst);
                self.cancel();
                false
            }
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.state().cancelled = true;
        self.changed.notify_all();
    }

    fn begin(&self) {
        self.state().inflight += 1;
    }

    fn end(&self) {
        self.state().inflight -= 1;
        self.changed.notify_all();
    }

    fn wait_idle(&self, grace: Duration) {
        let state = self.state();
        let _ = self
            .changed
            .wait_timeout_while(state, grace, |s| s.inflight > 0)
            .unwrap_or_else(|e| e.into_inner());
    }
}

/// The `.babashka-pod-<pid>.port` file babashka polls for in socket mode.
/// Removed when dropped.
struct PortFile(PathBuf);

impl PortFile {
    fn create(port: u16) -> io::Result<Self> {
        let path = PathBuf::from(format!(".babashka-pod-{}.port", std::process::id()));
        // babashka reads the file once it ends in a newline, so write it whole.
        let tmp = path.with_extension("port.tmp");
        fs::write(&tmp, format!("{port}\n"))?;
        fs::rename(&tmp, &path)?;
        Ok(PortFile(path))
    }
}

impl Drop for PortFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
