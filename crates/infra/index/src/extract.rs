//! One file → definitions, imports, identifier occurrences and syntax errors
//! (FR-INDEX-02, CE-DQ15).

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use domain::{Span, SymbolDef, SymbolKind};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, ParseOptions, Parser, QueryCursor};

use crate::lang::Lang;

/// Longest signature kept.
const MAX_SIGNATURE: usize = 200;

/// What one parse produced. Paths are filled in by the caller.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub defs: Vec<SymbolDef>,
    pub imports: Vec<String>,
    /// (identifier, 1-based line), deduplicated.
    pub idents: Vec<(String, u32)>,
    pub error_nodes: u32,
    pub first_error: Option<(u32, u32)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// No grammar compiled in for this language.
    NoGrammar,
    /// The parse ran past its budget (FR-INDEX-11).
    Timeout,
}

fn text<'a>(node: Node<'_>, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

/// 1-based character column of a node's start.
fn char_col(node: Node<'_>, src: &[u8]) -> u32 {
    let start = node.start_byte();
    let line_start = src[..start]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    String::from_utf8_lossy(&src[line_start..start])
        .chars()
        .count() as u32
        + 1
}

/// The name a Rust `impl` is filed under: the implementing type, without
/// generic arguments (`impl<T> Foo<T>` → `Foo`).
fn impl_type_name(node: Node<'_>, src: &[u8]) -> String {
    let base = if node.kind() == "generic_type" {
        node.child_by_field_name("type").unwrap_or(node)
    } else {
        node
    };
    let raw = text(base, src);
    raw.rsplit("::").next().unwrap_or(raw).trim().to_string()
}

/// Container names from the outside in, for a qualified name.
fn containers(node: Node<'_>, lang: Lang, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut cur = node.parent();
    while let Some(n) = cur {
        let name = match (lang, n.kind()) {
            (Lang::Rust, "impl_item") => n
                .child_by_field_name("type")
                .map(|t| impl_type_name(t, src)),
            (Lang::Rust, "trait_item" | "mod_item" | "function_item") => n
                .child_by_field_name("name")
                .map(|t| text(t, src).to_string()),
            (
                Lang::TypeScript | Lang::Tsx,
                "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "internal_module",
            ) => n
                .child_by_field_name("name")
                .map(|t| text(t, src).to_string()),
            (Lang::Python, "class_definition" | "function_definition") => n
                .child_by_field_name("name")
                .map(|t| text(t, src).to_string()),
            _ => None,
        };
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            names.push(name);
        }
        cur = n.parent();
    }
    names.reverse();
    names
}

/// Go's method receiver type: `func (s *Server) Run` → `Server`.
fn go_receiver(node: Node<'_>, src: &[u8]) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let raw = text(receiver, src);
    let inner = raw.trim_start_matches('(').trim_end_matches(')');
    let ty = inner.split_whitespace().last()?;
    let ty = ty.trim_start_matches('*');
    let ty = ty.split('[').next().unwrap_or(ty);
    (!ty.is_empty()).then(|| ty.to_string())
}

