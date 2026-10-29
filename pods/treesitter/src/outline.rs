//! Outlines: the definitions in a file, without their bodies.

use std::collections::HashMap;

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

/// One definition in an outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Def {
    pub name: String,
    pub kind: String,
    /// 1-based, inclusive.
    pub start_line: usize,
    /// 1-based, inclusive.
    pub end_line: usize,
    pub signature: String,
    /// The name of the enclosing definition, for methods and other nested
    /// definitions.
    pub parent: Option<String>,
}

/// 1-based first and last line of a node. A node that ends at the start of
/// a line (after a newline) ends on the line before.
pub(crate) fn lines(node: Node<'_>) -> (usize, usize) {
    let start = node.start_position();
    let end = node.end_position();
    let last = if end.column == 0 && end.row > start.row {
        end.row
    } else {
        end.row + 1
    };
    (start.row + 1, last)
}

fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

/// Collapses runs of whitespace to one space and trims.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// [`squash`], then drops the spaces and trailing commas that line breaks
/// leave inside brackets: `fn f( a: i32, b: i32, )` becomes `fn f(a: i32, b: i32)`.
fn squash_code(s: &str) -> String {
    let mut s = squash(s);
    for (from, to) in [("( ", "("), ("[ ", "["), (" )", ")"), (" ]", "]"), (",)", ")"), (",]", "]")] {
        s = s.replace(from, to);
    }
    s
}

/// Where a definition's body starts: its `body` field, the body of its
/// `value` (`const f = () => {...}`), or, for constants and statics, the
/// value itself.
fn body_start(node: Node<'_>) -> Option<usize> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body.start_byte());
    }
    let value = node.child_by_field_name("value")?;
    if let Some(body) = value.child_by_field_name("body") {
        return Some(body.start_byte());
    }
    matches!(node.kind(), "const_item" | "static_item").then(|| value.start_byte())
}

/// Where a definition's signature starts: before `export`, and before
/// `const`/`let`/`var` when the declaration has only this declarator.
fn signature_start(node: Node<'_>) -> usize {
    let mut start = node;
    while let Some(parent) = start.parent() {
        let wraps = match parent.kind() {
            "export_statement" => true,
            "lexical_declaration" | "variable_declaration" => parent.named_child_count() == 1,
            _ => false,
        };
        if !wraps {
            break;
        }
        start = parent;
    }
    start.start_byte()
}

/// The definition's text up to its body with whitespace collapsed, or its
/// first line when it has no body.
fn signature(node: Node<'_>, src: &str) -> String {
    let from = signature_start(node);
    let sig = match body_start(node) {
        Some(to) => squash_code(&src[from..to])
            .trim_end_matches(['=', ' '])
            .to_string(),
        None => {
            let rest = &src[from..node.end_byte()];
            let first = rest.lines().next().unwrap_or_default();
            squash_code(first)
                .trim_end_matches(['{', ';', ' '])
                .to_string()
        }
    };
    // Go specs leave their keyword on the enclosing declaration.
    let keyword = match node.kind() {
        "type_spec" | "type_alias" => "type ",
        "const_spec" => "const ",
        "var_spec" => "var ",
        _ => "",
    };
    format!("{keyword}{sig}")
}

/// Kinds whose functions are methods.
fn is_container(kind: &str) -> bool {
    matches!(kind, "impl" | "trait" | "class" | "interface")
}

