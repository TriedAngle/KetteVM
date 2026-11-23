#![forbid(unsafe_code)]

pub mod ast;
pub mod kind;
pub mod name;
pub mod ops;
pub mod resolve;
pub mod validate;

pub use name::{NameId, NameTable, SELF_NAME};

use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

pub use kind::SyntaxKind;

/// A half-open `[start, end)` range of UTF-8 byte offsets.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub const fn empty(at: u32) -> Self {
        Self { start: at, end: at }
    }

    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenId(pub u32);

/// Sentinel used by the C-facing layout instead of `Option<NodeId>`.
pub const NO_NODE: NodeId = NodeId(u32::MAX);

/// A leaf. `source[range]` is its text; trivia is just another token kind.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: SyntaxKind,
    pub range: TextRange,
    pub parent: NodeId,
}

/// An internal node. Children are a contiguous slice of [`Tree::children`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub kind: SyntaxKind,
    pub range: TextRange,
    pub parent: NodeId,
    pub first_child: u32,
    pub child_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum Child {
    Node(NodeId),
    Token(TokenId),
}

/// Immutable, lossless syntax tree.
#[derive(Debug)]
pub struct Tree {
    source: Arc<str>,
    root: NodeId,
    nodes: Vec<Node>,
    children: Vec<Child>,
    tokens: Vec<Token>,
    names: Arc<NameTable>,
    token_names: Vec<Option<NameId>>,
}

impl Tree {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn root(&self) -> SyntaxNode<'_> {
        SyntaxNode::new(self, self.root)
    }

    pub fn root_id(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn token(&self, id: TokenId) -> &Token {
        &self.tokens[id.0 as usize]
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub fn names(&self) -> &NameTable {
        &self.names
    }

    pub fn name_table(&self) -> &Arc<NameTable> {
        &self.names
    }

    pub fn token_name(&self, id: TokenId) -> Option<NameId> {
        self.token_names[id.0 as usize]
    }

    pub fn name(&self, id: NameId) -> &str {
        self.names.name(id)
    }

    pub fn children(&self, id: NodeId) -> &[Child] {
        let node = self.node(id);
        let start = node.first_child as usize;
        let end = start + node.child_count as usize;
        &self.children[start..end]
    }

    pub fn text(&self, range: TextRange) -> &str {
        &self.source[range.start as usize..range.end as usize]
    }

    pub fn token_text(&self, id: TokenId) -> &str {
        self.text(self.token(id).range)
    }

    pub fn node_text(&self, id: NodeId) -> &str {
        self.text(self.node(id).range)
    }

    /// Reconstructs the source by walking the tree.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_node(self.root, &mut out);
        out
    }

    fn render_node(&self, id: NodeId, out: &mut String) {
        for child in self.children(id) {
            match *child {
                Child::Node(node) => self.render_node(node, out),
                Child::Token(token) => out.push_str(self.token_text(token)),
            }
        }
    }

    pub fn is_lossless(&self) -> bool {
        self.render() == *self.source
    }

    /// Debug-time structural check.
    pub fn validate(&self) -> Result<(), &'static str> {
        let root = self.node(self.root);
        if root.parent != NO_NODE {
            return Err("root must not have a parent");
        }
        if root.range.start != 0 || root.range.end != self.source.len() as u32 {
            return Err("root range does not cover the source");
        }

        for index in 0..self.nodes.len() {
            let id = NodeId(index as u32);
            let node = self.node(id);
            let mut last_end = node.range.start;
            for child in self.children(id) {
                let (range, parent) = match *child {
                    Child::Node(child) => {
                        let child = self.node(child);
                        (child.range, child.parent)
                    }
                    Child::Token(child) => {
                        let child = self.token(child);
                        (child.range, child.parent)
                    }
                };
                if parent != id {
                    return Err("child parent does not match its container");
                }
                if !node.range.contains(range) {
                    return Err("child range escapes its parent");
                }
                if range.start < last_end {
                    return Err("sibling ranges overlap");
                }
                last_end = range.end;
            }
        }

        let mut end = 0;
        for token in &self.tokens {
            if token.range.start < end {
                return Err("token ranges overlap");
            }
            end = token.range.end;
        }
        if end != self.source.len() as u32 {
            return Err("tokens do not cover the source");
        }

        if !self.is_lossless() {
            return Err("render does not match the source");
        }
        Ok(())
    }
}

