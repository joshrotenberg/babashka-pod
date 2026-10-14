//! An example pod. Build it with `cargo build --example example_pod`, then run
//! `bb examples/example.bb` from the repository root.

use std::time::Duration;

use babashka_pod::serde_json::json;
use babashka_pod::{Emitter, Namespace, Pod, PodError};

const NS: &str = "pod.joshrotenberg.example";

/// Client-side wrapper around `count-up*`: calls `f` with each value as it
/// arrives and returns once the pod reports the stream is done.
const COUNT_UP: &str = r#"
(defn count-up
  "Streams the integers from 0 below n from the pod, calling f with each."
  [n f]
  (let [result (promise)]
    (babashka.pods/invoke "pod.joshrotenberg.example"
                          'pod.joshrotenberg.example/count-up*
                          [n]
                          {:handlers {:success f
                                      :error (fn [e] (deliver result e))
                                      :done (fn [] (deliver result nil))}})
    (when-let [{:keys [ex-message ex-data]} @result]
      (throw (ex-info ex-message ex-data)))
    nil))
"#;

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("this is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }

    let ns = Namespace::new(NS)
        .function("add", |nums: Vec<f64>| {
            Ok::<_, PodError>(nums.iter().sum::<f64>())
        })
        .meta(
            "add",
            r#"{:doc "Adds any number of numbers." :arglists ([& nums])}"#,
        )
        .function("greet", |(name,): (String,)| {
            Ok::<_, PodError>(json!({ "greeting": format!("hello, {name}"), "length": name.len() }))
        })
        .meta("greet", r#"{:doc "Greets someone." :arglists ([name])}"#)
        .function("divide", |(a, b): (i64, i64)| {
            if b == 0 {
                return Err(PodError::new("divide by zero").with_data(json!({ "a": a, "b": b })));
            }
            Ok(a / b)
        })
        .stream("count-up*", |(n,): (u64,), tx: Emitter| {
            tx.err(&format!("counting to {n}\n"))?;
            for i in 0..n {
                tx.send(&i)?;
                if tx.wait_cancelled(Duration::from_millis(20)) {
                    break;
                }
            }
            Ok(())
        })
        .code("count-up", COUNT_UP);

    if let Err(e) = Pod::new().namespace(ns).run() {
        eprintln!("example_pod: {e}");
        std::process::exit(1);
    }
}
