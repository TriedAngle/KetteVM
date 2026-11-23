use crate::{ByteSpan, Symbol, SymbolTable};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeList {
    pub start: u32,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    /// `name: value`
    Named,
    /// `name*: value` (a parent / trait)
    Parent,
    /// `name>> value` (a getter)
    Getter,
    /// `name<< value` (a setter)
    Setter,
    /// `[i]: value`
    Element,
}

/// Prefix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-x`
    Neg,
    /// `!x`
    Not,
}

/// Infix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
}

/// The single structured literal: `{ slots | params | body }`.
///
/// Every part is optional. Slots-only is a plain object; a `|params|`
/// marker or a non-empty body makes it callable (its body is compiled
/// as a function whose receiver is the object itself). Slot values are
/// evaluated in the enclosing scope when the object is created.
#[derive(Clone, Debug)]
pub struct ObjectParts {
    pub slots: NodeList,
    /// `None` when there is no `|...|` marker (even `{ || body }` keeps
    /// `Some` with an empty list: it is callable)
    pub params: Option<NodeList>,
    /// Trailing `...name` parameter.
    pub rest: Option<NodeId>,
    /// always a `StmtList` node, possibly empty
    pub body: NodeId,
}

#[derive(Clone, Debug)]
pub enum Node {
    Number(f64),
    String(Symbol),
    Bool(bool),
    Null,
    Self_,
    SelfContext,
    Ident(Symbol),
    /// A slot/send name reference; its span lives in the AST side table.
    Name(Symbol),

    Object(ObjectParts),
    Slot {
        kind: SlotKind,
        /// `Ident` for named/parent slots, an arbitrary expression for elements
        key: NodeId,
        value: NodeId,
    },
    /// `a, b, c: value` — one shared value for several named slots
    SlotMulti {
        kind: SlotKind,
        keys: NodeList,
        value: NodeId,
    },
    /// `[a, b, c]` — sugar for element slots
    Array {
        elements: NodeList,
    },
    /// `(a, b as c, ...)` binding pattern: read each `sources` field and
    /// bind it to the matching `names` entry.
    PatternFields {
        sources: NodeList,
        names: NodeList,
    },
    /// `[a, b, ...tail]` binding pattern: bind positions, with an optional
    /// fresh-array rest.
    PatternArray {
        elements: NodeList,
        rest: Option<NodeId>,
    },
    StmtList {
        stmts: NodeList,
    },

    /// `recv.name` slot read
    Get {
        recv: NodeId,
        name: NodeId,
    },
    /// `recv[key]` / `recv.0` element read
    Index {
        recv: NodeId,
        key: NodeId,
    },
    /// `recv.name(args)`
    Send {
        recv: NodeId,
        name: NodeId,
        args: NodeList,
    },
    /// `callee(args)` — the callee itself is the receiver (`self`)
    Call {
        callee: NodeId,
        args: NodeList,
    },

    Assign {
        target: NodeId,
        value: NodeId,
    },
    /// prefix `-` / `!`
    Unary {
        op: UnaryOp,
        op_span: ByteSpan,
        expr: NodeId,
    },
    Binary {
        op: BinaryOp,
        op_span: ByteSpan,
        lhs: NodeId,
        rhs: NodeId,
    },
    /// `return expr` returns from the enclosing block
    Return {
        value: NodeId,
    },
    /// `break` / `break expr` exits the innermost loop
    Break {
        value: Option<NodeId>,
    },
    Throw {
        value: NodeId,
    },
    Try {
        body: NodeId,
        param: NodeId,
        handler: NodeId,
    },

    Let {
        pattern: NodeId,
        init: NodeId,
    },
    ExprStmt {
        expr: NodeId,
    },

    /// `if cond { A } else { B }`; the branches are `StmtList`s compiled
    /// inline in the enclosing function
    If {
        cond: NodeId,
        then: NodeId,
        else_: Option<NodeId>,
    },

    /// `while cond { body }`; the body is a `StmtList` compiled inline and
    /// the whole expression evaluates to null.
    While {
        cond: NodeId,
        body: NodeId,
    },

    /// `for binding in iterable { body }`; the binding is a pattern in the
    /// enclosing scope and the body is a `StmtList` compiled inline.
    For {
        keyed: bool,
        pattern: NodeId,
        iterable: NodeId,
        body: NodeId,
    },
}

