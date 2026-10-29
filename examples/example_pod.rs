//! An example pod. Build it with `cargo build --example example_pod`, then run
//! `bb examples/example.bb` from the repository root.

use std::time::Duration;

use babashka_pod::serde_json::json;
use babashka_pod::{Emitter, Handle, Handles, Namespace, NoArgs, Pod, PodError};

const NS: &str = "pod.joshrotenberg.example";

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("this is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }

    // State kept between calls: babashka holds handles, the pod holds values.
    let counters: Handles<i64> = Handles::new("counter");
    let (c1, c2, c3) = (counters.clone(), counters.clone(), counters);

    let ns = Namespace::new(NS)
        .function("add", |nums: Vec<f64>| {
            // Folds from 0.0: f64's Sum starts at -0.0, so (add) would return -0.0.
            Ok::<_, PodError>(nums.iter().fold(0.0, |a, b| a + b))
        })
        .doc("add", "Adds any number of numbers.")
        .arglists("add", ["[& nums]"])
        .function("ping", |_: NoArgs| Ok::<_, PodError>("pong"))
        .doc("ping", "Returns \"pong\". Takes no arguments.")
        .arglists("ping", ["[]"])
        .function("greet", |(name,): (String,)| {
            Ok::<_, PodError>(json!({ "greeting": format!("hello, {name}"), "length": name.len() }))
        })
        .doc("greet", "Greets someone.")
        .arglists("greet", ["[name]"])
        .function("divide", |(a, b): (i64, i64)| {
            if b == 0 {
                return Err(PodError::new("divide by zero").with_data(json!({ "a": a, "b": b })));
            }
            Ok(a / b)
        })
        // Registers count-up* and a generated count-up wrapper on the client.
        .stream_fn("count-up", |(n,): (u64,), tx: Emitter| {
            tx.err(&format!("counting to {n}\n"))?;
            for i in 0..n {
                tx.send(&i)?;
                if tx.wait_cancelled(Duration::from_millis(20)) {
                    break;
                }
            }
            Ok(())
        })
        .doc(
            "count-up",
            "Streams the integers from 0 below n from the pod, calling f with each.",
        )
        .arglists("count-up", ["[n f]"])
        .function("counter", move |(start,): (i64,)| {
            Ok::<_, PodError>(c1.insert(start))
        })
        .doc(
            "counter",
            "Creates a counter in the pod and returns its handle.",
        )
        .function("inc!", move |(h,): (Handle,)| {
            c2.with(&h, |n| {
                *n += 1;
                Ok(*n)
            })
        })
        .doc("inc!", "Increments a counter and returns the new value.")
        .function("close!", move |(h,): (Handle,)| c3.remove(&h))
        .doc("close!", "Removes a counter.");

    if let Err(e) = Pod::new().namespace(ns).run() {
        eprintln!("example_pod: {e}");
        std::process::exit(1);
    }
}
