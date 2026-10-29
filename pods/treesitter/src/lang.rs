//! The languages compiled in, chosen by Cargo features.

use std::path::Path;

use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

/// How [`crate::outline`] finds definitions in a language.
pub(crate) enum Outline {
    /// A query whose patterns capture a definition node, named after its
    /// kind (`@function`), and its name (`@name`). Optional captures:
    /// `@trait_name` (Rust `impl Trait for Type`) and `@receiver` (a Go method's
    /// receiver type, used as its parent).
    Query(&'static str),
    /// Clojure: top-level `def...` and `ns` forms, read from the tree.
    Clojure,
}

/// A language: its name, the file extensions it is chosen for, its grammar
/// and its outline rules.
pub(crate) struct Lang {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    language: LanguageFn,
    pub outline: Outline,
}

impl Lang {
    pub fn language(&self) -> Language {
        self.language.into()
    }
}

#[cfg(feature = "rust")]
const RUST_OUTLINE: &str = r#"
(function_item name: (identifier) @name) @function
(function_signature_item name: (identifier) @name) @function
(struct_item name: (type_identifier) @name) @struct
(enum_item name: (type_identifier) @name) @enum
(union_item name: (type_identifier) @name) @union
(trait_item name: (type_identifier) @name) @trait
(impl_item trait: (_)? @trait_name type: (_) @name) @impl
(mod_item name: (identifier) @name) @module
(macro_definition name: (identifier) @name) @macro
(const_item name: (identifier) @name) @constant
(static_item name: (identifier) @name) @static
(type_item name: (type_identifier) @name) @type
"#;

#[cfg(feature = "python")]
const PYTHON_OUTLINE: &str = r#"
(function_definition name: (identifier) @name) @function
(class_definition name: (identifier) @name) @class
(module (expression_statement (assignment left: (identifier) @name) @variable))
"#;

#[cfg(feature = "go")]
const GO_OUTLINE: &str = r#"
(function_declaration name: (identifier) @name) @function
(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [(type_identifier) @receiver
             (pointer_type (type_identifier) @receiver)
             (generic_type type: (type_identifier) @receiver)
             (pointer_type (generic_type type: (type_identifier) @receiver))]))
  name: (field_identifier) @name) @method
(method_declaration name: (field_identifier) @name) @method
(type_spec name: (type_identifier) @name type: (struct_type)) @struct
(type_spec name: (type_identifier) @name type: (interface_type)) @interface
(type_spec name: (type_identifier) @name) @type
(type_alias name: (type_identifier) @name) @type
(method_elem name: (field_identifier) @name) @method
(source_file (const_declaration (const_spec name: (identifier) @name) @constant))
(source_file (var_declaration (var_spec name: (identifier) @name) @variable))
"#;

#[cfg(feature = "javascript")]
const JAVASCRIPT_OUTLINE: &str = r#"
(function_declaration name: (identifier) @name) @function
(generator_function_declaration name: (identifier) @name) @function
(class_declaration name: (identifier) @name) @class
(method_definition name: (_) @name) @method
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression) (generator_function)]) @function
"#;

#[cfg(feature = "typescript")]
const TYPESCRIPT_OUTLINE: &str = r#"
(function_declaration name: (identifier) @name) @function
(generator_function_declaration name: (identifier) @name) @function
(function_signature name: (identifier) @name) @function
(class_declaration name: (type_identifier) @name) @class
(abstract_class_declaration name: (type_identifier) @name) @class
(interface_declaration name: (type_identifier) @name) @interface
(type_alias_declaration name: (type_identifier) @name) @type
(enum_declaration name: (identifier) @name) @enum
(internal_module name: (_) @name) @module
(module name: (_) @name) @module
(method_definition name: (_) @name) @method
(method_signature name: (_) @name) @method
(abstract_method_signature name: (_) @name) @method
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression) (generator_function)]) @function
"#;

/// Every language compiled in, sorted by name.
pub(crate) static LANGS: &[Lang] = &[
    #[cfg(feature = "clojure")]
    Lang {
        name: "clojure",
        extensions: &["clj", "cljs", "cljc", "cljd", "edn", "bb"],
        language: tree_sitter_clojure_orchard::LANGUAGE,
        outline: Outline::Clojure,
    },
    #[cfg(feature = "go")]
    Lang {
        name: "go",
        extensions: &["go"],
        language: tree_sitter_go::LANGUAGE,
        outline: Outline::Query(GO_OUTLINE),
    },
    #[cfg(feature = "javascript")]
    Lang {
        name: "javascript",
        extensions: &["js", "mjs", "cjs", "jsx"],
        language: tree_sitter_javascript::LANGUAGE,
        outline: Outline::Query(JAVASCRIPT_OUTLINE),
    },
    #[cfg(feature = "python")]
    Lang {
        name: "python",
        extensions: &["py", "pyi"],
        language: tree_sitter_python::LANGUAGE,
        outline: Outline::Query(PYTHON_OUTLINE),
    },
    #[cfg(feature = "rust")]
    Lang {
        name: "rust",
        extensions: &["rs"],
        language: tree_sitter_rust::LANGUAGE,
        outline: Outline::Query(RUST_OUTLINE),
    },
    #[cfg(feature = "typescript")]
    Lang {
        name: "tsx",
        extensions: &["tsx"],
        language: tree_sitter_typescript::LANGUAGE_TSX,
        outline: Outline::Query(TYPESCRIPT_OUTLINE),
    },
    #[cfg(feature = "typescript")]
    Lang {
        name: "typescript",
        extensions: &["ts", "mts", "cts"],
        language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        outline: Outline::Query(TYPESCRIPT_OUTLINE),
    },
];

/// The language named `name`, if it is compiled in.
pub(crate) fn by_name(name: &str) -> Option<&'static Lang> {
    LANGS.iter().find(|l| l.name == name)
}

/// The language for a file, from its extension.
pub(crate) fn for_path(path: &Path) -> Option<&'static Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGS.iter().find(|l| l.extensions.contains(&ext.as_str()))
}
