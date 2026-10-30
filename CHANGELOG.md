# Changelog

All notable changes to this project will be documented in this file.

## [0.1.1] - 2026-10-29

### Features

- Filesystem watcher pod on notify 
- Tree-sitter pod with parse, query and outline 
- Lossless transit arguments with transit::Value and *_transit registration 

### Refactoring

- Pods as library plus binary, released per pod from their own tags 



## [0.1.0] - 2026-10-29

First release.

### Features

- Pod runtime: `describe`, `invoke`, `load-ns` and `shutdown` over stdin/stdout or a localhost socket
- Typed function handlers, stream handlers with generated client wrappers, and a metadata builder
- JSON and transit+json payload formats
- `Handles` registry for stateful pod resources, with `remove_with` and `handle`
- Per-call cancellation for stream handlers through `CancelToken` and `Emitter::on_cancel`
- Bounded shutdown grace period for in-flight calls

### Bug Fixes

- Decode a nil argument payload as an empty argument vector, so zero-argument calls work

### Documentation

- Record why the EDN payload format is not supported
