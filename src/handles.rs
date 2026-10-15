//! State that lives in the pod between calls, referenced by handles.
//!
//! A pod that opens a database connection, prepares a statement or compiles a
//! query keeps the value in a [`Handles`] registry and gives babashka a
//! [`Handle`]. Later calls pass the handle back as an argument.
//!
//! ```
//! use babashka_pod::{Handle, Handles, Namespace, PodError};
//!
//! let counters: Handles<i64> = Handles::new("counter");
//! let (c1, c2, c3) = (counters.clone(), counters.clone(), counters.clone());
//! let ns = Namespace::new("pod.example.counter")
//!     .function("counter", move |(start,): (i64,)| Ok::<_, PodError>(c1.insert(start)))
//!     .function("inc!", move |(h,): (Handle,)| {
//!         c2.with(&h, |n| {
//!             *n += 1;
//!             Ok(*n)
//!         })
//!     })
//!     .function("close!", move |(h,): (Handle,)| c3.remove_with(&h, |n| Ok(*n)));
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::PodError;

/// A reference to a value in a [`Handles`] registry.
///
/// babashka sees it as a map such as `{:handle "connection", :id 1}` and
/// passes it back unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Handle {
    /// The registry's kind, such as `"connection"`.
    pub handle: String,
    /// The id within that registry.
    pub id: u64,
}

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.handle, self.id)
    }
}

/// A registry of values of one kind, shared across handler threads.
///
/// Cloning is cheap and every clone refers to the same registry, so a clone
/// can be moved into each handler closure. Each value has its own lock:
/// concurrent calls on different handles do not wait for each other.
///
/// Values are dropped when they are [`remove`](Self::remove)d or when the
/// last clone of the registry is dropped. Handler closures own their clones,
/// so that happens when [`Pod::run`](crate::Pod::run) returns.
pub struct Handles<T> {
    inner: Arc<Inner<T>>,
}

struct Inner<T> {
    kind: String,
    next: AtomicU64,
    values: Mutex<HashMap<u64, Arc<Mutex<T>>>>,
}

