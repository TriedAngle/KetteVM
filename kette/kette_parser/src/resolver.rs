use crate::{Ast, Node, NodeId, SlotKind, plumbing::ByteSpan, plumbing::Symbol};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// the top-level program body
    Script,
    /// a callable object literal's body
    Block,
}

#[derive(Clone, Debug)]
pub struct Declaration {
    pub name: Symbol,
    pub span: ByteSpan,
}

pub struct ScopeInfo {
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    pub decls: Vec<Declaration>,
    /// A direct `eval` can reach this scope's bindings by name, so they must
    /// live in a runtime context. Set on the eval site and all its ancestors.
    pub direct_eval: bool,
    /// The root script scope owns a lexical context even without captures.
    pub force_context: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// declared in the current block scope
    Local { scope: ScopeId, decl: u32 },
    /// declared in an enclosing callable scope, `depth` contexts up
    Capture {
        scope: ScopeId,
        decl: u32,
        depth: u32,
    },
    /// unbound: a runtime lookup
    Global,
}

pub struct Resolved {
    /// parallel to the node arena; `Some` on identifier uses only
    resolutions: Vec<Option<Resolution>>,
    /// scope owned by a callable object / the root `StmtList` node
    node_scopes: Vec<Option<ScopeId>>,
    scopes: Vec<ScopeInfo>,
    /// per scope: whether it pushes a runtime context
    creates_context: Vec<bool>,
}

impl Resolved {
    pub fn resolution(&self, node: NodeId) -> Option<Resolution> {
        self.resolutions[node.0 as usize]
    }

    /// The scope a callable object node (or the root statement list) owns.
    pub fn scope_of(&self, node: NodeId) -> Option<ScopeId> {
        self.node_scopes[node.0 as usize]
    }

    pub fn scope(&self, id: ScopeId) -> &ScopeInfo {
        &self.scopes[id.0 as usize]
    }

    /// Whether the scope pushes a context at entry. Scopes that capture
    /// nothing and run no direct `eval` reuse the closure's context.
    pub fn creates_context(&self, scope: ScopeId) -> bool {
        self.creates_context[scope.0 as usize]
    }
}

pub fn resolve(ast: &Ast) -> Resolved {
    let mut resolver = Resolver {
        ast,
        resolutions: vec![None; ast.node_count()],
        capture_scopes: vec![None; ast.node_count()],
        node_scopes: vec![None; ast.node_count()],
        scopes: Vec::new(),
        stack: Vec::new(),
    };
    if let Some(root) = ast.root() {
        resolver.resolve_root(root);
    }
    let creates_context = resolver.finalize_contexts();
    Resolved {
        resolutions: resolver.resolutions,
        node_scopes: resolver.node_scopes,
        scopes: resolver.scopes,
        creates_context,
    }
}

struct Resolver<'a> {
    ast: &'a Ast,
    resolutions: Vec<Option<Resolution>>,
    /// the use-site scope of every capture, for the context-hop rewrite
    capture_scopes: Vec<Option<ScopeId>>,
    node_scopes: Vec<Option<ScopeId>>,
    scopes: Vec<ScopeInfo>,
    /// innermost scope last
    stack: Vec<ScopeId>,
}