/// A cursor into the tree. Cheap to copy; no allocation.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SyntaxNode<'a> {
    tree: &'a Tree,
    id: NodeId,
}

impl<'a> SyntaxNode<'a> {
    pub fn new(tree: &'a Tree, id: NodeId) -> Self {
        Self { tree, id }
    }

    pub fn tree(self) -> &'a Tree {
        self.tree
    }

    pub fn id(self) -> NodeId {
        self.id
    }

    pub fn kind(self) -> SyntaxKind {
        self.tree.node(self.id).kind
    }

    pub fn text_range(self) -> TextRange {
        self.tree.node(self.id).range
    }

    pub fn text(self) -> &'a str {
        self.tree.text(self.text_range())
    }

    pub fn parent(self) -> Option<Self> {
        let parent = self.tree.node(self.id).parent;
        (parent != NO_NODE).then(|| Self::new(self.tree, parent))
    }

    pub fn index_in_parent(self) -> Option<usize> {
        let parent = self.parent()?;
        parent
            .tree
            .children(parent.id)
            .iter()
            .position(|child| matches!(child, Child::Node(id) if *id == self.id))
    }

    pub fn prev_sibling(self) -> Option<Self> {
        let parent = self.parent()?;
        let index = self.index_in_parent()?;
        parent.tree.children(parent.id)[..index]
            .iter()
            .rev()
            .find_map(|child| match child {
                Child::Node(id) => Some(Self::new(self.tree, *id)),
                Child::Token(_) => None,
            })
    }

    pub fn next_sibling(self) -> Option<Self> {
        let parent = self.parent()?;
        let index = self.index_in_parent()?;
        parent.tree.children(parent.id)[index + 1..]
            .iter()
            .find_map(|child| match child {
                Child::Node(id) => Some(Self::new(self.tree, *id)),
                Child::Token(_) => None,
            })
    }

    pub fn ancestors(self) -> impl Iterator<Item = Self> + 'a {
        std::iter::successors(self.parent(), |node| node.parent())
    }

    pub fn descendants(self) -> Descendants<'a> {
        Descendants { stack: vec![self] }
    }

    pub fn children_with_tokens(self) -> impl Iterator<Item = SyntaxElement<'a>> + 'a {
        self.tree
            .children(self.id)
            .iter()
            .map(move |child| match *child {
                Child::Node(id) => SyntaxElement::Node(Self::new(self.tree, id)),
                Child::Token(id) => SyntaxElement::Token(SyntaxToken::new(self.tree, id)),
            })
    }

    pub fn child_nodes(self) -> impl Iterator<Item = Self> + 'a {
        self.children_with_tokens().filter_map(|child| match child {
            SyntaxElement::Node(node) => Some(node),
            SyntaxElement::Token(_) => None,
        })
    }

    pub fn tokens(self) -> impl Iterator<Item = SyntaxToken<'a>> + 'a {
        self.children_with_tokens().filter_map(|child| match child {
            SyntaxElement::Node(_) => None,
            SyntaxElement::Token(token) => Some(token),
        })
    }

    pub fn child_of_kind(self, kind: SyntaxKind) -> Option<Self> {
        self.child_nodes().find(|node| node.kind() == kind)
    }

    pub fn child_token_of_kind(self, kind: SyntaxKind) -> Option<SyntaxToken<'a>> {
        self.tokens().find(|token| token.kind() == kind)
    }

    pub fn first_token(self) -> Option<SyntaxToken<'a>> {
        for child in self.tree.children(self.id) {
            match *child {
                Child::Token(id) => return Some(SyntaxToken::new(self.tree, id)),
                Child::Node(id) => {
                    if let Some(token) = Self::new(self.tree, id).first_token() {
                        return Some(token);
                    }
                }
            }
        }
        None
    }

    pub fn last_token(self) -> Option<SyntaxToken<'a>> {
        for child in self.tree.children(self.id).iter().rev() {
            match *child {
                Child::Token(id) => return Some(SyntaxToken::new(self.tree, id)),
                Child::Node(id) => {
                    if let Some(token) = Self::new(self.tree, id).last_token() {
                        return Some(token);
                    }
                }
            }
        }
        None
    }

    pub fn child_as<N: AstNode<'a>>(self) -> Option<N> {
        self.children_as().next()
    }

    pub fn children_as<N: AstNode<'a>>(self) -> AstChildren<'a, N> {
        AstChildren::new(self.tree, self.id)
    }
}

