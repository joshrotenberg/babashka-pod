//! Runtime tests that drive a pod over in-memory streams.

use std::io::{self, Cursor, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::bencode::{Value, read_value};
use crate::{Emitter, Error, Namespace, NoArgs, Pod, PodError};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BrokenPipe;

impl Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn msg(pairs: &[(&str, &str)]) -> Value {
    Value::dict(pairs.iter().map(|(k, v)| (*k, Value::from(*v))))
}

fn invoke(id: &str, var: &str, args: &str) -> Value {
    msg(&[("op", "invoke"), ("id", id), ("var", var), ("args", args)])
}

fn input(msgs: &[Value]) -> Cursor<Vec<u8>> {
    Cursor::new(msgs.iter().flat_map(Value::to_bytes).collect())
}

fn decode_all(bytes: &[u8]) -> Vec<Value> {
    let mut r = Cursor::new(bytes);
    let mut out = Vec::new();
    while let Some(v) = read_value(&mut r).unwrap() {
        out.push(v);
    }
    out
}

fn run(pod: Pod, msgs: &[Value]) -> (Result<(), Error>, Vec<Value>) {
    let buf = SharedBuf::default();
    let result = pod.run_with(input(msgs), buf.clone());
    let bytes = buf.0.lock().unwrap().clone();
    (result, decode_all(&bytes))
}

fn replies<'a>(out: &'a [Value], id: &str) -> Vec<&'a Value> {
    out.iter().filter(|m| m.get_str("id") == Some(id)).collect()
}

