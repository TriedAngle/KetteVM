#![allow(clippy::missing_safety_doc)]

use core::ptr;
use std::ffi::CStr;

use kette_parser::ParseError;
use kette_syntax::{Child, NodeId, SyntaxKind, TokenId, Tree};

const TOKEN_BIT: u32 = 0x8000_0000;

pub struct KetteTree {
    tree: Tree,
    errors: Vec<ParseError>,
}

unsafe fn as_tree<'a>(tree: *const KetteTree) -> &'a KetteTree {
    unsafe { &*tree }
}

fn parse_source(source: &str) -> *mut KetteTree {
    let parsed = kette_parser::parse(source);
    Box::into_raw(Box::new(KetteTree {
        tree: parsed.tree,
        errors: parsed.errors,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_parse(source: *const std::ffi::c_char) -> *mut KetteTree {
    let Ok(source) = (unsafe { CStr::from_ptr(source) }).to_str() else {
        return ptr::null_mut();
    };
    parse_source(source)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_parse_file(path: *const std::ffi::c_char) -> *mut KetteTree {
    let Ok(path) = (unsafe { CStr::from_ptr(path) }).to_str() else {
        return ptr::null_mut();
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return ptr::null_mut();
    };
    parse_source(&source)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_tree_free(tree: *mut KetteTree) {
    if tree.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(tree) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_tree_source_ptr(tree: *const KetteTree) -> *const u8 {
    unsafe { as_tree(tree) }.tree.source().as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_tree_source_len(tree: *const KetteTree) -> usize {
    unsafe { as_tree(tree) }.tree.source().len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_tree_root(tree: *const KetteTree) -> u32 {
    unsafe { as_tree(tree) }.tree.root_id().0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_tree_node_count(tree: *const KetteTree) -> u32 {
    unsafe { as_tree(tree) }.tree.nodes().len() as u32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_tree_token_count(tree: *const KetteTree) -> u32 {
    unsafe { as_tree(tree) }.tree.tokens().len() as u32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_tree_error_count(tree: *const KetteTree) -> u32 {
    unsafe { as_tree(tree) }.errors.len() as u32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_error_start(tree: *const KetteTree, index: u32) -> u32 {
    unsafe { as_tree(tree) }
        .errors
        .get(index as usize)
        .map_or(0, |error| error.range.start)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_error_end(tree: *const KetteTree, index: u32) -> u32 {
    unsafe { as_tree(tree) }
        .errors
        .get(index as usize)
        .map_or(0, |error| error.range.end)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_error_message_ptr(tree: *const KetteTree, index: u32) -> *const u8 {
    unsafe { as_tree(tree) }
        .errors
        .get(index as usize)
        .map_or(ptr::null(), |error| error.message.as_ptr())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_error_message_len(tree: *const KetteTree, index: u32) -> usize {
    unsafe { as_tree(tree) }
        .errors
        .get(index as usize)
        .map_or(0, |error| error.message.len())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_kind(tree: *const KetteTree, node: u32) -> u16 {
    unsafe { as_tree(tree) }.tree.node(NodeId(node)).kind as u16
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_start(tree: *const KetteTree, node: u32) -> u32 {
    unsafe { as_tree(tree) }.tree.node(NodeId(node)).range.start
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_end(tree: *const KetteTree, node: u32) -> u32 {
    unsafe { as_tree(tree) }.tree.node(NodeId(node)).range.end
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_parent(tree: *const KetteTree, node: u32) -> u32 {
    unsafe { as_tree(tree) }.tree.node(NodeId(node)).parent.0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_child_count(tree: *const KetteTree, node: u32) -> u32 {
    unsafe { as_tree(tree) }.tree.node(NodeId(node)).child_count
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_child(tree: *const KetteTree, node: u32, index: u32) -> u32 {
    let tree = unsafe { as_tree(tree) };
    match tree.tree.children(NodeId(node)).get(index as usize) {
        Some(Child::Node(child)) => child.0,
        Some(Child::Token(child)) => child.0 | TOKEN_BIT,
        None => u32::MAX,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_text_ptr(tree: *const KetteTree, node: u32) -> *const u8 {
    unsafe { as_tree(tree) }
        .tree
        .node_text(NodeId(node))
        .as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_node_text_len(tree: *const KetteTree, node: u32) -> usize {
    unsafe { as_tree(tree) }.tree.node_text(NodeId(node)).len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_token_kind(tree: *const KetteTree, token: u32) -> u16 {
    unsafe { as_tree(tree) }.tree.token(TokenId(token)).kind as u16
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_token_start(tree: *const KetteTree, token: u32) -> u32 {
    unsafe { as_tree(tree) }
        .tree
        .token(TokenId(token))
        .range
        .start
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_token_end(tree: *const KetteTree, token: u32) -> u32 {
    unsafe { as_tree(tree) }
        .tree
        .token(TokenId(token))
        .range
        .end
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_token_parent(tree: *const KetteTree, token: u32) -> u32 {
    unsafe { as_tree(tree) }.tree.token(TokenId(token)).parent.0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_token_text_ptr(tree: *const KetteTree, token: u32) -> *const u8 {
    unsafe { as_tree(tree) }
        .tree
        .token_text(TokenId(token))
        .as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_token_text_len(tree: *const KetteTree, token: u32) -> usize {
    unsafe { as_tree(tree) }
        .tree
        .token_text(TokenId(token))
        .len()
}

#[unsafe(no_mangle)]
pub extern "C" fn kette_kind_count() -> u32 {
    SyntaxKind::ALL.len() as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn kette_kind_at(index: u32) -> u16 {
    SyntaxKind::ALL
        .get(index as usize)
        .map_or(u16::MAX, |kind| *kind as u16)
}

#[unsafe(no_mangle)]
pub extern "C" fn kette_kind_name_ptr(kind: u16) -> *const u8 {
    SyntaxKind::from_u16(kind).map_or(ptr::null(), |kind| kind.name().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn kette_kind_name_len(kind: u16) -> usize {
    SyntaxKind::from_u16(kind).map_or(0, |kind| kind.name().len())
}

#[unsafe(no_mangle)]
pub extern "C" fn kette_kind_is_trivia(kind: u16) -> u8 {
    SyntaxKind::from_u16(kind).is_some_and(SyntaxKind::is_trivia) as u8
}

/// A resolved symbol table for one parsed tree.
pub struct KetteResolved {
    resolved: kette_syntax::resolve::Resolved,
}

unsafe fn as_resolved<'a>(resolved: *const KetteResolved) -> &'a KetteResolved {
    unsafe { &*resolved }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve(tree: *const KetteTree) -> *mut KetteResolved {
    let tree = unsafe { as_tree(tree) };
    Box::into_raw(Box::new(KetteResolved {
        resolved: kette_syntax::resolve::resolve(&tree.tree),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_free(resolved: *mut KetteResolved) {
    if resolved.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(resolved) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_of_token(
    resolved: *const KetteResolved,
    token: u32,
) -> i64 {
    unsafe { as_resolved(resolved) }
        .resolved
        .binding_of_token(TokenId(token))
        .map_or(-1, |binding| binding.0 as i64)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_resolution(resolved: *const KetteResolved, node: u32) -> u8 {
    match unsafe { as_resolved(resolved) }
        .resolved
        .resolution(NodeId(node))
    {
        Some(kette_syntax::resolve::Resolution::Local(_)) => 1,
        Some(kette_syntax::resolve::Resolution::Upvar(_)) => 2,
        Some(kette_syntax::resolve::Resolution::Global) => 3,
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_of_node(
    resolved: *const KetteResolved,
    node: u32,
) -> i64 {
    match unsafe { as_resolved(resolved) }
        .resolved
        .resolution(NodeId(node))
    {
        Some(kette_syntax::resolve::Resolution::Local(binding))
        | Some(kette_syntax::resolve::Resolution::Upvar(binding)) => binding.0 as i64,
        _ => -1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_name_ptr(
    resolved: *const KetteResolved,
    binding: u32,
) -> *const u8 {
    let resolved = unsafe { as_resolved(resolved) };
    let binding = resolved
        .resolved
        .binding(kette_syntax::resolve::BindingId(binding));
    resolved.resolved.name(binding.name).as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_name_len(
    resolved: *const KetteResolved,
    binding: u32,
) -> usize {
    let resolved = unsafe { as_resolved(resolved) };
    let binding = resolved
        .resolved
        .binding(kette_syntax::resolve::BindingId(binding));
    resolved.resolved.name(binding.name).len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_kind(
    resolved: *const KetteResolved,
    binding: u32,
) -> u8 {
    use kette_syntax::resolve::BindingKind;
    let binding = unsafe { as_resolved(resolved) }
        .resolved
        .binding(kette_syntax::resolve::BindingId(binding));
    match binding.kind {
        BindingKind::Let => 0,
        BindingKind::Param => 1,
        BindingKind::Rest => 2,
        BindingKind::CatchParam => 3,
        BindingKind::ForBinding => 4,
        BindingKind::SelfParam => 5,
        BindingKind::Import => 6,
        BindingKind::Export => 7,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_start(
    resolved: *const KetteResolved,
    binding: u32,
) -> u32 {
    unsafe { as_resolved(resolved) }
        .resolved
        .binding(kette_syntax::resolve::BindingId(binding))
        .declaration
        .start
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_end(
    resolved: *const KetteResolved,
    binding: u32,
) -> u32 {
    unsafe { as_resolved(resolved) }
        .resolved
        .binding(kette_syntax::resolve::BindingId(binding))
        .declaration
        .end
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_count(resolved: *const KetteResolved) -> u32 {
    unsafe { as_resolved(resolved) }.resolved.bindings().len() as u32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kette_resolve_binding_top_level(
    resolved: *const KetteResolved,
    binding: u32,
) -> u8 {
    use kette_syntax::resolve::ScopeKind;
    let resolved = unsafe { as_resolved(resolved) };
    let binding = resolved
        .resolved
        .binding(kette_syntax::resolve::BindingId(binding));
    let scope = resolved.resolved.scope(binding.scope);
    match scope.kind {
        ScopeKind::Script => 1,
        ScopeKind::Module | ScopeKind::Function { .. } => 0,
    }
}
