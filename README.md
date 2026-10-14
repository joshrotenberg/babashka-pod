# babashka-pod

A Rust library for writing [babashka pods](https://github.com/babashka/pods).

The crate implements the pod side of the protocol: `describe`, `invoke`, `load-ns` and
`shutdown`, over stdin/stdout or a localhost socket, with JSON payloads. You register namespaces
and typed Rust functions; the runtime handles bencode framing, concurrency, error replies and
shutdown.

```rust
use babashka_pod::{Emitter, Namespace, Pod, PodError};

fn main() -> Result<(), babashka_pod::Error> {
    Pod::new()
        .namespace(
            Namespace::new("pod.example.math")
                .function("add", |(a, b): (i64, i64)| Ok::<_, PodError>(a + b))
                .meta("add", r#"{:doc "Adds two integers" :arglists ([a b])}"#)
                .stream("count-up*", |(n,): (u64,), tx: Emitter| {
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
```

## Registering vars

| Method | Client sees | Notes |
|---|---|---|
| `function(name, Fn(A) -> Result<R, PodError>)` | a function returning one value | `A` is deserialized from the argument vector: `(T,)` for one argument, `(T, U)` for two, `NoArgs` for none, `Vec<T>` for variadic. `R` is serialized as JSON. |
| `stream(name, Fn(A, Emitter) -> Result<(), PodError>)` | values delivered to `:success` handlers | Use with client `code` that calls `babashka.pods/invoke` with `:handlers`. `Emitter` can be cloned and moved to other threads. |
| `code(name, source)` | a var defined by Clojure code evaluated in babashka | For idiomatic wrappers around raw pod functions. |
| `meta(name, edn)` | var metadata such as `:doc` and `:arglists` | |
| `Namespace::deferred()` | namespace loaded on first `require` | Served through the `load-ns` op. |

JSON object keys arrive in babashka as keywords.

## Errors

A handler that returns `Err(PodError)` produces an error reply, which babashka raises as
`ex-info`. `PodError::with_data` sets `ex-data`. Any `std::error::Error` converts into `PodError`,
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
cargo build --example example_pod
bb examples/example.bb
bb examples/example.bb socket
```

`tests/babashka.rs` runs the same script against both transports when `bb` is on `PATH`.

## Not yet supported

- EDN and transit+json payload formats.
- Async (tokio) handlers.
- `readers`, `arg-meta` and dynamic metadata, which depend on EDN or transit.

## License

MIT OR Apache-2.0
