//! A babashka pod for parsing source code with tree-sitter: syntax trees,
//! query captures, and outlines of the definitions in a file.
//!
//! Grammars are compiled in by Cargo feature: `clojure`, `go`, `javascript`,
//! `python`, `rust` and `typescript` (TypeScript and TSX), all on by default.
//! See `pods/treesitter/README.md` for the client API.
//!
//! The `pod-treesitter` binary serves [`namespace`] on its own. Another pod
//! can serve it next to other namespaces:
//!
//! ```no_run
//! use babashka_pod::{Format, Pod};
//!
//! Pod::new()
//!     .format(Format::TransitJson)
//!     .namespace(pod_treesitter::namespace())
//!     .run()
//!     .unwrap();
//! ```
//!
//! Kinds and language names go back to babashka as keywords, which needs
//! [`Format::TransitJson`](babashka_pod::Format).

// Built with some languages left out, the outline code for those languages
// is unused.
#![cfg_attr(
    not(all(
        feature = "clojure",
        feature = "go",
        feature = "javascript",
        feature = "python",
        feature = "rust",
        feature = "typescript"
    )),
    allow(dead_code, unused_imports)
)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use babashka_pod::serde_json::{Map, Value, json};
use babashka_pod::transit::Keyword;
use babashka_pod::{Namespace, PodError};
use tree_sitter::{Node, Parser, Query, QueryCursor, QueryErrorKind, StreamingIterator, Tree};

mod lang;
mod outline;

use lang::{LANGS, Lang, Outline};
use outline::Def;

/// The namespace this pod defines.
pub const NAMESPACE: &str = "pod.joshrotenberg.treesitter";

/// The names of the languages compiled in, sorted.
pub fn languages() -> Vec<&'static str> {
    LANGS.iter().map(|l| l.name).collect()
}

fn usage(var: &str, expected: &str) -> PodError {
    PodError::new(format!("{NAMESPACE}/{var} expects {expected}"))
}

fn lang(name: &str) -> Result<&'static Lang, PodError> {
    lang::by_name(name).ok_or_else(|| {
        PodError::new(format!(
            "unknown language {name}; this pod has {}",
            languages().join(", ")
        ))
        .with_data(json!({ "language": name, "available": languages() }))
    })
}

/// A language argument: a keyword or a string.
fn lang_arg(var: &str, v: Option<&Value>) -> Result<&'static Lang, PodError> {
    match v {
        Some(Value::String(s)) => lang(s.strip_prefix(':').unwrap_or(s)),
        _ => Err(usage(var, "a language keyword such as :rust as its first argument")),
    }
}

fn string_arg<'a>(var: &str, v: Option<&'a Value>, what: &str) -> Result<&'a str, PodError> {
    v.and_then(Value::as_str)
        .ok_or_else(|| usage(var, &format!("{what} as a string")))
}

fn parse_source(lang: &Lang, src: &str) -> Result<Tree, PodError> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.language())
        .map_err(|e| PodError::new(format!("{}: {e}", lang.name)))?;
    parser
        .parse(src, None)
        .ok_or_else(|| PodError::new(format!("{}: the parser returned no tree", lang.name)))
}

/// `[line column]`: 1-based line, 0-based byte column.
fn point(p: tree_sitter::Point) -> Value {
    json!([p.row + 1, p.column])
}

