use super::*;

#[allow(dead_code)]
fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// `(name kind start end signature parent)` rows, one per definition.
#[allow(dead_code)]
fn rows(lang_name: &str, src: &str) -> Vec<(String, String, usize, usize, String, String)> {
    Outlines::default()
        .defs(lang(lang_name).unwrap(), src)
        .unwrap()
        .into_iter()
        .map(|d| {
            (
                d.name,
                d.kind,
                d.start_line,
                d.end_line,
                d.signature,
                d.parent.unwrap_or_default(),
            )
        })
        .collect()
}

#[allow(dead_code)]
fn expect(rows_: &[(&str, &str, usize, usize, &str, &str)]) -> Vec<(String, String, usize, usize, String, String)> {
    rows_
        .iter()
        .map(|(n, k, s, e, sig, p)| (n.to_string(), k.to_string(), *s, *e, sig.to_string(), p.to_string()))
        .collect()
}

#[test]
fn languages_are_sorted_and_found_by_extension() {
    let names = languages();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    #[cfg(all(
        feature = "clojure",
        feature = "go",
        feature = "javascript",
        feature = "python",
        feature = "rust",
        feature = "typescript"
    ))]
    {
        assert_eq!(
            names,
            ["clojure", "go", "javascript", "python", "rust", "tsx", "typescript"]
        );
        for (path, name) in [
            ("src/lib.rs", "rust"),
            ("src/app/core.cljc", "clojure"),
            ("bb.edn", "clojure"),
            ("x.PY", "python"),
            ("index.mjs", "javascript"),
            ("App.tsx", "tsx"),
            ("main.ts", "typescript"),
            ("main.go", "go"),
        ] {
            assert_eq!(lang::for_path(Path::new(path)).map(|l| l.name), Some(name), "{path}");
        }
    }
    assert!(lang::for_path(Path::new("README.md")).is_none());
    assert!(lang::for_path(Path::new("Makefile")).is_none());
}

#[test]
fn unknown_languages_are_errors() {
    let Err(err) = lang("cobol") else {
        panic!("cobol is not a language here")
    };
    assert!(err.message().starts_with("unknown language cobol; this pod has "));
    assert_eq!(err.data().unwrap()["language"], "cobol");
}

#[test]
fn every_outline_query_compiles() {
    for l in LANGS {
        if let Outline::Query(source) = l.outline {
            compile(l, source).unwrap_or_else(|e| panic!("{}: {}", l.name, e.message()));
        }
    }
}

#[cfg(feature = "rust")]
#[test]
fn outlines_rust() {
    assert_eq!(
        rows("rust", &fixture("sample.rs")),
        expect(&[
            ("LIMIT", "constant", 5, 5, "pub const LIMIT: usize", ""),
            ("GREETING", "static", 7, 7, "static GREETING: &str", ""),
            ("Pair", "type", 9, 9, "pub type Pair = (i32, i32)", ""),
            ("Point", "struct", 12, 15, "pub struct Point", ""),
            ("Shape", "enum", 17, 20, "pub enum Shape", ""),
            ("Area", "trait", 22, 24, "pub trait Area", ""),
            ("area", "method", 23, 23, "fn area(&self) -> f64", "Area"),
            ("Area for Shape", "impl", 26, 33, "impl Area for Shape", ""),
            ("area", "method", 27, 32, "fn area(&self) -> f64", "Area for Shape"),
            ("fmt::Display for Point", "impl", 35, 39, "impl fmt::Display for Point", ""),
            (
                "fmt",
                "method",
                36,
                38,
                "fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result",
                "fmt::Display for Point",
            ),
            ("add", "function", 41, 46, "pub fn add(a: i32, b: i32) -> i32", ""),
            ("inner", "module", 48, 50, "mod inner", ""),
            ("helper", "function", 49, 49, "pub(crate) fn helper()", "inner"),
            ("square", "macro", 52, 56, "macro_rules! square", ""),
        ])
    );
}

