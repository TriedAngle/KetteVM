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