fn node_value(node: Node<'_>, field: Option<&str>, src: &str) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), Value::from(node.kind()));
    if let Some(field) = field {
        m.insert("field".into(), Value::from(field));
    }
    m.insert("start".into(), point(node.start_position()));
    m.insert("end".into(), point(node.end_position()));
    let mut cursor = node.walk();
    let mut children = Vec::new();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.is_named() {
                children.push(node_value(child, cursor.field_name(), src));
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    if children.is_empty() {
        m.insert("text".into(), Value::from(&src[node.byte_range()]));
    } else {
        m.insert("children".into(), Value::Array(children));
    }
    Value::Object(m)
}

fn parse(args: Vec<Value>) -> Result<Value, PodError> {
    let var = "parse";
    if !(2..=3).contains(&args.len()) {
        return Err(usage(var, "[lang source] or [lang source opts]"));
    }
    let lang = lang_arg(var, args.first())?;
    let src = string_arg(var, args.get(1), "source")?;
    let as_ = match args.get(2) {
        None | Some(Value::Null) => "tree",
        Some(opts) => opts.get("as").and_then(Value::as_str).unwrap_or("tree"),
    };
    let tree = parse_source(lang, src)?;
    match as_.strip_prefix(':').unwrap_or(as_) {
        "tree" => Ok(node_value(tree.root_node(), None, src)),
        "sexp" => Ok(Value::from(tree.root_node().to_sexp())),
        other => Err(usage(var, &format!(":as :tree or :sexp, not {other}"))),
    }
}

fn compile(lang: &Lang, source: &str) -> Result<Query, PodError> {
    Query::new(&lang.language(), source).map_err(|e| {
        let kind = match e.kind {
            QueryErrorKind::Syntax => "syntax error",
            QueryErrorKind::NodeType => "unknown node type",
            QueryErrorKind::Field => "unknown field",
            QueryErrorKind::Capture => "unknown capture",
            QueryErrorKind::Predicate => "invalid predicate",
            QueryErrorKind::Structure => "impossible pattern",
            QueryErrorKind::Language => "language error",
        };
        let detail = e.message.trim();
        PodError::new(format!(
            "{} query: {kind} at row {}, column {}{}{detail}",
            lang.name,
            e.row + 1,
            e.column,
            if detail.is_empty() { "" } else { ": " },
        ))
        .with_data(json!({ "row": e.row + 1, "column": e.column, "kind": kind }))
    })
}

fn query(lang: &Lang, src: &str, query_src: &str) -> Result<Vec<Value>, PodError> {
    let query = compile(lang, query_src)?;
    let tree = parse_source(lang, src)?;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), src.as_bytes());
    let mut out = Vec::new();
    while let Some((m, i)) = captures.next() {
        let c = m.captures()[*i];
        out.push(json!({
            "capture": names[c.index as usize],
            "pattern": m.pattern_index,
            "type": c.node.kind(),
            "text": &src[c.node.byte_range()],
            "start": point(c.node.start_position()),
            "end": point(c.node.end_position()),
        }));
    }
    Ok(out)
}

/// Compiled outline queries, one per language, built on first use.
#[derive(Clone, Default)]
struct Outlines {
    queries: Arc<Mutex<HashMap<&'static str, Arc<Query>>>>,
}

impl Outlines {
    fn query(&self, lang: &'static Lang, source: &str) -> Result<Arc<Query>, PodError> {
        let mut queries = self.queries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(q) = queries.get(lang.name) {
            return Ok(q.clone());
        }
        let q = Arc::new(compile(lang, source)?);
        queries.insert(lang.name, q.clone());
        Ok(q)
    }

    fn defs(&self, lang: &'static Lang, src: &str) -> Result<Vec<Def>, PodError> {
        let tree = parse_source(lang, src)?;
        Ok(match lang.outline {
            Outline::Query(source) => outline::by_query(&tree, &*self.query(lang, source)?, src),
            Outline::Clojure => outline::clojure(&tree, src),
        })
    }

    fn outline(&self, lang: &'static Lang, src: &str) -> Result<Vec<Value>, PodError> {
        Ok(self.defs(lang, src)?.into_iter().map(def_value).collect())
    }
}

fn def_value(d: Def) -> Value {
    let mut v = json!({
        "name": d.name,
        "kind": Keyword::new(d.kind),
        "start-line": d.start_line,
        "end-line": d.end_line,
        "signature": d.signature,
    });
    if let Some(parent) = d.parent {
        v["parent"] = Value::from(parent);
    }
    v
}

