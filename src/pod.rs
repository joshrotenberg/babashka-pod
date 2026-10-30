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
use crate::transit;

/// How long in-flight calls get to finish after shutdown, by default.
const DEFAULT_GRACE: Duration = Duration::from_secs(2);

type Json = serde_json::Value;

/// A registered var's handler. It gets the argument payload as sent and
/// decodes it itself, so each var decodes into the form it asked for.
type Handler = Arc<dyn Fn(&str, Format, Emitter) -> Result<Option<Json>, PodError> + Send + Sync>;

/// The payload format: how arguments, return values and `ex-data` are
/// encoded inside the bencode messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// JSON, the default. Keywords, symbols, sets and insts sent by babashka
    /// arrive as strings, arrays and strings. Return values are written as
    /// plain JSON: the [`transit`](crate::transit) types degrade to it (a
    /// keyword to its name, a set to an array). babashka keywordizes map keys.
    #[default]
    Json,
    /// [Transit](crate::transit) over JSON.
    ///
    /// Return values can carry keywords, symbols, sets, insts, uuids, tagged
    /// values and metadata through the [`transit`](crate::transit) types. Map
    /// keys are written as keywords.
    ///
    /// Arguments to [`function`](Namespace::function),
    /// [`stream`](Namespace::stream) and [`stream_fn`](Namespace::stream_fn)
    /// are decoded into `serde_json::Value` first, so keywords and symbols
    /// arrive as strings, sets and lists as arrays, and insts as epoch millis.
    /// Arguments to the `*_transit` methods, such as
    /// [`function_transit`](Namespace::function_transit), keep their Clojure
    /// types as [`transit::Value`](crate::transit::Value)s. Only this format
    /// supports [`Namespace::arg_meta`].
    TransitJson,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::TransitJson => "transit+json",
        }
    }

    fn decode(self, text: &str) -> Result<Json, String> {
        match self {
            Format::Json => serde_json::from_str(text).map_err(|e| e.to_string()),
            Format::TransitJson => transit::decode(text).map_err(|e| e.to_string()),
        }
    }

    fn encode(self, value: &Json) -> String {
        match self {
            Format::Json => transit::to_plain_json(value).to_string(),
            Format::TransitJson => transit::encode(value),
        }
    }
}

/// Returns true when the process was started by babashka as a pod.
///
/// babashka sets `BABASHKA_POD=true` in the pod's environment. A binary that is
/// also a regular CLI can use this to decide which mode to run in.
pub fn is_pod_mode() -> bool {
    std::env::var("BABASHKA_POD").is_ok_and(|v| v == "true")
}

/// Arguments for a function that takes none.
///
/// Deserializes only from an empty argument vector. babashka sends `nil` for a
/// call with no arguments; the runtime passes it to handlers as an empty vector.
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
    /// Client code generated for a [`Namespace::stream_fn`], calling the raw
    /// streaming var named here.
    StreamWrapper {
        raw: String,
    },
}

struct Var {
    name: String,
    /// Raw EDN metadata from [`Namespace::meta`].
    meta: Option<String>,
    doc: Option<String>,
    arglists: Option<Vec<String>>,
    arg_meta: bool,
    kind: VarKind,
}

impl Var {
    /// The EDN metadata map for `describe`, if any. Client code vars ignore
    /// metadata, so wrappers carry theirs inside the generated code instead.
    fn meta_edn(&self) -> Option<String> {
        if self.meta.is_some() {
            return self.meta.clone();
        }
        if matches!(self.kind, VarKind::StreamWrapper { .. }) {
            return None;
        }
        let mut entries = Vec::new();
        if let Some(doc) = &self.doc {
            entries.push(format!(":doc {}", edn_string(doc)));
        }
        if let Some(arglists) = &self.arglists {
            entries.push(format!(":arglists ({})", arglists.join(" ")));
        }
        (!entries.is_empty()).then(|| format!("{{{}}}", entries.join(" ")))
    }
}