#[cfg(feature = "clojure")]
#[test]
fn outlines_clojure() {
    assert_eq!(
        rows("clojure", &fixture("sample.clj")),
        expect(&[
            ("example.core", "module", 1, 3, "(ns example.core)", ""),
            ("limit", "variable", 5, 5, "(def limit)", ""),
            ("state", "variable", 7, 7, "(defonce state)", ""),
            ("add", "function", 9, 12, "(defn add [a b])", ""),
            ("shout", "function", 14, 15, "(defn- shout [s])", ""),
            ("area", "function", 17, 19, "(defn area ([w]) ([w h]))", ""),
            ("unless", "macro", 21, 22, "(defmacro unless [test & body])", ""),
            ("describe", "function", 24, 24, "(defmulti describe)", ""),
            ("describe", "method", 26, 27, "(defmethod describe :circle [shape])", ""),
            ("Shape", "interface", 29, 30, "(defprotocol Shape)", ""),
            ("Square", "type", 32, 34, "(defrecord Square [side])", ""),
        ])
    );
}

#[cfg(feature = "clojure")]
#[test]
fn clojure_defmethod_dispatch_vectors_are_not_arguments() {
    let src = "(defmethod area [:circle :big] [shape] 1)\n(deftest adds (is true))";
    assert_eq!(
        rows("clojure", src),
        expect(&[
            ("area", "method", 1, 1, "(defmethod area [:circle :big] [shape])", ""),
            ("adds", "test", 2, 2, "(deftest adds)", ""),
        ])
    );
}

#[cfg(feature = "python")]
#[test]
fn outlines_python() {
    assert_eq!(
        rows("python", &fixture("sample.py")),
        expect(&[
            ("LIMIT", "variable", 5, 5, "LIMIT = 10", ""),
            ("add", "function", 8, 9, "def add(a, b):", ""),
            ("Shape", "class", 12, 20, "class Shape:", ""),
            ("__init__", "method", 15, 16, "def __init__(self, name):", "Shape"),
            ("area", "method", 19, 20, "def area(self) -> float:", "Shape"),
            ("Circle", "class", 23, 29, "class Circle(Shape):", ""),
            ("__init__", "method", 24, 26, "def __init__(self, r):", "Circle"),
            ("area", "method", 28, 29, "def area(self) -> float:", "Circle"),
            (
                "fetch",
                "function",
                32,
                34,
                "async def fetch(url: str, timeout: int = 10) -> bytes:",
                "",
            ),
        ])
    );
}

#[cfg(feature = "javascript")]
#[test]
fn outlines_javascript() {
    assert_eq!(
        rows("javascript", &fixture("sample.js")),
        expect(&[
            ("add", "function", 7, 9, "export function add(a, b)", ""),
            ("count", "function", 11, 13, "function* count(n)", ""),
            ("square", "function", 15, 15, "const square = (x) =>", ""),
            ("load", "function", 17, 19, "const load = async function (path)", ""),
            ("Shape", "class", 21, 33, "export class Shape", ""),
            ("constructor", "method", 22, 24, "constructor(name)", "Shape"),
            ("area", "method", 26, 28, "area()", "Shape"),
            ("unit", "method", 30, 32, "static unit()", "Shape"),
        ])
    );
}

#[cfg(feature = "typescript")]
#[test]
fn outlines_typescript() {
    assert_eq!(
        rows("typescript", &fixture("sample.ts")),
        expect(&[
            ("Point", "interface", 3, 6, "export interface Point", ""),
            ("Pair", "type", 8, 8, "export type Pair = [number, number]", ""),
            ("Color", "enum", 10, 13, "export enum Color", ""),
            ("add", "function", 15, 17, "export function add(a: number, b: number): number", ""),
            ("square", "function", 19, 19, "export const square = (x: number): number =>", ""),
            ("Shape", "class", 21, 27, "export abstract class Shape", ""),
            ("area", "method", 22, 22, "abstract area(): number", "Shape"),
            ("describe", "method", 24, 26, "describe(): string", "Shape"),
            ("Circle", "class", 29, 37, "export class Circle extends Shape", ""),
            ("constructor", "method", 30, 32, "constructor(private r: number)", "Circle"),
            ("area", "method", 34, 36, "area(): number", "Circle"),
            ("Geometry", "module", 39, 43, "export namespace Geometry", ""),
            ("origin", "function", 40, 42, "export function origin(): Point", "Geometry"),
        ])
    );
}