pub struct Ast {
    nodes: Vec<Node>,
    spans: Vec<ByteSpan>,
    /// shared pool backing every NodeList
    lists: Vec<NodeId>,
    strings: SymbolTable,
    /// root `StmtList` of the parsed unit, set by the parser
    root: Option<NodeId>,
    /// interned spelling of `self`, used for hidden self bindings
    self_symbol: Option<Symbol>,
    /// protocol names interned by the parser for `for` lowering
    iterator_symbol: Option<Symbol>,
    has_next_symbol: Option<Symbol>,
    next_symbol: Option<Symbol>,
    next_keyed_symbol: Option<Symbol>,
    size_symbol: Option<Symbol>,
}

impl Default for Ast {
    fn default() -> Self {
        Self::new()
    }
}

impl Ast {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            spans: Vec::new(),
            lists: Vec::new(),
            strings: SymbolTable::default(),
            root: None,
            self_symbol: None,
            iterator_symbol: None,
            has_next_symbol: None,
            next_symbol: None,
            next_keyed_symbol: None,
            size_symbol: None,
        }
    }

    pub fn set_root(&mut self, root: NodeId) {
        self.root = Some(root);
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    pub fn self_symbol(&self) -> Option<Symbol> {
        self.self_symbol
    }

    pub fn set_self_symbol(&mut self, symbol: Symbol) {
        self.self_symbol = Some(symbol);
    }

    pub fn set_iteration_symbols(
        &mut self,
        iterator: Symbol,
        has_next: Symbol,
        next: Symbol,
        next_keyed: Symbol,
        size: Symbol,
    ) {
        self.iterator_symbol = Some(iterator);
        self.has_next_symbol = Some(has_next);
        self.next_symbol = Some(next);
        self.next_keyed_symbol = Some(next_keyed);
        self.size_symbol = Some(size);
    }

    pub fn iterator_symbol(&self) -> Symbol {
        self.iterator_symbol
            .expect("parser interns the iteration protocol names")
    }

    pub fn has_next_symbol(&self) -> Symbol {
        self.has_next_symbol
            .expect("parser interns the iteration protocol names")
    }

    pub fn next_symbol(&self) -> Symbol {
        self.next_symbol
            .expect("parser interns the iteration protocol names")
    }

    pub fn next_keyed_symbol(&self) -> Symbol {
        self.next_keyed_symbol
            .expect("parser interns the iteration protocol names")
    }

    pub fn size_symbol(&self) -> Symbol {
        self.size_symbol
            .expect("parser interns the iteration protocol names")
    }

    pub fn add(&mut self, node: Node, span: ByteSpan) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        self.spans.push(span);
        id
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn span(&self, id: NodeId) -> ByteSpan {
        self.spans[id.0 as usize]
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn list(&mut self, items: &[NodeId]) -> NodeList {
        let start = self.lists.len() as u32;
        self.lists.extend_from_slice(items);
        NodeList {
            start,
            len: items.len() as u32,
        }
    }

    pub fn list_items(&self, list: NodeList) -> &[NodeId] {
        &self.lists[list.start as usize..(list.start + list.len) as usize]
    }

    /// The statement list of an object body (an empty slice for
    /// non-`StmtList` bodies, which do not occur).
    pub fn object_body_stmts(&self, parts: &ObjectParts) -> &[NodeId] {
        match self.node(parts.body) {
            Node::StmtList { stmts } => self.list_items(*stmts),
            _ => &[],
        }
    }

    /// Whether a `{ ... }` literal is compiled as a function: it carries a
    /// `|params|` marker or a non-empty body.
    pub fn is_closure(&self, id: NodeId) -> bool {
        match self.node(id) {
            Node::Object(parts) => {
                parts.params.is_some() || !self.object_body_stmts(parts).is_empty()
            }
            _ => false,
        }
    }

    pub fn intern(&mut self, s: &[u8]) -> Symbol {
        self.strings.intern(s)
    }

    pub fn symbol(&self, sym: Symbol) -> &[u8] {
        self.strings.get(sym)
    }

    pub fn set_symbol_table(&mut self, strings: SymbolTable) {
        self.strings = strings;
    }
}
