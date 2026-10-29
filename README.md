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

## State between calls

`Handles<T>` keeps values in the pod (connections, prepared statements, compiled queries) and
gives babashka a `Handle`, which it sees as `{:handle "connection", :id 1}` and passes back as an
argument. Clone the registry into each handler that needs it.

```rust
let conns: Handles<Connection> = Handles::new("connection");
let (open, query) = (conns.clone(), conns.clone());
Namespace::new("pod.example.db")
    .function("open", move |(path,): (String,)| Ok::<_, PodError>(open.insert(connect(&path)?)))
    .function("query", move |(h, sql): (Handle, String)| query.with(&h, |c| run(c, &sql)));
```

A pod that has to match an existing client API with another shape, such as `{:watcher/id 1}`, can
return its own value built from `h.id` and rebuild the handle with `Handles::handle(id)`, which
uses the registry's kind. The `Handles` docs have an example.

`with` locks only the value it runs on, so calls on different handles run concurrently. A handle of
another kind, or one already removed, produces an error reply. Values are dropped on `remove`, or
when `Pod::run` returns and the handlers holding the registry are dropped.

`remove_with(&h, |v| ...)` removes the entry and runs a closure on the value in one step, for
releasing a value while taking something out of it (a stop sender, final statistics). No call can
reach the handle after it returns; a call already using the value finishes first.

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
2. Cancel in-flight calls (see [Cancellation](#cancellation)).
3. Wait up to `Pod::shutdown_grace` (2 seconds by default) for in-flight calls, then return from
   `Pod::run`.

The runtime does not panic on I/O errors.

## Cancellation

A call is cancelled when the pod shuts down, or when its `CancelToken` is cancelled. Cancellation
only signals; the call ends when its handler returns.

| API | Use |
|---|---|
| `Emitter::is_cancelled()` | Check between units of work |
| `Emitter::wait_cancelled(timeout)` | Block until cancelled, in place of `sleep` |
| `Emitter::on_cancel(f)` | Run `f` once on cancellation, so a handler blocked on its own channel can be woken by forwarding into that channel. Runs immediately if the call is already cancelled; dropped unrun if the call finishes first |
| `Emitter::cancel_token()` | A cloneable `CancelToken` that cancels this call alone. Store it (for example in a `Handles` registry) so another call such as `stop` or `unwatch` can end it |

```rust
.stream("watch", move |(path,): (String,), tx: Emitter| {
    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let stop = events_tx.clone();
    tx.on_cancel(move || { let _ = stop.send(Event::Stop); });
    watches.insert(tx.cancel_token()); // `unwatch` cancels it
    start_watching(&path, events_tx)?;
    for event in events_rx {
        match event {
            Event::Stop => return Ok(()),
            Event::Change(change) => tx.send(&change)?,
        }
    }
    Ok(())
})
```

`on_cancel` hooks run on the thread that cancels: the caller of `CancelToken::cancel`, or a
runtime thread during shutdown. Keep them short and non-blocking.

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

- Async (tokio) handlers.
- The EDN payload format, and `readers`, which the pod protocol defines for EDN only. This is a
  decision, not a gap; see below.

### Why not EDN

The payload format is the pod's choice and script authors never see it, so EDN would only be
worth adding for something transit+json cannot do. Transit already covers it:

- Clojure types going back to babashka: keywords, symbols, sets, insts, uuids, tagged values.
- Custom tags: return a `transit::Tagged` and register a read handler from the pod's `code`, the
  transit equivalent of EDN `readers` (see `examples/transit_pod.rs`).
- Metadata and `arg-meta`, which the protocol supports for transit only.

EDN would not keep more types on the way in either: arguments decode into `serde_json::Value` in
any format, which loses keywords, chars, ratios, big numbers and lists. That is tracked in
[#22](https://github.com/joshrotenberg/babashka-pod/issues/22).

It would cost a third format to test across every example and transport, plus a reader. The most
likely crate, `clojure-reader`, does not read `##Inf`/`##NaN` or `\uXXXX` characters, and reads `^`
metadata and trailing input after the first value wrongly without an error.

We would revisit it for a pod client other than `babashka.pods` that speaks EDN but not transit,
or if readable payloads became necessary and a debug log of decoded traffic would not do.

## Pods in this repository

This repository holds the library and the pods built on it. The library is the crate published
to crates.io; its package does not include `pods/`. Each pod is a workspace member under
`pods/<name>` with its own version, tag and release.

| Pod | Namespace | Path |
|---|---|---|
| File watching, on notify; a drop-in for `org.babashka/fswatcher` | `pod.babashka.fswatcher` | [`pods/fswatcher`](pods/fswatcher/README.md) |
| SQLite, on rusqlite with bundled SQLite | `pod.joshrotenberg.sqlite` | [`pods/sqlite`](pods/sqlite/README.md) |
| Parsing source code with tree-sitter: syntax trees, queries, outlines of definitions | `pod.joshrotenberg.treesitter` | [`pods/treesitter`](pods/treesitter/README.md) |

### Layout

| | Rule |
|---|---|
| Directory | `pods/<name>`. The workspace includes `pods/*`, so a new pod needs no change to the root `Cargo.toml` |
| Crate | `pod-<name>`, with `publish = false` and the library as a path dependency |
| Library | `pod_<name>`: `NAMESPACE`, the namespace name, and `namespace()`, which builds the `Namespace` with fresh state |
| Binary | `pod-<name>`: `src/main.rs` serves `namespace()` with `Format::TransitJson` and does nothing else |
| Format | `Format::TransitJson` for every pod, so any set of them can be served by one `Pod` |
| Tests | `test.bb` loads the binary named by `<NAME>_POD` (for example `SQLITE_POD`) and prints one line per check; `tests/bb.rs` runs it over stdio and socket and asserts on the lines |
| Registry entry | `[package.metadata.pod-registry]` in the pod's `Cargo.toml`: `name` (`:pod/name`) and `description` (`:pod/description`) |

Because each pod is a library, a pod binary can serve several of them:

```rust
use babashka_pod::{Format, Pod};

fn main() -> Result<(), babashka_pod::Error> {
    Pod::new()
        .format(Format::TransitJson)
        .namespace(pod_sqlite::namespace())
        .namespace(pod_fswatcher::namespace())
        .run()
}
```

### Versions, tags and releases

- A pod's version is the `version` in `pods/<name>/Cargo.toml`. It changes only with that pod and
  is independent of the library's version and of other pods.
- A pod is released by pushing the tag `pod-<name>-v<version>`, such as `pod-sqlite-v0.2.0`. Tags
  for the library do not start with `pod-`.
- `.github/workflows/release-pod.yml` runs on `pod-*-v*` tags. `bb release-plan <tag>` checks that
  the tag names an existing pod and matches its `Cargo.toml` version, and gives the build the pod's
  name. The workflow builds that pod for macOS `aarch64` and `x86_64`, Linux `x86_64` and
  `aarch64`, and Windows `x86_64`, runs its `test.bb` where the runner can execute the binary,
  packages each build, generates `manifest.edn`, and creates a GitHub release with all of it.
  Nothing else in the workspace is built or released.

To release a pod: bump `version` in `pods/<name>/Cargo.toml`, merge, then tag the merge commit
and push the tag:

```bash
git tag pod-sqlite-v0.2.0 && git push origin pod-sqlite-v0.2.0
```

Registry manifests point at public URLs, so submitting a manifest to the pod registry needs the
release assets to be public.

### Adding a pod

1. Create `pods/<name>` with a `Cargo.toml` for `pod-<name>` (`publish = false`, `babashka-pod = {
   path = "../.." }`, and a `[package.metadata.pod-registry]` table), a `src/lib.rs` with
   `NAMESPACE` and `namespace()`, and a `src/main.rs` like `pods/sqlite/src/main.rs`.
2. Add `test.bb` and `tests/bb.rs` following `pods/sqlite`.
3. Add a `README.md` with the client API, and a row to the table above.
4. Check: `cargo test -p pod-<name>` and `bb release-plan pod-<name>-v<version>`.

### Release tooling

`bb.edn` at the repository root has the release tasks (code in `release/`); each takes `--help`.

| Task | Does |
|---|---|
| `bb release-plan <tag>` | Checks a `pod-<name>-v<version>` tag against `pods/<name>/Cargo.toml` and prints `pod=`, `crate=` and `version=` lines (the workflow appends them to `$GITHUB_OUTPUT`) |
| `bb check-version <tag>` | The same check, printing a message |
| `bb arch <file>` | Reads a binary's Mach-O, ELF, or PE header and prints the OS and CPU it was built for |
| `bb package --pod <name> --target <triple>` | Zips `target/<triple>/release/pod-<name>[.exe]` as `dist/pod-<name>-<version>-<os>-<arch>.zip` with a `.sha256`, after checking the header matches the target |
| `bb manifest --pod <name> --base-url <url>` | Writes `dist/manifest.edn` in the pod-registry format from that pod's zips in `dist/`, opening each one and checking the binary's header against its `:os/name` and `:os/arch`; refuses on a mismatch |
| `bb release-test` | Tests for the above (CI runs it) |

The registry matches `:os/arch` against babashka's `os.arch`: `aarch64` or `x86_64` on macOS,
`amd64` or `aarch64` on Linux and Windows. Nothing in the registry checks the binary itself, which
is how a manifest can list an x86_64 binary as `aarch64` (fswatcher 0.0.5) or leave out a build
the release has (go-sqlite3 0.3.13 has no macOS `aarch64` entry). `bb manifest` builds the
manifest from the binaries, so neither can happen.

## License

MIT OR Apache-2.0
