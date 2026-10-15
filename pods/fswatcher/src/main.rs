//! A babashka pod for watching files, built on `babashka-pod` and notify.
//!
//! A drop-in replacement for `org.babashka/fswatcher`: the same namespace,
//! client functions, options and event maps. See `pods/fswatcher/README.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use babashka_pod::serde_json::{Value, json};
use babashka_pod::{Emitter, Handle, Handles, Namespace, Pod, PodError};
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;

const NS: &str = "pod.babashka.fswatcher";

/// How often a running watch checks whether the pod is shutting down.
const POLL: Duration = Duration::from_millis(50);

/// The client side of `watch`, the same as the Go pod's: create the watcher,
/// start streaming its events to `callback`, return `{:watcher/id n}`.
const WATCH: &str = r#"(defn watch
  "Watches path and calls (callback event) for each change. event is a map
  such as {:type :write, :path \"src/a.clj\"}; :type is one of :create,
  :write, :remove, :rename, :chmod or :error. Returns a watcher for unwatch.

  opts:
    :recursive  watch subdirectories too (default false)
    :delay-ms   with :dedup, how long a path must be quiet before its event
                is delivered (default 2000)
    :dedup      deliver only the first event of each burst per path
                (default true)"
  ([path callback]
   (watch path callback {}))
  ([path callback opts]
   (let [ret (pod.babashka.fswatcher/-create-watcher path opts)]
     (babashka.pods/invoke
      "pod.babashka.fswatcher"
      'pod.babashka.fswatcher/-start-watcher
      [(:watcher/id ret)]
      {:handlers {:success (fn [event]
                             (callback (update event :type keyword)))
                  :error (fn [{:keys [ex-message]}]
                           (binding [*out* *err*]
                             (println "ERROR:" ex-message)))}})
     ret)))"#;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct Opts {
    delay_ms: u64,
    recursive: bool,
    dedup: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            delay_ms: 2000,
            recursive: false,
            dedup: true,
        }
    }
}

enum Msg {
    Event(notify::Result<notify::Event>),
    Stop,
}

/// A watcher created by `-create-watcher`. `stop` is set once
/// `-start-watcher` is running it.
struct Watch {
    root: Root,
    opts: Opts,
    stop: Option<Sender<Msg>>,
}

/// The watched path as the caller spelled it, and the absolute forms notify
/// may report paths under.
#[derive(Debug, Clone)]
struct Root {
    given: String,
    absolute: PathBuf,
    canonical: PathBuf,
}

impl Root {
    fn new(given: &str) -> Result<Root, PodError> {
        let canonical = std::fs::canonicalize(given).map_err(|e| {
            PodError::new(format!("cannot watch {given}: {e}")).with_data(json!({ "path": given }))
        })?;
        let absolute = std::path::absolute(given).unwrap_or_else(|_| canonical.clone());
        Ok(Root {
            given: given.to_string(),
            absolute,
            canonical,
        })
    }

    /// `path` relative to the caller's spelling of the root, as fsnotify
    /// reports it: watching "src" yields "src/a.clj".
    fn display(&self, path: &Path) -> String {
        for base in [&self.canonical, &self.absolute] {
            if let Ok(rest) = path.strip_prefix(base) {
                if rest.as_os_str().is_empty() {
                    return self.given.clone();
                }
                return Path::new(&self.given)
                    .join(rest)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        path.to_string_lossy().into_owned()
    }
}

/// The fsnotify name of the change to each path in a notify event. Access
/// events and events notify cannot classify are dropped.
fn changes(event: &notify::Event) -> Vec<(&'static str, &Path)> {
    let kind = match event.kind {
        EventKind::Create(_) => "create",
        EventKind::Remove(_) => "remove",
        EventKind::Modify(ModifyKind::Metadata(_)) => "chmod",
        EventKind::Modify(ModifyKind::Name(mode)) => return renames(mode, &event.paths),
        EventKind::Modify(_) => "write",
        EventKind::Access(_) | EventKind::Any | EventKind::Other => return Vec::new(),
    };
    event.paths.iter().map(|p| (kind, p.as_path())).collect()
}

/// fsnotify reports a rename as `rename` on the old path and `create` on the
/// new one.
fn renames(mode: RenameMode, paths: &[PathBuf]) -> Vec<(&'static str, &Path)> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let kind = match mode {
                RenameMode::From => "rename",
                RenameMode::To => "create",
                RenameMode::Both if i == 0 => "rename",
                RenameMode::Both => "create",
                // FSEvents does not say which side of the rename a path is on.
                RenameMode::Any | RenameMode::Other if p.exists() => "create",
                RenameMode::Any | RenameMode::Other => "rename",
            };
            (kind, p.as_path())
        })
        .collect()
}

