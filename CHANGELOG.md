# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-10-29

First release.

### Features

- Pod runtime: `describe`, `invoke`, `load-ns` and `shutdown` over stdin/stdout or a localhost socket
- Typed function handlers, stream handlers with generated client wrappers, and a metadata builder
- JSON and transit+json payload formats
- `Handles` registry for stateful pod resources, with `remove_with` and `handle`
- Per-call cancellation for stream handlers through `CancelToken` and `Emitter::on_cancel`
- Bounded shutdown grace period for in-flight calls

### Documentation

- Record why the EDN payload format is not supported