fn outline_file(outlines: &Outlines, args: Vec<Value>) -> Result<Vec<Value>, PodError> {
    let var = "outline-file";
    if !(1..=2).contains(&args.len()) {
        return Err(usage(var, "[path] or [path lang]"));
    }
    let path = string_arg(var, args.first(), "a path")?;
    let lang = match args.get(1) {
        None | Some(Value::Null) => lang::for_path(Path::new(path)).ok_or_else(|| {
            PodError::new(format!(
                "no language for {path}; pass one, as in (outline-file path :rust)"
            ))
            .with_data(json!({ "path": path }))
        })?,
        Some(v) => lang_arg(var, Some(v))?,
    };
    let src = std::fs::read_to_string(path).map_err(|e| {
        PodError::new(format!("cannot read {path}: {e}")).with_data(json!({ "path": path }))
    })?;
    outlines.outline(lang, &src)
}

/// Builds the `pod.joshrotenberg.treesitter` namespace.
///
/// Outline queries are compiled on first use and kept for the life of the
/// namespace. Serve it with [`Format::TransitJson`](babashka_pod::Format).
pub fn namespace() -> Namespace {
    let outlines = Outlines::default();
    let (o1, o2) = (outlines.clone(), outlines);
    Namespace::new(NAMESPACE)
        // Takes `Value`, not `NoArgs`: babashka sends nil for a call with no
        // arguments, which `NoArgs` rejects (#28).
        .function("languages", |_: Value| {
            Ok::<_, PodError>(languages().into_iter().map(Keyword::new).collect::<Vec<_>>())
        })
        .doc("languages", "The languages this pod can parse, as keywords.")
        .arglists("languages", ["[]"])
        .function("language-of", |(path,): (String,)| {
            Ok::<_, PodError>(lang::for_path(Path::new(&path)).map(|l| Keyword::new(l.name)))
        })
        .doc(
            "language-of",
            "The language for a file, from its extension, or nil.",
        )
        .arglists("language-of", ["[path]"])
        .function("parse", parse)
        .doc(
            "parse",
            "Parses source. Returns the tree as nested maps of named nodes with :type, :field, :start and :end ([line column], line 1-based), and :children, or :text on leaves. With {:as :sexp}, returns tree-sitter's S-expression string.",
        )
        .arglists("parse", ["[lang source]", "[lang source opts]"])
        .function("query", |args: Vec<Value>| {
            let var = "query";
            if args.len() != 3 {
                return Err(usage(var, "[lang source query]"));
            }
            let lang = lang_arg(var, args.first())?;
            let src = string_arg(var, args.get(1), "source")?;
            let q = string_arg(var, args.get(2), "a tree-sitter query")?;
            query(lang, src, q)
        })
        .doc(
            "query",
            "Runs a tree-sitter query over source. Returns its captures in document order as maps with :capture, :pattern, :type, :text, :start and :end.",
        )
        .arglists("query", ["[lang source query]"])
        .function("outline", move |args: Vec<Value>| {
            let var = "outline";
            if args.len() != 2 {
                return Err(usage(var, "[lang source]"));
            }
            let lang = lang_arg(var, args.first())?;
            let src = string_arg(var, args.get(1), "source")?;
            o1.outline(lang, src)
        })
        .doc(
            "outline",
            "The definitions in source, without bodies, in source order: maps with :name, :kind, :start-line, :end-line, :signature, and :parent for nested definitions.",
        )
        .arglists("outline", ["[lang source]"])
        .function("outline-file", move |args: Vec<Value>| outline_file(&o2, args))
        .doc(
            "outline-file",
            "outline for a file read by the pod. The language comes from the file extension unless given.",
        )
        .arglists("outline-file", ["[path]", "[path lang]"])
}

#[cfg(test)]
mod tests;
