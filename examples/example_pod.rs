//! An example pod. Build it with `cargo build --example example_pod`, then run
//! `bb examples/example.bb` from the repository root.

use std::time::Duration;

use babashka_pod::serde_json::json;
use babashka_pod::{Emitter, Namespace, Pod, PodError};

const NS: &str = "pod.joshrotenberg.example";

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("this is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }

    let ns = Namespace::new(NS)
        .function("add", |nums: Vec<f64>| {
            Ok::<_, PodError>(nums.iter().sum::<f64>())
        })
        .doc("add", "Adds any number of numbers.")
        .arglists("add", ["[& nums]"])
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
        .arglists("count-up", ["[n f]"]);

    if let Err(e) = Pod::new().namespace(ns).run() {
        eprintln!("example_pod: {e}");
        std::process::exit(1);
    }
}
