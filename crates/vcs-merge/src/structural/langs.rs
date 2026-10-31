//! Language tables for the shared structural merge. Each language is behind its own cargo feature.
#![allow(unused_imports, dead_code)]

use super::{merge_with, norm_ws, parses_as, text_of, Lang, StructuralMerge};
use crate::Algorithm;
use tree_sitter::Node;

fn named(src: &[u8], n: Node) -> Option<String> {
    n.child_by_field_name("name").map(|x| text_of(src, x))
}

fn fallback_key(src: &[u8], n: Node) -> String {
    format!("{}:{}", n.kind(), norm_ws(&text_of(src, n)))
}

/// A `{ ... }` body (first byte `{`, last byte `}`) of the given field.
fn brace_body<'a>(n: Node<'a>, src_kinds: &[&str]) -> Option<Node<'a>> {
    let b = n.child_by_field_name("body")?;
    src_kinds.contains(&b.kind()).then_some(b)
}

// ---------------------------------------------------------------- Rust

#[cfg(feature = "structural")]
mod rust {
    use super::*;

    fn body(n: Node) -> Option<Node> {
        if matches!(n.kind(), "impl_item" | "trait_item" | "mod_item") {
            n.child_by_field_name("body").filter(|b| b.kind() == "declaration_list")
        } else {
            None
        }
    }

    fn key(src: &[u8], n: Node) -> String {
        if let Some(b) = body(n) {
            // Header text without the body: `impl Trait for Type`, `mod name`, ...
            let h = String::from_utf8_lossy(&src[n.start_byte()..b.start_byte()]).into_owned();
            return format!("{}:{}", n.kind(), norm_ws(&h));
        }
        match named(src, n) {
            Some(name) => format!("{}:{}", n.kind(), name),
            None => fallback_key(src, n),
        }
    }

    pub static RUST: Lang = Lang {
        name: "rust",
        language: || tree_sitter_rust::LANGUAGE.into(),
        trivia: &["line_comment", "block_comment", "attribute_item", "inner_attribute_item"],
        body,
        key,
        delims: (1, 1),
    };
}
#[cfg(feature = "structural")]
pub use rust::RUST;

/// Whether `src` parses as Rust without any error node.
#[cfg(feature = "structural")]
pub fn parses(src: &[u8]) -> bool {
    parses_as(&RUST, src)
}

/// Three-way merge of Rust sources, structural first, line-based diff3 as the fallback.
#[cfg(feature = "structural")]
pub fn merge_rust(alg: Algorithm, base: &[u8], left: &[u8], right: &[u8]) -> StructuralMerge {
    merge_with(&RUST, alg, base, left, right)
}

// ---------------------------------------------------------------- Python

#[cfg(feature = "structural-python")]
mod python {
    use super::*;

    /// `class` (possibly decorated) with an indentation-delimited block.
    fn body(n: Node) -> Option<Node> {
        let n = if n.kind() == "decorated_definition" {
            n.child_by_field_name("definition")?
        } else {
            n
        };
        if n.kind() == "class_definition" {
            n.child_by_field_name("body").filter(|b| b.kind() == "block")
        } else {
            None
        }
    }

    fn key(src: &[u8], n: Node) -> String {
        match n.kind() {
            "decorated_definition" => match n.child_by_field_name("definition") {
                Some(d) => key(src, d),
                None => fallback_key(src, n),
            },
            "expression_statement" => match n.named_child(0).filter(|c| c.kind() == "assignment") {
                Some(a) => match a.child_by_field_name("left") {
                    Some(l) => format!("assign:{}", norm_ws(&text_of(src, l))),
                    None => fallback_key(src, n),
                },
                None => fallback_key(src, n),
            },
            _ => match named(src, n) {
                Some(name) => format!("{}:{}", n.kind(), name),
                None => fallback_key(src, n),
            },
        }
    }

    pub static PYTHON: Lang = Lang {
        name: "python",
        language: || tree_sitter_python::LANGUAGE.into(),
        trivia: &["comment"],
        body,
        key,
        delims: (0, 0),
    };
}
#[cfg(feature = "structural-python")]
pub use python::PYTHON;

/// Three-way merge of Python sources, structural first, line-based diff3 as the fallback.
#[cfg(feature = "structural-python")]
pub fn merge_python(alg: Algorithm, base: &[u8], left: &[u8], right: &[u8]) -> StructuralMerge {
    merge_with(&PYTHON, alg, base, left, right)
}

// ---------------------------------------------------------------- TypeScript

#[cfg(feature = "structural-typescript")]
mod typescript {
    use super::*;