/// An EDN (and Clojure) string literal for `s`.
fn edn_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Clojure source for the client side of a [`Namespace::stream_fn`].
fn stream_wrapper_code(ns: &str, var: &Var, raw: &str) -> String {
    let doc = var
        .doc
        .as_deref()
        .map(|d| format!("\n  {}", edn_string(d)))
        .unwrap_or_default();
    let attrs = var
        .arglists
        .as_ref()
        .map(|a| format!("\n  {{:arglists '({})}}", a.join(" ")))
        .unwrap_or_default();
    let name = &var.name;
    let arg_meta = if var.arg_meta {
        "\n                           :arg-meta true"
    } else {
        ""
    };
    format!(
        r#"(defn {name}{doc}{attrs}
  [& args]
  (let [callback (last args)
        result (promise)]
    (when-not (ifn? callback)
      (throw (ex-info "the last argument to {ns}/{name} must be a callback fn"
                      {{:var '{ns}/{name}}})))
    (babashka.pods/invoke "{ns}" '{ns}/{raw} (vec (butlast args))
                          {{:handlers {{:success callback
                                      :error (fn [e] (deliver result e))
                                      :done (fn [] (deliver result nil))}}{arg_meta}}})
    (when-let [{{:keys [ex-message ex-data]}} @result]
      (throw (ex-info ex-message ex-data)))
    nil))"#
    )
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
        let handler: Handler = Arc::new(move |raw, format, _tx| {
            let args = decode_args(&qualified, json_args(&qualified, raw, format)?)?;
            let value = f(args)?;
            serde_json::to_value(&value)
                .map(Some)
                .map_err(PodError::from)
        });
        self.push(name, VarKind::Handler(handler))
    }

    /// Registers a function whose arguments keep their Clojure types.
    ///
    /// Like [`function`](Self::function), but the handler gets the argument
    /// vector as [`transit::Value`]s: keywords, symbols, chars, big numbers,
    /// ratios, lists, sets and maps with any keys stay distinct. Under
    /// [`Format::Json`], which cannot carry those types, the arguments are
    /// built from the plain JSON.
    pub fn function_transit<R, F>(self, name: impl Into<String>, f: F) -> Self
    where
        R: Serialize,
        F: Fn(Vec<transit::Value>) -> Result<R, PodError> + Send + Sync + 'static,
    {
        let name = name.into();
        let qualified = format!("{}/{}", self.name, name);
        let handler: Handler = Arc::new(move |raw, format, _tx| {
            let value = f(tree_args(&qualified, raw, format)?)?;
            serde_json::to_value(&value)
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
        let handler: Handler = Arc::new(move |raw, format, tx| {
            let args = decode_args(&qualified, json_args(&qualified, raw, format)?)?;
            f(args, tx)?;
            Ok(None)
        });
        self.push(name, VarKind::Handler(handler))
    }

    /// Registers a streaming function whose arguments keep their Clojure
    /// types: [`stream`](Self::stream) with arguments as in
    /// [`function_transit`](Self::function_transit).
    pub fn stream_transit<F>(self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(Vec<transit::Value>, Emitter) -> Result<(), PodError> + Send + Sync + 'static,
    {
        let name = name.into();
        let qualified = format!("{}/{}", self.name, name);
        let handler: Handler = Arc::new(move |raw, format, tx| {
            f(tree_args(&qualified, raw, format)?, tx)?;
            Ok(None)
        });
        self.push(name, VarKind::Handler(handler))
    }

    /// Registers a streaming function together with its client-side wrapper.
    ///
    /// The handler is registered as the raw var `name*` (see
    /// [`stream`](Self::stream)). The pod also defines `name` with client
    /// code generated by the crate: `(name arg ... callback)` calls
    /// `callback` with each value, blocks until the stream is done, returns
    /// `nil`, and rethrows a pod error as `ex-info`.
    ///
    /// [`doc`](Self::doc) and [`arglists`](Self::arglists) on `name` go into
    /// the generated `defn`.
    pub fn stream_fn<A, F>(self, name: impl Into<String>, f: F) -> Self
    where
        A: DeserializeOwned,
        F: Fn(A, Emitter) -> Result<(), PodError> + Send + Sync + 'static,
    {
        let name = name.into();
        let raw = format!("{name}*");
        self.stream(raw.clone(), f)
            .push(name, VarKind::StreamWrapper { raw })
    }

    /// Registers a streaming function and its client-side wrapper, with
    /// arguments that keep their Clojure types: [`stream_fn`](Self::stream_fn)
    /// with arguments as in [`function_transit`](Self::function_transit).
    pub fn stream_fn_transit<F>(self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(Vec<transit::Value>, Emitter) -> Result<(), PodError> + Send + Sync + 'static,
    {
        let name = name.into();
        let raw = format!("{name}*");
        self.stream_transit(raw.clone(), f)
            .push(name, VarKind::StreamWrapper { raw })
    }

    /// Registers a var whose definition is Clojure code evaluated by the client.
    ///
    /// The code runs in this namespace, so it can call the pod's other vars.
    pub fn code(self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.push(name.into(), VarKind::Code(source.into()))
    }

    /// Asks babashka to send this var's arguments with their metadata.
    ///
    /// Only takes effect with [`Format::TransitJson`]. Arguments that carry
    /// metadata arrive wrapped; take them as
    /// [`transit::WithMeta<T>`](crate::transit::WithMeta). For a
    /// [`stream_fn`](Self::stream_fn), the generated wrapper passes the flag
    /// on.
    ///
    /// # Panics
    ///
    /// Panics if no var with this name has been registered yet, or if it is a
    /// [`code`](Self::code) var.
    pub fn arg_meta(mut self, name: &str) -> Self {
        let ns = self.name.clone();
        let var = self.var_mut(name, "arg_meta");
        assert!(
            !matches!(var.kind, VarKind::Code(_)),
            "arg_meta for {ns}/{name}: code vars are not invoked through the pod"
        );
        var.arg_meta = true;
        self
    }

    /// Sets a var's docstring. The crate escapes it.
    ///
    /// # Panics
    ///
    /// Panics if no var with this name has been registered yet, if the var
    /// already has raw [`meta`](Self::meta), or if it is a [`code`](Self::code)
    /// var (the client ignores metadata there; put the docstring in the code).
    pub fn doc(mut self, name: &str, text: impl Into<String>) -> Self {
        self.typed_meta_var(name, "doc").doc = Some(text.into());
        self
    }

    /// Sets a var's `:arglists`, one EDN vector per arity, such as
    /// `["[a b]", "[a b & more]"]`.
    ///
    /// # Panics
    ///
    /// Same conditions as [`doc`](Self::doc).
    pub fn arglists<I, S>(mut self, name: &str, arglists: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let arglists = arglists.into_iter().map(Into::into).collect();
        self.typed_meta_var(name, "arglists").arglists = Some(arglists);
        self
    }

    /// Attaches metadata to a registered var, as a raw EDN map string such as
    /// `{:doc "Adds two numbers" :arglists ([a b])}`. Prefer [`doc`](Self::doc)
    /// and [`arglists`](Self::arglists); use this for other keys.
    ///
    /// The client ignores metadata on [`code`](Self::code) vars; put a
    /// docstring in the code instead.
    ///
    /// # Panics
    ///
    /// Panics if no var with this name has been registered yet, or if the var
    /// already has a [`doc`](Self::doc) or [`arglists`](Self::arglists): EDN
    /// rejects a map with duplicate keys, so the two forms do not mix.
    pub fn meta(mut self, name: &str, edn: impl Into<String>) -> Self {
        let ns = self.name.clone();
        let var = self.var_mut(name, "meta");
        assert!(
            var.doc.is_none() && var.arglists.is_none(),
            "meta for {ns}/{name}: it already has doc or arglists; use one form or the other"
        );
        var.meta = Some(edn.into());
        self
    }

    fn var_mut(&mut self, name: &str, what: &str) -> &mut Var {
        let ns = &self.name;
        self.vars
            .iter_mut()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("{what} for unknown var {ns}/{name}"))
    }

    fn typed_meta_var(&mut self, name: &str, what: &str) -> &mut Var {
        let ns = self.name.clone();
        let var = self.var_mut(name, what);
        assert!(
            var.meta.is_none(),
            "{what} for {ns}/{name}: it already has raw meta; use one form or the other"
        );
        assert!(
            !matches!(var.kind, VarKind::Code(_)),
            "{what} for {ns}/{name}: the client ignores metadata on code vars; put it in the code"
        );
        var
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
            doc: None,
            arglists: None,
            arg_meta: false,
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
                    match &var.kind {
                        VarKind::Code(code) => pairs.push(("code", Value::from(code.as_str()))),
                        VarKind::StreamWrapper { raw } => pairs.push((
                            "code",
                            Value::from(stream_wrapper_code(&self.name, var, raw)),
                        )),
                        VarKind::Handler(_) => {
                            if var.arg_meta {
                                pairs.push(("arg-meta", Value::from("true")));
                            }
                        }
                    }
                    if let Some(meta) = var.meta_edn() {
                        pairs.push(("meta", Value::from(meta)));
                    }
                    Value::dict(pairs)
                })
                .collect(),
        )
    }
}

fn bad_payload(var: &str, raw: &str, e: impl std::fmt::Display) -> PodError {
    PodError::new(format!("invalid arguments for {var}: {e}"))
        .with_data(serde_json::json!({ "var": var, "args": raw }))
}

/// Decodes an argument payload in the pod's format into JSON.
fn json_args(var: &str, raw: &str, format: Format) -> Result<Json, PodError> {
    format.decode(raw).map_err(|e| bad_payload(var, raw, e))
}

/// Decodes an argument payload into transit values. babashka sends nil, not
/// [], for a call with no arguments.
fn tree_args(var: &str, raw: &str, format: Format) -> Result<Vec<transit::Value>, PodError> {
    let decoded = match format {
        Format::TransitJson => transit::read(raw).map_err(|e| e.to_string()),
        Format::Json => serde_json::from_str::<Json>(raw)
            .map(|j| transit::Value::from(&j))
            .map_err(|e| e.to_string()),
    };
    match decoded.map_err(|e| bad_payload(var, raw, e))? {
        transit::Value::Nil => Ok(Vec::new()),
        transit::Value::Vector(items) | transit::Value::List(items) => Ok(items),
        other => Err(bad_payload(
            var,
            raw,
            format!("expected an argument vector, got {other:?}"),
        )),
    }
}

fn decode_args<A: DeserializeOwned>(var: &str, args: Json) -> Result<A, PodError> {
    // babashka sends nil, not [], for a call with no arguments.
    let args = if args.is_null() {
        Json::Array(vec![])
    } else {
        args
    };
    serde_json::from_value(args.clone()).map_err(|e| {
        PodError::new(format!("invalid arguments for {var}: {e}"))
            .with_data(serde_json::json!({ "var": var, "args": args }))
    })
}

/// A babashka pod: a set of namespaces plus the runtime that serves them.
pub struct Pod {
    namespaces: Vec<Namespace>,
    grace: Duration,
    format: Format,
}

impl Default for Pod {
    fn default() -> Self {
        Self::new()
    }
}

impl Pod {
    /// Creates a pod with no namespaces. Payloads are JSON until
    /// [`format`](Self::format) says otherwise.
    pub fn new() -> Self {
        Pod {
            namespaces: Vec::new(),
            grace: DEFAULT_GRACE,
            format: Format::Json,
        }
    }

    /// Sets the payload format. Defaults to [`Format::Json`].
    pub fn format(mut self, format: Format) -> Self {
        self.format = format;
        self
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
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the socket cannot be bound or accepted, or the
    /// port file babashka reads cannot be written, and otherwise fails as
    /// [`run_with`](Self::run_with) does.
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
    /// `shutdown`, or writing fails because the client went away.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] when the input is not valid bencode, and
    /// [`Error::Io`] when reading fails.
    pub fn run_with<R, W>(self, reader: R, writer: W) -> Result<(), Error>
    where
        R: Read,
        W: Write + Send + 'static,
    {
        let shared = Arc::new(Shared::new(Box::new(writer), self.format));
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
                            self.format,
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
            ("format", Value::from(self.format.name())),
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
            None => error_reply(
                id,
                &PodError::new(format!("unknown namespace: {name}")),
                self.format,
            ),
        }
    }
}

fn invoke(shared: &Arc<Shared>, handlers: &HashMap<String, Handler>, msg: &Value) {
    let id = msg.get_str("id").unwrap_or_default().to_string();
    let var = msg.get_str("var").unwrap_or_default().to_string();
    let raw_args = msg.get_str("args").unwrap_or("[]");
    let format = shared.format;

    let Some(handler) = handlers.get(&var).cloned() else {
        let err = PodError::new(format!("unknown var: {var}"))
            .with_data(serde_json::json!({ "var": var }));
        shared.write(&error_reply(&id, &err, format));
        return;
    };
    let raw_args = raw_args.to_string();

    let call = shared.begin();
    let tx = Emitter::new(id.clone(), shared.clone(), call.clone());
    let spawned = thread::Builder::new()
        .name(format!("pod:{var}"))
        .spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| handler(&raw_args, format, tx.clone())));
            // The handler has returned, so cancellation no longer applies.
            tx.call.finish();
            let reply = match outcome {
                Ok(Ok(Some(value))) => Value::dict([
                    ("id", Value::from(tx.id.as_str())),
                    ("value", Value::from(format.encode(&value))),
                    ("status", Value::List(vec!["done".into()])),
                ]),
                Ok(Ok(None)) => Value::dict([
                    ("id", Value::from(tx.id.as_str())),
                    ("status", Value::List(vec!["done".into()])),
                ]),
                Ok(Err(e)) => error_reply(&tx.id, &e, format),
                Err(panic) => {
                    let detail = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    error_reply(
                        &tx.id,
                        &PodError::new(format!("{var} panicked: {detail}")),
                        format,
                    )
                }
            };
            tx.finish(&reply);
            tx.shared.end(&tx.call);
        });
    if let Err(e) = spawned {
        shared.end(&call);
        let err = PodError::new(format!(
            "could not start a thread for {}: {e}",
            msg.get_str("var").unwrap_or_default()
        ));
        shared.write(&error_reply(&id, &err, format));
    }
}

