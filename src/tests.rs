//! Runtime tests that drive a pod over in-memory streams.

use std::collections::HashMap;
use std::io::{self, Cursor, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::bencode::{Value, read_value};
use crate::{CancelToken, Emitter, Error, Namespace, NoArgs, Pod, PodError};

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
    // ex-data :args is the decoded argument vector, not the raw payload.
    assert_eq!(json_of(r, "ex-data")["args"], json!([1]));
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

#[test]
fn transit_format_round_trips_through_the_runtime() {
    use crate::Format;
    use crate::transit::{Keyword, Set};
    let pod = Pod::new().format(Format::TransitJson).namespace(
        Namespace::new("pod.test.t")
            .function("tags", |(tags,): (Vec<Keyword>,)| {
                Ok::<_, PodError>(Set(tags))
            })
            .function("fail", |_: NoArgs| {
                Err::<i64, _>(PodError::new("no").with_data(json!({"kind": Keyword::new("k")})))
            })
            .arg_meta("tags"),
    );
    let (result, out) = run(
        pod,
        &[
            msg(&[("op", "describe"), ("id", "0")]),
            // As babashka writes [[:a :b]].
            invoke("1", "pod.test.t/tags", r#"[["~:a","~:b"]]"#),
            invoke("2", "pod.test.t/fail", "[]"),
            invoke("3", "pod.test.t/tags", "not transit"),
        ],
    );
    result.unwrap();
    let d = &out[0];
    assert_eq!(d.get_str("format"), Some("transit+json"));
    let Some(Value::List(nss)) = d.get("namespaces") else {
        panic!("no namespaces")
    };
    let Some(Value::List(vars)) = nss[0].get("vars") else {
        panic!("no vars")
    };
    assert_eq!(vars[0].get_str("arg-meta"), Some("true"));
    assert!(vars[1].get("arg-meta").is_none());

    assert_eq!(
        replies(&out, "1")[0].get_str("value"),
        Some(r#"["~#set",["~:a","~:b"]]"#)
    );
    assert_eq!(
        replies(&out, "2")[0].get_str("ex-data"),
        Some(r#"["^ ","~:kind","~:k"]"#)
    );
    let r = replies(&out, "3")[0];
    assert_eq!(status(r), ["done", "error"]);
    assert!(r.get_str("ex-message").unwrap().contains("invalid transit"));
}

#[test]
fn null_args_decode_as_no_arguments() {
    use crate::Format;
    for format in [Format::Json, Format::TransitJson] {
        let pod = Pod::new().format(format).namespace(
            Namespace::new("pod.test.z")
                .function("none", |_: NoArgs| Ok::<_, PodError>(1))
                .function("sum", |nums: Vec<i64>| {
                    Ok::<_, PodError>(nums.iter().sum::<i64>())
                })
                .stream("tick", |_: NoArgs, tx: Emitter| tx.send(&7)),
        );
        let (result, out) = run(
            pod,
            &[
                invoke("1", "pod.test.z/none", "null"),
                invoke("2", "pod.test.z/sum", "null"),
                invoke("3", "pod.test.z/tick", "null"),
            ],
        );
        result.unwrap();
        for (id, value) in [("1", "1"), ("2", "0"), ("3", "7")] {
            let r = replies(&out, id);
            assert_eq!(r[0].get_str("value"), Some(value), "{format:?} id {id}");
            assert!(
                !status(r.last().unwrap()).contains(&"error"),
                "{format:?} id {id}"
            );
        }
    }
}

#[test]
fn transit_handlers_get_typed_arguments() {
    use crate::Format;
    use crate::transit::Value as V;
    let ns = || {
        Namespace::new("pod.test.tv")
            .function_transit("echo", |args: Vec<V>| Ok::<_, PodError>(V::Vector(args)))
            .function_transit("count", |args: Vec<V>| Ok::<_, PodError>(args.len()))
            .stream_transit("each", |args: Vec<V>, tx: Emitter| {
                for a in &args {
                    tx.send(a)?;
                }
                Ok(())
            })
    };
    // (echo :a "a" '(1) 1/3), as babashka writes it.
    let args = r#"["~:a","a",["~#list",[1]],["~#ratio",["~n1","~n3"]]]"#;
    let (result, out) = run(
        Pod::new().format(Format::TransitJson).namespace(ns()),
        &[
            invoke("1", "pod.test.tv/echo", args),
            invoke("2", "pod.test.tv/count", "null"),
            invoke("3", "pod.test.tv/each", r#"["~:x","~cy"]"#),
            invoke("4", "pod.test.tv/echo", r#"["~:a""#),
            invoke("5", "pod.test.tv/echo", r#""not a vector""#),
        ],
    );
    result.unwrap();
    assert_eq!(replies(&out, "1")[0].get_str("value"), Some(args));
    assert_eq!(replies(&out, "2")[0].get_str("value"), Some("0"));
    let each: Vec<_> = replies(&out, "3")
        .iter()
        .filter_map(|m| m.get_str("value"))
        .collect();
    assert_eq!(each, [r#""~:x""#, r#""~cy""#]);
    for id in ["4", "5"] {
        let r = replies(&out, id)[0];
        assert_eq!(status(r), ["done", "error"]);
        assert!(
            r.get_str("ex-message")
                .unwrap()
                .starts_with("invalid arguments for pod.test.tv/echo"),
            "{r:?}"
        );
    }

    // stream_fn_transit registers the raw var each*; arg_meta keeps metadata.
    let ns2 = Namespace::new("pod.test.tv2")
        .stream_fn_transit("each", |args: Vec<V>, tx: Emitter| {
            for a in &args {
                tx.send(a)?;
            }
            Ok(())
        })
        .function_transit("meta", |args: Vec<V>| match args.first() {
            Some(V::WithMeta(_, meta)) => Ok::<_, PodError>((**meta).clone()),
            _ => Ok(V::Nil),
        })
        .arg_meta("meta");
    let (result, out) = run(
        Pod::new().format(Format::TransitJson).namespace(ns2),
        &[
            invoke("1", "pod.test.tv2/each*", r#"["~$s"]"#),
            invoke(
                "2",
                "pod.test.tv2/meta",
                r#"[["~#with-meta",[[1],["^ ","~:k",1]]]]"#,
            ),
        ],
    );
    result.unwrap();
    assert_eq!(replies(&out, "1")[0].get_str("value"), Some(r#""~$s""#));
    assert_eq!(
        replies(&out, "2")[0].get_str("value"),
        Some(r#"["^ ","~:k",1]"#)
    );

    // Under JSON the arguments carry no Clojure types.
    let (result, out) = run(
        Pod::new().namespace(ns()),
        &[invoke("1", "pod.test.tv/echo", r#"["a",{"k":1}]"#)],
    );
    result.unwrap();
    assert_eq!(
        replies(&out, "1")[0].get_str("value"),
        Some(r#"["a",{"k":1}]"#)
    );
}

#[test]
fn json_format_writes_transit_types_as_plain_json() {
    use crate::transit::{Inst, Keyword, Set};
    let pod = Pod::new().namespace(Namespace::new("pod.test.j").function("v", |_: NoArgs| {
        Ok::<_, PodError>((Keyword::new("a"), Set(vec![1]), Inst(5)))
    }));
    let (result, out) = run(pod, &[invoke("1", "pod.test.j/v", "[]")]);
    result.unwrap();
    assert_eq!(
        replies(&out, "1")[0].get_str("value"),
        Some(r#"["a",[1],5]"#)
    );
}

#[test]
fn stream_fn_wrapper_passes_arg_meta() {
    let vars = described_vars(
        Namespace::new("pod.test.am")
            .stream_fn("s", |_: NoArgs, _tx: Emitter| Ok(()))
            .arg_meta("s"),
    );
    let code = vars[1].get_str("code").unwrap();
    assert!(code.contains(":arg-meta true"), "{code}");
}

/// A reader fed message by message from the test, so a pod can be driven
/// while it runs. Dropping the sender ends the input.
struct ChannelReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Cursor<Vec<u8>>,
}

impl io::Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = io::Read::read(&mut self.buf, out)?;
            if n > 0 {
                return Ok(n);
            }
            match self.rx.recv() {
                Ok(bytes) => self.buf = Cursor::new(bytes),
                Err(_) => return Ok(0),
            }
        }
    }
}

/// A pod running on its own thread, fed through a channel.
struct LivePod {
    tx: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    out: SharedBuf,
    done: std::sync::mpsc::Receiver<Result<(), Error>>,
}

impl LivePod {
    fn start(pod: Pod) -> Self {
        Self::start_with(pod, SharedBuf::default(), |pod, reader, out| {
            pod.run_with(reader, out)
        })
    }

    fn start_with(
        pod: Pod,
        out: SharedBuf,
        run: impl FnOnce(Pod, ChannelReader, SharedBuf) -> Result<(), Error> + Send + 'static,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done) = std::sync::mpsc::channel();
        let reader = ChannelReader {
            rx,
            buf: Cursor::new(Vec::new()),
        };
        let writer = out.clone();
        std::thread::spawn(move || {
            let _ = done_tx.send(run(pod, reader, writer));
        });
        LivePod {
            tx: Some(tx),
            out,
            done,
        }
    }

    fn send(&self, msg: Value) {
        self.tx.as_ref().unwrap().send(msg.to_bytes()).unwrap();
    }

    fn output(&self) -> Vec<Value> {
        decode_all(&self.out.0.lock().unwrap())
    }

    /// Waits until `pred` holds for the output so far.
    fn wait_for(&self, what: &str, pred: impl Fn(&[Value]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pred(&self.output()) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Ends the input and waits for the pod to return.
    fn finish(mut self) -> (Result<(), Error>, Vec<Value>) {
        drop(self.tx.take());
        let result = self
            .done
            .recv_timeout(Duration::from_secs(5))
            .expect("pod did not return");
        (result, self.output())
    }
}

fn is_done(out: &[Value], id: &str) -> bool {
    replies(out, id).iter().any(|m| status(m).contains(&"done"))
}

/// A pod whose `wait` streams block until cancelled, either polling with
/// `wait_cancelled` or blocked on a channel fed by `on_cancel`, and whose
/// `stop` cancels one of them by name.
fn cancellable_pod(tokens: Arc<Mutex<HashMap<String, CancelToken>>>) -> Pod {
    let (t1, t2, t3) = (tokens.clone(), tokens.clone(), tokens);
    Pod::new()
        .shutdown_grace(Duration::from_secs(10))
        .namespace(
            Namespace::new("pod.test.cancel")
                .stream("wait", move |(name,): (String,), tx: Emitter| {
                    t1.lock().unwrap().insert(name, tx.cancel_token());
                    // No timeout short enough to matter: only cancellation
                    // ends this call within the test.
                    while !tx.wait_cancelled(Duration::from_secs(60)) {}
                    tx.send("cancelled")?;
                    Ok(())
                })
                .stream("recv", move |(name,): (String,), tx: Emitter| {
                    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
                    tx.on_cancel(move || {
                        let _ = stop_tx.send(());
                    });
                    t2.lock().unwrap().insert(name, tx.cancel_token());
                    // Blocks with no timeout; only the hook wakes it.
                    stop_rx.recv().map_err(|e| PodError::new(e.to_string()))?;
                    tx.send("woken")?;
                    Ok(())
                })
                .function("stop", move |(name,): (String,)| {
                    let token = t3
                        .lock()
                        .unwrap()
                        .remove(&name)
                        .ok_or_else(|| PodError::new(format!("no call named {name}")))?;
                    token.cancel();
                    Ok::<_, PodError>(token.is_cancelled())
                }),
        )
}

fn wait_registered(tokens: &Arc<Mutex<HashMap<String, CancelToken>>>, names: &[&str]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !names
        .iter()
        .all(|n| tokens.lock().unwrap().contains_key(*n))
    {
        assert!(Instant::now() < deadline, "calls never registered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn cancel_token_ends_one_call_while_others_keep_running() {
    let tokens = Arc::default();
    let pod = LivePod::start(cancellable_pod(Arc::clone(&tokens)));
    pod.send(invoke("1", "pod.test.cancel/wait", "[\"a\"]"));
    pod.send(invoke("2", "pod.test.cancel/wait", "[\"b\"]"));
    pod.send(invoke("3", "pod.test.cancel/recv", "[\"c\"]"));
    wait_registered(&tokens, &["a", "b", "c"]);

    pod.send(invoke("4", "pod.test.cancel/stop", "[\"a\"]"));
    pod.wait_for("call 1 to finish", |out| is_done(out, "1"));
    pod.send(invoke("5", "pod.test.cancel/stop", "[\"c\"]"));
    pod.wait_for("call 3 to finish", |out| is_done(out, "3"));

    let out = pod.output();
    assert_eq!(replies(&out, "4")[0].get_str("value"), Some("true"));
    assert_eq!(
        replies(&out, "1")[0].get_str("value"),
        Some("\"cancelled\"")
    );
    assert_eq!(replies(&out, "3")[0].get_str("value"), Some("\"woken\""));
    assert!(!is_done(&out, "2"), "call 2 ended with call 1");

    // End of input still cancels the remaining call.
    let start = Instant::now();
    let (result, out) = pod.finish();
    result.unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(
        replies(&out, "2")[0].get_str("value"),
        Some("\"cancelled\"")
    );
    assert!(is_done(&out, "2"));
}

#[test]
fn shutdown_runs_on_cancel_hooks_for_blocked_handlers() {
    let tokens = Arc::default();
    let pod = LivePod::start(cancellable_pod(Arc::clone(&tokens)));
    pod.send(invoke("1", "pod.test.cancel/recv", "[\"a\"]"));
    pod.send(invoke("2", "pod.test.cancel/recv", "[\"b\"]"));
    wait_registered(&tokens, &["a", "b"]);
    pod.send(msg(&[("op", "shutdown"), ("id", "3")]));
    let (result, out) = pod.finish();
    result.unwrap();
    for id in ["1", "2"] {
        assert_eq!(replies(&out, id)[0].get_str("value"), Some("\"woken\""));
        assert!(is_done(&out, id));
    }
}

#[test]
fn on_cancel_after_cancellation_runs_immediately() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = ran.clone();
    let pod = Pod::new().namespace(Namespace::new("pod.test.late").stream(
        "late",
        move |_: NoArgs, tx: Emitter| {
            tx.cancel_token().cancel();
            assert!(tx.is_cancelled());
            assert!(tx.wait_cancelled(Duration::from_secs(60)));
            let flag = flag.clone();
            tx.on_cancel(move || flag.store(true, Ordering::SeqCst));
            Ok(())
        },
    ));
    let (result, out) = run(pod, &[invoke("1", "pod.test.late/late", "[]")]);
    result.unwrap();
    assert!(is_done(&out, "1"));
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn on_cancel_hooks_do_not_run_for_calls_that_finish() {
    let ran = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let (flag, drop_flag) = (ran.clone(), dropped.clone());
    let token_slot: Arc<Mutex<Option<CancelToken>>> = Arc::default();
    let slot = token_slot.clone();

    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let pod = Pod::new().namespace(Namespace::new("pod.test.quick").stream(
        "quick",
        move |_: NoArgs, tx: Emitter| {
            let (flag, guard) = (flag.clone(), SetOnDrop(drop_flag.clone()));
            tx.on_cancel(move || {
                let _guard = guard;
                flag.store(true, Ordering::SeqCst);
            });
            *slot.lock().unwrap() = Some(tx.cancel_token());
            Ok(())
        },
    ));
    let live = LivePod::start(pod);
    live.send(invoke("1", "pod.test.quick/quick", "[]"));
    live.wait_for("call 1 to finish", |out| is_done(out, "1"));
    // Cancelling the leftover token after the call finished does nothing,
    // and neither does the pod-wide cancellation at end of input.
    token_slot.lock().unwrap().take().unwrap().cancel();
    let (result, _) = live.finish();
    result.unwrap();
    assert!(!ran.load(Ordering::SeqCst));
    assert!(dropped.load(Ordering::SeqCst), "hook was not dropped");
}

/// Accepts writes until `fail` is set, then fails them as a broken pipe.
#[derive(Clone, Default)]
struct FailSwitch {
    buf: SharedBuf,
    fail: Arc<AtomicBool>,
}

impl Write for FailSwitch {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.buf.write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn hook_that_sends_during_a_failed_write_does_not_deadlock() {
    let send_result: Arc<Mutex<Option<bool>>> = Arc::default();
    let registered = Arc::new(AtomicBool::new(false));
    let (result_slot, reg) = (send_result.clone(), registered.clone());
    let pod = Pod::new().namespace(Namespace::new("pod.test.hook").stream(
        "hook",
        move |_: NoArgs, tx: Emitter| {
            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            let (sender, slot) = (tx.clone(), result_slot.clone());
            tx.on_cancel(move || {
                // Sends from inside the hook: the writer lock must be free.
                *slot.lock().unwrap() = Some(sender.send("bye").is_ok());
                let _ = stop_tx.send(());
            });
            reg.store(true, Ordering::SeqCst);
            let _ = stop_rx.recv();
            Ok(())
        },
    ));

    let writer = FailSwitch::default();
    let fail = writer.fail.clone();
    let live = LivePod::start_with(pod, writer.buf.clone(), move |pod, reader, _| {
        pod.run_with(reader, writer)
    });
    live.send(invoke("1", "pod.test.hook/hook", "[]"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !registered.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "hook never registered");
        std::thread::sleep(Duration::from_millis(5));
    }
    fail.store(true, Ordering::SeqCst);
    // This reply fails to write, which cancels the in-flight call.
    live.send(msg(&[("op", "describe")]));
    let (result, _) = live.finish();
    result.unwrap();
    assert_eq!(*send_result.lock().unwrap(), Some(false));
}