impl PartialEq for SyntaxNode<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.tree, other.tree) && self.id == other.id
    }
}

impl Eq for SyntaxNode<'_> {}

impl Hash for SyntaxNode<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.tree as *const Tree).hash(state);
        self.id.hash(state);
    }
}

/// Pre-order node traversal.
pub struct Descendants<'a> {
    stack: Vec<SyntaxNode<'a>>,
}

impl<'a> Iterator for Descendants<'a> {
    type Item = SyntaxNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        let mut children: Vec<_> = node.child_nodes().collect();
        children.reverse();
        self.stack.extend(children);
        Some(node)
    }
}

/// A token cursor. Tokens are leaves; trivia is a token kind.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SyntaxToken<'a> {
    tree: &'a Tree,
    id: TokenId,
}

impl<'a> SyntaxToken<'a> {
    pub fn new(tree: &'a Tree, id: TokenId) -> Self {
        Self { tree, id }
    }

    pub fn tree(self) -> &'a Tree {
        self.tree
    }

    pub fn id(self) -> TokenId {
        self.id
    }

    pub fn kind(self) -> SyntaxKind {
        self.tree.token(self.id).kind
    }

    pub fn text_range(self) -> TextRange {
        self.tree.token(self.id).range
    }

    pub fn text(self) -> &'a str {
        self.tree.token_text(self.id)
    }

    pub fn parent(self) -> Option<SyntaxNode<'a>> {
        let parent = self.tree.token(self.id).parent;
        (parent != NO_NODE).then(|| SyntaxNode::new(self.tree, parent))
    }

    pub fn index_in_parent(self) -> Option<usize> {
        let parent = self.parent()?;
        parent
            .tree
            .children(parent.id)
            .iter()
            .position(|child| matches!(child, Child::Token(id) if *id == self.id))
    }
}

impl PartialEq for SyntaxToken<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.tree, other.tree) && self.id == other.id
    }
}

impl Eq for SyntaxToken<'_> {}

impl Hash for SyntaxToken<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.tree as *const Tree).hash(state);
        self.id.hash(state);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum SyntaxElement<'a> {
    Node(SyntaxNode<'a>),
    Token(SyntaxToken<'a>),
}

impl<'a> SyntaxElement<'a> {
    pub fn kind(self) -> SyntaxKind {
        match self {
            Self::Node(node) => node.kind(),
            Self::Token(token) => token.kind(),
        }
    }

    pub fn text_range(self) -> TextRange {
        match self {
            Self::Node(node) => node.text_range(),
            Self::Token(token) => token.text_range(),
        }
    }

    pub fn text(self) -> &'a str {
        match self {
            Self::Node(node) => node.text(),
            Self::Token(token) => token.text(),
        }
    }

    pub fn parent(self) -> Option<SyntaxNode<'a>> {
        match self {
            Self::Node(node) => node.parent(),
            Self::Token(token) => token.parent(),
        }
    }

    pub fn as_node(self) -> Option<SyntaxNode<'a>> {
        match self {
            Self::Node(node) => Some(node),
            Self::Token(_) => None,
        }
    }

    pub fn as_token(self) -> Option<SyntaxToken<'a>> {
        match self {
            Self::Node(_) => None,
            Self::Token(token) => Some(token),
        }
    }
}

