# pod-fswatcher

A [babashka](https://babashka.org) pod for watching files, built on [babashka-pod](../../README.md)
and [notify](https://github.com/notify-rs/notify). It is a drop-in replacement for
[`org.babashka/fswatcher`](https://github.com/babashka/pod-babashka-fswatcher): same namespace,
functions, options and event maps.

```bash
cargo build --release -p pod-fswatcher
```

```clojure
(require '[babashka.pods :as pods])
(pods/load-pod ["target/release/pod-fswatcher"])
(require '[pod.babashka.fswatcher :as fw])

(def w (fw/watch "src" (fn [event] (prn event)) {:recursive true :delay-ms 200}))
;; {:type :write, :path "src/app/core.clj"}
(fw/unwatch w)
```

## API

All functions are in `pod.babashka.fswatcher`.

| Function | Does |
|---|---|
| `(watch path callback)`, `(watch path callback opts)` | Watches `path` (a directory or a file) and calls `callback` with each event. Returns `{:watcher/id n}` |
| `(unwatch watcher)` | Stops the watcher and returns its argument. Unknown or already stopped watchers are ignored |

| Option | Default | Meaning |
|---|---|---|
| `:recursive` | `false` | Watch subdirectories too, including ones created later |
| `:dedup` | `true` | Hold events per path until the path has been quiet for `:delay-ms`, then deliver the first event of the burst |
| `:delay-ms` | `2000` | The quiet period for `:dedup`. Ignored without it |

Events are maps with `:type` and `:path`. `:path` starts with `path` as given to `watch`.

| `:type` | When |
|---|---|
| `:create` | A file or directory was created, or renamed to this path |
| `:write` | Contents changed |
| `:remove` | Removed |
| `:rename` | Renamed away from this path |
| `:chmod` | Metadata changed. On macOS `touch` reports this, as do Spotlight and editors updating attributes |
| `:error` | The OS watcher reported an error; the map also has `:error` with the message |

## As a library

The crate is also a library, `pod_fswatcher`, so another pod binary can serve this namespace next to
others. `pod_fswatcher::namespace()` builds it with its own state; serve it with `Format::TransitJson`:

```rust
babashka_pod::Pod::new()
    .format(babashka_pod::Format::TransitJson)
    .namespace(pod_fswatcher::namespace())
    .run()
```

## Releases

Released from this repository with the tag `pod-fswatcher-v<version>`; see
[Pods in this repository](../../README.md#pods-in-this-repository).

## Differences from the Go pod

- `watch` on a path that does not exist throws `ex-info`. The Go pod returns a watcher that never
  delivers events.
- Each event has one type. The Go pod can combine them, as in `:write|chmod`.
- On macOS events come from FSEvents (the Go pod uses kqueue). FSEvents can report the creation of
  the watched directory itself when the directory was created just before `watch`.
- `:recursive` also watches directories created after `watch`. The Go pod only watches the ones
  that existed when it started.
- When babashka exits, the pod stops its watchers and exits within the shutdown grace period. The
  Go pod panics on end of input.

## How it works

`watch` calls `-create-watcher`, which checks the path and stores the watcher in a
`babashka_pod::Handles` registry, then invokes the streaming var `-start-watcher` with
`:handlers`. That call stays open: its handler thread owns the notify watcher, debounces events
and sends each one with the `Emitter`. `unwatch` sends the handler a stop message, and the pod
shutting down cancels it; either way the handler returns, the call completes with `done`, and the
notify watcher is dropped.