/// Holds events per path until the path has been quiet for `delay`, then
/// releases the first event of the burst, as the Go pod does with `:dedup`.
struct Debouncer {
    delay: Duration,
    pending: HashMap<String, (&'static str, Instant)>,
}

impl Debouncer {
    fn new(delay: Duration) -> Self {
        Debouncer {
            delay,
            pending: HashMap::new(),
        }
    }

    fn push(&mut self, kind: &'static str, path: String, now: Instant) {
        let due = now + self.delay;
        self.pending
            .entry(path)
            .and_modify(|e| e.1 = due)
            .or_insert((kind, due));
    }

    fn next_due(&self) -> Option<Instant> {
        self.pending.values().map(|e| e.1).min()
    }

    /// Removes and returns the events due at `now`, oldest first.
    fn take_due(&mut self, now: Instant) -> Vec<(&'static str, String)> {
        let mut due: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, (_, t))| *t <= now)
            .map(|(path, (kind, t))| (*t, *kind, path.clone()))
            .collect();
        due.sort();
        for (_, _, path) in &due {
            self.pending.remove(path);
        }
        due.into_iter()
            .map(|(_, kind, path)| (kind, path))
            .collect()
    }
}

fn event(kind: &str, path: &str) -> Value {
    json!({ "type": kind, "path": path })
}

fn handle(id: u64) -> Handle {
    Handle {
        handle: "watcher".into(),
        id,
    }
}

fn watcher_id(watcher: &Value) -> Result<u64, PodError> {
    watcher
        .get("watcher/id")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            PodError::new(format!("{NS}/unwatch expects a watcher returned by watch"))
                .with_data(json!({ "watcher": watcher }))
        })
}

fn create(watches: &Handles<Watch>, path: &str, opts: Opts) -> Result<Value, PodError> {
    let root = Root::new(path)?;
    let h = watches.insert(Watch {
        root,
        opts,
        stop: None,
    });
    Ok(json!({ "watcher/id": h.id }))
}

/// Runs a watcher until `unwatch` stops it or the pod shuts down, sending
/// each change to the client.
fn run(watches: &Handles<Watch>, id: u64, tx: &Emitter) -> Result<(), PodError> {
    let (msgs, rx) = mpsc::channel();
    let (root, opts) = watches.with(&handle(id), |w| {
        if w.stop.is_some() {
            return Err(PodError::new(format!("watcher {id} is already running")));
        }
        w.stop = Some(msgs.clone());
        Ok((w.root.clone(), w.opts.clone()))
    })?;

    let notify_err = |e: notify::Error| {
        PodError::new(format!("cannot watch {}: {e}", root.given))
            .with_data(json!({ "path": root.given }))
    };
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = msgs.send(Msg::Event(res));
        },
        notify::Config::default(),
    )
    .map_err(notify_err)?;
    let mode = if opts.recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher.watch(&root.canonical, mode).map_err(notify_err)?;

    let mut pending = Debouncer::new(Duration::from_millis(opts.delay_ms));
    loop {
        if tx.is_cancelled() {
            return Ok(());
        }
        let wait = pending.next_due().map_or(POLL, |due| {
            due.saturating_duration_since(Instant::now()).min(POLL)
        });
        match rx.recv_timeout(wait) {
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Ok(Msg::Event(Ok(ev))) => {
                for (kind, path) in changes(&ev) {
                    let path = root.display(path);
                    if opts.dedup {
                        pending.push(kind, path, Instant::now());
                    } else {
                        tx.send(&event(kind, &path))?;
                    }
                }
            }
            Ok(Msg::Event(Err(e))) => tx.send(&json!({
                "type": "error",
                "path": root.given,
                "error": e.to_string(),
            }))?,
            Err(RecvTimeoutError::Timeout) => {}
        }
        for (kind, path) in pending.take_due(Instant::now()) {
            tx.send(&event(kind, &path))?;
        }
    }
}

/// Stops and forgets a watcher. Unknown or already stopped watchers are
/// ignored, as in the Go pod.
fn stop(watches: &Handles<Watch>, watcher: Value) -> Result<Value, PodError> {
    let h = handle(watcher_id(&watcher)?);
    if let Ok(Some(stop)) = watches.with(&h, |w| Ok(w.stop.take())) {
        let _ = stop.send(Msg::Stop);
    }
    let _ = watches.remove(&h);
    Ok(watcher)
}