/// The outline of a parsed file, using a language's outline query. See
/// [`crate::lang::Outline::Query`] for the captures it expects.
pub(crate) fn by_query(tree: &Tree, query: &Query, src: &str) -> Vec<Def> {
    struct Found<'t> {
        pattern: usize,
        node: Node<'t>,
        kind: String,
        name: String,
        receiver: Option<String>,
    }

    let names = query.capture_names();
    let mut found: HashMap<usize, Found<'_>> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src.as_bytes());
    while let Some(m) = matches.next() {
        let (mut def, mut name, mut trait_name, mut receiver) = (None, None, None, None);
        for c in m.captures() {
            match names[c.index as usize] {
                "name" => name = Some(text(c.node, src)),
                "trait_name" => trait_name = Some(text(c.node, src)),
                "receiver" => receiver = Some(text(c.node, src)),
                kind => def = Some((kind, c.node)),
            }
        }
        let (Some((kind, node)), Some(name)) = (def, name) else {
            continue;
        };
        if found
            .get(&node.id())
            .is_some_and(|f| f.pattern <= m.pattern_index)
        {
            continue;
        }
        let name = match trait_name {
            Some(t) => format!("{} for {}", squash(t), squash(name)),
            None => squash(name),
        };
        found.insert(
            node.id(),
            Found {
                pattern: m.pattern_index,
                node,
                kind: kind.to_string(),
                name,
                receiver: receiver.map(squash),
            },
        );
    }

    let mut defs: Vec<&Found<'_>> = found.values().collect();
    defs.sort_by_key(|f| (f.node.start_byte(), std::cmp::Reverse(f.node.end_byte())));
    defs.iter()
        .map(|f| {
            let enclosing = std::iter::successors(f.node.parent(), |n| n.parent())
                .find_map(|n| found.get(&n.id()));
            let kind = match enclosing {
                Some(e) if f.kind == "function" && is_container(&e.kind) => "method".to_string(),
                _ => f.kind.clone(),
            };
            let (start_line, end_line) = lines(f.node);
            Def {
                name: f.name.clone(),
                kind,
                start_line,
                end_line,
                signature: signature(f.node, src),
                parent: f
                    .receiver
                    .clone()
                    .or_else(|| enclosing.map(|e| e.name.clone())),
            }
        })
        .collect()
}

/// The values of a Clojure collection node (its `value` fields), skipping
/// comments and `#_` forms.
fn values<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let mut out = Vec::new();
    if cursor.goto_first_child() {
        loop {
            if cursor.field_name() == Some("value") {
                out.push(cursor.node());
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    out
}

/// A symbol's name without metadata or namespace.
fn sym_name<'a>(node: Node<'_>, src: &'a str) -> Option<&'a str> {
    (node.kind() == "sym_lit")
        .then(|| node.child_by_field_name("name"))
        .flatten()
        .map(|n| text(n, src))
}

fn clojure_kind(head: &str) -> &'static str {
    match head {
        "ns" => "module",
        "defn" | "defn-" | "defmulti" => "function",
        "defmacro" => "macro",
        "defmethod" => "method",
        "def" | "defonce" => "variable",
        "defprotocol" | "definterface" => "interface",
        "defrecord" | "deftype" | "defstruct" => "type",
        "deftest" => "test",
        _ => "definition",
    }
}

/// `(head name args)`: the arguments of function-like forms are the first
/// argument vector, or the vectors of each arity. Docstrings, attribute maps
/// and bodies are left out.
fn clojure_signature(head: &str, name: &str, rest: &[Node<'_>], src: &str) -> String {
    let mut parts = vec![head.to_string(), name.to_string()];
    let with_args = matches!(
        head,
        "defn" | "defn-" | "defmacro" | "defmethod" | "defrecord" | "deftype"
    );
    if with_args {
        let mut rest = rest.iter().copied().peekable();
        if head == "defmethod" {
            // The dispatch value, which may itself be a vector.
            if let Some(dispatch) = rest.next() {
                parts.push(squash(text(dispatch, src)));
            }
        }
        while let Some(node) = rest.next() {
            match node.kind() {
                "str_lit" | "map_lit" => {}
                "vec_lit" => {
                    parts.push(squash(text(node, src)));
                    break;
                }
                "list_lit" => {
                    // Multi-arity: ([a] ...) ([a b] ...).
                    let mut arity = Some(node);
                    while let Some(list) = arity {
                        match values(list).first() {
                            Some(v) if v.kind() == "vec_lit" => {
                                parts.push(format!("({})", squash(text(*v, src))));
                            }
                            _ => break,
                        }
                        arity = rest.next_if(|n| n.kind() == "list_lit");
                    }
                    break;
                }
                _ => break,
            }
        }
    }
    format!("({})", parts.join(" "))
}

/// The outline of a Clojure file: top-level `ns` and `def...` forms.
pub(crate) fn clojure(tree: &Tree, src: &str) -> Vec<Def> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|n| n.kind() == "list_lit")
        .filter_map(|form| {
            let vals = values(form);
            let head = sym_name(*vals.first()?, src)?;
            if !(head == "ns" || head.starts_with("def")) {
                return None;
            }
            let name = sym_name(*vals.get(1)?, src)?;
            let (start_line, end_line) = lines(form);
            Some(Def {
                name: name.to_string(),
                kind: clojure_kind(head).to_string(),
                start_line,
                end_line,
                signature: clojure_signature(head, name, &vals[2..], src),
                parent: None,
            })
        })
        .collect()
}