/// The node whose range a definition covers: the declaration itself, widened
/// to a wrapper that belongs to it (`export …`, a decorated definition,
/// Go's single-spec `type (…)` group).
fn outer(node: Node<'_>, lang: Lang) -> Node<'_> {
    let Some(parent) = node.parent() else {
        return node;
    };
    let wraps = match (lang, parent.kind()) {
        (Lang::TypeScript | Lang::Tsx, "export_statement") => true,
        (Lang::Python, "decorated_definition") => true,
        (Lang::Go, "type_declaration" | "const_declaration") => parent.named_child_count() == 1,
        _ => false,
    };
    if wraps {
        outer(parent, lang)
    } else {
        node
    }
}

/// Widen `node` over contiguous leading doc comments, attributes and
/// decorators — no blank line between them and the definition.
fn with_leading_trivia(node: Node<'_>) -> Node<'_> {
    let mut first = node;
    while let Some(prev) = first.prev_named_sibling() {
        let trivia = matches!(
            prev.kind(),
            "line_comment" | "block_comment" | "comment" | "attribute_item" | "decorator"
        );
        let adjacent = prev.end_position().row + 1 >= first.start_position().row;
        if !(trivia && adjacent) {
            break;
        }
        first = prev;
    }
    first
}

fn span_of(first: Node<'_>, last: Node<'_>) -> Span {
    Span {
        start_line: first.start_position().row as u32 + 1,
        end_line: last.end_position().row as u32 + 1,
        start_byte: first.start_byte() as u32,
        end_byte: last.end_byte() as u32,
    }
}

fn kind_of(capture: &str) -> Option<SymbolKind> {
    Some(match capture {
        "def.function" => SymbolKind::Function,
        "def.method" => SymbolKind::Method,
        "def.struct" => SymbolKind::Struct,
        "def.enum" => SymbolKind::Enum,
        "def.trait" => SymbolKind::Trait,
        "def.interface" => SymbolKind::Interface,
        "def.class" => SymbolKind::Class,
        "def.type" => SymbolKind::Type,
        "def.const" => SymbolKind::Const,
        "def.module" => SymbolKind::Module,
        "def.impl" => SymbolKind::Impl,
        _ => return None,
    })
}

/// Whether a function is really a method: it sits in a type.
fn in_type(node: Node<'_>, lang: Lang) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        let hit = match lang {
            Lang::Rust => matches!(n.kind(), "impl_item" | "trait_item"),
            Lang::Python => n.kind() == "class_definition",
            _ => false,
        };
        if hit {
            return true;
        }
        if matches!(n.kind(), "function_item" | "function_definition") {
            return false; // a nested function is not a method
        }
        cur = n.parent();
    }
    false
}

/// TypeScript `const`s and arrow functions count only at module level —
/// every local variable would drown the outline.
fn ts_module_level(node: Node<'_>) -> bool {
    match node.parent() {
        Some(p) if p.kind() == "program" => true,
        Some(p) if p.kind() == "export_statement" => {
            p.parent().is_some_and(|g| g.kind() == "program")
        }
        _ => false,
    }
}

fn count_errors(root: Node<'_>) -> (u32, Option<(u32, u32)>) {
    let mut count = 0;
    let mut first = None;
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.is_error() || n.is_missing() {
            count += 1;
            let p = n.start_position();
            let here = (p.row as u32 + 1, p.column as u32 + 1);
            if first.is_none_or(|f| here < f) {
                first = Some(here);
            }
        }
        if n.has_error() {
            stack.extend(n.children(&mut cursor));
        }
    }
    (count, first)
}

/// A definition's signature: its name's line, trimmed and clipped. Derived
/// rather than stored, so the index recomputes it from the file on demand
/// and gets the same answer the parse did.
pub fn signature_at(src: &str, line: u32) -> String {
    let text = src
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("")
        .trim();
    text.chars().take(MAX_SIGNATURE).collect()
}