fn namespace(watches: &Handles<Watch>) -> Namespace {
    let (c, r, s) = (watches.clone(), watches.clone(), watches.clone());
    Namespace::new(NS)
        .function(
            "-create-watcher",
            move |(path, opts): (String, Option<Opts>)| create(&c, &path, opts.unwrap_or_default()),
        )
        .stream("-start-watcher", move |(id,): (u64,), tx: Emitter| {
            run(&r, id, &tx)
        })
        .code("watch", WATCH)
        .function("unwatch", move |(watcher,): (Value,)| stop(&s, watcher))
        .doc(
            "unwatch",
            "Stops a watcher returned by watch. Returns the watcher.",
        )
        .arglists("unwatch", ["[watcher]"])
}

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("pod-fswatcher is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }
    let watches: Handles<Watch> = Handles::new("watcher");
    if let Err(e) = Pod::new().namespace(namespace(&watches)).run() {
        eprintln!("pod-fswatcher: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, MetadataKind, RemoveKind};

    fn ev(kind: EventKind, paths: &[&str]) -> notify::Event {
        notify::Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    fn names(event: &notify::Event) -> Vec<(&'static str, String)> {
        changes(event)
            .into_iter()
            .map(|(k, p)| (k, p.display().to_string()))
            .collect()
    }

    #[test]
    fn event_kinds_use_fsnotify_names() {
        let cases = [
            (EventKind::Create(CreateKind::File), "create"),
            (EventKind::Remove(RemoveKind::Any), "remove"),
            (
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                "write",
            ),
            (EventKind::Modify(ModifyKind::Any), "write"),
            (
                EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions)),
                "chmod",
            ),
        ];
        for (kind, name) in cases {
            assert_eq!(names(&ev(kind, &["/a"])), [(name, "/a".to_string())]);
        }
        assert!(names(&ev(EventKind::Any, &["/a"])).is_empty());
        assert!(
            names(&ev(
                EventKind::Access(notify::event::AccessKind::Any),
                &["/a"]
            ))
            .is_empty()
        );
    }

    #[test]
    fn renames_are_rename_then_create() {
        let both = ev(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &["/old", "/new"],
        );
        assert_eq!(
            names(&both),
            [("rename", "/old".into()), ("create", "/new".into())]
        );
        let from = ev(
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            &["/x"],
        );
        assert_eq!(names(&from), [("rename", "/x".into())]);
        let to = ev(EventKind::Modify(ModifyKind::Name(RenameMode::To)), &["/x"]);
        assert_eq!(names(&to), [("create", "/x".into())]);
    }

    #[test]
    fn paths_use_the_callers_spelling_of_the_root() {
        let dir = std::env::temp_dir().join(format!("pod-fswatcher-root-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root::new(dir.to_str().unwrap()).unwrap();
        let given = dir.to_string_lossy();
        assert_eq!(root.display(&root.canonical), given);
        assert_eq!(
            root.display(&root.canonical.join("sub/a.clj")),
            format!("{given}/sub/a.clj")
        );
        assert_eq!(root.display(Path::new("/elsewhere/b")), "/elsewhere/b");
        std::fs::remove_dir_all(&dir).unwrap();

        let err = Root::new("/no/such/path/here").unwrap_err();
        assert!(err.message().starts_with("cannot watch /no/such/path/here"));
    }

    #[test]
    fn debouncer_releases_the_first_event_after_quiet() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut d = Debouncer::new(ms(100));
        d.push("create", "a".into(), t0);
        d.push("write", "a".into(), t0 + ms(50));
        d.push("write", "b".into(), t0 + ms(60));
        assert_eq!(d.next_due(), Some(t0 + ms(150)));
        assert!(d.take_due(t0 + ms(149)).is_empty());
        assert_eq!(d.take_due(t0 + ms(150)), [("create", "a".to_string())]);
        assert_eq!(d.take_due(t0 + ms(200)), [("write", "b".to_string())]);
        assert_eq!(d.next_due(), None);
    }

    #[test]
    fn opts_default_like_the_go_pod() {
        let opts: Opts = babashka_pod::serde_json::from_value(json!({})).unwrap();
        assert_eq!(opts, Opts::default());
        assert_eq!(opts.delay_ms, 2000);
        let opts: Opts = babashka_pod::serde_json::from_value(
            json!({"recursive": true, "delay-ms": 50, "other": 1}),
        )
        .unwrap();
        assert!(opts.recursive && opts.dedup);
        assert_eq!(opts.delay_ms, 50);
    }

    #[test]
    fn unwatch_stops_a_running_watcher_and_ignores_unknown_ones() {
        let watches = Handles::new("watcher");
        let dir = std::env::temp_dir();
        let w = create(&watches, dir.to_str().unwrap(), Opts::default()).unwrap();
        let (tx, rx) = mpsc::channel();
        watches
            .with(&handle(watcher_id(&w).unwrap()), |w| {
                w.stop = Some(tx);
                Ok(())
            })
            .unwrap();
        assert_eq!(stop(&watches, w.clone()).unwrap(), w);
        assert!(matches!(rx.try_recv(), Ok(Msg::Stop)));
        assert!(watches.is_empty());
        // Again, and with a watcher that never existed.
        assert_eq!(stop(&watches, w.clone()).unwrap(), w);
        assert!(stop(&watches, json!({"watcher/id": 99})).is_ok());
        let err = stop(&watches, json!("nope")).unwrap_err();
        assert!(err.message().contains("expects a watcher"));
    }
}
