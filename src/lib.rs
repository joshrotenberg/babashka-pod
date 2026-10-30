//! Write [babashka pods](https://github.com/babashka/pods) in Rust.
//!
//! A pod is a program that babashka starts with `babashka.pods/load-pod` and
//! talks to over bencode messages. This crate implements the pod side of the
//! protocol: `describe`, `invoke`, `load-ns` and `shutdown` over stdin/stdout
//! or a localhost socket, with JSON or [transit+json](transit) payloads
//! ([`Format`]). You register namespaces and
//! typed Rust functions; the runtime handles framing, concurrency, errors and
//! shutdown.
//!
//! ```no_run
//! use babashka_pod::{Emitter, Namespace, Pod, PodError};
//!
//! fn main() -> Result<(), babashka_pod::Error> {
//!     Pod::new()
//!         .namespace(
//!             Namespace::new("pod.example.math")
//!                 .function("add", |(a, b): (i64, i64)| Ok::<_, PodError>(a + b))
//!                 .doc("add", "Adds two integers")
//!                 .arglists("add", ["[a b]"])
//!                 .stream_fn("count-up", |(n,): (u64,), tx: Emitter| {
//!                     for i in 0..n {
//!                         tx.send(&i)?;
//!                     }
//!                     Ok(())
//!                 }),
//!         )
//!         .run()
//! }
//! ```
//!
//! From babashka:
//!
//! ```clojure
//! (require '[babashka.pods :as pods])
//! (pods/load-pod ["./my-pod"])
//! (require '[pod.example.math :as math])
//! (math/add 1 2) ;; => 3
//! (math/count-up 3 println) ;; prints 0, 1, 2 and returns nil
//! ```
//!
//! # Registering vars
//!
//! | Method | Client sees |
//! |---|---|
//! | [`function`](Namespace::function) | a function returning one value |
//! | [`stream_fn`](Namespace::stream_fn) | `(name arg ... callback)`: calls `callback` with each value, blocks until done, returns `nil`, rethrows pod errors as `ex-info` |
//! | [`stream`](Namespace::stream) | values delivered to `:success` handlers; pair it with client [`code`](Namespace::code) that calls `babashka.pods/invoke` |
//! | [`function_transit`](Namespace::function_transit), [`stream_transit`](Namespace::stream_transit), [`stream_fn_transit`](Namespace::stream_fn_transit) | the same, with arguments that keep their Clojure types (see [Payload formats](#payload-formats)) |
//! | [`code`](Namespace::code) | a var defined by Clojure code evaluated in babashka |
//! | [`doc`](Namespace::doc), [`arglists`](Namespace::arglists), [`meta`](Namespace::meta) | var metadata |
//! | [`deferred`](Namespace::deferred) | a namespace loaded on first `require` (the `load-ns` op) |
//!
//! A handler's argument vector is deserialized into its argument type: `(T,)`
//! for one argument, `(T, U)` for two, [`NoArgs`] for none, `Vec<T>` for any
//! number. The return value is serialized with serde. Map keys reach babashka
//! as keywords in both formats.
//!
//! # Payload formats
//!
//! | | [`Format::Json`] (default) | [`Format::TransitJson`] |
//! |---|---|---|
//! | Choose with | nothing | `Pod::new().format(Format::TransitJson)` |
//! | Keywords, symbols from bb | strings | strings, or [`transit::Value::Keyword`] and [`transit::Value::Symbol`] in `*_transit` handlers |
//! | Sets, lists from bb | arrays | arrays, or [`transit::Value::Set`] and [`transit::Value::List`] in `*_transit` handlers |
//! | `#inst` from bb | string | epoch millis ([`transit::Inst`]) |
//! | Values to bb | plain JSON | keywords, symbols, sets, insts, uuids and tagged values through the [`transit`] types |
//! | `arg-meta` | not available | [`Namespace::arg_meta`]; take the argument as [`transit::WithMeta`] |
//!
//! To send a Clojure type back, return one of the [`transit`] types, such as
//! [`transit::Keyword`] or [`transit::Set`]. Under [`Format::Json`] they are
//! written as plain JSON (a keyword as its name, a set as an array).
//!
//! A handler that needs to tell a keyword from a string, a list from a vector,
//! or `{:a 1}` from `{"a" 1}` registers with one of the `*_transit` methods and
//! gets its arguments as [`transit::Value`]s. Returning a `transit::Value`
//! sends back the same value.
//!
//! ```
//! use babashka_pod::transit::Value;
//! use babashka_pod::{Format, Namespace, Pod, PodError};
//!
//! let ns = Namespace::new("pod.example.data")
//!     // (kind :a) => "keyword", (kind "a") => "string", (kind 1/3) => "ratio"
//!     .function_transit("kind", |args: Vec<Value>| {
//!         Ok::<_, PodError>(match args.first() {
//!             Some(Value::Keyword(_)) => "keyword",
//!             Some(Value::String(_)) => "string",
//!             Some(Value::Ratio { .. }) => "ratio",
//!             _ => "other",
//!         })
//!     });
//! let pod = Pod::new().format(Format::TransitJson).namespace(ns);
//! ```
//!
//! # State between calls
//!
//! [`Handles`] keeps values in the pod (connections, prepared statements,
//! compiled queries) and gives babashka a [`Handle`], which it sees as
//! `{:handle "counter", :id 1}` and passes back. Clone the registry into each
//! handler that needs it.
//!
//! ```
//! use babashka_pod::{Handle, Handles, Namespace, PodError};
//!
//! let counters: Handles<i64> = Handles::new("counter");
//! let (new, inc, close) = (counters.clone(), counters.clone(), counters);
//! let ns = Namespace::new("pod.example.counter")
//!     .function("counter", move |(start,): (i64,)| Ok::<_, PodError>(new.insert(start)))
//!     .function("inc!", move |(h,): (Handle,)| {
//!         inc.with(&h, |n| {
//!             *n += 1;
//!             Ok(*n)
//!         })
//!     })
//!     .function("close!", move |(h,): (Handle,)| close.remove(&h));
//! ```
//!
//! [`Handles::with`] locks only the value it runs on, so calls on different
//! handles run concurrently. A handle of another kind, or one already removed,
//! produces an error reply.
//!
//! # Errors
//!
//! A handler that returns `Err(PodError)` produces an error reply, which
//! babashka raises as `ex-info`. [`PodError::with_data`] sets `ex-data`, encoded
//! in the pod's format. Any `std::error::Error` converts into [`PodError`], so
//! `?` works in handlers. Argument decoding failures, unknown vars and handler
//! panics are also error replies, and the pod keeps serving. [`Error`] is for
//! failures that stop the runtime, returned by [`Pod::run`].
//!
//! # Concurrency and shutdown
//!
//! - Each `invoke` runs on its own thread. Replies are written whole, so
//!   concurrent calls never interleave on the wire.
//! - The pod always declares the `shutdown` op. babashka sends it when the
//!   script exits and then waits for the pod process with no timeout. On
//!   `shutdown`, end of input, or a failed write, the runtime stops reading,
//!   cancels in-flight calls, and waits up to [`Pod::shutdown_grace`] (two
//!   seconds by default) for them before [`Pod::run`] returns.
//! - The runtime never panics on I/O errors.
//!
//! # Cancellation
//!
//! A call is cancelled when the pod shuts down, or when its [`CancelToken`] is
//! cancelled. Cancellation only signals; the call ends when its handler
//! returns.
//!
//! | API | Use |
//! |---|---|
//! | [`Emitter::is_cancelled`] | check between units of work |
//! | [`Emitter::wait_cancelled`] | block until cancelled, in place of `sleep` |
//! | [`Emitter::on_cancel`] | run a closure once on cancellation, for example to wake a handler blocked on its own channel |
//! | [`Emitter::cancel_token`] | a cloneable [`CancelToken`] that cancels this call alone, for a `stop` or `unwatch` var |
//!
//! ```
//! use std::sync::mpsc;
//! use babashka_pod::{CancelToken, Emitter, Handle, Handles, Namespace, NoArgs, PodError};
//!
//! enum Event {
//!     Tick(u64),
//!     Stop,
//! }
//!
//! // (def t (ticker)), (ticks t println) in a future, then (stop! t).
//! let tickers: Handles<Option<CancelToken>> = Handles::new("ticker");
//! let (new, start, stop) = (tickers.clone(), tickers.clone(), tickers);
//! let ns = Namespace::new("pod.example.ticker")
//!     .function("ticker", move |_: NoArgs| Ok::<_, PodError>(new.insert(None)))
//!     .stream_fn("ticks", move |(h,): (Handle,), tx: Emitter| {
//!         // stop! cancels this call through the token stored with the handle.
//!         start.with(&h, |slot| {
//!             *slot = Some(tx.cancel_token());
//!             Ok(())
//!         })?;
//!         let (events, rx) = mpsc::channel();
//!         let wake = events.clone();
//!         tx.on_cancel(move || {
//!             let _ = wake.send(Event::Stop);
//!         });
//!         std::thread::spawn(move || {
//!             for i in 0.. {
//!                 if events.send(Event::Tick(i)).is_err() {
//!                     break;
//!                 }
//!                 std::thread::sleep(std::time::Duration::from_millis(100));
//!             }
//!         });
//!         for event in rx {
//!             match event {
//!                 Event::Tick(i) => tx.send(&i)?,
//!                 Event::Stop => break,
//!             }
//!         }
//!         Ok(())
//!     })
//!     .function("stop!", move |(h,): (Handle,)| {
//!         stop.remove_with(&h, |slot| {
//!             if let Some(token) = slot.take() {
//!                 token.cancel();
//!             }
//!             Ok(())
//!         })
//!     });
//! ```
//!
//! [`Emitter::on_cancel`] hooks run on the thread that cancels: the caller of
//! [`CancelToken::cancel`], or a runtime thread during shutdown. Keep them
//! short and non-blocking.
//!
//! # Transport
//!
//! [`Pod::run`] uses a localhost socket when babashka sets
//! `BABASHKA_POD_TRANSPORT=socket` (`load-pod` with `{:transport :socket}`)
//! and stdin/stdout otherwise. In stdin/stdout mode stdout carries the
//! protocol, so the pod must not print to it; log to stderr or use
//! [`Emitter::out`]. [`is_pod_mode`] checks `BABASHKA_POD=true`, so one binary
//! can serve as both a CLI and a pod. [`Pod::run_with`] serves over any
//! `Read`/`Write` pair.
//!
//! # Not supported
//!
//! - Async (tokio) handlers.
//! - The EDN payload format, and `readers`, which the pod protocol defines for
//!   EDN only. Transit covers what EDN would add; the README records why.

#![warn(missing_docs, clippy::missing_errors_doc)]

mod bencode;
mod error;
mod handles;
mod pod;
pub mod transit;

pub use error::{Error, PodError};
pub use handles::{Handle, Handles};
pub use pod::{CancelToken, Emitter, Format, Namespace, NoArgs, Pod, is_pod_mode};
pub use serde_json;

#[cfg(test)]
mod tests;