impl Resolver<'_> {
    fn push_scope(&mut self, kind: ScopeKind) -> ScopeId {
        let parent = self.stack.last().copied();
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(ScopeInfo {
            kind,
            parent,
            decls: Vec::new(),
            direct_eval: false,
            force_context: false,
        });
        self.stack.push(id);
        id
    }

    fn pop_scope(&mut self) {
        self.stack.pop();
    }

    fn current_scope(&self) -> ScopeId {
        *self.stack.last().expect("a scope is always active")
    }

    fn declare(&mut self, name: Symbol, span: ByteSpan) {
        let scope = self.current_scope();
        self.scopes[scope.0 as usize]
            .decls
            .push(Declaration { name, span });
    }

    fn resolve_root(&mut self, root: NodeId) {
        let scope = self.push_scope(ScopeKind::Script);
        self.scopes[scope.0 as usize].force_context = true;
        self.node_scopes[root.0 as usize] = Some(scope);
        let stmts = match self.ast.node(root) {
            Node::StmtList { stmts } => *stmts,
            _ => return self.pop_scope(),
        };
        let stmts: Vec<NodeId> = self.ast.list_items(stmts).to_vec();
        self.collect_stmt_decls(&stmts);
        for stmt in stmts {
            self.resolve_node(stmt);
        }
        self.pop_scope();
    }

    /// Declare the `let`s a statement list binds, in emission order.
    /// `let`s nested in `if` branches (or in a `let` initializer's
    /// expressions) bind in the same scope and are counted first, since
    /// codegen stores them before the enclosing `let` itself. Callable
    /// objects bind in their own scope.
    fn collect_stmt_decls(&mut self, stmts: &[NodeId]) {
        for &stmt in stmts {
            match *self.ast.node(stmt) {
                Node::Let { name, init } => {
                    self.collect_expr_decls(init);
                    let span = self.ast.span(stmt);
                    self.declare(name, span);
                }
                Node::LetMulti { names, init, .. } => {
                    self.collect_expr_decls(init);
                    let span = self.ast.span(stmt);
                    let names: Vec<NodeId> = self.ast.list_items(names).to_vec();
                    for name_node in names {
                        let Node::Ident(name) = *self.ast.node(name_node) else {
                            unreachable!("import names are identifiers");
                        };
                        self.declare(name, span);
                    }
                }
                Node::ExprStmt { expr } => self.collect_expr_decls(expr),
                _ => {}
            }
        }
    }

    fn collect_expr_decls(&mut self, node: NodeId) {
        match *self.ast.node(node) {
            Node::If { then, else_, .. } => {
                self.collect_branch_decls(then);
                if let Some(else_) = else_ {
                    self.collect_branch_decls(else_);
                }
            }
            Node::While { body, .. } => self.collect_branch_decls(body),
            Node::Object(ref parts) => {
                // slot values evaluate in the enclosing scope
                for &slot in self.ast.list_items(parts.slots) {
                    if let Node::Slot { kind, key, value } = *self.ast.node(slot) {
                        if kind == SlotKind::Element {
                            self.collect_expr_decls(key);
                        }
                        self.collect_expr_decls(value);
                    }
                }
            }
            Node::Array { elements } => {
                for &element in self.ast.list_items(elements) {
                    self.collect_expr_decls(element);
                }
            }
            Node::Get { recv, .. } => self.collect_expr_decls(recv),
            Node::Index { recv, key } => {
                self.collect_expr_decls(recv);
                self.collect_expr_decls(key);
            }
            Node::Send { recv, args, .. } => {
                self.collect_expr_decls(recv);
                for &arg in self.ast.list_items(args) {
                    self.collect_expr_decls(arg);
                }
            }
            Node::Call { callee, args } => {
                self.collect_expr_decls(callee);
                for &arg in self.ast.list_items(args) {
                    self.collect_expr_decls(arg);
                }
            }
            Node::Assign { target, value } => {
                self.collect_expr_decls(target);
                self.collect_expr_decls(value);
            }
            Node::Unary { expr, .. } => self.collect_expr_decls(expr),
            Node::Binary { lhs, rhs, .. } => {
                self.collect_expr_decls(lhs);
                self.collect_expr_decls(rhs);
            }
            Node::Return { value } => self.collect_expr_decls(value),
            _ => {}
        }
    }

    /// An `if` branch: a `StmtList` inline, or a chained `else if`.
    fn collect_branch_decls(&mut self, branch: NodeId) {
        match *self.ast.node(branch) {
            Node::StmtList { stmts } => {
                let stmts: Vec<NodeId> = self.ast.list_items(stmts).to_vec();
                self.collect_stmt_decls(&stmts);
            }
            Node::If { .. } => self.collect_expr_decls(branch),
            _ => {}
        }
    }

    /// Enter a callable object's scope: params, then the body's `let`s;
    /// then walk the body. Slot values evaluate in the enclosing scope
    /// and are resolved before the scope is pushed.
    fn resolve_closure(&mut self, object: NodeId) {
        let parts = match self.ast.node(object) {
            Node::Object(parts) => parts,
            _ => unreachable!("closure objects are Object nodes"),
        };
        for &slot in self.ast.list_items(parts.slots) {
            if let Node::Slot { kind, key, value } = *self.ast.node(slot) {
                if kind == SlotKind::Element {
                    self.resolve_node(key);
                }
                self.resolve_node(value);
            }
        }
        let scope = self.push_scope(ScopeKind::Block);
        self.node_scopes[object.0 as usize] = Some(scope);
        if let Some(params) = parts.params {
            for &param in self.ast.list_items(params) {
                if let Node::Ident(sym) = self.ast.node(param) {
                    let (sym, span) = (*sym, self.ast.span(param));
                    self.declare(sym, span);
                }
            }
        }
        if let Node::StmtList { stmts } = self.ast.node(parts.body) {
            let stmts: Vec<NodeId> = self.ast.list_items(*stmts).to_vec();
            self.collect_stmt_decls(&stmts);
            for stmt in stmts {
                self.resolve_node(stmt);
            }
        }
        self.pop_scope();
    }

    fn resolve_node(&mut self, node: NodeId) {
        let ast = self.ast;
        match ast.node(node) {
            Node::StmtList { stmts } => {
                let stmts: Vec<NodeId> = ast.list_items(*stmts).to_vec();
                for stmt in stmts {
                    self.resolve_node(stmt);
                }
            }
            Node::Let { init, .. } | Node::LetMulti { init, .. } => self.resolve_node(*init),
            Node::ExprStmt { expr } => self.resolve_node(*expr),
            Node::Ident(sym) => self.resolve_ident(node, *sym),
            Node::Object(_) => {
                if ast.is_closure(node) {
                    self.resolve_closure(node);
                } else {
                    let parts = match ast.node(node) {
                        Node::Object(parts) => parts,
                        _ => unreachable!(),
                    };
                    for &slot in ast.list_items(parts.slots) {
                        if let Node::Slot { kind, key, value } = ast.node(slot) {
                            if *kind == SlotKind::Element {
                                self.resolve_node(*key);
                            }
                            self.resolve_node(*value);
                        }
                    }
                }
            }
            Node::If { cond, then, else_ } => {
                let (cond, then, else_) = (*cond, *then, *else_);
                self.resolve_node(cond);
                self.resolve_branch(then);
                if let Some(else_) = else_ {
                    self.resolve_branch(else_);
                }
            }
            Node::While { cond, body } => {
                let (cond, body) = (*cond, *body);
                self.resolve_node(cond);
                self.resolve_branch(body);
            }
            Node::Array { elements } => {
                for &element in ast.list_items(*elements) {
                    self.resolve_node(element);
                }
            }
            Node::Get { recv, .. } => self.resolve_node(*recv),
            Node::Index { recv, key } => {
                let (recv, key) = (*recv, *key);
                self.resolve_node(recv);
                self.resolve_node(key);
            }
            Node::Send { recv, args, .. } => {
                let (recv, args) = (*recv, *args);
                self.resolve_node(recv);
                for &arg in ast.list_items(args) {
                    self.resolve_node(arg);
                }
            }
            Node::Call { callee, args } => {
                let (callee, args) = (*callee, *args);
                self.resolve_node(callee);
                for &arg in ast.list_items(args) {
                    self.resolve_node(arg);
                }
            }
            Node::Assign { target, value } => {
                let (target, value) = (*target, *value);
                self.resolve_node(target);
                self.resolve_node(value);
            }
            Node::Unary { expr, .. } => self.resolve_node(*expr),
            Node::Binary { lhs, rhs, .. } => {
                let (lhs, rhs) = (*lhs, *rhs);
                self.resolve_node(lhs);
                self.resolve_node(rhs);
            }
            Node::Return { value } => self.resolve_node(*value),
            Node::Slot { .. } => {}
            Node::Number(_) | Node::String(_) | Node::Bool(_) | Node::Null | Node::Self_ => {}
        }
    }

    /// An `if` branch: statements inline in the current scope.
    fn resolve_branch(&mut self, branch: NodeId) {
        match *self.ast.node(branch) {
            Node::StmtList { stmts } => {
                let stmts: Vec<NodeId> = self.ast.list_items(stmts).to_vec();
                for stmt in stmts {
                    self.resolve_node(stmt);
                }
            }
            Node::If { .. } => self.resolve_node(branch),
            _ => {}
        }
    }

    /// Compute the context predicate and rewrite capture depths.
    ///
    /// A scope creates a context when one of its declarations is captured or
    /// a direct `eval` can reach its bindings. Capture depths therefore count
    /// context-creating scopes, not lexical levels: a frame without a context
    /// resolves outer slots through its inherited (closure) context.
    fn finalize_contexts(&mut self) -> Vec<bool> {
        let mut captured: Vec<Vec<bool>> = self
            .scopes
            .iter()
            .map(|scope| vec![false; scope.decls.len()])
            .collect();
        for resolution in self.resolutions.iter().flatten() {
            if let Resolution::Capture { scope, decl, .. } = *resolution {
                captured[scope.0 as usize][decl as usize] = true;
            }
        }
        let creates: Vec<bool> = self
            .scopes
            .iter()
            .zip(&captured)
            .map(|(scope, decls)| {
                scope.force_context || scope.direct_eval || decls.iter().any(|&c| c)
            })
            .collect();
        for node in 0..self.resolutions.len() {
            let Some(Resolution::Capture { scope, decl, .. }) = self.resolutions[node] else {
                continue;
            };
            // Top-level lexical declarations live in the script/eval context,
            // so captures from nested callables hop through it like any other.
            let use_scope = self.capture_scopes[node].expect("captures record their use scope");
            let mut hops = 0u32;
            let mut cur = use_scope;
            while let Some(parent) = self.scopes[cur.0 as usize].parent {
                if creates[parent.0 as usize] {
                    hops += 1;
                }
                cur = parent;
                if cur == scope {
                    break;
                }
            }
            debug_assert!(hops > 0, "a captured declaration always lives in a context");
            let depth = hops - u32::from(!creates[use_scope.0 as usize]);
            self.resolutions[node] = Some(Resolution::Capture { scope, decl, depth });
        }
        creates
    }

    fn resolve_ident(&mut self, node: NodeId, sym: Symbol) {
        let top = self.stack.len() - 1;
        let mut found = None;
        for (depth_from_root, &scope) in self.stack.iter().enumerate().rev() {
            let decl = self.scopes[scope.0 as usize]
                .decls
                .iter()
                .rposition(|d| d.name == sym);
            if let Some(decl) = decl {
                found = Some((scope, decl as u32, (top - depth_from_root) as u32));
                break;
            }
        }
        self.resolutions[node.0 as usize] = Some(match found {
            Some((scope, decl, 0)) => Resolution::Local { scope, decl },
            Some((scope, decl, depth)) => {
                self.capture_scopes[node.0 as usize] = Some(self.current_scope());
                Resolution::Capture { scope, decl, depth }
            }
            None => Resolution::Global,
        });
    }
}