fn status(v: &Value) -> Vec<&str> {
    match v.get("status") {
        Some(Value::List(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn json_of(v: &Value, key: &str) -> serde_json::Value {
    serde_json::from_str(v.get_str(key).unwrap()).unwrap()
}

fn example_pod() -> Pod {
    Pod::new()
        .namespace(
            Namespace::new("pod.test.math")
                .function("add", |(a, b): (i64, i64)| Ok::<_, PodError>(a + b))
                .meta("add", "{:doc \"adds\"}")
                .function("answer", |_: NoArgs| Ok::<_, PodError>(42))
                .function("fail", |(n,): (i64,)| -> Result<(), PodError> {
                    Err(PodError::new("nope").with_data(json!({ "n": n })))
                })
                .function("io-fail", |_: NoArgs| -> Result<(), PodError> {
                    Err(io::Error::other("disk on fire"))?
                })
                .function("boom", |_: NoArgs| -> Result<(), PodError> {
                    panic!("kaboom")
                })
                .stream("count", |(n,): (u64,), tx: Emitter| {
                    tx.out("counting\n")?;
                    for i in 0..n {
                        tx.send(&i)?;
                    }
                    Ok(())
                })
                .code("twice", "(defn twice [x] (add x x))"),
        )
        .namespace(
            Namespace::new("pod.test.lazy")
                .deferred()
                .function("hello", |_: NoArgs| Ok::<_, PodError>("hi")),
        )
}

#[test]
fn describe_lists_namespaces_vars_and_ops() {
    let (result, out) = run(example_pod(), &[msg(&[("op", "describe"), ("id", "0")])]);
    result.unwrap();
    assert_eq!(out.len(), 1);
    let d = &out[0];
    assert_eq!(d.get_str("format"), Some("json"));
    assert!(d.get("ops").unwrap().get("shutdown").is_some());

    let Some(Value::List(nss)) = d.get("namespaces") else {
        panic!("no namespaces")
    };
    assert_eq!(nss[0].get_str("name"), Some("pod.test.math"));
    let Some(Value::List(vars)) = nss[0].get("vars") else {
        panic!("no vars")
    };
    let names: Vec<_> = vars.iter().filter_map(|v| v.get_str("name")).collect();
    assert_eq!(
        names,
        ["add", "answer", "fail", "io-fail", "boom", "count", "twice"]
    );
    assert_eq!(vars[0].get_str("meta"), Some("{:doc \"adds\"}"));
    assert_eq!(vars[6].get_str("code"), Some("(defn twice [x] (add x x))"));
    assert!(vars[0].get("code").is_none());

    assert_eq!(nss[1].get_str("name"), Some("pod.test.lazy"));
    assert_eq!(nss[1].get_str("defer"), Some("true"));
    assert!(nss[1].get("vars").is_none());
}

#[test]
fn invoke_returns_value_and_done_in_one_reply() {
    let (result, out) = run(
        example_pod(),
        &[
            invoke("1", "pod.test.math/add", "[1,2]"),
            invoke("2", "pod.test.math/answer", "[]"),
        ],
    );
    result.unwrap();
    let r1 = replies(&out, "1");
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0].get_str("value"), Some("3"));
    assert_eq!(status(r1[0]), ["done"]);
    assert_eq!(replies(&out, "2")[0].get_str("value"), Some("42"));
}

#[test]
fn handler_errors_become_error_replies() {
    let (result, out) = run(
        example_pod(),
        &[
            invoke("1", "pod.test.math/fail", "[7]"),
            invoke("2", "pod.test.math/io-fail", "[]"),
        ],
    );
    result.unwrap();
    let r = replies(&out, "1")[0];
    assert_eq!(status(r), ["done", "error"]);
    assert_eq!(r.get_str("ex-message"), Some("nope"));
    assert_eq!(json_of(r, "ex-data"), json!({ "n": 7 }));

    let r = replies(&out, "2")[0];
    assert_eq!(r.get_str("ex-message"), Some("disk on fire"));
    assert_eq!(json_of(r, "ex-data"), json!({}));
}

#[test]
fn bad_arguments_and_unknown_vars_are_errors() {
    let (result, out) = run(
        example_pod(),
        &[
            invoke("1", "pod.test.math/add", "[1]"),
            invoke("2", "pod.test.math/answer", "[1]"),
            invoke("3", "pod.test.math/nope", "[]"),
        ],
    );
    result.unwrap();
    let r = replies(&out, "1")[0];
    assert_eq!(status(r), ["done", "error"]);
    assert!(
        r.get_str("ex-message")
            .unwrap()
            .starts_with("invalid arguments for pod.test.math/add")
    );
    assert_eq!(json_of(r, "ex-data")["args"], "[1]");
    assert_eq!(status(replies(&out, "2")[0]), ["done", "error"]);
    let r = replies(&out, "3")[0];
    assert_eq!(
        r.get_str("ex-message"),
        Some("unknown var: pod.test.math/nope")
    );
}

#[test]
fn panics_become_error_replies_and_the_pod_keeps_serving() {
    let (result, out) = run(
        example_pod(),
        &[
            invoke("1", "pod.test.math/boom", "[]"),
            invoke("2", "pod.test.math/add", "[2,2]"),
        ],
    );
    result.unwrap();
    let r = replies(&out, "1")[0];
    assert_eq!(status(r), ["done", "error"]);
    assert_eq!(
        r.get_str("ex-message"),
        Some("pod.test.math/boom panicked: kaboom")
    );
    assert_eq!(replies(&out, "2")[0].get_str("value"), Some("4"));
}

#[test]
fn stream_sends_output_values_then_done() {
    let (result, out) = run(example_pod(), &[invoke("9", "pod.test.math/count", "[3]")]);
    result.unwrap();
    let r = replies(&out, "9");
    assert_eq!(r.len(), 5);
    assert_eq!(r[0].get_str("out"), Some("counting\n"));
    let values: Vec<_> = r[1..4]
        .iter()
        .map(|m| m.get_str("value").unwrap())
        .collect();
    assert_eq!(values, ["0", "1", "2"]);
    assert!(r[1..4].iter().all(|m| status(m).is_empty()));
    assert!(r[4].get("value").is_none());
    assert_eq!(status(r[4]), ["done"]);
}

#[test]
fn load_ns_returns_deferred_vars() {
    let (result, out) = run(
        example_pod(),
        &[
            msg(&[("op", "load-ns"), ("id", "5"), ("ns", "pod.test.lazy")]),
            msg(&[("op", "load-ns"), ("id", "6"), ("ns", "pod.test.missing")]),
            invoke("7", "pod.test.lazy/hello", "[]"),
        ],
    );
    result.unwrap();
    let r = replies(&out, "5")[0];
    assert_eq!(r.get_str("name"), Some("pod.test.lazy"));
    let Some(Value::List(vars)) = r.get("vars") else {
        panic!("no vars")
    };
    assert_eq!(vars[0].get_str("name"), Some("hello"));
    assert_eq!(status(replies(&out, "6")[0]), ["done", "error"]);
    assert_eq!(replies(&out, "7")[0].get_str("value"), Some("\"hi\""));
}

#[test]
fn shutdown_stops_reading_and_finishes_in_flight_calls() {
    let slow = Pod::new().namespace(Namespace::new("pod.test.slow").function(
        "slow",
        |(ms,): (u64,)| {
            std::thread::sleep(Duration::from_millis(ms));
            Ok::<_, PodError>(ms)
        },
    ));
    let (result, out) = run(
        slow,
        &[
            invoke("1", "pod.test.slow/slow", "[50]"),
            msg(&[("op", "shutdown"), ("id", "2")]),
            invoke("3", "pod.test.slow/slow", "[0]"),
        ],
    );
    result.unwrap();
    assert_eq!(replies(&out, "1")[0].get_str("value"), Some("50"));
    assert!(replies(&out, "3").is_empty());
}

#[test]
fn end_of_input_cancels_long_running_streams() {
    let pod = Pod::new()
        .shutdown_grace(Duration::from_secs(10))
        .namespace(
            Namespace::new("pod.test.watch").stream("watch", |_: NoArgs, tx: Emitter| {
                while !tx.wait_cancelled(Duration::from_millis(5)) {
                    tx.send("tick")?;
                }
                Ok(())
            }),
        );
    let start = Instant::now();
    let (result, out) = run(pod, &[invoke("1", "pod.test.watch/watch", "[]")]);
    result.unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    let last = *replies(&out, "1").last().unwrap();
    assert_eq!(status(last), ["done"]);
}

#[test]
fn grace_period_bounds_shutdown() {
    let pod = Pod::new()
        .shutdown_grace(Duration::from_millis(100))
        .namespace(
            Namespace::new("pod.test.stuck").function("stuck", |_: NoArgs| {
                std::thread::sleep(Duration::from_secs(30));
                Ok::<_, PodError>(())
            }),
        );
    let start = Instant::now();
    let (result, _) = run(pod, &[invoke("1", "pod.test.stuck/stuck", "[]")]);
    result.unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn broken_pipe_is_a_clean_exit() {
    let result = example_pod().run_with(
        input(&[
            msg(&[("op", "describe")]),
            invoke("1", "pod.test.math/add", "[1,2]"),
        ]),
        BrokenPipe,
    );
    result.unwrap();
}

#[test]
fn emitter_refuses_sends_after_the_call_finishes() {
    let leaked: Arc<Mutex<Option<Emitter>>> = Arc::default();
    let slot = leaked.clone();
    let pod = Pod::new().namespace(Namespace::new("pod.test.leak").stream(
        "leak",
        move |_: NoArgs, tx: Emitter| {
            *slot.lock().unwrap() = Some(tx);
            Ok(())
        },
    ));
    let (result, out) = run(pod, &[invoke("1", "pod.test.leak/leak", "[]")]);
    result.unwrap();
    assert_eq!(replies(&out, "1").len(), 1);
    let tx = leaked.lock().unwrap().take().unwrap();
    assert!(tx.send(&1).is_err());
}

#[test]
fn malformed_input_is_a_protocol_error() {
    let pod = example_pod();
    let result = pod.run_with(Cursor::new(b"x".to_vec()), SharedBuf::default());
    assert!(matches!(result, Err(Error::Protocol(_))));
}

#[test]
#[should_panic(expected = "registered twice")]
fn duplicate_vars_panic() {
    let _ = Namespace::new("pod.test.dup")
        .function("a", |_: NoArgs| Ok::<_, PodError>(1))
        .code("a", "(defn a [] 2)");
}

fn described_vars(ns: Namespace) -> Vec<Value> {
    let (result, out) = run(
        Pod::new().namespace(ns),
        &[msg(&[("op", "describe"), ("id", "0")])],
    );
    result.unwrap();
    let Some(Value::List(nss)) = out[0].get("namespaces") else {
        panic!("no namespaces")
    };
    let Some(Value::List(vars)) = nss[0].get("vars") else {
        panic!("no vars")
    };
    vars.clone()
}

#[test]
fn doc_and_arglists_become_escaped_edn_meta() {
    let vars = described_vars(
        Namespace::new("pod.test.meta")
            .function("f", |(_x,): (i64,)| Ok::<_, PodError>(1))
            .doc("f", r#"Say "hi" \ bye"#)
            .arglists("f", ["[x]", "[x & more]"]),
    );
    assert_eq!(
        vars[0].get_str("meta"),
        Some(r#"{:doc "Say \"hi\" \\ bye" :arglists ([x] [x & more])}"#)
    );
}

#[test]
fn stream_fn_registers_raw_var_and_generated_wrapper() {
    let vars = described_vars(
        Namespace::new("pod.test.s")
            .stream_fn("nums", |(n,): (u64,), tx: Emitter| {
                for i in 0..n {
                    tx.send(&i)?;
                }
                Ok(())
            })
            .doc("nums", "Streams \"n\" numbers.")
            .arglists("nums", ["[n f]"]),
    );
    let names: Vec<_> = vars.iter().filter_map(|v| v.get_str("name")).collect();
    assert_eq!(names, ["nums*", "nums"]);

    // The raw var is a plain handler: no code, no meta.
    assert!(vars[0].get("code").is_none());
    assert!(vars[0].get("meta").is_none());

    // The wrapper carries doc and arglists in its code, not in meta, because
    // the client ignores metadata on code vars.
    assert!(vars[1].get("meta").is_none());
    let code = vars[1].get_str("code").unwrap();
    assert!(
        code.starts_with("(defn nums\n  \"Streams \\\"n\\\" numbers.\"\n  {:arglists '([n f])}")
    );
    assert!(
        code.contains(
            r#"(babashka.pods/invoke "pod.test.s" 'pod.test.s/nums* (vec (butlast args))"#
        )
    );
    assert!(code.contains("(throw (ex-info ex-message ex-data))"));
}

#[test]
fn stream_fn_raw_var_streams_values() {
    let pod = Pod::new().namespace(Namespace::new("pod.test.s").stream_fn(
        "nums",
        |(n,): (u64,), tx: Emitter| {
            for i in 0..n {
                tx.send(&i)?;
            }
            Ok(())
        },
    ));
    let (result, out) = run(pod, &[invoke("1", "pod.test.s/nums*", "[3]")]);
    result.unwrap();
    let values: Vec<_> = replies(&out, "1")
        .iter()
        .filter_map(|m| m.get_str("value"))
        .collect();
    assert_eq!(values, ["0", "1", "2"]);
    assert_eq!(status(replies(&out, "1").last().unwrap()), ["done"]);
}

#[test]
#[should_panic(expected = "ignores metadata on code vars")]
fn doc_on_code_var_panics() {
    let _ = Namespace::new("pod.test.p")
        .code("c", "(defn c [] 1)")
        .doc("c", "nope");
}

#[test]
#[should_panic(expected = "already has raw meta")]
fn doc_after_raw_meta_panics() {
    let _ = Namespace::new("pod.test.p")
        .function("f", |_: NoArgs| Ok::<_, PodError>(1))
        .meta("f", "{:doc \"x\"}")
        .doc("f", "y");
}

#[test]
#[should_panic(expected = "already has doc or arglists")]
fn raw_meta_after_doc_panics() {
    let _ = Namespace::new("pod.test.p")
        .function("f", |_: NoArgs| Ok::<_, PodError>(1))
        .arglists("f", ["[]"])
        .meta("f", "{:doc \"x\"}");
}

#[test]
#[should_panic(expected = "doc for unknown var")]
fn doc_for_unknown_var_panics() {
    let _ = Namespace::new("pod.test.p").doc("missing", "x");
}
