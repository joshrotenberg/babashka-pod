//! A pod using the transit+json format. Build it with
//! `cargo build --example transit_pod`, then run `bb examples/transit.bb`
//! from the repository root.

use babashka_pod::serde_json::{Value, json};
use babashka_pod::transit::{self, Inst, Keyword, Set, Tagged, WithMeta};
use babashka_pod::{Emitter, Format, Namespace, NoArgs, Pod, PodError};

const NS: &str = "pod.joshrotenberg.transit";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("this is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }

    let ns = Namespace::new(NS)
        // Keywords arrive as their names; Set<Keyword> goes back as #{:a :b}.
        .function("tag-set", |(tags,): (Vec<Keyword>,)| {
            let mut tags = tags;
            tags.sort();
            tags.dedup();
            Ok::<_, PodError>(Set(tags))
        })
        .doc("tag-set", "Returns the distinct keywords as a set.")
        .arglists("tag-set", ["[keywords]"])
        .function("kw-info", |(kw,): (Keyword,)| {
            let (ns, name) = match kw.0.split_once('/') {
                Some((ns, name)) => (Some(ns.to_string()), name.to_string()),
                None => (None, kw.0.clone()),
            };
            Ok::<_, PodError>(json!({ "ns": ns, "name": name, "kw": Keyword::new(&kw.0) }))
        })
        .doc("kw-info", "Splits a keyword into namespace and name.")
        .arglists("kw-info", ["[kw]"])
        .function("plus-days", |(at, days): (Inst, i64)| {
            Ok::<_, PodError>(Inst(at.0 + days * DAY_MS))
        })
        .doc("plus-days", "Adds days to an inst.")
        .arglists("plus-days", ["[inst days]"])
        // A custom tag: babashka turns it into a value with the read handler
        // registered below.
        .function("point", |(x, y): (i64, i64)| {
            Ok::<_, PodError>(Tagged {
                tag: "point".into(),
                rep: json!([x, y]),
            })
        })
        .doc("point", "Returns a point as a custom transit tag.")
        .arglists("point", ["[x y]"])
        .code(
            "-point-reader",
            r#"(babashka.pods/add-transit-read-handler! "point" (fn [[x y]] {:x x :y y}))"#,
        )
        // Metadata on arguments: the client sends it because of arg_meta.
        .function("meta-of", |(v,): (WithMeta<Value>,)| {
            Ok::<_, PodError>(v.meta.unwrap_or(Value::Null))
        })
        .arg_meta("meta-of")
        .doc("meta-of", "Returns the metadata of its argument, or nil.")
        .arglists("meta-of", ["[x]"])
        .function("ping", |_: NoArgs| Ok::<_, PodError>(Keyword::new("pong")))
        .doc("ping", "Returns :pong. Takes no arguments.")
        .arglists("ping", ["[]"])
        // Arguments as transit::Value keep their Clojure types, so the value
        // goes back exactly as it came.
        .function_transit("echo", |args: Vec<transit::Value>| {
            Ok::<_, PodError>(args.into_iter().next().unwrap_or(transit::Value::Nil))
        })
        .doc(
            "echo",
            "Returns its argument unchanged, with its Clojure type.",
        )
        .arglists("echo", ["[x]"])
        .function("fail", |(kind,): (Keyword,)| {
            Err::<Value, _>(PodError::new("failed on purpose").with_data(json!({ "kind": kind })))
        })
        .doc("fail", "Throws ex-info whose ex-data holds a keyword.")
        .arglists("fail", ["[kind]"])
        .stream_fn("ticks", |(n,): (u64,), tx: Emitter| {
            for i in 0..n {
                tx.send(&Keyword::new(format!("tick/{i}")))?;
            }
            Ok(())
        })
        .doc("ticks", "Calls f with :tick/0, :tick/1, ... up to n.")
        .arglists("ticks", ["[n f]"]);

    if let Err(e) = Pod::new().format(Format::TransitJson).namespace(ns).run() {
        eprintln!("transit_pod: {e}");
        std::process::exit(1);
    }
}
