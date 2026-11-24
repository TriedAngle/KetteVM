use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use crate::ast::{
    AstNode, ExportDecl, Expr, ForExpr, IfExpr, Object, ObjectEntry, ParamList, Pattern, Script,
    SlotEntry, Stmt, TryExpr, UseDecl, UseTree, WhileExpr,
};
use crate::name::{NameId, NameTable, SELF_NAME};
use crate::{AstChildren, NodeId, SyntaxToken, TextRange, TokenId, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Script,
    Module,
    Function { owns_self: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    Let,
    Param,
    Rest,
    CatchParam,
    ForBinding,
    SelfParam,
    Import,
    Export,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Local(BindingId),
    Upvar(BindingId),
    Global,
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    pub start: u32,
    pub bindings: Vec<BindingId>,
    /// Outer bindings needed by this scope or by objects nested in it.
    pub upvars: Vec<BindingId>,
    pub self_binding: Option<BindingId>,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub name: NameId,
    pub kind: BindingKind,
    pub scope: ScopeId,
    pub declaration: TextRange,
    pub assigned: bool,
    /// Declared in a loop body, whose slot is reused per iteration.
    pub in_loop: bool,
}

#[derive(Default)]
struct NameHasher(u64);

impl Hasher for NameHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        self.0 = hash;
    }

    fn write_u32(&mut self, value: u32) {
        self.0 = (u64::from(value)).wrapping_mul(0x9e3779b97f4a7c15);
    }
}

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<NameHasher>>;

pub struct Resolved {
    names: Arc<NameTable>,
    scopes: Vec<Scope>,
    bindings: Vec<Binding>,
    resolutions: Vec<Option<Resolution>>,
    node_scopes: Vec<Option<ScopeId>>,
    token_bindings: Vec<Option<BindingId>>,
}

impl Resolved {
    pub fn resolution(&self, node: NodeId) -> Option<Resolution> {
        self.resolutions[node.0 as usize]
    }

    /// The binding declared at a declaration token.
    pub fn binding_of_token(&self, token: TokenId) -> Option<BindingId> {
        self.token_bindings[token.0 as usize]
    }

    pub fn scope_of(&self, node: NodeId) -> Option<ScopeId> {
        self.node_scopes[node.0 as usize]
    }

    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    pub fn binding(&self, id: BindingId) -> &Binding {
        &self.bindings[id.0 as usize]
    }

    pub fn name(&self, id: NameId) -> &str {
        self.names.name(id)
    }

    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }
}

pub fn resolve(tree: &Tree) -> Resolved {
    let mut resolver = Resolver {
        tree,
        scopes: Vec::new(),
        bindings: Vec::new(),
        by_name: Vec::new(),
        resolutions: vec![None; tree.nodes().len()],
        node_scopes: vec![None; tree.nodes().len()],
        token_bindings: vec![None; tree.tokens().len()],
        stack: Vec::new(),
        module: None,
        excluded: Vec::new(),
        loop_depth: 0,
    };
    let script = tree.script();
    resolver.build_script(script);
    resolver.resolve_statements(
        script.statements(),
        resolver.scope_of_node(script.syntax().id()),
    );
    Resolved {
        names: tree.name_table().clone(),
        scopes: resolver.scopes,
        bindings: resolver.bindings,
        resolutions: resolver.resolutions,
        node_scopes: resolver.node_scopes,
        token_bindings: resolver.token_bindings,
    }
}

struct Resolver<'a> {
    tree: &'a Tree,
    scopes: Vec<Scope>,
    bindings: Vec<Binding>,
    by_name: Vec<Option<FastMap<NameId, Vec<BindingId>>>>,
    resolutions: Vec<Option<Resolution>>,
    node_scopes: Vec<Option<ScopeId>>,
    token_bindings: Vec<Option<BindingId>>,
    stack: Vec<ScopeId>,
    module: Option<ScopeId>,
    excluded: Vec<BindingId>,
    loop_depth: u32,
}

