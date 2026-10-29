# pod-treesitter

A [babashka](https://babashka.org) pod for parsing source code, built on
[babashka-pod](../../README.md) and [tree-sitter](https://tree-sitter.github.io/tree-sitter/). It
returns syntax trees, the captures of tree-sitter queries, and outlines: the definitions in a file
with their names, kinds, line ranges and signatures, without bodies. Outlines are compact enough to
give an agent the skeleton of a repository.

```bash
cargo build --release -p pod-treesitter
```

```clojure
(require '[babashka.pods :as pods])
(pods/load-pod ["target/release/pod-treesitter"])
(require '[pod.joshrotenberg.treesitter :as ts])

(ts/outline-file "src/lib.rs")
;; => [{:name "Point", :kind :struct, :start-line 12, :end-line 15, :signature "pub struct Point"}
;;     {:name "area", :kind :method, :start-line 27, :end-line 32,
;;      :signature "fn area(&self) -> f64", :parent "Area for Shape"} ...]
```

`examples/skeleton.bb` prints the outline of every supported file under a directory:

```bash
bb pods/treesitter/examples/skeleton.bb src
```

```
src/lib.rs
  12-15     struct     pub struct Point
  26-33     impl       impl Area for Shape
    27-32     method     fn area(&self) -> f64
```

## Languages

Each grammar is a Cargo feature; all are on by default. Build with `--no-default-features
--features rust,clojure` for a smaller binary.

| Language | Feature | Extensions | Grammar crate |
|---|---|---|---|
| `:clojure` | `clojure` | `.clj` `.cljs` `.cljc` `.cljd` `.edn` `.bb` | `tree-sitter-clojure-orchard` 0.2.8 |
| `:go` | `go` | `.go` | `tree-sitter-go` 0.25.0 |
| `:javascript` | `javascript` | `.js` `.mjs` `.cjs` `.jsx` | `tree-sitter-javascript` 0.25.0 |
| `:python` | `python` | `.py` `.pyi` | `tree-sitter-python` 0.25.0 |
| `:rust` | `rust` | `.rs` | `tree-sitter-rust` 0.24.2 |
| `:typescript`, `:tsx` | `typescript` | `.ts` `.mts` `.cts`, `.tsx` | `tree-sitter-typescript` 0.23.2 |

The pod uses tree-sitter 0.27.1, which needs Rust 1.90. Grammar crates that depend on another
`tree-sitter` version cannot be linked with it; `tree-sitter-clojure` 0.1.0 depends on 0.25, so
the Clojure grammar is `tree-sitter-clojure-orchard`, a fork of sogaiu/tree-sitter-clojure.

## API

All functions are in `pod.joshrotenberg.treesitter`. `lang` is a keyword or string such as
`:rust`. Lines are 1-based; columns are 0-based byte offsets; positions are `[line column]`.

| Function | Returns |
|---|---|
| `(languages)` | The languages compiled in, as keywords |
| `(language-of path)` | The language for a file extension, or nil |
| `(parse lang source)` | The tree as nested maps of named nodes: `:type`, `:field` (when the node is a field of its parent), `:start`, `:end`, and `:children`, or `:text` on leaves |
| `(parse lang source {:as :sexp})` | tree-sitter's S-expression for the tree |
| `(query lang source query)` | The captures of a [tree-sitter query](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/), in document order: `:capture`, `:pattern` (the index of the pattern that matched), `:type`, `:text`, `:start`, `:end`. Predicates such as `#match?` and `#eq?` apply |
| `(outline lang source)` | The definitions, in source order: `:name`, `:kind`, `:start-line`, `:end-line`, `:signature`, and `:parent` for nested definitions |
| `(outline-file path)`, `(outline-file path lang)` | `outline` for a file the pod reads; the language comes from the extension unless given |

Errors are raised as `ex-info`: an unknown language (with `:language` and `:available` in
`ex-data`), a query that does not compile (with `:row`, `:column` and `:kind`), a file that cannot be
read or has no known extension.

```clojure
(ts/query :rust "fn add(a: i32) -> i32 { a }" "(function_item name: (identifier) @fn)")
;; => [{:capture "fn", :pattern 0, :type "identifier", :text "add", :start [1 3], :end [1 6]}]

(ts/parse :rust "fn f() {}" {:as :sexp})
;; => "(source_file (function_item name: (identifier) parameters: (parameters) body: (block)))"
```

## Outlines

| Language | Definitions | `:kind` |
|---|---|---|
| Rust | functions, structs, enums, unions, traits, impls, modules, `macro_rules!`, consts, statics, type aliases | `:function` (`:method` inside an impl or trait), `:struct`, `:enum`, `:union`, `:trait`, `:impl`, `:module`, `:macro`, `:constant`, `:static`, `:type` |
| Clojure | top-level `ns` and `def...` forms | `:module` (ns), `:function` (defn, defn-, defmulti), `:macro`, `:method` (defmethod), `:variable` (def, defonce), `:interface` (defprotocol, definterface), `:type` (defrecord, deftype), `:test` (deftest), `:definition` (any other `def...`) |
| Python | functions, classes, module-level assignments | `:function` (`:method` inside a class), `:class`, `:variable` |
| JavaScript | functions, generators, classes, methods, `const f = () => ...` and `const f = function ...` | `:function`, `:class`, `:method` |
| TypeScript, TSX | the JavaScript ones, plus interfaces, type aliases, enums, namespaces, abstract classes, function and method signatures | also `:interface`, `:type`, `:enum`, `:module` |
| Go | functions, methods, struct, interface and other type declarations, interface methods, top-level consts and vars | `:function`, `:method`, `:struct`, `:interface`, `:type`, `:constant`, `:variable` |

`:signature` is the definition's text up to its body, with whitespace collapsed and the trailing
commas of multi-line parameter lists dropped: `pub fn add(a: i32, b: i32) -> i32`, `def area(self)
-> float:`, `export class Circle extends Shape`, `func (c *Circle) Area() float64`. Definitions
without a body (constants, type aliases, signatures) use their first line. Clojure signatures are
the head, the name, and the argument vector or the vector of each arity: `(defn add [a b])`,
`(defn area ([w]) ([w h]))`, `(defmethod describe :circle [shape])`.

`:parent` is the name of the enclosing definition: the class, trait, impl (`Display for Point`),
module or namespace a definition is in, or a Go method's receiver type.

## As a library

The crate is also a library, `pod_treesitter`, so another pod binary can serve this namespace next
to others. `pod_treesitter::namespace()` builds it; serve it with `Format::TransitJson`:

```rust
babashka_pod::Pod::new()
    .format(babashka_pod::Format::TransitJson)
    .namespace(pod_treesitter::namespace())
    .run()
```

## Releases

Released from this repository with the tag `pod-treesitter-v<version>`; see
[Pods in this repository](../../README.md#pods-in-this-repository).

## Tests

`src/tests.rs` checks the outline of each fixture in `tests/fixtures/` (one per language), tree
data, S-expressions, query captures and errors. `tests/bb.rs` runs `test.bb` under babashka over
both transports.

## Not supported

- Incremental reparsing: each call parses its source from scratch.
- Outlines of nested Clojure definitions (inside `comment`, `let` or reader conditionals) and of
  protocol method signatures.
