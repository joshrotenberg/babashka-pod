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
//! use babashka_pod::{Namespace, Pod, PodError};
//!
//! fn main() -> Result<(), babashka_pod::Error> {
//!     Pod::new()
//!         .namespace(
//!             Namespace::new("pod.example.math")
//!                 .function("add", |(a, b): (i64, i64)| Ok::<_, PodError>(a + b))
//!                 .meta("add", "{:doc \"Adds two integers\" :arglists ([a b])}"),
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
//! ```
//!
//! # Behavior
//!
//! - Each `invoke` runs on its own thread. Replies are written whole, so
//!   concurrent calls never interleave on the wire.
//! - A handler error or panic becomes an error reply, which babashka raises as
//!   `ex-info`. The pod keeps running.
//! - The pod always declares the `shutdown` op. On `shutdown`, end of input, or
//!   a failed write, in-flight calls are cancelled (see
//!   [`Emitter::is_cancelled`]) and get a bounded grace period
//!   ([`Pod::shutdown_grace`]) before [`Pod::run`] returns. The runtime never
//!   panics on I/O errors.

mod bencode;
mod error;
mod pod;
pub mod transit;

pub use error::{Error, PodError};
pub use pod::{Emitter, Format, Namespace, NoArgs, Pod, is_pod_mode};
pub use serde_json;

#[cfg(test)]
mod tests;
