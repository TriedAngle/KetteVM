use bytecode::Reg;
use kette_syntax::ast::{
    AstNode, Expr, ForExpr, IfExpr, LetStmt, Object, ObjectEntry, Pattern, Script, Stmt, TryExpr,
    WhileExpr,
};
use kette_syntax::resolve::{BindingId, BindingKind, Resolved, ScopeId};
use kette_syntax::{NodeId, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Script,
    Object(NodeId),
    Module(NodeId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Reg(Reg),
    Context(u32),
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub kind: FrameKind,
    pub scope: ScopeId,
    pub parent: Option<FrameId>,
    /// This frame plus every scope inlined into it.
    pub scopes: Vec<ScopeId>,
    pub params: Vec<BindingId>,
    pub rest: Option<BindingId>,
    pub self_binding: Option<BindingId>,
    pub staging: Option<Reg>,
    pub local_count: u32,
    pub context_slots: u32,
    pub creates_context: bool,
    /// The `let*` binding whose initializer created this frame.
    pub recursive_binding: Option<BindingId>,
}

pub struct Plan {
    pub frames: Vec<Frame>,
    pub frame_of_scope: Vec<Option<FrameId>>,
    pub function_of_node: Vec<Option<FrameId>>,
    pub binding_frame: Vec<Option<FrameId>>,
    pub locations: Vec<Option<Location>>,
}

impl Plan {
    pub fn build(tree: &Tree, resolved: &Resolved) -> Plan {
        let mut planner = Planner {
            resolved,
            frames: Vec::new(),
            frame_of_scope: vec![None; resolved.scopes().len()],
            function_of_node: vec![None; tree.nodes().len()],
            recursive_values: vec![None; tree.nodes().len()],
        };
        planner.build_script(tree.script());

        let binding_count = resolved.bindings().len();
        let mut binding_frame = vec![None; binding_count];
        for (index, frame) in planner.frames.iter().enumerate() {
            for &scope in &frame.scopes {
                for &binding in &resolved.scope(scope).bindings {
                    binding_frame[binding.0 as usize] = Some(FrameId(index as u32));
                }
            }
        }

        let mut captured = vec![false; binding_count];
        for (index, scope) in resolved.scopes().iter().enumerate() {
            let Some(scope_frame) = planner.frame_of_scope[index] else {
                continue;
            };
            for &binding in &scope.upvars {
                if binding_frame[binding.0 as usize] != Some(scope_frame) {
                    captured[binding.0 as usize] = true;
                }
            }
        }
        if let Some(first) = planner.frames.first() {
            for &scope in &first.scopes {
                for &binding in &resolved.scope(scope).bindings {
                    captured[binding.0 as usize] = true;
                }
            }
        }

        let mut locations = vec![None; binding_count];
        for frame_index in 0..planner.frames.len() {
            let frame = &mut planner.frames[frame_index];
            let mut register = 0u32;
            let mut context = 0u32;
            for &scope in &frame.scopes {
                for &binding in &resolved.scope(scope).bindings {
                    let slot = if captured[binding.0 as usize] {
                        let slot = context;
                        context += 1;
                        Location::Context(slot)
                    } else {
                        let slot = Location::Reg(Reg::new(register as i32));
                        register += 1;
                        slot
                    };
                    locations[binding.0 as usize] = Some(slot);
                }
            }
            if matches!(frame.kind, FrameKind::Module(_)) {
                frame.staging = Some(Reg::new(register as i32));
                register += 1;
            }
            frame.local_count = register;
            frame.context_slots = context;
            frame.creates_context = context > 0 || matches!(frame.kind, FrameKind::Script);
        }

        for frame_index in 0..planner.frames.len() {
            let frame = &mut planner.frames[frame_index];
            let scope = resolved.scope(frame.scope);
            frame.params = scope
                .bindings
                .iter()
                .copied()
                .filter(|binding| matches!(resolved.binding(*binding).kind, BindingKind::Param))
                .collect();
            frame.rest = scope
                .bindings
                .iter()
                .copied()
                .find(|binding| matches!(resolved.binding(*binding).kind, BindingKind::Rest));
            frame.self_binding =
                scope.bindings.iter().copied().find(|binding| {
                    matches!(resolved.binding(*binding).kind, BindingKind::SelfParam)
                });
        }

        Plan {
            frames: planner.frames,
            frame_of_scope: planner.frame_of_scope,
            function_of_node: planner.function_of_node,
            binding_frame,
            locations,
        }
    }

    pub fn frame(&self, id: FrameId) -> &Frame {
        &self.frames[id.0 as usize]
    }

    pub fn location(&self, binding: BindingId) -> Location {
        self.locations[binding.0 as usize].expect("binding has a location")
    }

    pub fn binding_frame(&self, binding: BindingId) -> FrameId {
        self.binding_frame[binding.0 as usize].expect("binding has a frame")
    }

    /// Context-chain distance between two frames.
    pub fn context_depth(&self, use_frame: FrameId, binding_frame: FrameId) -> u32 {
        if use_frame == binding_frame {
            return 0;
        }
        let mut hops = 0u32;
        let mut current = self.frame(use_frame).parent;
        while let Some(frame) = current {
            if self.frame(frame).creates_context {
                hops += 1;
            }
            if frame == binding_frame {
                break;
            }
            current = self.frame(frame).parent;
        }
        hops - u32::from(!self.frame(use_frame).creates_context)
    }
}

struct Planner<'a> {
    resolved: &'a Resolved,
    frames: Vec<Frame>,
    frame_of_scope: Vec<Option<FrameId>>,
    function_of_node: Vec<Option<FrameId>>,
    /// Object created by a `let*` initializer -> its binding.
    recursive_values: Vec<Option<BindingId>>,
}

