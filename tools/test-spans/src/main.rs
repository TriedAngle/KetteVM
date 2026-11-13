//! Test-span extraction for the negative-control gate.
//!
//! Parse a Rust file and print the line spans of every test-code
//! region as JSON: `#[test]`-family functions (with their enclosing
//! module path) and whole `#[cfg(test)]` items.  The gate diffs
//! these regions (whitespace-neutral) instead of whole files, so a
//! fault may share a FILE with tests as long as the test REGIONS are
//! identical modulo whitespace.
//!
//! Usage: test-spans <file.rs>
//! Output: `[{"path":"mod::name","kind":"fn","start":10,"end":20},...]`
//! (1-based, inclusive; the span starts at the attribute so edits to
//! `#[...]` count as test edits).  `kind` is `"fn"` for a runnable
//! test function and `"mod"` for a whole `#[cfg(test)]` region: the
//! gate diffs both, but only `fn` spans map to a test runner.

use ra_ap_syntax::ast::{HasAttrs, HasName};
use ra_ap_syntax::{AstNode, Edition, SyntaxKind, SyntaxNode, TextSize, ast};

#[derive(serde::Serialize)]
pub struct Span {
    pub path: String,
    /// "fn" — a runnable test; "mod" — a cfg(test) region (diffed but
    /// never run: `cargo test -- mod --exact` matches nothing).
    pub kind: &'static str,
    pub start: u32,
    pub end: u32,
}

pub fn spans(text: &str) -> Vec<Span> {
    let parse = ra_ap_syntax::SourceFile::parse(text, Edition::CURRENT);
    let mut out = Vec::new();
    walk(parse.tree().syntax(), "", text, &mut out);
    out.sort_by_key(|s| (s.start, s.end));
    out
}

fn line_of(text: &str, offset: TextSize) -> u32 {
    let bytes = text.as_bytes();
    let cut = usize::from(offset).min(bytes.len());
    1 + bytes[..cut].iter().filter(|b| **b == b'\n').count() as u32
}

fn is_test_attr(attr_text: &str) -> bool {
    let a = attr_text.trim();
    // `#[test]`, `#[test ...]`, and the family: `tokio::test`,
    // `quick::test`, harnesses ending in `::test]`.
    a.starts_with("#[test") || a.contains("::test]") || a.contains(" test]")
}

fn walk(node: &SyntaxNode, prefix: &str, text: &str, out: &mut Vec<Span>) {
    for child in node.children() {
        let kind = child.kind();
        // Items can live directly (source file, item lists) — cast
        // and inspect; recurse into nesting containers through their
        // ITEM_LIST (a MODULE's items are children of the list, not
        // of the module node itself).
        if matches!(
            kind,
            SyntaxKind::MODULE | SyntaxKind::IMPL | SyntaxKind::TRAIT
        ) {
            if let Some(item) = ast::Item::cast(child.clone()) {
                let attrs: Vec<String> = item.attrs().map(|a| a.to_string()).collect();
                let cfg_test = attrs.iter().any(|a| a.contains("cfg(test)"));
                if cfg_test {
                    push_span(&item, &child, prefix, text, out);
                }
                let inner_prefix = match &item {
                    ast::Item::Module(m) => match m.name() {
                        Some(n) => {
                            if prefix.is_empty() {
                                n.text().to_string()
                            } else {
                                format!("{prefix}::{}", n.text())
                            }
                        }
                        None => prefix.to_string(),
                    },
                    _ => prefix.to_string(),
                };
                // Descend through the module's item list.
                for sub in child.children() {
                    walk(&sub, &inner_prefix, text, out);
                }
                let _ = cfg_test;
                continue;
            }
        }
        if kind == SyntaxKind::FN {
            if let Some(item) = ast::Item::cast(child.clone()) {
                let attrs: Vec<String> = item.attrs().map(|a| a.to_string()).collect();
                if attrs.iter().any(|a| is_test_attr(a))
                    || attrs.iter().any(|a| a.contains("cfg(test)"))
                {
                    push_span(&item, &child, prefix, text, out);
                }
            }
        }
        // Blocks, item lists, etc. — keep descending generically.
        walk_generic(&child, prefix, text, out);
    }
}

/// Descend containers that are not themselves items.
fn walk_generic(node: &SyntaxNode, prefix: &str, text: &str, out: &mut Vec<Span>) {
    for child in node.children() {
        walk(&child, prefix, text, out);
    }
}

fn push_span(item: &ast::Item, node: &SyntaxNode, prefix: &str, text: &str, out: &mut Vec<Span>) {
    let attrs_have_test = item.attrs().any(|a| is_test_attr(&a.to_string()));
    let name = match item {
        ast::Item::Fn(f) => f.name().map(|n| n.text().to_string()),
        ast::Item::Module(m) => m.name().map(|n| n.text().to_string()),
        _ => None,
    };
    // A cfg(test) module is a region even when it holds no direct
    // #[test] (its contents are all test code); a plain item needs
    // the test attribute.
    let is_module = matches!(item, ast::Item::Module(_));
    if !attrs_have_test && !is_module {
        return;
    }
    let range = node.text_range();
    let path = match (&name, prefix.is_empty()) {
        (Some(n), true) => n.clone(),
        (Some(n), false) => format!("{prefix}::{n}"),
        (None, false) => prefix.to_string(),
        (None, true) => "test".to_string(),
    };
    out.push(Span {
        path,
        kind: if is_module { "mod" } else { "fn" },
        start: line_of(text, range.start()),
        end: line_of(text, range.end()),
    });
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: test-spans <file.rs>");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let spans = spans(&text);
    println!("{}", serde_json::to_string(&spans).unwrap());
}