impl PartialEq for SyntaxElement<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Node(a), Self::Node(b)) => a == b,
            (Self::Token(a), Self::Token(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for SyntaxElement<'_> {}

impl Hash for SyntaxElement<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Node(node) => {
                0u8.hash(state);
                node.hash(state);
            }
            Self::Token(token) => {
                1u8.hash(state);
                token.hash(state);
            }
        }
    }
}

/// Zero-copy typed view over a node.
pub trait AstNode<'a>: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;

    fn cast(node: SyntaxNode<'a>) -> Option<Self>;

    fn syntax(&self) -> SyntaxNode<'a>;

    fn kind(&self) -> SyntaxKind {
        self.syntax().kind()
    }

    fn text_range(&self) -> TextRange {
        self.syntax().text_range()
    }

    fn text(&self) -> &'a str {
        self.syntax().text()
    }

    fn parent(&self) -> Option<SyntaxNode<'a>> {
        self.syntax().parent()
    }
}

/// Iterator over direct child nodes that cast to `N`.
#[derive(Clone)]
pub struct AstChildren<'a, N: AstNode<'a>> {
    tree: &'a Tree,
    iter: std::slice::Iter<'a, Child>,
    marker: PhantomData<N>,
}

impl<'a, N: AstNode<'a>> AstChildren<'a, N> {
    pub fn new(tree: &'a Tree, parent: NodeId) -> Self {
        Self {
            tree,
            iter: tree.children(parent).iter(),
            marker: PhantomData,
        }
    }
}

impl<'a, N: AstNode<'a>> Iterator for AstChildren<'a, N> {
    type Item = N;

    fn next(&mut self) -> Option<Self::Item> {
        for child in self.iter.by_ref() {
            if let Child::Node(id) = *child
                && let Some(node) = N::cast(SyntaxNode::new(self.tree, id))
            {
                return Some(node);
            }
        }
        None
    }
}

impl<'a, N: AstNode<'a>> DoubleEndedIterator for AstChildren<'a, N> {
    fn next_back(&mut self) -> Option<Self::Item> {
        while let Some(child) = self.iter.next_back() {
            if let Child::Node(id) = *child
                && let Some(node) = N::cast(SyntaxNode::new(self.tree, id))
            {
                return Some(node);
            }
        }
        None
    }
}

struct Frame {
    id: NodeId,
    start: u32,
    pending_start: usize,
}

/// Opaque position for wrapping already-parsed children.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Checkpoint {
    frame: usize,
    pending: usize,
    start: u32,
}

/// Builds a tree bottom-up with an explicit open-node stack.
pub struct Builder {
    source: Arc<str>,
    nodes: Vec<Node>,
    /// Direct children of the node currently being finished.
    pending: Vec<Child>,
    /// Flattened child arena; each node owns a contiguous slice.
    children: Vec<Child>,
    tokens: Vec<Token>,
    names: Option<Arc<NameTable>>,
    token_names: Vec<Option<NameId>>,
    stack: Vec<Frame>,
    root: Option<NodeId>,
    pos: u32,
}

impl Builder {
    pub fn new(source: Arc<str>) -> Self {
        Self {
            source,
            nodes: Vec::new(),
            pending: Vec::new(),
            children: Vec::new(),
            tokens: Vec::new(),
            names: None,
            token_names: Vec::new(),
            stack: Vec::new(),
            root: None,
            pos: 0,
        }
    }

    pub fn set_names(&mut self, names: Arc<NameTable>) {
        self.names = Some(names);
    }