fn error_reply(id: &str, err: &PodError, format: Format) -> Value {
    let empty = serde_json::json!({});
    let data = format.encode(err.data().unwrap_or(&empty));
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
///
/// A call is cancelled when the pod shuts down (the `shutdown` op, end of
/// input, or a failed write) or when its [`CancelToken`] is cancelled.
/// Cancellation only signals: the call ends when the handler returns.
#[derive(Clone)]
pub struct Emitter {
    id: String,
    finished: Arc<AtomicBool>,
    call: Arc<Call>,
    shared: Arc<Shared>,
}

impl Emitter {
    fn new(id: String, shared: Arc<Shared>, call: Arc<Call>) -> Self {
        Emitter {
            id,
            finished: Arc::new(AtomicBool::new(false)),
            call,
            shared,
        }
    }

    /// Sends one value to the client's `:success` handler.
    ///
    /// # Errors
    ///
    /// Fails when `value` cannot be serialized, once the call has finished, or
    /// when the client has gone away (which also cancels every call).
    pub fn send<T: Serialize + ?Sized>(&self, value: &T) -> Result<(), PodError> {
        let encoded = self.shared.format.encode(&serde_json::to_value(value)?);
        self.emit(Value::dict([
            ("id", Value::from(self.id.as_str())),
            ("value", Value::from(encoded)),
        ]))
    }

    /// Prints text to the calling script's `*out*`.
    ///
    /// # Errors
    ///
    /// Fails once the call has finished, or when the client has gone away.
    pub fn out(&self, text: &str) -> Result<(), PodError> {
        self.emit(Value::dict([
            ("id", Value::from(self.id.as_str())),
            ("out", Value::from(text)),
        ]))
    }

    /// Prints text to the calling script's `*err*`.
    ///
    /// # Errors
    ///
    /// Fails like [`out`](Self::out).
    pub fn err(&self, text: &str) -> Result<(), PodError> {
        self.emit(Value::dict([
            ("id", Value::from(self.id.as_str())),
            ("err", Value::from(text)),
        ]))
    }

    /// True once this call is cancelled, either through its [`CancelToken`]
    /// or because the pod is shutting down. Long-running handlers should
    /// check this and return.
    pub fn is_cancelled(&self) -> bool {
        let state = self.shared.state();
        state.cancelled || self.call.cancelled.load(Ordering::SeqCst)
    }

    /// Blocks until this call is cancelled or `timeout` passes. Returns true
    /// if it is cancelled.
    ///
    /// Use it in place of `thread::sleep` in polling loops, or with a long
    /// timeout to keep a call open while other threads send values. A handler
    /// that blocks on its own channel can use [`on_cancel`](Self::on_cancel)
    /// instead.
    pub fn wait_cancelled(&self, timeout: Duration) -> bool {
        let state = self.shared.state();
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |s| {
                !(s.cancelled || self.call.cancelled.load(Ordering::SeqCst))
            })
            .unwrap_or_else(|e| e.into_inner());
        state.cancelled || self.call.cancelled.load(Ordering::SeqCst)
    }

    /// Returns a token that cancels this call alone.
    ///
    /// Store it where another call can reach it, for example in a
    /// [`Handles`](crate::Handles) registry, to implement a `stop` or
    /// `unwatch` function.
    pub fn cancel_token(&self) -> CancelToken {
        CancelToken {
            call: self.call.clone(),
            shared: self.shared.clone(),
        }
    }

    /// Runs `f` once when this call is cancelled, through its [`CancelToken`]
    /// or because the pod is shutting down.
    ///
    /// If the call is already cancelled, `f` runs right away on this thread.
    /// If the call finishes first, `f` is dropped without running, which
    /// drops anything it captured (a channel sender, for example).
    ///
    /// `f` runs on the thread that cancels: the caller of
    /// [`CancelToken::cancel`], or a runtime thread during shutdown. Keep it
    /// short and non-blocking, such as sending on a channel the handler is
    /// waiting on.
    pub fn on_cancel(&self, f: impl FnOnce() + Send + 'static) {
        self.call.on_cancel(Box::new(f));
    }

    fn emit(&self, msg: Value) -> Result<(), PodError> {
        let written = {
            let mut writer = self.shared.writer();
            if self.finished.load(Ordering::SeqCst) {
                return Err(PodError::new("call already finished"));
            }
            self.shared.write_locked(&mut writer, &msg)
        };
        if !written {
            self.shared.cancel();
            return Err(PodError::new("client disconnected"));
        }
        Ok(())
    }

    fn finish(&self, reply: &Value) {
        let written = {
            let mut writer = self.shared.writer();
            self.finished.store(true, Ordering::SeqCst);
            self.shared.write_locked(&mut writer, reply)
        };
        if !written {
            self.shared.cancel();
        }
    }
}