impl<'a> Resolver<'a> {
    fn push_scope(&mut self, kind: ScopeKind, start: u32) -> ScopeId {
        let parent = self.stack.last().copied();
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope {
            parent,
            kind,
            start,
            bindings: Vec::new(),
            upvars: Vec::new(),
            self_binding: None,
        });
        self.by_name.push(None);
        self.stack.push(id);
        id
    }

    fn pop_scope(&mut self) {
        self.stack.pop();
    }

    fn current_scope(&self) -> ScopeId {
        *self.stack.last().expect("a scope is always active")
    }

    fn scope_of_node(&self, node: NodeId) -> ScopeId {
        assert!((node.0 as usize) < self.tree.nodes().len());
        self.node_scopes[node.0 as usize].expect("node has a scope")
    }

    fn bind(
        &mut self,
        scope: ScopeId,
        name: NameId,
        kind: BindingKind,
        declaration: TextRange,
        node: Option<NodeId>,
        token: Option<TokenId>,
    ) -> BindingId {
        let id = BindingId(self.bindings.len() as u32);
        self.bindings.push(Binding {
            name,
            kind,
            scope,
            declaration,
            assigned: false,
            in_loop: self.loop_depth > 0,
        });
        self.scopes[scope.0 as usize].bindings.push(id);
        if let Some(map) = &mut self.by_name[scope.0 as usize] {
            map.entry(name).or_default().push(id);
        } else if self.scopes[scope.0 as usize].bindings.len() >= 4 {
            let mut map: FastMap<NameId, Vec<BindingId>> = HashMap::default();
            for &existing in &self.scopes[scope.0 as usize].bindings {
                map.entry(self.bindings[existing.0 as usize].name)
                    .or_default()
                    .push(existing);
            }
            self.by_name[scope.0 as usize] = Some(map);
        }
        if let Some(node) = node {
            self.resolutions[node.0 as usize] = Some(Resolution::Local(id));
        }
        if let Some(token) = token {
            self.token_bindings[token.0 as usize] = Some(id);
        }
        id
    }

    fn token_name(&self, token: SyntaxToken<'a>) -> Option<NameId> {
        self.tree.token_name(token.id())
    }

    fn pattern_bindings(&self, pattern: Pattern<'a>, out: &mut Vec<BindingId>) {
        match pattern {
            Pattern::Name(pattern) => {
                if let Some(name) = pattern.name()
                    && let Some(binding) = self.token_bindings[name.id().0 as usize]
                {
                    out.push(binding);
                }
            }
            Pattern::Array(array) => {
                for element in array.elements() {
                    self.pattern_bindings(element, out);
                }
                if let Some(rest) = array.rest()
                    && let Some(binding) = self.token_bindings[rest.id().0 as usize]
                {
                    out.push(binding);
                }
            }
            Pattern::Alias(alias) => {
                if let Some(inner) = alias.pattern() {
                    self.pattern_bindings(inner, out);
                }
                if let Some(token) = alias.alias()
                    && let Some(binding) = self.token_bindings[token.id().0 as usize]
                {
                    out.push(binding);
                }
            }
            Pattern::Literal(_) | Pattern::Wildcard(_) => {}
            Pattern::Object(object) => {
                for field in object.fields() {
                    if let Some(inner) = field.pattern() {
                        self.pattern_bindings(inner, out);
                    } else {
                        let token = field.alias().or_else(|| field.key());
                        if let Some(token) = token
                            && let Some(binding) = self.token_bindings[token.id().0 as usize]
                        {
                            out.push(binding);
                        }
                    }
                }
            }
            Pattern::Type(pattern) => {
                if let Some(token) = pattern.binding()
                    && let Some(binding) = self.token_bindings[token.id().0 as usize]
                {
                    out.push(binding);
                }
            }
        }
    }

    fn declare_token(
        &mut self,
        scope: ScopeId,
        token: SyntaxToken<'a>,
        kind: BindingKind,
    ) -> Option<BindingId> {
        let name = self.token_name(token)?;
        Some(self.bind(
            scope,
            name,
            kind,
            token.text_range(),
            None,
            Some(token.id()),
        ))
    }

    fn ensure_self(&mut self, scope: ScopeId) -> BindingId {
        if let Some(binding) = self.scopes[scope.0 as usize].self_binding {
            return binding;
        }
        let range = TextRange::empty(self.scopes[scope.0 as usize].start);
        let binding = self.bind(scope, SELF_NAME, BindingKind::SelfParam, range, None, None);
        self.scopes[scope.0 as usize].self_binding = Some(binding);
        binding
    }

    fn scope_owns_self(&self, scope: ScopeId) -> bool {
        match self.scopes[scope.0 as usize].kind {
            ScopeKind::Script | ScopeKind::Module => true,
            ScopeKind::Function { owns_self } => owns_self,
        }
    }

    fn build_script(&mut self, script: Script<'a>) {
        let scope = self.push_scope(ScopeKind::Script, 0);
        self.node_scopes[script.syntax().id().0 as usize] = Some(scope);
        self.collect_statements(script.statements());
    }

    fn collect_statements(&mut self, statements: AstChildren<'a, Stmt<'a>>) {
        for statement in statements {
            self.collect_statement(statement);
        }
    }

    fn collect_statement(&mut self, statement: Stmt<'a>) {
        match statement {
            Stmt::Let(statement) => {
                let scope = self.current_scope();
                if let Some(pattern) = statement.pattern() {
                    self.declare_pattern(scope, pattern, BindingKind::Let);
                }
                if let Some(value) = statement.value() {
                    self.collect_expr(value, false);
                }
            }
            Stmt::Expr(statement) => {
                if let Some(expr) = statement.expr() {
                    self.collect_expr(expr, false);
                }
            }
            Stmt::Mod(module) => {
                let scope = self.push_scope(ScopeKind::Module, module.syntax().text_range().start);
                self.node_scopes[module.syntax().id().0 as usize] = Some(scope);
                let saved = self.module;
                self.module = Some(scope);
                self.collect_statements(module.statements());
                self.module = saved;
                self.pop_scope();
            }
            Stmt::Use(declaration) => self.collect_use(declaration),
            Stmt::Export(export) => self.collect_export(export),
        }
    }

    fn collect_export(&mut self, export: ExportDecl<'a>) {
        // Export names are module slots, not lexical bindings.
        if let Some(value) = export.value() {
            self.collect_expr(value, false);
        }
    }

    fn collect_use(&mut self, declaration: UseDecl<'a>) {
        let scope = self.current_scope();
        if let Some(tree) = declaration.tree() {
            self.collect_use_tree(tree, scope);
            return;
        }
        let Some(path) = declaration.path() else {
            return;
        };
        let token = declaration.alias().or_else(|| path.last_segment());
        if let Some(token) = token {
            self.declare_token(scope, token, BindingKind::Import);
        }
    }

    fn collect_use_tree(&mut self, tree: UseTree<'a>, scope: ScopeId) {
        for item in tree.items() {
            if let Some(nested) = item.tree
                && item.alias.is_none()
            {
                self.collect_use_tree(nested, scope);
                continue;
            }
            let token = item.alias.or_else(|| item.path.last_segment());
            if let Some(token) = token {
                self.declare_token(scope, token, BindingKind::Import);
            }
        }
    }

    fn declare_pattern(&mut self, scope: ScopeId, pattern: Pattern<'a>, kind: BindingKind) {
        match pattern {
            Pattern::Name(pattern) => {
                if let Some(name) = pattern.name()
                    && let Some(name_id) = self.token_name(name)
                {
                    self.bind(
                        scope,
                        name_id,
                        kind,
                        name.text_range(),
                        Some(pattern.syntax().id()),
                        Some(name.id()),
                    );
                }
            }
            Pattern::Array(array) => {
                for element in array.elements() {
                    self.declare_pattern(scope, element, kind);
                }
                if let Some(rest) = array.rest() {
                    self.declare_token(scope, rest, BindingKind::Rest);
                }
            }
            Pattern::Alias(alias) => {
                if let Some(inner) = alias.pattern() {
                    self.declare_pattern(scope, inner, kind);
                }
                if let Some(token) = alias.alias() {
                    self.declare_token(scope, token, kind);
                }
            }
            Pattern::Literal(_) | Pattern::Wildcard(_) => {}
            Pattern::Object(object) => {
                for field in object.fields() {
                    if let Some(inner) = field.pattern() {
                        self.declare_pattern(scope, inner, kind);
                    } else {
                        let token = field.alias().or_else(|| field.key());
                        if let Some(token) = token {
                            self.declare_token(scope, token, kind);
                        }
                    }
                }
            }
            Pattern::Type(pattern) => {
                if let Some(token) = pattern.binding() {
                    self.declare_token(scope, token, kind);
                }
            }
        }
    }

    fn collect_expr(&mut self, expr: Expr<'a>, method_position: bool) {
        match expr {
            Expr::Atom(_) => {}
            Expr::Paren(paren) => {
                if let Some(inner) = paren.expr() {
                    self.collect_expr(inner, false);
                }
            }
            Expr::Object(object) => {
                self.collect_object(object, method_position);
            }
            Expr::Array(array) => {
                for element in array.elements() {
                    self.collect_expr(element, false);
                }
            }
            Expr::If(expr) => {
                if let Some(condition) = expr.condition() {
                    self.collect_expr(condition, false);
                }
                if let Some(then_branch) = expr.then_branch() {
                    self.collect_object(then_branch, false);
                }
                if let Some(else_branch) = expr.else_branch() {
                    self.collect_expr(else_branch, false);
                }
            }
            Expr::While(expr) => {
                if let Some(condition) = expr.condition() {
                    self.collect_expr(condition, false);
                }
                if let Some(body) = expr.body() {
                    self.loop_depth += 1;
                    self.collect_object(body, false);
                    self.loop_depth -= 1;
                }
            }
            Expr::For(expr) => self.collect_for(expr),
            Expr::Try(expr) => self.collect_try(expr),
            Expr::Match(expr) => {
                if let Some(scrutinee) = expr.scrutinee() {
                    self.collect_expr(scrutinee, false);
                }
                for arm in expr.arms() {
                    let scope = self.push_scope(
                        ScopeKind::Function { owns_self: false },
                        arm.syntax().text_range().start,
                    );
                    self.node_scopes[arm.syntax().id().0 as usize] = Some(scope);
                    if let Some(pattern) = arm.pattern() {
                        self.declare_pattern(scope, pattern, BindingKind::Let);
                    }
                    if let Some(guard) = arm.guard() {
                        self.collect_expr(guard, false);
                    }
                    if let Some(body) = arm.body() {
                        self.collect_expr(body, false);
                    }
                    self.pop_scope();
                }
            }
            Expr::Throw(expr) => {
                if let Some(value) = expr.value() {
                    self.collect_expr(value, false);
                }
            }
            Expr::Return(statement) => {
                if let Some(value) = statement.value() {
                    self.collect_expr(value, false);
                }
            }
            Expr::Break(statement) => {
                if let Some(value) = statement.value() {
                    self.collect_expr(value, false);
                }
            }
            Expr::Unary(expr) => {
                if let Some(operand) = expr.operand() {
                    self.collect_expr(operand, false);
                }
            }
            Expr::Binary(expr) => {
                if let Some(lhs) = expr.lhs() {
                    self.collect_expr(lhs, false);
                }
                if let Some(rhs) = expr.rhs() {
                    self.collect_expr(rhs, false);
                }
            }
            Expr::Assign(expr) => {
                if let Some(place) = expr.place() {
                    self.collect_expr(place, false);
                }
                if let Some(value) = expr.value() {
                    self.collect_expr(value, false);
                }
            }
            Expr::Get(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.collect_expr(receiver, false);
                }
            }
            Expr::Index(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.collect_expr(receiver, false);
                }
                if let Some(index) = expr.index() {
                    self.collect_expr(index, false);
                }
            }
            Expr::Send(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.collect_expr(receiver, false);
                }
                if let Some(args) = expr.args() {
                    for arg in args.args() {
                        self.collect_expr(arg, false);
                    }
                }
            }
            Expr::Call(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.collect_expr(receiver, false);
                }
                if let Some(args) = expr.args() {
                    for arg in args.args() {
                        self.collect_expr(arg, false);
                    }
                }
            }
        }
    }

    fn collect_for(&mut self, expr: ForExpr<'a>) {
        if let Some(iterable) = expr.iterable() {
            self.collect_expr(iterable, false);
        }
        let Some(body) = expr.body() else {
            return;
        };
        self.loop_depth += 1;
        let scope = self.collect_object(body, false);
        if let (Some(scope), Some(pattern)) = (scope, expr.pattern()) {
            self.declare_pattern(scope, pattern, BindingKind::ForBinding);
        }
        self.loop_depth -= 1;
    }

    fn collect_try(&mut self, expr: TryExpr<'a>) {
        if let Some(body) = expr.body() {
            self.collect_object(body, false);
        }
        let Some(handler) = expr.catch() else {
            return;
        };
        let scope = self.collect_object(handler, false);
        if let (Some(scope), Some(name)) = (scope, expr.catch_name()) {
            self.declare_token(scope, name, BindingKind::CatchParam);
        }
    }

    fn collect_object(&mut self, object: Object<'a>, method_position: bool) -> Option<ScopeId> {
        for entry in object.entries() {
            if let ObjectEntry::Slot(slot) = entry {
                self.collect_slot_value(slot);
            }
        }

        if !object.is_callable() {
            return None;
        }

        let owns_self = method_position || object.has_slots();
        let scope = self.push_scope(
            ScopeKind::Function { owns_self },
            object.syntax().text_range().start,
        );
        self.node_scopes[object.syntax().id().0 as usize] = Some(scope);

        for entry in object.entries() {
            match entry {
                ObjectEntry::Slot(_) => {}
                ObjectEntry::Params(params) => self.declare_params(scope, params),
                ObjectEntry::Let(statement) => {
                    if let Some(pattern) = statement.pattern() {
                        self.declare_pattern(scope, pattern, BindingKind::Let);
                    }
                    if let Some(value) = statement.value() {
                        self.collect_expr(value, false);
                    }
                }
                ObjectEntry::Export(export) => self.collect_export(export),
                ObjectEntry::Expr(statement) => {
                    if let Some(expr) = statement.expr() {
                        self.collect_expr(expr, false);
                    }
                }
            }
        }
        self.pop_scope();
        Some(scope)
    }

    fn declare_params(&mut self, scope: ScopeId, params: ParamList<'a>) {
        for param in params.params() {
            self.declare_token(scope, param, BindingKind::Param);
        }
        if let Some(rest) = params.rest() {
            self.declare_token(scope, rest, BindingKind::Rest);
        }
    }

    fn collect_slot_value(&mut self, slot: SlotEntry<'a>) {
        match slot {
            SlotEntry::Slot(slot) => {
                if let Some(value) = slot.value() {
                    self.collect_expr(value, true);
                }
            }
            SlotEntry::Multi(slot) => {
                if let Some(value) = slot.value() {
                    self.collect_expr(value, true);
                }
            }
        }
    }

    fn resolve_statements(&mut self, statements: AstChildren<'a, Stmt<'a>>, scope: ScopeId) {
        for statement in statements {
            self.resolve_statement(statement, scope);
        }
    }

    fn resolve_statement(&mut self, statement: Stmt<'a>, scope: ScopeId) {
        match statement {
            Stmt::Let(statement) => {
                if let Some(pattern) = statement.pattern() {
                    self.resolve_pattern_types(pattern, scope);
                }
                if let Some(value) = statement.value() {
                    let mut bindings = Vec::new();
                    if let Some(pattern) = statement.pattern() {
                        self.pattern_bindings(pattern, &mut bindings);
                    }
                    let mark = self.excluded.len();
                    if !statement.is_recursive() {
                        self.excluded.extend(bindings);
                    }
                    self.resolve_expr(value, scope);
                    self.excluded.truncate(mark);
                }
            }
            Stmt::Expr(statement) => {
                if let Some(expr) = statement.expr() {
                    self.resolve_expr(expr, scope);
                }
            }
            Stmt::Mod(module) => {
                let module_scope = self.scope_of_node(module.syntax().id());
                self.resolve_statements(module.statements(), module_scope);
            }
            Stmt::Use(_) => {}
            Stmt::Export(export) => {
                if let Some(value) = export.value() {
                    self.resolve_expr(value, scope);
                }
            }
        }
    }

    fn resolve_expr(&mut self, expr: Expr<'a>, scope: ScopeId) {
        match expr {
            Expr::Atom(atom) => self.resolve_atom(atom, scope),
            Expr::Paren(paren) => {
                if let Some(inner) = paren.expr() {
                    self.resolve_expr(inner, scope);
                }
            }
            Expr::Object(object) => self.resolve_object(object, scope),
            Expr::Array(array) => {
                for element in array.elements() {
                    self.resolve_expr(element, scope);
                }
            }
            Expr::If(expr) => self.resolve_if(expr, scope),
            Expr::While(expr) => self.resolve_while(expr, scope),
            Expr::For(expr) => self.resolve_for(expr, scope),
            Expr::Try(expr) => self.resolve_try(expr, scope),
            Expr::Match(expr) => {
                if let Some(scrutinee) = expr.scrutinee() {
                    self.resolve_expr(scrutinee, scope);
                }
                for arm in expr.arms() {
                    let arm_scope = self.scope_of_node(arm.syntax().id());
                    if let Some(pattern) = arm.pattern() {
                        self.resolve_pattern_types(pattern, scope);
                    }
                    if let Some(guard) = arm.guard() {
                        self.resolve_expr(guard, arm_scope);
                    }
                    if let Some(body) = arm.body() {
                        self.resolve_expr(body, arm_scope);
                    }
                }
            }
            Expr::Throw(expr) => {
                if let Some(value) = expr.value() {
                    self.resolve_expr(value, scope);
                }
            }
            Expr::Return(statement) => {
                if let Some(value) = statement.value() {
                    self.resolve_expr(value, scope);
                }
            }
            Expr::Break(statement) => {
                if let Some(value) = statement.value() {
                    self.resolve_expr(value, scope);
                }
            }
            Expr::Unary(expr) => {
                if let Some(operand) = expr.operand() {
                    self.resolve_expr(operand, scope);
                }
            }
            Expr::Binary(expr) => {
                if let Some(lhs) = expr.lhs() {
                    self.resolve_expr(lhs, scope);
                }
                if let Some(rhs) = expr.rhs() {
                    self.resolve_expr(rhs, scope);
                }
            }
            Expr::Assign(expr) => {
                if let Some(place) = expr.place() {
                    self.resolve_expr(place, scope);
                    self.mark_assigned(place);
                }
                if let Some(value) = expr.value() {
                    self.resolve_expr(value, scope);
                }
            }
            Expr::Get(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.resolve_expr(receiver, scope);
                }
            }
            Expr::Index(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.resolve_expr(receiver, scope);
                }
                if let Some(index) = expr.index() {
                    self.resolve_expr(index, scope);
                }
            }
            Expr::Send(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.resolve_expr(receiver, scope);
                }
                if let Some(args) = expr.args() {
                    for arg in args.args() {
                        self.resolve_expr(arg, scope);
                    }
                }
            }
            Expr::Call(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.resolve_expr(receiver, scope);
                }
                if let Some(args) = expr.args() {
                    for arg in args.args() {
                        self.resolve_expr(arg, scope);
                    }
                }
            }
        }
    }

    fn resolve_if(&mut self, expr: IfExpr<'a>, scope: ScopeId) {
        if let Some(condition) = expr.condition() {
            self.resolve_expr(condition, scope);
        }
        if let Some(then_branch) = expr.then_branch() {
            self.resolve_object(then_branch, scope);
        }
        if let Some(else_branch) = expr.else_branch() {
            self.resolve_expr(else_branch, scope);
        }
    }

    fn resolve_while(&mut self, expr: WhileExpr<'a>, scope: ScopeId) {
        if let Some(condition) = expr.condition() {
            self.resolve_expr(condition, scope);
        }
        if let Some(body) = expr.body() {
            self.resolve_object(body, scope);
        }
    }

    fn resolve_for(&mut self, expr: ForExpr<'a>, scope: ScopeId) {
        if let Some(iterable) = expr.iterable() {
            self.resolve_expr(iterable, scope);
        }
        if let Some(body) = expr.body() {
            self.resolve_object(body, scope);
            if let Some(pattern) = expr.pattern() {
                let body_scope = self.node_scopes[body.syntax().id().0 as usize].unwrap_or(scope);
                self.resolve_pattern_types(pattern, body_scope);
            }
        }
    }

    fn resolve_try(&mut self, expr: TryExpr<'a>, scope: ScopeId) {
        if let Some(body) = expr.body() {
            self.resolve_object(body, scope);
        }
        if let Some(handler) = expr.catch() {
            self.resolve_object(handler, scope);
        }
    }

    fn resolve_object(&mut self, object: Object<'a>, enclosing: ScopeId) {
        for entry in object.entries() {
            if let ObjectEntry::Slot(slot) = entry {
                match slot {
                    SlotEntry::Slot(slot) => {
                        if let Some(value) = slot.value() {
                            self.resolve_expr(value, enclosing);
                        }
                    }
                    SlotEntry::Multi(slot) => {
                        if let Some(value) = slot.value() {
                            self.resolve_expr(value, enclosing);
                        }
                    }
                }
            }
        }

        if !object.is_callable() {
            return;
        }
        let scope = self.scope_of_node(object.syntax().id());
        for entry in object.entries() {
            match entry {
                ObjectEntry::Slot(_) => {}
                ObjectEntry::Params(_) => {}
                ObjectEntry::Let(statement) => {
                    if let Some(value) = statement.value() {
                        let mut bindings = Vec::new();
                        if let Some(pattern) = statement.pattern() {
                            self.pattern_bindings(pattern, &mut bindings);
                        }
                        let mark = self.excluded.len();
                        if !statement.is_recursive() {
                            self.excluded.extend(bindings);
                        }
                        self.resolve_expr(value, scope);
                        self.excluded.truncate(mark);
                    }
                }
                ObjectEntry::Export(export) => {
                    if let Some(value) = export.value() {
                        self.resolve_expr(value, scope);
                    }
                }
                ObjectEntry::Expr(statement) => {
                    if let Some(expr) = statement.expr() {
                        self.resolve_expr(expr, scope);
                    }
                }
            }
        }
    }

    fn mark_assigned(&mut self, place: Expr<'a>) {
        let Expr::Atom(atom) = place else {
            return;
        };
        let node = atom.syntax().id().0 as usize;
        if let Some(Resolution::Local(binding) | Resolution::Upvar(binding)) =
            self.resolutions[node]
        {
            self.bindings[binding.0 as usize].assigned = true;
        }
    }

    fn resolve_pattern_types(&mut self, pattern: Pattern<'a>, scope: ScopeId) {
        match pattern {
            Pattern::Type(pattern) => {
                if let Some(token) = pattern.type_name()
                    && let Some(name) = self.token_name(token)
                {
                    let node = pattern.syntax().id();
                    let start = token.text_range().start;
                    if let Some((binding, binding_scope)) = self.lookup(scope, name, start) {
                        if binding_scope == scope {
                            self.resolutions[node.0 as usize] = Some(Resolution::Local(binding));
                        } else {
                            self.record_upvar(scope, binding, binding_scope);
                            self.resolutions[node.0 as usize] = Some(Resolution::Upvar(binding));
                        }
                    } else {
                        self.resolutions[node.0 as usize] = Some(Resolution::Global);
                    }
                }
            }
            Pattern::Object(object) => {
                for field in object.fields() {
                    if let Some(inner) = field.pattern() {
                        self.resolve_pattern_types(inner, scope);
                    }
                }
            }
            Pattern::Array(array) => {
                for element in array.elements() {
                    self.resolve_pattern_types(element, scope);
                }
            }
            Pattern::Alias(alias) => {
                if let Some(inner) = alias.pattern() {
                    self.resolve_pattern_types(inner, scope);
                }
            }
            Pattern::Name(_) | Pattern::Literal(_) | Pattern::Wildcard(_) => {}
        }
    }

    fn resolve_atom(&mut self, atom: crate::ast::Atom<'a>, scope: ScopeId) {
        let Some(token) = atom.token() else {
            return;
        };
        let node = atom.syntax().id();
        match token.kind() {
            crate::SyntaxKind::SelfKw => self.resolve_self(node, scope),
            crate::SyntaxKind::Ident => {
                let Some(name) = self.token_name(token) else {
                    self.resolutions[node.0 as usize] = Some(Resolution::Global);
                    return;
                };
                let start = token.text_range().start;
                let Some((binding, binding_scope)) = self.lookup(scope, name, start) else {
                    self.resolutions[node.0 as usize] = Some(Resolution::Global);
                    return;
                };
                if binding_scope == scope {
                    self.resolutions[node.0 as usize] = Some(Resolution::Local(binding));
                } else {
                    self.record_upvar(scope, binding, binding_scope);
                    self.resolutions[node.0 as usize] = Some(Resolution::Upvar(binding));
                }
            }
            _ => {}
        }
    }

    fn resolve_self(&mut self, node: NodeId, scope: ScopeId) {
        let mut owner = None;
        let mut current = Some(scope);
        while let Some(candidate) = current {
            if self.scope_owns_self(candidate) {
                owner = Some(candidate);
                break;
            }
            current = self.scopes[candidate.0 as usize].parent;
        }
        let Some(owner) = owner else {
            self.resolutions[node.0 as usize] = Some(Resolution::Global);
            return;
        };
        let binding = self.ensure_self(owner);
        if owner == scope {
            self.resolutions[node.0 as usize] = Some(Resolution::Local(binding));
        } else {
            self.record_upvar(scope, binding, owner);
            self.resolutions[node.0 as usize] = Some(Resolution::Upvar(binding));
        }
    }

    fn lookup(&self, scope: ScopeId, name: NameId, start: u32) -> Option<(BindingId, ScopeId)> {
        let mut current = Some(scope);
        while let Some(scope) = current {
            if let Some(binding) = self.lookup_scope(scope, name, start) {
                return Some((binding, scope));
            }
            current = self.scopes[scope.0 as usize].parent;
        }
        None
    }

    fn lookup_scope(&self, scope: ScopeId, name: NameId, start: u32) -> Option<BindingId> {
        if let Some(map) = &self.by_name[scope.0 as usize] {
            let mut fallback = None;
            for &id in map.get(&name)?.iter().rev() {
                if self.excluded.contains(&id) {
                    continue;
                }
                let binding = &self.bindings[id.0 as usize];
                fallback = Some(id);
                if binding.declaration.end <= start {
                    return Some(id);
                }
            }
            return fallback;
        }
        let mut fallback = None;
        for &id in self.scopes[scope.0 as usize].bindings.iter().rev() {
            if self.excluded.contains(&id) {
                continue;
            }
            let binding = &self.bindings[id.0 as usize];
            if binding.name != name {
                continue;
            }
            fallback = Some(id);
            if binding.declaration.end <= start {
                return Some(id);
            }
        }
        fallback
    }

    fn record_upvar(&mut self, scope: ScopeId, binding: BindingId, binding_scope: ScopeId) {
        let mut current = Some(scope);
        while let Some(id) = current {
            if id == binding_scope {
                break;
            }
            let scope = &mut self.scopes[id.0 as usize];
            if !scope.upvars.contains(&binding) {
                scope.upvars.push(binding);
            }
            current = scope.parent;
        }
    }
}