#[cfg(feature = "typescript")]
#[test]
fn outlines_tsx() {
    let src = "export function App(props: Props) {\n  return <div>{props.name}</div>;\n}\n";
    assert_eq!(
        rows("tsx", src),
        expect(&[("App", "function", 1, 3, "export function App(props: Props)", "")])
    );
}

#[cfg(feature = "go")]
#[test]
fn outlines_go() {
    assert_eq!(
        rows("go", &fixture("sample.go")),
        expect(&[
            ("Limit", "constant", 6, 6, "const Limit = 10", ""),
            ("greeting", "variable", 8, 8, "var greeting = \"hi\"", ""),
            ("Point", "struct", 11, 13, "type Point struct", ""),
            ("Shape", "interface", 15, 17, "type Shape interface", ""),
            ("Area", "method", 16, 16, "Area() float64", "Shape"),
            ("Celsius", "type", 19, 19, "type Celsius float64", ""),
            ("Circle", "struct", 21, 23, "type Circle struct", ""),
            ("Area", "method", 25, 27, "func (c *Circle) Area() float64", "Circle"),
            ("Add", "function", 29, 31, "func Add(a, b int) int", ""),
        ])
    );
}

#[cfg(feature = "rust")]
#[test]
fn parses_to_tree_data_and_sexp() {
    let src = "fn f() {}\n";
    let tree = parse(vec![json!("rust"), json!(src)]).unwrap();
    assert_eq!(tree["type"], "source_file");
    let f = &tree["children"][0];
    assert_eq!(f["type"], "function_item");
    assert_eq!(f["start"], json!([1, 0]));
    assert_eq!(f["end"], json!([1, 9]));
    assert_eq!(f["children"][0], json!({"type": "identifier", "field": "name", "start": [1, 3], "end": [1, 4], "text": "f"}));
    assert_eq!(f["children"][2]["field"], "body");

    let sexp = parse(vec![json!("rust"), json!(src), json!({"as": "sexp"})]).unwrap();
    assert_eq!(
        sexp,
        "(source_file (function_item name: (identifier) parameters: (parameters) body: (block)))"
    );
    let err = parse(vec![json!("rust"), json!(src), json!({"as": "xml"})]).unwrap_err();
    assert!(err.message().contains(":as :tree or :sexp, not xml"));
}

#[cfg(feature = "rust")]
#[test]
fn queries_return_captures_in_document_order() {
    let src = "fn add(a: i32) -> i32 { a }\nfn sub() {}\nstruct S;\n";
    let caps = query(
        lang("rust").unwrap(),
        src,
        r#"(function_item name: (identifier) @fn (#match? @fn "^a")) (struct_item name: (_) @type)"#,
    )
    .unwrap();
    assert_eq!(
        caps,
        [
            json!({"capture": "fn", "pattern": 0, "type": "identifier", "text": "add", "start": [1, 3], "end": [1, 6]}),
            json!({"capture": "type", "pattern": 1, "type": "type_identifier", "text": "S", "start": [3, 7], "end": [3, 8]}),
        ]
    );

    let err = query(lang("rust").unwrap(), src, "(function_item name: (nope))").unwrap_err();
    assert!(
        err.message().starts_with("rust query: unknown node type at row 1, column "),
        "{}",
        err.message()
    );
}

#[cfg(feature = "rust")]
#[test]
fn outline_file_reads_the_file_and_picks_the_language() {
    let outlines = Outlines::default();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rs");
    let path = path.to_str().unwrap();
    let defs = outline_file(&outlines, vec![json!(path)]).unwrap();
    assert_eq!(defs[0]["name"], "LIMIT");
    assert_eq!(defs[0]["start-line"], 5);
    assert_eq!(defs[0]["kind"], serde_json_keyword("constant"));

    let same = outline_file(&outlines, vec![json!(path), json!("rust")]).unwrap();
    assert_eq!(defs, same);

    let err = outline_file(&outlines, vec![json!("README.md")]).unwrap_err();
    assert!(err.message().starts_with("no language for README.md"));
    let err = outline_file(&outlines, vec![json!("nope.rs")]).unwrap_err();
    assert!(err.message().starts_with("cannot read nope.rs"));
}

#[allow(dead_code)]
fn serde_json_keyword(name: &str) -> Value {
    babashka_pod::serde_json::to_value(Keyword::new(name)).unwrap()
}