impl<'a> Planner<'a> {
    fn build_script(&mut self, script: Script<'a>) {
        let scope = self
            .resolved
            .scope_of(script.syntax().id())
            .expect("script scope");
        self.frames.push(Frame {
            kind: FrameKind::Script,
            scope,
            parent: None,
            scopes: vec![scope],
            params: Vec::new(),
            rest: None,
            self_binding: None,
            staging: None,
            local_count: 0,
            context_slots: 0,
            creates_context: true,
            recursive_binding: None,
        });
        self.frame_of_scope[scope.0 as usize] = Some(FrameId(0));
        self.statements(script.statements(), FrameId(0));
    }

    fn new_frame(&mut self, kind: FrameKind, scope: ScopeId, parent: FrameId) -> FrameId {
        let id = FrameId(self.frames.len() as u32);
        let recursive_binding = match kind {
            FrameKind::Object(node) => self.recursive_values[node.0 as usize],
            _ => None,
        };
        self.frames.push(Frame {
            kind,
            scope,
            parent: Some(parent),
            scopes: vec![scope],
            params: Vec::new(),
            rest: None,
            self_binding: None,
            staging: None,
            local_count: 0,
            context_slots: 0,
            creates_context: false,
            recursive_binding,
        });
        self.frame_of_scope[scope.0 as usize] = Some(id);
        if let FrameKind::Object(node) | FrameKind::Module(node) = kind {
            self.function_of_node[node.0 as usize] = Some(id);
        }
        id
    }

    fn inline_scope(&mut self, frame: FrameId, scope: ScopeId) {
        self.frames[frame.0 as usize].scopes.push(scope);
        self.frame_of_scope[scope.0 as usize] = Some(frame);
    }

    fn note_recursive_value(&mut self, statement: LetStmt<'a>) {
        if !statement.is_recursive() {
            return;
        }
        let Some(Pattern::Name(name)) = statement.pattern() else {
            return;
        };
        let Some(token) = name.name() else {
            return;
        };
        let Some(binding) = self.resolved.binding_of_token(token.id()) else {
            return;
        };
        let Some(object) = statement.value().and_then(Self::object_value) else {
            return;
        };
        self.recursive_values[object.syntax().id().0 as usize] = Some(binding);
    }

