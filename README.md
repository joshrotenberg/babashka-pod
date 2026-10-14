# babashka-pod

A Rust library for writing [babashka pods](https://github.com/babashka/pods).

The crate implements the pod side of the protocol: `describe`, `invoke`, `load-ns` and
`shutdown`, over stdin/stdout or a localhost socket, with JSON or transit+json payloads. You register namespaces
and typed Rust functions; the runtime handles bencode framing, concurrency, error replies and
shutdown.

```rust
use babashka_pod::{Emitter, Namespace, Pod, PodError};

fn main() -> Result<(), babashka_pod::Error> {
    Pod::new()
        .namespace(
            Namespace::new("pod.example.math")
                .function("add", |(a, b): (i64, i64)| Ok::<_, PodError>(a + b))
                .doc("add", "Adds two integers")
                .arglists("add", ["[a b]"])
                .stream_fn("count-up", |(n,): (u64,), tx: Emitter| {
                    for i in 0..n {
                        tx.send(&i)?;
                    }
                    Ok(())
                }),
        )
        .run()
}
```

```clojure
(require '[babashka.pods :as pods])
(pods/load-pod ["./my-pod"])
(require '[pod.example.math :as math])
(math/add 1 2) ;; => 3
(math/count-up 3 println) ;; prints 0, 1, 2 and returns nil
```

## Registering vars

| Method | Client sees | Notes |
|---|---|---|
| `function(name, Fn(A) -> Result<R, PodError>)` | a function returning one value | `A` is deserialized from the argument vector: `(T,)` for one argument, `(T, U)` for two, `NoArgs` for none, `Vec<T>` for variadic. `R` is serialized as JSON. |
| `stream_fn(name, Fn(A, Emitter) -> Result<(), PodError>)` | `(name arg ... callback)`: calls `callback` with each value, blocks until done, returns `nil`, rethrows pod errors as `ex-info` | Registers the raw var `name*` plus a `name` var whose client code the crate generates. `Emitter` can be cloned and moved to other threads. |
| `stream(name, Fn(A, Emitter) -> Result<(), PodError>)` | values delivered to `:success` handlers | The raw form: pair it with client `code` that calls `babashka.pods/invoke` with `:handlers`. |
| `code(name, source)` | a var defined by Clojure code evaluated in babashka | For idiomatic wrappers around raw pod functions. |
| `doc(name, text)`, `arglists(name, ["[a b]", ...])` | `:doc` and `:arglists` metadata | Escaped by the crate. On a `stream_fn` they go into the generated `defn`. |
| `meta(name, edn)` | any var metadata, as a raw EDN map | Does not mix with `doc`/`arglists` on the same var (EDN rejects duplicate keys); registering both panics. |
| `Namespace::deferred()` | namespace loaded on first `require` | Served through the `load-ns` op. |

Map keys arrive in babashka as keywords in both formats.

## Payload formats

| | `Format::Json` (default) | `Format::TransitJson` |
|---|---|---|
| Choose with | nothing | `Pod::new().format(Format::TransitJson)` |
| Keywords, symbols from bb | strings | strings (`:a` becomes `"a"`, `:ns/a` becomes `"ns/a"`) |
| Sets, lists from bb | arrays | arrays |
| `#inst` from bb | string | epoch millis (`transit::Inst`) |
| Values to bb | plain JSON | keywords, symbols, sets, insts, uuids, and tagged values via the `transit` types |
| `arg-meta` | not available | `Namespace::arg_meta(name)`; take the argument as `transit::WithMeta<T>` |

Arguments decode into `serde_json::Value` first, so handlers keep using ordinary serde types in
either format. To send a Clojure type back, return one of the `babashka_pod::transit` types:

| Rust | babashka |
|---|---|
| `Keyword::new("a")`, `Keyword::new("ns/a")` | `:a`, `:ns/a` |
| `Symbol::new("f")` | `f` |
| `Set(vec![...])` | `#{...}` |
| `Inst(millis)` | `#inst "..."` |
| `Uuid(string)` | `#uuid "..."` |
| `Tagged { tag, rep }` | whatever a read handler registered with `babashka.pods/add-transit-read-handler!` makes of it |
| `WithMeta { value, meta }` | `value` with metadata |

Under `Format::Json` these types degrade to plain JSON (a keyword to its name, a set to an array,
an inst to millis). Values handled as an untyped `serde_json::Value` lose keyword-ness on the way
back: a keyword that arrived as `"a"` returns as the string `"a"`.

The transit codec (`src/transit.rs`) depends only on serde and serde_json. It reads babashka's
transit writer output, including the key and tag cache.

`examples/transit_pod.rs` and `examples/transit.bb` cover each type, a custom tag with a read
handler registered from the pod's `code`, `arg-meta`, keyword `ex-data`, and a stream of keywords.

## Errors

A handler that returns `Err(PodError)` produces an error reply, which babashka raises as
`ex-info`. `PodError::with_data` sets `ex-data`, encoded in the pod's format (so transit types
work there too). For argument decoding failures, `ex-data` holds `:var` and the decoded `:args`. Any `std::error::Error` converts into `PodError`,
so `?` works in handlers. Argument decoding failures, unknown vars and handler panics are also
returned as error replies, and the pod keeps serving.

## Shutdown

The pod always declares the `shutdown` op. babashka sends it when the script exits and then waits
for the pod process with no timeout. The runtime treats `shutdown`, end of input and a failed write
(the client went away) the same way:

1. Stop reading requests.
2. Mark in-flight calls cancelled. Long-running handlers should poll `Emitter::is_cancelled` or
   use `Emitter::wait_cancelled` in place of `sleep`.
3. Wait up to `Pod::shutdown_grace` (2 seconds by default) for in-flight calls, then return from
   `Pod::run`.

The runtime does not panic on I/O errors.

## Transport

`Pod::run` uses a localhost socket when babashka sets `BABASHKA_POD_TRANSPORT=socket`
(`load-pod` with `{:transport :socket}`) and stdin/stdout otherwise. In stdin/stdout mode stdout
carries the protocol, so the pod must not print to it; log to stderr or use `Emitter::out`.

`babashka_pod::is_pod_mode()` checks `BABASHKA_POD=true`, so one binary can serve as both a CLI
and a pod.

`Pod::run_with(reader, writer)` serves over any `Read`/`Write` pair, which is how the tests drive
the runtime in memory.

## Example

```bash
cargo build --examples
bb examples/example.bb             # JSON
bb examples/example.bb socket
bb examples/transit.bb             # transit+json
bb examples/transit.bb socket
```

`tests/babashka.rs` runs both scripts against both transports when `bb` is on `PATH`.

## Not yet supported

- The EDN payload format, and `readers`, which the pod protocol defines for EDN only.
- Async (tokio) handlers.

## License

MIT OR Apache-2.0