/// Parse `src` as `lang` within `budget`.
pub fn parse(lang: Lang, src: &str, budget: Duration) -> Result<Extracted, ParseError> {
    let language = lang.language().ok_or(ParseError::NoGrammar)?;
    let queries = lang.queries().ok_or(ParseError::NoGrammar)?;
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| ParseError::NoGrammar)?;
    let bytes = src.as_bytes();
    let started = Instant::now();
    let mut progress = |_: &tree_sitter::ParseState| {
        if started.elapsed() > budget {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser
        .parse_with_options(
            &mut |i, _| &bytes[i.min(bytes.len())..],
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .ok_or(ParseError::Timeout)?;
    let root = tree.root_node();
    let mut out = Extracted::default();
    (out.error_nodes, out.first_error) = count_errors(root);

    // Definitions. A node can match several patterns (Go's typed
    // `type_spec`, TS function-valued consts); the earliest pattern — the
    // more specific one — wins. Matches do not arrive in pattern order, so
    // they are collected and settled first.
    let names = queries.defs.capture_names();
    let mut candidates: Vec<(usize, Node<'_>, Node<'_>, SymbolKind)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&queries.defs, root, bytes);
    while let Some(m) = matches.next() {
        let mut def_node = None;
        let mut name_node = None;
        let mut kind = None;
        for c in m.captures {
            let cap = names[c.index as usize];
            if cap == "name" {
                name_node = Some(c.node);
            } else if let Some(k) = kind_of(cap) {
                kind = Some(k);
                def_node = Some(c.node);
            }
        }
        if let (Some(node), Some(name_node), Some(kind)) = (def_node, name_node, kind) {
            candidates.push((m.pattern_index, node, name_node, kind));
        }
    }
    candidates.sort_by_key(|(pattern, node, name, _)| (node.id(), name.id(), *pattern));
    candidates.dedup_by_key(|(_, node, name, _)| (node.id(), name.id()));
    for (_, node, name_node, mut kind) in candidates {
        if matches!(lang, Lang::TypeScript | Lang::Tsx)
            && node.kind() == "lexical_declaration"
            && !ts_module_level(node)
        {
            continue;
        }
        if kind == SymbolKind::Function && in_type(node, lang) {
            kind = SymbolKind::Method;
        }
        let name = if kind == SymbolKind::Impl {
            impl_type_name(name_node, bytes)
        } else {
            text(name_node, bytes).to_string()
        };
        if name.is_empty() {
            continue;
        }
        let mut chain = containers(node, lang, bytes);
        if lang == Lang::Go && node.kind() == "method_declaration" {
            if let Some(receiver) = go_receiver(node, bytes) {
                chain.push(receiver);
            }
        }
        let depth = chain.len().min(u8::MAX as usize) as u8;
        let qualified = if chain.is_empty() {
            name.clone()
        } else {
            format!(
                "{}{}{}",
                chain.join(lang.separator()),
                lang.separator(),
                name
            )
        };
        let wrapper = outer(node, lang);
        let first = with_leading_trivia(wrapper);
        let body = node
            .child_by_field_name("body")
            .or_else(|| {
                // `const f = () => { … }`: the arrow function's body.
                node.named_children(&mut node.walk())
                    .find(|c| c.kind() == "variable_declarator")
                    .and_then(|d| d.child_by_field_name("value"))
                    .and_then(|v| v.child_by_field_name("body"))
            })
            .map(|b| span_of(b, b));
        let signature = signature_at(src, name_node.start_position().row as u32 + 1);
        out.defs.push(SymbolDef {
            name,
            qualified,
            kind,
            path: String::new(),
            span: span_of(first, wrapper),
            body,
            name_line: name_node.start_position().row as u32 + 1,
            name_col: char_col(name_node, bytes),
            signature,
            depth,
        });
    }
    out.defs.sort_by_key(|d| (d.span.start_byte, d.depth));

    // Imports.
    let mut cursor = QueryCursor::new();
    let mut imports = cursor.matches(&queries.imports, root, bytes);
    while let Some(m) = imports.next() {
        for c in m.captures {
            let raw = text(c.node, bytes).trim_matches(|ch| ch == '"' || ch == '\'' || ch == '`');
            if !raw.is_empty() && !out.imports.iter().any(|i| i == raw) {
                out.imports.push(raw.to_string());
            }
        }
    }

    // Identifier occurrences, one per (name, line).
    let mut idents: HashSet<(String, u32)> = HashSet::new();
    let mut cursor = QueryCursor::new();
    let mut found = cursor.matches(&queries.idents, root, bytes);
    while let Some(m) = found.next() {
        for c in m.captures {
            let t = text(c.node, bytes);
            if t.len() >= 2 {
                idents.insert((t.to_string(), c.node.start_position().row as u32 + 1));
            }
        }
    }
    let mut idents: Vec<(String, u32)> = idents.into_iter().collect();
    idents.sort();
    out.idents = idents;
    Ok(out)
}