    fn object_value(expr: Expr<'a>) -> Option<Object<'a>> {
        match expr {
            Expr::Object(object) => Some(object),
            Expr::Paren(paren) => paren.expr().and_then(Self::object_value),
            _ => None,
        }
    }

    fn statements(&mut self, statements: kette_syntax::AstChildren<'a, Stmt<'a>>, frame: FrameId) {
        for statement in statements {
            self.statement(statement, frame);
        }
    }

    fn statement(&mut self, statement: Stmt<'a>, frame: FrameId) {
        match statement {
            Stmt::Let(statement) => {
                self.note_recursive_value(statement);
                if let Some(value) = statement.value() {
                    self.expr(value, frame);
                }
            }
            Stmt::Expr(statement) => {
                if let Some(expr) = statement.expr() {
                    self.expr(expr, frame);
                }
            }
            Stmt::Mod(module) => {
                let scope = self
                    .resolved
                    .scope_of(module.syntax().id())
                    .expect("module scope");
                let child = self.new_frame(FrameKind::Module(module.syntax().id()), scope, frame);
                self.statements(module.statements(), child);
            }
            Stmt::Use(_) => {}
            Stmt::Export(export) => {
                if let Some(value) = export.value() {
                    self.expr(value, frame);
                }
            }
        }
    }

    fn expr(&mut self, expr: Expr<'a>, frame: FrameId) {
        match expr {
            Expr::Atom(_) => {}
            Expr::Paren(paren) => {
                if let Some(inner) = paren.expr() {
                    self.expr(inner, frame);
                }
            }
            Expr::Object(object) => self.object(object, frame, false),
            Expr::Array(array) => {
                for element in array.elements() {
                    self.expr(element, frame);
                }
            }
            Expr::If(expr) => self.if_expr(expr, frame),
            Expr::While(expr) => self.while_expr(expr, frame),
            Expr::For(expr) => self.for_expr(expr, frame),
            Expr::Try(expr) => self.try_expr(expr, frame),
            Expr::Throw(expr) => {
                if let Some(value) = expr.value() {
                    self.expr(value, frame);
                }
            }
            Expr::Return(statement) => {
                if let Some(value) = statement.value() {
                    self.expr(value, frame);
                }
            }
            Expr::Break(statement) => {
                if let Some(value) = statement.value() {
                    self.expr(value, frame);
                }
            }
            Expr::Unary(expr) => {
                if let Some(operand) = expr.operand() {
                    self.expr(operand, frame);
                }
            }
            Expr::Binary(expr) => {
                if let Some(lhs) = expr.lhs() {
                    self.expr(lhs, frame);
                }
                if let Some(rhs) = expr.rhs() {
                    self.expr(rhs, frame);
                }
            }
            Expr::Assign(expr) => {
                if let Some(place) = expr.place() {
                    self.expr(place, frame);
                }
                if let Some(value) = expr.value() {
                    self.expr(value, frame);
                }
            }
            Expr::Get(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.expr(receiver, frame);
                }
            }
            Expr::Index(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.expr(receiver, frame);
                }
                if let Some(index) = expr.index() {
                    self.expr(index, frame);
                }
            }
            Expr::Send(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.expr(receiver, frame);
                }
                if let Some(args) = expr.args() {
                    for arg in args.args() {
                        self.expr(arg, frame);
                    }
                }
            }
            Expr::Call(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.expr(receiver, frame);
                }
                if let Some(args) = expr.args() {
                    for arg in args.args() {
                        self.expr(arg, frame);
                    }
                }
            }
            Expr::Match(expr) => {
                if let Some(scrutinee) = expr.scrutinee() {
                    self.expr(scrutinee, frame);
                }
                for arm in expr.arms() {
                    let scope = self
                        .resolved
                        .scope_of(arm.syntax().id())
                        .expect("match arm scope");
                    self.inline_scope(frame, scope);
                    if let Some(guard) = arm.guard() {
                        self.expr(guard, frame);
                    }
                    if let Some(body) = arm.body() {
                        match body {
                            Expr::Object(object)
                                if object.params().is_none()
                                    && !object.has_slots()
                                    && object.statements().next().is_some() =>
                            {
                                self.control_object(object, frame);
                            }
                            other => self.expr(other, frame),
                        }
                    }
                }
            }
        }
    }

    fn object(&mut self, object: Object<'a>, frame: FrameId, control: bool) {
        if control {
            if let Some(scope) = self.resolved.scope_of(object.syntax().id()) {
                self.inline_scope(frame, scope);
                self.body(object, frame);
            }
            return;
        }

        for entry in object.entries() {
            if let ObjectEntry::Slot(slot) = entry {
                match slot {
                    kette_syntax::ast::SlotEntry::Slot(slot) => {
                        if let Some(value) = slot.value() {
                            self.expr(value, frame);
                        }
                    }
                    kette_syntax::ast::SlotEntry::Multi(slot) => {
                        if let Some(value) = slot.value() {
                            self.expr(value, frame);
                        }
                    }
                }
            }
        }

        if let Some(scope) = self.resolved.scope_of(object.syntax().id()) {
            let child = self.new_frame(FrameKind::Object(object.syntax().id()), scope, frame);
            self.body(object, child);
        }
    }

    fn body(&mut self, object: Object<'a>, frame: FrameId) {
        for entry in object.entries() {
            match entry {
                ObjectEntry::Slot(_) | ObjectEntry::Params(_) => {}
                ObjectEntry::Let(statement) => {
                    self.note_recursive_value(statement);
                    if let Some(value) = statement.value() {
                        self.expr(value, frame);
                    }
                }
                ObjectEntry::Export(export) => {
                    if let Some(value) = export.value() {
                        self.expr(value, frame);
                    }
                }
                ObjectEntry::Expr(statement) => {
                    if let Some(expr) = statement.expr() {
                        self.expr(expr, frame);
                    }
                }
            }
        }
    }

    fn control_object(&mut self, object: Object<'a>, frame: FrameId) {
        self.object(object, frame, true);
    }

    fn if_expr(&mut self, expr: IfExpr<'a>, frame: FrameId) {
        if let Some(condition) = expr.condition() {
            self.expr(condition, frame);
        }
        if let Some(then_branch) = expr.then_branch() {
            self.control_object(then_branch, frame);
        }
        if let Some(else_branch) = expr.else_branch() {
            match else_branch {
                Expr::Object(object) => self.control_object(object, frame),
                other => self.expr(other, frame),
            }
        }
    }

    fn while_expr(&mut self, expr: WhileExpr<'a>, frame: FrameId) {
        if let Some(condition) = expr.condition() {
            self.expr(condition, frame);
        }
        if let Some(body) = expr.body() {
            self.control_object(body, frame);
        }
    }

    fn for_expr(&mut self, expr: ForExpr<'a>, frame: FrameId) {
        if let Some(iterable) = expr.iterable() {
            self.expr(iterable, frame);
        }
        if let Some(body) = expr.body() {
            self.control_object(body, frame);
        }
    }

    fn try_expr(&mut self, expr: TryExpr<'a>, frame: FrameId) {
        if let Some(body) = expr.body() {
            self.control_object(body, frame);
        }
        if let Some(handler) = expr.catch() {
            self.control_object(handler, frame);
        }
    }
}