    pub fn position(&self) -> u32 {
        self.pos
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Closes open nodes down to `depth`.
    pub fn close_to(&mut self, depth: usize) {
        while self.stack.len() > depth {
            let end = self.pos;
            self.finish_node(end);
        }
    }

    pub fn checkpoint(&self) -> Checkpoint {
        assert!(!self.stack.is_empty(), "checkpoint outside any node");
        Checkpoint {
            frame: self.stack.len() - 1,
            pending: self.pending.len(),
            start: self.pos,
        }
    }

    pub fn start_node(&mut self, kind: SyntaxKind, start: u32) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            kind,
            range: TextRange::empty(start),
            parent: NO_NODE,
            first_child: 0,
            child_count: 0,
        });
        self.stack.push(Frame {
            id,
            start,
            pending_start: self.pending.len(),
        });
        id
    }

    pub fn start_node_here(&mut self, kind: SyntaxKind) -> NodeId {
        self.start_node(kind, self.pos)
    }

    /// Wraps the children parsed since `checkpoint`.
    pub fn start_node_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) -> NodeId {
        let frame_index = self
            .stack
            .len()
            .checked_sub(1)
            .expect("checkpoint outside any node");
        assert_eq!(
            checkpoint.frame, frame_index,
            "checkpoint from another frame"
        );
        assert!(
            checkpoint.pending <= self.pending.len(),
            "checkpoint is no longer valid"
        );

        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            kind,
            range: TextRange::empty(checkpoint.start),
            parent: NO_NODE,
            first_child: 0,
            child_count: 0,
        });
        self.stack.push(Frame {
            id,
            start: checkpoint.start,
            pending_start: checkpoint.pending,
        });
        id
    }

    pub fn finish_node(&mut self, end: u32) -> NodeId {
        let frame = self.stack.pop().expect("finish_node without start_node");
        let first_child = self.children.len() as u32;
        let child_count = (self.pending.len() - frame.pending_start) as u32;
        self.children
            .extend_from_slice(&self.pending[frame.pending_start..]);
        self.pending.truncate(frame.pending_start);

        let node = &mut self.nodes[frame.id.0 as usize];
        let end = end.max(frame.start);
        node.range = TextRange::new(frame.start, end);
        node.first_child = first_child;
        node.child_count = child_count;
        self.pos = self.pos.max(end);

        if self.stack.is_empty() {
            debug_assert!(self.root.is_none(), "multiple root nodes");
            self.root = Some(frame.id);
        } else {
            self.pending.push(Child::Node(frame.id));
        }
        frame.id
    }

    pub fn finish_node_here(&mut self) -> NodeId {
        self.finish_node(self.pos)
    }

    pub fn token(&mut self, kind: SyntaxKind, range: TextRange) -> TokenId {
        self.token_named(kind, range, None)
    }

    pub fn token_named(
        &mut self,
        kind: SyntaxKind,
        range: TextRange,
        name: Option<NameId>,
    ) -> TokenId {
        let id = TokenId(self.tokens.len() as u32);
        self.tokens.push(Token {
            kind,
            range,
            parent: NO_NODE,
        });
        self.token_names.push(name);
        assert!(!self.stack.is_empty(), "token outside any syntax node");
        self.pending.push(Child::Token(id));
        self.pos = self.pos.max(range.end);
        id
    }

    pub fn missing_token(&mut self, kind: SyntaxKind, at: u32) -> TokenId {
        self.token(kind, TextRange::empty(at))
    }

    pub fn error_token(&mut self, range: TextRange) -> TokenId {
        self.token(SyntaxKind::Error, range)
    }

    pub fn finish(mut self) -> Tree {
        assert!(self.stack.is_empty(), "unclosed syntax nodes");
        assert!(self.pending.is_empty(), "children outside the root");
        let root = self.root.expect("no root node");

        for index in 0..self.nodes.len() {
            let node = self.nodes[index];
            let end = node.first_child + node.child_count;
            for child_index in node.first_child..end {
                match self.children[child_index as usize] {
                    Child::Node(id) => {
                        self.nodes[id.0 as usize].parent = NodeId(index as u32);
                    }
                    Child::Token(id) => {
                        self.tokens[id.0 as usize].parent = NodeId(index as u32);
                    }
                }
            }
        }

        let tree = Tree {
            source: self.source,
            root,
            nodes: self.nodes,
            children: self.children,
            tokens: self.tokens,
            names: self.names.expect("builder names were not set"),
            token_names: self.token_names,
        };
        debug_assert!(
            tree.validate().is_ok(),
            "invalid tree: {:?}",
            tree.validate()
        );
        tree
    }
}