    fn unwrap_export(n: Node) -> Node {
        if n.kind() == "export_statement" {
            n.child_by_field_name("declaration").unwrap_or(n)
        } else {
            n
        }
    }

    /// `class`, `interface` and `namespace` bodies, with or without `export`.
    fn body(n: Node) -> Option<Node> {
        let n = unwrap_export(n);
        if matches!(
            n.kind(),
            "class_declaration" | "abstract_class_declaration" | "interface_declaration" | "internal_module" | "module"
        ) {
            brace_body(n, &["class_body", "interface_body", "object_type", "statement_block"])
        } else {
            None
        }
    }

    fn key(src: &[u8], n: Node) -> String {
        let d = unwrap_export(n);
        let prefix = if d.id() != n.id() { "export:" } else { "" };
        let k = match d.kind() {
            "lexical_declaration" | "variable_declaration" => {
                let mut c = d.walk();
                let name = d
                    .named_children(&mut c)
                    .find(|x| x.kind() == "variable_declarator")
                    .and_then(|v| named(src, v));
                match name {
                    Some(nm) => format!("decl:{nm}"),
                    None => fallback_key(src, d),
                }
            }
            _ => match named(src, d) {
                Some(name) => format!("{}:{}", d.kind(), name),
                None => fallback_key(src, d),
            },
        };
        format!("{prefix}{k}")
    }

    pub static TYPESCRIPT: Lang = Lang {
        name: "typescript",
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        trivia: &["comment"],
        body,
        key,
        delims: (1, 1),
    };
}
#[cfg(feature = "structural-typescript")]
pub use typescript::TYPESCRIPT;

/// Three-way merge of TypeScript sources, structural first, line-based diff3 as the fallback.
#[cfg(feature = "structural-typescript")]
pub fn merge_typescript(alg: Algorithm, base: &[u8], left: &[u8], right: &[u8]) -> StructuralMerge {
    merge_with(&TYPESCRIPT, alg, base, left, right)
}

// ---------------------------------------------------------------- Go

#[cfg(feature = "structural-go")]
mod go {
    use super::*;

    fn body(_: Node) -> Option<Node> {
        None // Go has no nested containers of items; methods are top-level declarations.
    }

    fn spec_name(src: &[u8], n: Node) -> Option<String> {
        let mut c = n.walk();
        let name = n
            .named_children(&mut c)
            .find(|x| matches!(x.kind(), "type_spec" | "type_alias" | "const_spec" | "var_spec"))
            .and_then(|s| named(src, s));
        name
    }

    fn key(src: &[u8], n: Node) -> String {
        match n.kind() {
            "package_clause" => "package".into(),
            "method_declaration" => {
                let recv = n
                    .child_by_field_name("receiver")
                    .map(|r| norm_ws(&text_of(src, r)))
                    .unwrap_or_default();
                format!("method:{recv}:{}", named(src, n).unwrap_or_default())
            }
            "type_declaration" | "const_declaration" | "var_declaration" => match spec_name(src, n) {
                Some(nm) => format!("{}:{nm}", n.kind()),
                None => fallback_key(src, n),
            },
            "import_declaration" => fallback_key(src, n),
            _ => match named(src, n) {
                Some(name) => format!("{}:{}", n.kind(), name),
                None => fallback_key(src, n),
            },
        }
    }

    pub static GO: Lang = Lang {
        name: "go",
        language: || tree_sitter_go::LANGUAGE.into(),
        trivia: &["comment"],
        body,
        key,
        delims: (1, 1),
    };
}
#[cfg(feature = "structural-go")]
pub use go::GO;

/// Three-way merge of Go sources, structural first, line-based diff3 as the fallback.
#[cfg(feature = "structural-go")]
pub fn merge_go(alg: Algorithm, base: &[u8], left: &[u8], right: &[u8]) -> StructuralMerge {
    merge_with(&GO, alg, base, left, right)
}

// ---------------------------------------------------------------- dispatch

/// The structural driver for a path, by file extension, among the enabled features.
pub fn lang_for_path(path: &str) -> Option<&'static Lang> {
    let ext = path.rsplit_once('.')?.1;
    match ext {
        #[cfg(feature = "structural")]
        "rs" => Some(&RUST),
        #[cfg(feature = "structural-python")]
        "py" => Some(&PYTHON),
        #[cfg(feature = "structural-typescript")]
        "ts" | "mts" | "cts" => Some(&TYPESCRIPT),
        #[cfg(feature = "structural-go")]
        "go" => Some(&GO),
        _ => None,
    }
}