/// Cancels one in-flight call. Obtained from [`Emitter::cancel_token`].
///
/// Cloneable and `Send`. Cancelling wakes [`Emitter::wait_cancelled`] and
/// runs the call's [`Emitter::on_cancel`] hooks; it does nothing once the call
/// has finished.
#[derive(Clone)]
pub struct CancelToken {
    call: Arc<Call>,
    shared: Arc<Shared>,
}

impl CancelToken {
    /// Cancels the call. Hooks registered with [`Emitter::on_cancel`] run on
    /// this thread before it returns. Cancelling twice has no further effect.
    pub fn cancel(&self) {
        {
            // Set the flag under the state lock so a waiter in
            // `wait_cancelled` cannot miss the wakeup.
            let _state = self.shared.state();
            self.call.cancelled.store(true, Ordering::SeqCst);
        }
        self.shared.changed.notify_all();
        self.call.run_hooks();
    }

    /// True once the call is cancelled, through this token or pod-wide.
    pub fn is_cancelled(&self) -> bool {
        let state = self.shared.state();
        state.cancelled || self.call.cancelled.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

type Hook = Box<dyn FnOnce() + Send>;

enum Hooks {
    Pending(Vec<Hook>),
    Cancelled,
    Finished,
}

/// Per-call cancellation state.
struct Call {
    key: u64,
    cancelled: AtomicBool,
    hooks: Mutex<Hooks>,
}

impl Call {
    fn hooks(&self) -> MutexGuard<'_, Hooks> {
        self.hooks.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn on_cancel(&self, f: Hook) {
        let mut hooks = self.hooks();
        match &mut *hooks {
            Hooks::Pending(pending) => pending.push(f),
            Hooks::Cancelled => {
                drop(hooks);
                f();
            }
            Hooks::Finished => {}
        }
    }

    /// Runs pending hooks once, outside the lock.
    fn run_hooks(&self) {
        let pending = {
            let mut hooks = self.hooks();
            match std::mem::replace(&mut *hooks, Hooks::Cancelled) {
                Hooks::Pending(pending) => pending,
                Hooks::Cancelled => Vec::new(),
                Hooks::Finished => {
                    *hooks = Hooks::Finished;
                    Vec::new()
                }
            }
        };
        for hook in pending {
            hook();
        }
    }

    /// Drops hooks that never ran, once the call is over.
    fn finish(&self) {
        let mut hooks = self.hooks();
        if matches!(*hooks, Hooks::Pending(_)) {
            *hooks = Hooks::Finished;
        }
    }
}

struct State {
    cancelled: bool,
    next_key: u64,
    calls: HashMap<u64, Arc<Call>>,
}

struct Shared {
    format: Format,
    writer: Mutex<Box<dyn Write + Send>>,
    closed: AtomicBool,
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn new(writer: Box<dyn Write + Send>, format: Format) -> Self {
        Shared {
            format,
            writer: Mutex::new(writer),
            closed: AtomicBool::new(false),
            state: Mutex::new(State {
                cancelled: false,
                next_key: 0,
                calls: HashMap::new(),
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
        let written = {
            let mut writer = self.writer();
            self.write_locked(&mut writer, msg)
        };
        if !written {
            self.cancel();
        }
    }

    /// Writes one whole message. A failed write means the client is gone, so
    /// it marks the connection closed. Callers cancel in-flight calls after
    /// releasing the writer lock, because `on_cancel` hooks may send.
    fn write_locked(&self, writer: &mut Box<dyn Write + Send>, msg: &Value) -> bool {
        if self.is_closed() {
            return false;
        }
        let bytes = msg.to_bytes();
        match writer.write_all(&bytes).and_then(|_| writer.flush()) {
            Ok(()) => true,
            Err(_) => {
                self.closed.store(true, Ordering::SeqCst);
                false
            }
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Cancels every in-flight call and runs their hooks. Must not be called
    /// with the writer lock held.
    fn cancel(&self) {
        let calls: Vec<Arc<Call>> = {
            let mut state = self.state();
            state.cancelled = true;
            state.calls.values().cloned().collect()
        };
        self.changed.notify_all();
        for call in calls {
            call.run_hooks();
        }
    }

    /// Registers a new in-flight call. If the pod is already shutting down,
    /// the call starts out cancelled.
    fn begin(&self) -> Arc<Call> {
        let mut state = self.state();
        let key = state.next_key;
        state.next_key += 1;
        let call = Arc::new(Call {
            key,
            cancelled: AtomicBool::new(false),
            hooks: Mutex::new(if state.cancelled {
                Hooks::Cancelled
            } else {
                Hooks::Pending(Vec::new())
            }),
        });
        state.calls.insert(key, call.clone());
        call
    }

    fn end(&self, call: &Call) {
        call.finish();
        self.state().calls.remove(&call.key);
        self.changed.notify_all();
    }

    fn wait_idle(&self, grace: Duration) {
        let state = self.state();
        let _ = self
            .changed
            .wait_timeout_while(state, grace, |s| !s.calls.is_empty())
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