impl<T> Clone for Handles<T> {
    fn clone(&self) -> Self {
        Handles {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Handles<T> {
    /// Creates an empty registry. `kind` names the values in handles and
    /// error messages, such as `"connection"`.
    pub fn new(kind: impl Into<String>) -> Self {
        Handles {
            inner: Arc::new(Inner {
                kind: kind.into(),
                next: AtomicU64::new(1),
                values: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The kind given to [`new`](Self::new).
    pub fn kind(&self) -> &str {
        &self.inner.kind
    }

    /// Stores a value and returns its handle.
    pub fn insert(&self, value: T) -> Handle {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        self.values().insert(id, Arc::new(Mutex::new(value)));
        Handle {
            handle: self.inner.kind.clone(),
            id,
        }
    }

    /// Runs `f` with the value behind `handle`.
    ///
    /// Fails with an error reply when the handle is of another kind, or was
    /// never issued or already removed.
    pub fn with<R>(
        &self,
        handle: &Handle,
        f: impl FnOnce(&mut T) -> Result<R, PodError>,
    ) -> Result<R, PodError> {
        let entry = self.entry(handle)?;
        let mut value = entry.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut value)
    }

    /// Removes the value behind `handle` and drops it once no call is using
    /// it. Fails like [`with`](Self::with) for a handle it does not hold.
    pub fn remove(&self, handle: &Handle) -> Result<(), PodError> {
        self.check_kind(handle)?;
        self.values()
            .remove(&handle.id)
            .map(|_| ())
            .ok_or_else(|| self.unknown(handle))
    }

    /// Removes the value behind `handle` and runs `f` on it, in one step.
    ///
    /// Use it to pull something out of a value while releasing it, such as a
    /// sender that stops a background task. Once the entry is removed no new
    /// call can reach the handle. A call already running [`with`](Self::with)
    /// on the value finishes first, and `f` sees its changes.
    ///
    /// The value is dropped once no call is using it, as with
    /// [`remove`](Self::remove). The entry is removed even if `f` returns an
    /// error. Fails like `remove` for a handle it does not hold, without
    /// running `f`.
    pub fn remove_with<R>(
        &self,
        handle: &Handle,
        f: impl FnOnce(&mut T) -> Result<R, PodError>,
    ) -> Result<R, PodError> {
        self.check_kind(handle)?;
        let entry = self
            .values()
            .remove(&handle.id)
            .ok_or_else(|| self.unknown(handle))?;
        let mut value = entry.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut value)
    }

    /// How many values are stored.
    pub fn len(&self) -> usize {
        self.values().len()
    }

    /// True when no values are stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn entry(&self, handle: &Handle) -> Result<Arc<Mutex<T>>, PodError> {
        self.check_kind(handle)?;
        self.values()
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| self.unknown(handle))
    }

    fn check_kind(&self, handle: &Handle) -> Result<(), PodError> {
        if handle.handle == self.inner.kind {
            return Ok(());
        }
        Err(PodError::new(format!(
            "expected a {} handle, got a {} handle",
            self.inner.kind, handle.handle
        ))
        .with_data(serde_json::json!({
            "expected": self.inner.kind,
            "handle": handle.handle,
            "id": handle.id,
        })))
    }

    fn unknown(&self, handle: &Handle) -> PodError {
        PodError::new(format!(
            "unknown or closed {} handle {}",
            handle.handle, handle.id
        ))
        .with_data(serde_json::json!({ "handle": handle.handle, "id": handle.id }))
    }

    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<Mutex<T>>>> {
        self.inner.values.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    #[test]
    fn insert_with_remove() {
        let h: Handles<Vec<i32>> = Handles::new("list");
        let a = h.insert(vec![1]);
        let b = h.insert(vec![]);
        assert_eq!(
            a,
            Handle {
                handle: "list".into(),
                id: 1
            }
        );
        assert_eq!(b.id, 2);
        h.with(&a, |v| {
            v.push(2);
            Ok(())
        })
        .unwrap();
        assert_eq!(h.with(&a, |v| Ok(v.clone())).unwrap(), [1, 2]);
        assert_eq!(h.len(), 2);
        h.remove(&a).unwrap();
        assert_eq!(h.len(), 1);
        let err = h.with(&a, |_| Ok(())).unwrap_err();
        assert_eq!(err.message(), "unknown or closed list handle 1");
        assert!(h.remove(&a).is_err());
    }

    #[test]
    fn remove_with_returns_from_the_value() {
        let h: Handles<Vec<i32>> = Handles::new("list");
        let a = h.insert(vec![1, 2]);
        let taken = h.remove_with(&a, |v| Ok(std::mem::take(v))).unwrap();
        assert_eq!(taken, [1, 2]);
        assert!(h.is_empty());
        let err = h.with(&a, |_| Ok(())).unwrap_err();
        assert_eq!(err.message(), "unknown or closed list handle 1");
    }

    #[test]
    fn remove_with_errors_match_remove() {
        let conns: Handles<()> = Handles::new("connection");
        let stmts: Handles<()> = Handles::new("statement");
        let s = stmts.insert(());
        let gone = Handle {
            handle: "connection".into(),
            id: 9,
        };
        for h in [&s, &gone] {
            let via_remove = conns.remove(h).unwrap_err();
            let via_remove_with = conns
                .remove_with(h, |_| -> Result<(), PodError> { panic!("ran f") })
                .unwrap_err();
            assert_eq!(via_remove, via_remove_with);
        }
    }

    #[test]
    fn remove_with_removes_even_when_f_fails() {
        let h: Handles<i32> = Handles::new("n");
        let a = h.insert(1);
        let err = h
            .remove_with(&a, |_| -> Result<(), PodError> { Err(PodError::new("no")) })
            .unwrap_err();
        assert_eq!(err.message(), "no");
        assert!(h.is_empty());
    }

    #[test]
    fn remove_with_waits_for_a_call_holding_the_value() {
        use std::sync::mpsc;
        use std::time::Duration;

        let h: Handles<Vec<&str>> = Handles::new("log");
        let a = h.insert(vec![]);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let (h, a) = (h.clone(), a.clone());
            thread::spawn(move || {
                h.with(&a, |v| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    v.push("holder");
                    Ok(())
                })
                .unwrap()
            })
        };
        entered_rx.recv().unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let remover = {
            let (h, a) = (h.clone(), a.clone());
            thread::spawn(move || {
                let seen = h.remove_with(&a, |v| Ok(v.clone())).unwrap();
                done_tx.send(()).unwrap();
                seen
            })
        };
        // The entry is gone at once, but remove_with waits for the holder.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !h.is_empty() {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(h.with(&a, |_| Ok(())).is_err());
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(remover.join().unwrap(), ["holder"]);
    }

    #[test]
    fn wrong_kind_is_an_error() {
        let conns: Handles<()> = Handles::new("connection");
        let stmts: Handles<()> = Handles::new("statement");
        let s = stmts.insert(());
        let err = conns.with(&s, |_| Ok(())).unwrap_err();
        assert_eq!(
            err.message(),
            "expected a connection handle, got a statement handle"
        );
        assert_eq!(err.data().unwrap()["expected"], "connection");
    }

    #[test]
    fn concurrent_with_on_one_handle_is_serialized() {
        let h: Handles<u64> = Handles::new("counter");
        let c = h.insert(0);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (h, c) = (h.clone(), c.clone());
                thread::spawn(move || {
                    for _ in 0..1000 {
                        h.with(&c, |n| {
                            *n += 1;
                            Ok(())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(h.with(&c, |n| Ok(*n)).unwrap(), 8000);
    }

    #[test]
    fn values_drop_with_the_last_registry_clone() {
        struct Tracked(Arc<AtomicUsize>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let h = Handles::new("tracked");
        let a = h.insert(Tracked(dropped.clone()));
        h.insert(Tracked(dropped.clone()));
        let clone = h.clone();
        clone.remove(&a).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        drop(h);
        assert_eq!(dropped.load(Ordering::SeqCst), 1, "a clone is still alive");
        drop(clone);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn handles_round_trip_through_both_formats() {
        let h = Handle {
            handle: "connection".into(),
            id: 7,
        };
        let json = serde_json::to_value(&h).unwrap();
        assert_eq!(json, serde_json::json!({"handle": "connection", "id": 7}));
        let transit = crate::transit::encode(&json);
        assert_eq!(transit, r#"["^ ","~:handle","connection","~:id",7]"#);
        let back: Handle =
            serde_json::from_value(crate::transit::decode(&transit).unwrap()).unwrap();
        assert_eq!(back, h);
    }
}
