use bytecode::{
    CallableKind, Constant, FnBuilder, Function, FunctionId, FunctionMeta, GlobalDeclarations,
    Label, Program, Reg, RegList, RtArg, ScopeLayout, ScopeLocal, SourceMode, TempScope,
    TestTypeKind,
};
use kette_syntax::ast::{
    AssignExpr, AstNode, BinaryExpr, CallExpr, ExportDecl, Expr, Field, ForExpr, GetExpr, IfExpr,
    IndexExpr, LetStmt, MatchArm, MatchExpr, Object, ObjectEntry, Pattern, PatternField, SendExpr,
    SlotEntry, SlotKind, SlotMulti, Stmt, ThrowExpr, TryExpr, UnaryExpr, UseDecl, UseTree,
    WhileExpr,
};
use kette_syntax::ops::{AssignOp, BinaryOp, UnaryOp};
use kette_syntax::resolve::{Binding, BindingId, BindingKind, Resolution, Resolved};
use kette_syntax::{NodeId, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, Tree};
use vm_core::{
    INFO_GLOBAL, MODE_LET, MODE_VAR, NativeIndex, SCOPE_DECLARATION, SCOPE_REPL, SCOPE_SCRIPT, Smi,
};

use crate::CompileError;
use crate::plan::{Frame, FrameId, FrameKind, Location, Plan};

pub fn generate(
    tree: &Tree,
    resolved: &Resolved,
    mode: SourceMode,
    declare_globals: NativeIndex,
    install_accessor: NativeIndex,
) -> Result<Program, CompileError> {
    let _span = trace::info_span!("kette::lower").entered();
    let plan = Plan::build(tree, resolved);
    let mut program = Program::with_capacity(plan.frames.len());
    for frame in 0..plan.frames.len() {
        let frame = FrameId(frame as u32);
        let generator = FunctionGen::new(
            tree,
            resolved,
            &plan,
            frame,
            mode,
            declare_globals,
            install_accessor,
        );
        program.add_function(generator.generate()?);
    }
    debug_assert!(
        bytecode::validate(&program).is_ok(),
        "compiler produced invalid bytecode: {:?}",
        bytecode::validate(&program)
    );
    Ok(program)
}

#[derive(Clone, Copy)]
struct LoopCtx {
    break_label: Label,
    result: Reg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TailMode {
    None,
    Guard,
    Loop,
}

struct FunctionGen<'a> {
    tree: &'a Tree,
    resolved: &'a Resolved,
    plan: &'a Plan,
    frame_id: FrameId,
    frame: &'a Frame,
    mode: SourceMode,
    declare_globals: NativeIndex,
    install_accessor: NativeIndex,
    b: FnBuilder,
    ctx_save: Reg,
    completion: Reg,
    loops: Vec<LoopCtx>,
    try_depth: u32,
    loop_label: Option<Label>,
}

impl<'a> FunctionGen<'a> {
    fn new(
        tree: &'a Tree,
        resolved: &'a Resolved,
        plan: &'a Plan,
        frame_id: FrameId,
        mode: SourceMode,
        declare_globals: NativeIndex,
        install_accessor: NativeIndex,
    ) -> Self {
        let frame = plan.frame(frame_id);
        let arity = match frame.kind {
            FrameKind::Script => 0,
            FrameKind::Module(_) => 1,
            FrameKind::Object(_) => frame.params.len() as u32,
        };
        let ctx_save = Reg::new(frame.local_count as i32);
        let completion = Reg::new(frame.local_count as i32 + 1);
        let mut b = FnBuilder::new(arity);
        b.set_temp_base(frame.local_count + 2);
        Self {
            tree,
            resolved,
            plan,
            frame_id,
            frame,
            mode,
            declare_globals,
            install_accessor,
            b,
            ctx_save,
            completion,
            loops: Vec::new(),
            try_depth: 0,
            loop_label: None,
        }
    }

    fn finish(self) -> Function {
        let tail_callable = matches!(self.frame.kind, FrameKind::Object(_))
            && !self.frame.creates_context
            && self.frame.context_slots == 0
            && self.frame.rest.is_none();
        let mut function = self
            .b
            .finish(FunctionMeta {
                name: None,
                kind: CallableKind::Method,
                length: self.frame.params.len() as u32,
                strict: true,
            })
            .expect("kette function builder invariants");
        function.tail_callable = tail_callable;
        function
    }

    fn err<T>(&self, range: TextRange, message: impl Into<String>) -> Result<T, CompileError> {
        Err(CompileError {
            range,
            message: message.into(),
        })
    }

    fn err_node<T>(&self, node: NodeId, message: impl Into<String>) -> Result<T, CompileError> {
        self.err(self.tree.node(node).range, message)
    }

    fn binding(&self, token: SyntaxToken<'a>) -> Result<BindingId, CompileError> {
        self.resolved
            .binding_of_token(token.id())
            .ok_or_else(|| CompileError {
                range: token.text_range(),
                message: format!("unbound declaration `{}`", token.text()),
            })
    }

    fn binding_at(&self, binding: BindingId) -> Result<&'a Binding, CompileError> {
        Ok(self.resolved.binding(binding))
    }

    fn load_binding(&mut self, binding: BindingId) -> Result<(), CompileError> {
        let info = self.binding_at(binding)?;
        if info.kind == BindingKind::Export && matches!(self.frame.kind, FrameKind::Module(_)) {
            let Some(staging) = self.frame.staging else {
                return self.err_node(self.tree.root_id(), "module staging");
            };
            let name = self.resolved.name(info.name).as_bytes().to_vec();
            let name = self.b.name(&name);
            let feedback = self.b.new_feedback();
            self.b.load_named_property(staging, name, feedback);
            return Ok(());
        }
        match self.plan.location(binding) {
            Location::Reg(reg) => {
                if self.plan.binding_frame(binding) != self.frame_id {
                    let name = self.resolved.name(info.name).to_string();
                    return self.err_node(
                        self.tree.root_id(),
                        format!(
                            "register upvar `{name}` binding_frame={:?} use_frame={:?}",
                            self.plan.binding_frame(binding),
                            self.frame_id
                        ),
                    );
                }
                self.b.load(reg);
            }
            Location::Context(slot) => {
                let depth = self
                    .plan
                    .context_depth(self.frame_id, self.plan.binding_frame(binding));
                self.b.load_context_slot(slot, depth);
            }
        }
        Ok(())
    }

    fn store_binding(&mut self, binding: BindingId) -> Result<(), CompileError> {
        let info = self.binding_at(binding)?;
        if info.kind == BindingKind::Export && matches!(self.frame.kind, FrameKind::Module(_)) {
            let Some(staging) = self.frame.staging else {
                return self.err_node(self.tree.root_id(), "module staging");
            };
            let name = self.resolved.name(info.name).as_bytes().to_vec();
            let name = self.b.name(&name);
            let feedback = self.b.new_feedback();
            self.b
                .store_named_property_no_shadow(staging, name, feedback);
            return Ok(());
        }
        match self.plan.location(binding) {
            Location::Reg(reg) => {
                if self.plan.binding_frame(binding) != self.frame_id {
                    let name = self.resolved.name(info.name).to_string();
                    return self.err_node(
                        self.tree.root_id(),
                        format!(
                            "register upvar `{name}` binding_frame={:?} use_frame={:?}",
                            self.plan.binding_frame(binding),
                            self.frame_id
                        ),
                    );
                }
                self.b.store(reg);
            }
            Location::Context(slot) => {
                let depth = self
                    .plan
                    .context_depth(self.frame_id, self.plan.binding_frame(binding));
                self.b.store_context_slot(slot, depth);
            }
        }
        Ok(())
    }

    fn load_resolution(&mut self, node: NodeId) -> Result<(), CompileError> {
        match self.resolved.resolution(node) {
            Some(Resolution::Local(binding) | Resolution::Upvar(binding)) => {
                self.load_binding(binding)
            }
            Some(Resolution::Global) => {
                let name = self.node_name(node);
                self.load_global(&name);
                Ok(())
            }
            None => self.err_node(node, "identifier resolution"),
        }
    }

    fn store_resolution(&mut self, node: NodeId) -> Result<(), CompileError> {
        match self.resolved.resolution(node) {
            Some(Resolution::Local(binding) | Resolution::Upvar(binding)) => {
                self.store_binding(binding)
            }
            Some(Resolution::Global) => {
                let name = self.node_name(node);
                let name = self.b.name(name.as_bytes());
                let feedback = self.b.new_feedback();
                self.b.store_global(name, feedback);
                Ok(())
            }
            None => self.err_node(node, "identifier resolution"),
        }
    }

    fn node_name(&self, node: NodeId) -> String {
        let syntax = SyntaxNode::new(self.tree, node);
        syntax
            .first_token()
            .map(|token| token.text().to_string())
            .unwrap_or_default()
    }

    fn load_global(&mut self, name: &str) {
        let name = self.b.name(name.as_bytes());
        let feedback = self.b.new_feedback();
        self.b.load_global(name, feedback);
    }

    fn generate(mut self) -> Result<Function, CompileError> {
        self.prologue()?;
        if self.tail_callable() {
            let label = self.b.new_label();
            self.b.bind(label);
            self.loop_label = Some(label);
        }
        match self.frame.kind {
            FrameKind::Script => {
                let script = self.tree.script();
                self.statements(script.statements(), self.completion)?;
            }
            FrameKind::Object(node) => {
                let object = Object::cast(SyntaxNode::new(self.tree, node))
                    .expect("object frame points at an Object");
                let tail = self.tail_callable();
                self.object_body(object, self.completion, tail)?;
            }
            FrameKind::Module(node) => {
                let module = kette_syntax::ast::ModDecl::cast(SyntaxNode::new(self.tree, node))
                    .expect("module frame points at a ModDecl");
                self.statements(module.statements(), self.completion)?;
            }
        }
        self.b.load(self.completion);
        if self.frame.creates_context {
            self.b.pop_context(self.ctx_save);
        }
        self.b.ret();
        Ok(self.finish())
    }

    fn prologue(&mut self) -> Result<(), CompileError> {
        let root_global = matches!(self.frame.kind, FrameKind::Script)
            && matches!(self.mode, SourceMode::Script | SourceMode::Repl);
        if self.frame.creates_context {
            let mut locals = Vec::new();
            for &scope in &self.frame.scopes {
                for &binding in &self.resolved.scope(scope).bindings {
                    let binding = self.resolved.binding(binding);
                    let mut info = if matches!(
                        binding.kind,
                        BindingKind::Param | BindingKind::Rest | BindingKind::SelfParam
                    ) {
                        MODE_VAR
                    } else {
                        MODE_LET
                    };
                    if root_global
                        && !matches!(
                            binding.kind,
                            BindingKind::Param | BindingKind::Rest | BindingKind::SelfParam
                        )
                    {
                        info |= INFO_GLOBAL;
                    }
                    locals.push(ScopeLocal {
                        name: self.resolved.name(binding.name).as_bytes().into(),
                        info: info as u32,
                    });
                }
            }
            let mut flags = SCOPE_DECLARATION;
            if root_global {
                flags |= SCOPE_SCRIPT;
                if self.mode == SourceMode::Repl {
                    flags |= SCOPE_REPL;
                }
            }
            let info = self.b.constant(Constant::ScopeInfo(ScopeLayout {
                locals: locals.into_boxed_slice(),
                flags,
            }));
            self.b.create_function_context(info);
            self.b.push_context(self.ctx_save);
        }
        if root_global {
            let declarations = GlobalDeclarations {
                vars: Box::new([]),
                funcs: Box::new([]),
            };
            let index = self.b.constant(Constant::GlobalDeclarations(declarations));
            self.b
                .call_native_staged(self.declare_globals, &[RtArg::Const(index)]);
        }
        self.b.load_undefined();
        self.b.store(self.completion);

        for (index, &binding) in self.frame.params.iter().enumerate() {
            let param = self.b.param(index as u32);
            match self.plan.location(binding) {
                Location::Reg(reg) => self.b.move_reg(reg, param),
                Location::Context(slot) => {
                    self.b.load(param);
                    self.b.store_context_slot(slot, 0);
                }
            }
        }
        if let Some(rest) = self.frame.rest {
            self.b.create_rest_parameter(self.frame.params.len() as u32);
            self.store_binding(rest)?;
        }
        if let Some(this) = self.frame.self_binding {
            self.b.load(self.b.this_reg());
            self.store_binding(this)?;
        }
        if matches!(self.frame.kind, FrameKind::Module(_))
            && let Some(staging) = self.frame.staging
        {
            self.b.load(self.b.param(0));
            self.b.store(staging);
        }
        Ok(())
    }

    fn statements(
        &mut self,
        statements: kette_syntax::AstChildren<'a, Stmt<'a>>,
        sink: Reg,
    ) -> Result<(), CompileError> {
        for statement in statements {
            if !self.b.is_live() {
                break;
            }
            self.statement(statement, sink)?;
        }
        Ok(())
    }

    fn statement(&mut self, statement: Stmt<'a>, sink: Reg) -> Result<(), CompileError> {
        match statement {
            Stmt::Let(statement) => self.let_statement(statement),
            Stmt::Expr(statement) => {
                if let Some(expr) = statement.expr() {
                    self.expr(expr)?;
                    if self.b.is_live() {
                        self.b.store(sink);
                    }
                }
                Ok(())
            }
            Stmt::Mod(module) => self.module(module.syntax().id()),
            Stmt::Use(declaration) => self.use_decl(declaration),
            Stmt::Export(export) => self.export(export),
        }
    }

    fn object_body(
        &mut self,
        object: Object<'a>,
        sink: Reg,
        tail: bool,
    ) -> Result<(), CompileError> {
        let entries: Vec<_> = object.entries().collect();
        let last = entries.len().saturating_sub(1);
        for (index, entry) in entries.into_iter().enumerate() {
            if !self.b.is_live() {
                break;
            }
            match entry {
                ObjectEntry::Slot(_) | ObjectEntry::Params(_) => {}
                ObjectEntry::Let(statement) => self.let_statement(statement)?,
                ObjectEntry::Export(export) => self.export(export)?,
                ObjectEntry::Expr(statement) => {
                    if let Some(expr) = statement.expr() {
                        let mut tailed = false;
                        if tail && index == last && self.try_depth == 0 {
                            tailed = self.tail_expr(expr)?;
                        }
                        if !tailed {
                            self.expr(expr)?;
                        }
                        if self.b.is_live() {
                            self.b.store(sink);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn tail_callable(&self) -> bool {
        matches!(self.frame.kind, FrameKind::Object(_))
            && !self.frame.creates_context
            && self.frame.context_slots == 0
            && self.frame.rest.is_none()
    }

    fn tail_expr(&mut self, expr: Expr<'a>) -> Result<bool, CompileError> {
        match expr {
            Expr::Call(call) => {
                self.call_expr(call, true)?;
                Ok(true)
            }
            Expr::Send(send) => {
                self.send_expr(send, true)?;
                Ok(true)
            }
            Expr::If(expr) => {
                self.if_expr(expr, true)?;
                Ok(true)
            }
            Expr::Match(expr) => {
                self.match_expr(expr, true)?;
                Ok(true)
            }
            Expr::Paren(paren) => {
                let Some(inner) = paren.expr() else {
                    return Ok(false);
                };
                self.tail_expr(inner)
            }
            _ => Ok(false),
        }
    }

    fn tail_mode_call(&self, callee: Option<Expr<'a>>, argc: usize) -> TailMode {
        if argc != self.frame.params.len() {
            return TailMode::None;
        }
        match callee {
            Some(Expr::Atom(atom)) => {
                let Some(token) = atom.token() else {
                    return TailMode::None;
                };
                if token.kind() == SyntaxKind::SelfContext {
                    return TailMode::Loop;
                }
                let Some(resolution) = self.resolved.resolution(atom.syntax().id()) else {
                    return TailMode::None;
                };
                let binding = match resolution {
                    Resolution::Local(binding) | Resolution::Upvar(binding) => binding,
                    Resolution::Global => return TailMode::None,
                };
                if self.frame.recursive_binding != Some(binding) {
                    return TailMode::None;
                }
                let binding = self.resolved.binding(binding);
                if binding.assigned || binding.in_loop {
                    TailMode::Guard
                } else {
                    TailMode::Loop
                }
            }
            Some(Expr::Paren(paren)) => paren.expr().map_or(TailMode::None, |inner| {
                self.tail_mode_call(Some(inner), argc)
            }),
            _ => TailMode::None,
        }
    }

    fn tail_mode_send(&self, receiver: Option<Expr<'a>>, argc: usize) -> TailMode {
        if argc != self.frame.params.len() {
            return TailMode::None;
        }
        match receiver {
            Some(Expr::Atom(atom))
                if atom
                    .token()
                    .is_some_and(|token| token.kind() == SyntaxKind::SelfKw) =>
            {
                TailMode::Guard
            }
            Some(Expr::Paren(paren)) => paren.expr().map_or(TailMode::None, |inner| {
                self.tail_mode_send(Some(inner), argc)
            }),
            _ => TailMode::None,
        }
    }

    fn let_statement(&mut self, statement: LetStmt<'a>) -> Result<(), CompileError> {
        if let Some(value) = statement.value() {
            self.expr(value)?;
        }
        if let Some(pattern) = statement.pattern() {
            self.bind_pattern_acc(pattern)?;
        }
        Ok(())
    }

    fn inline_body(&mut self, object: Object<'a>, sink: Reg) -> Result<(), CompileError> {
        self.inline_body_with(object, sink, false)
    }

    fn inline_body_with(
        &mut self,
        object: Object<'a>,
        sink: Reg,
        tail: bool,
    ) -> Result<(), CompileError> {
        if object.params().is_some() {
            return self.err_node(
                object.syntax().id(),
                "parameters in an inlined control body",
            );
        }
        if object.has_slots() {
            return self.err_node(object.syntax().id(), "slots in a control body");
        }
        self.object_body(object, sink, tail)
    }

    fn expr(&mut self, expr: Expr<'a>) -> Result<(), CompileError> {
        match expr {
            Expr::Atom(atom) => self.atom(atom),
            Expr::Paren(paren) => {
                if let Some(expr) = paren.expr() {
                    self.expr(expr)?;
                }
                Ok(())
            }
            Expr::Object(object) => self.object(object),
            Expr::Array(array) => {
                let elements: Vec<_> = array.elements().collect();
                let temps = self.b.temp_scope();
                if elements.is_empty() {
                    self.b.create_empty_array_literal();
                } else {
                    self.b.create_sized_array_literal(elements.len() as u32);
                }
                let array = self.b.stage_acc(&temps);
                for (index, element) in elements.into_iter().enumerate() {
                    self.expr(element)?;
                    let feedback = self.b.new_feedback();
                    self.b.store_in_array_literal(array, index as u32, feedback);
                }
                self.b.load(array);
                Ok(())
            }
            Expr::If(expr) => self.if_expr(expr, false),
            Expr::While(expr) => self.while_expr(expr),
            Expr::For(expr) => self.for_expr(expr),
            Expr::Try(expr) => self.try_expr(expr),
            Expr::Throw(expr) => self.throw_expr(expr),
            Expr::Return(statement) => {
                let tail = self.try_depth == 0 && self.tail_callable();
                let mut tailed = false;
                if let Some(value) = statement.value() {
                    if tail {
                        tailed = self.tail_expr(value)?;
                    }
                    if !tailed {
                        self.expr(value)?;
                    }
                } else {
                    self.b.load_null();
                }
                if self.frame.creates_context {
                    self.b.pop_context(self.ctx_save);
                }
                self.b.ret();
                Ok(())
            }
            Expr::Break(statement) => {
                let Some(context) = self.loops.last().copied() else {
                    return self.err_node(statement.syntax().id(), "break outside a loop");
                };
                if let Some(value) = statement.value() {
                    self.expr(value)?;
                } else {
                    self.b.load_null();
                }
                self.b.store(context.result);
                self.b.jump(context.break_label);
                Ok(())
            }
            Expr::Unary(expr) => self.unary_expr(expr),
            Expr::Binary(expr) => self.binary_expr(expr),
            Expr::Assign(expr) => self.assign_expr(expr),
            Expr::Get(expr) => self.get_expr(expr),
            Expr::Index(expr) => self.index_expr(expr),
            Expr::Send(expr) => self.send_expr(expr, false),
            Expr::Call(expr) => self.call_expr(expr, false),
            Expr::Match(expr) => self.match_expr(expr, false),
        }
    }

    fn atom(&mut self, atom: kette_syntax::ast::Atom<'a>) -> Result<(), CompileError> {
        let Some(token) = atom.token() else {
            return self.err_node(atom.syntax().id(), "empty atom");
        };
        match token.kind() {
            SyntaxKind::Ident | SyntaxKind::SelfKw => self.load_resolution(atom.syntax().id()),
            SyntaxKind::SelfContext => {
                self.b.load_current_closure();
                Ok(())
            }
            SyntaxKind::True => {
                self.b.load_true();
                Ok(())
            }
            SyntaxKind::False => {
                self.b.load_false();
                Ok(())
            }
            SyntaxKind::Null => {
                self.b.load_null();
                Ok(())
            }
            SyntaxKind::Number => {
                let value = token.text().parse::<f64>().unwrap_or(0.0);
                self.number(value);
                Ok(())
            }
            SyntaxKind::String => {
                let bytes = decode_string(token.text());
                let index = self.b.constant(Constant::String(bytes.into_boxed_slice()));
                self.b.load_constant(index);
                Ok(())
            }
            _ => self.err_node(atom.syntax().id(), "unsupported literal"),
        }
    }

    fn number(&mut self, value: f64) {
        const MAX_EXACT_INT: f64 = 9007199254740991.0;
        let is_int = value.fract() == 0.0 && !(value == 0.0 && value.is_sign_negative());
        if is_int && value >= i16::MIN as f64 && value <= i16::MAX as f64 {
            self.b.load_smi(value as i32);
        } else if is_int && value.abs() <= MAX_EXACT_INT {
            let index = self.b.constant(Constant::Smi(value as i64));
            self.b.load_constant(index);
        } else {
            let index = self.b.constant(Constant::Float(value));
            self.b.load_constant(index);
        }
    }

    fn get_expr(&mut self, expr: GetExpr<'a>) -> Result<(), CompileError> {
        let Some(receiver) = expr.receiver() else {
            return self.err_node(expr.syntax().id(), "missing receiver");
        };
        let Some(name) = expr.name() else {
            return self.err_node(expr.syntax().id(), "missing property name");
        };
        let temps = self.b.temp_scope();
        self.expr(receiver)?;
        let receiver = self.b.stage_acc(&temps);
        let name = self.b.name(name.text().as_bytes());
        let feedback = self.b.new_feedback();
        self.b.load_named_property(receiver, name, feedback);
        Ok(())
    }

    fn index_expr(&mut self, expr: IndexExpr<'a>) -> Result<(), CompileError> {
        let Some(receiver) = expr.receiver() else {
            return self.err_node(expr.syntax().id(), "missing receiver");
        };
        let temps = self.b.temp_scope();
        self.expr(receiver)?;
        let receiver = self.b.stage_acc(&temps);
        if let Some(number) = expr.number() {
            let value = number.text().parse::<i32>().unwrap_or(0);
            self.b.load_smi(value);
        } else if let Some(index) = expr.index() {
            self.expr(index)?;
        } else {
            return self.err_node(expr.syntax().id(), "missing index");
        }
        let feedback = self.b.new_feedback();
        self.b.load_indexed(receiver, feedback);
        Ok(())
    }

    fn send_expr(&mut self, expr: SendExpr<'a>, tail: bool) -> Result<(), CompileError> {
        let Some(receiver) = expr.receiver() else {
            return self.err_node(expr.syntax().id(), "missing receiver");
        };
        let Some(name) = expr.name() else {
            return self.err_node(expr.syntax().id(), "missing method name");
        };
        let Some(args) = expr.args() else {
            return self.err_node(expr.syntax().id(), "missing arguments");
        };
        let args: Vec<_> = args.args().collect();
        let mode = if tail {
            self.tail_mode_send(Some(receiver), args.len())
        } else {
            TailMode::None
        };
        let temps = self.b.temp_scope();
        self.expr(receiver)?;
        let receiver = self.b.stage_acc(&temps);
        let name = self.b.name(name.text().as_bytes());
        let feedback = self.b.new_feedback();
        self.b.load_named_property(receiver, name, feedback);
        let callee = self.b.stage_acc(&temps);
        if mode == TailMode::Guard {
            return self.guarded_tail_args(&temps, callee, receiver, &args, false);
        }
        self.call_args(&temps, callee, Some(receiver), &args)
    }

    fn call_expr(&mut self, expr: CallExpr<'a>, tail: bool) -> Result<(), CompileError> {
        let Some(receiver) = expr.receiver() else {
            return self.err_node(expr.syntax().id(), "missing callee");
        };
        let Some(args) = expr.args() else {
            return self.err_node(expr.syntax().id(), "missing arguments");
        };
        let args: Vec<_> = args.args().collect();
        let mode = if tail {
            self.tail_mode_call(Some(receiver), args.len())
        } else {
            TailMode::None
        };
        let temps = self.b.temp_scope();
        if mode == TailMode::Loop {
            return self.tail_loop_args(&temps, &args, true);
        }
        self.expr(receiver)?;
        let callee = self.b.stage_acc(&temps);
        if mode == TailMode::Guard {
            return self.guarded_tail_args(&temps, callee, callee, &args, true);
        }
        self.call_args(&temps, callee, None, &args)
    }

    fn call_args(
        &mut self,
        temps: &TempScope,
        callee: Reg,
        receiver: Option<Reg>,
        args: &[Expr<'a>],
    ) -> Result<(), CompileError> {
        let receiver = receiver.unwrap_or(callee);
        let feedback = self.b.new_feedback();
        match args.len() {
            0 => self.b.call_method0(callee, receiver, feedback),
            1 => {
                self.expr(args[0])?;
                let arg = self.b.stage_acc(temps);
                self.b.call_method1(callee, receiver, arg, feedback)
            }
            2 => {
                self.expr(args[0])?;
                let arg0 = self.b.stage_acc(temps);
                self.expr(args[1])?;
                let arg1 = self.b.stage_acc(temps);
                self.b.call_method2(callee, receiver, arg0, arg1, feedback)
            }
            count => {
                let base = temps.reserve(count as u32 + 2);
                let top = base.index() + count as i32;
                let window_callee = Reg::new(top + 1);
                self.b.move_reg(window_callee, callee);
                self.b.move_reg(Reg::new(top), receiver);
                for (index, arg) in args.iter().enumerate() {
                    self.expr(*arg)?;
                    self.b.store(Reg::new(top - 1 - index as i32));
                }
                self.b.call(
                    window_callee,
                    RegList::new(base, count as u32 + 1),
                    feedback,
                )
            }
        }
        Ok(())
    }

    fn tail_loop_args(
        &mut self,
        temps: &TempScope,
        args: &[Expr<'a>],
        rebind_self: bool,
    ) -> Result<(), CompileError> {
        let Some(label) = self.loop_label else {
            return self.err_node(self.tree.root_id(), "tail loop outside a loop frame");
        };
        debug_assert_eq!(args.len(), self.frame.params.len());
        let count = args.len();
        let mut params = Vec::with_capacity(count);
        for &binding in &self.frame.params {
            let Location::Reg(reg) = self.plan.location(binding) else {
                return self.err_node(
                    self.tree.root_id(),
                    "tail loop parameter is not in a register",
                );
            };
            params.push(reg);
        }
        let staged = count.saturating_sub(1);
        let base = temps.reserve(staged as u32);
        for (index, arg) in args.iter().enumerate() {
            self.expr(*arg)?;
            if index == staged {
                self.b.store(params[index]);
            } else {
                self.b.store(Reg::new(base.index() + index as i32));
            }
        }
        for (index, param) in params.iter().take(staged).enumerate() {
            self.b
                .move_reg(*param, Reg::new(base.index() + index as i32));
        }
        if rebind_self && let Some(binding) = self.frame.self_binding {
            self.b.load_current_closure();
            self.store_binding(binding)?;
        }
        self.b.jump_loop(label);
        Ok(())
    }

    fn guarded_tail_args(
        &mut self,
        temps: &TempScope,
        callee: Reg,
        receiver: Reg,
        args: &[Expr<'a>],
        rebind_self: bool,
    ) -> Result<(), CompileError> {
        if self.loop_label.is_none() {
            return self.err_node(
                self.tree.root_id(),
                "guarded tail call outside a loop frame",
            );
        }
        debug_assert_eq!(args.len(), self.frame.params.len());
        self.b.tail_guard(callee);
        let slow = self.b.new_label();
        self.b.jump_if_falsy(slow);
        self.tail_loop_args(temps, args, rebind_self)?;
        self.b.bind(slow);
        let mut arg_regs = Vec::with_capacity(args.len());
        for arg in args {
            self.expr(*arg)?;
            arg_regs.push(self.b.stage_acc(temps));
        }
        self.call_regs(temps, callee, receiver, &arg_regs)
    }

    fn call_regs(
        &mut self,
        temps: &TempScope,
        callee: Reg,
        receiver: Reg,
        args: &[Reg],
    ) -> Result<(), CompileError> {
        let feedback = self.b.new_feedback();
        match args.len() {
            0 => self.b.call_method0(callee, receiver, feedback),
            1 => self.b.call_method1(callee, receiver, args[0], feedback),
            2 => self
                .b
                .call_method2(callee, receiver, args[0], args[1], feedback),
            count => {
                let base = temps.reserve(count as u32 + 2);
                let top = base.index() + count as i32;
                let window_callee = Reg::new(top + 1);
                self.b.move_reg(window_callee, callee);
                self.b.move_reg(Reg::new(top), receiver);
                for (index, arg) in args.iter().enumerate() {
                    self.b.load(*arg);
                    self.b.store(Reg::new(top - 1 - index as i32));
                }
                self.b.call(
                    window_callee,
                    RegList::new(base, count as u32 + 1),
                    feedback,
                )
            }
        }
        Ok(())
    }

    fn send_reg0(
        &mut self,
        receiver: Reg,
        name: &str,
        temps: &TempScope,
    ) -> Result<Reg, CompileError> {
        let name = self.b.name(name.as_bytes());
        let feedback = self.b.new_feedback();
        self.b.load_named_property(receiver, name, feedback);
        let callee = self.b.stage_acc(temps);
        self.call_regs(temps, callee, receiver, &[])?;
        Ok(self.b.stage_acc(temps))
    }

    fn binary_expr(&mut self, expr: BinaryExpr<'a>) -> Result<(), CompileError> {
        let (Some(lhs), Some(rhs), Some(op)) = (expr.lhs(), expr.rhs(), expr.op()) else {
            return self.err_node(expr.syntax().id(), "incomplete binary expression");
        };
        let temps = self.b.temp_scope();
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            self.expr(lhs)?;
            let value = self.b.stage_acc(&temps);
            let end = self.b.new_label();
            if op == BinaryOp::And {
                self.b.jump_if_falsy(end);
            } else {
                self.b.jump_if_truthy(end);
            }
            self.expr(rhs)?;
            self.b.store(value);
            self.b.bind(end);
            self.b.load(value);
            return Ok(());
        }
        self.expr(lhs)?;
        let lhs = self.b.stage_acc(&temps);
        self.expr(rhs)?;
        let rhs = self.b.stage_acc(&temps);
        self.binary_regs(op, lhs, rhs);
        Ok(())
    }

    fn binary_regs(&mut self, op: BinaryOp, lhs: Reg, rhs: Reg) {
        match op {
            BinaryOp::EqEq
            | BinaryOp::NotEq
            | BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::LtEq
            | BinaryOp::GtEq => {
                self.b.load(lhs);
                match op {
                    BinaryOp::EqEq => self.b.equal(rhs),
                    BinaryOp::NotEq => {
                        self.b.equal(rhs);
                        self.invert_acc();
                    }
                    BinaryOp::Lt => self.b.less_than(rhs),
                    BinaryOp::Gt => self.b.greater_than(rhs),
                    BinaryOp::LtEq => self.b.less_than_or_equal(rhs),
                    BinaryOp::GtEq => self.b.greater_than_or_equal(rhs),
                    _ => unreachable!(),
                }
            }
            _ => {
                self.b.load(rhs);
                match op {
                    BinaryOp::Plus => self.b.add(lhs),
                    BinaryOp::Minus => self.b.sub(lhs),
                    BinaryOp::Star => self.b.mul(lhs),
                    BinaryOp::Slash => self.b.div(lhs),
                    BinaryOp::Percent => self.b.mod_(lhs),
                    BinaryOp::LtLt => self.b.shift_left(lhs),
                    BinaryOp::GtGt => self.b.shift_right(lhs),
                    _ => unreachable!(),
                }
            }
        }
    }

    fn unary_expr(&mut self, expr: UnaryExpr<'a>) -> Result<(), CompileError> {
        let Some(operand) = expr.operand() else {
            return self.err_node(expr.syntax().id(), "missing operand");
        };
        self.expr(operand)?;
        match expr.op() {
            Some(UnaryOp::Neg) => self.b.negate(),
            Some(UnaryOp::Not) => self.invert_acc(),
            None => return self.err_node(expr.syntax().id(), "unsupported unary operator"),
        }
        Ok(())
    }

    fn invert_acc(&mut self) {
        let truthy = self.b.new_label();
        let end = self.b.new_label();
        self.b.jump_if_falsy(truthy);
        self.b.load_false();
        self.b.jump(end);
        self.b.bind(truthy);
        self.b.load_true();
        self.b.bind(end);
    }

    fn assign_expr(&mut self, expr: AssignExpr<'a>) -> Result<(), CompileError> {
        let (Some(target), Some(value), Some(op)) = (expr.place(), expr.value(), expr.op()) else {
            return self.err_node(expr.syntax().id(), "incomplete assignment");
        };
        if op == AssignOp::Assign {
            return self.assign(target, value);
        }
        if matches!(op, AssignOp::And | AssignOp::Or) {
            return self.logical_assign(target, value, op == AssignOp::And);
        }
        let binary = match op {
            AssignOp::Add => BinaryOp::Plus,
            AssignOp::Sub => BinaryOp::Minus,
            AssignOp::Mul => BinaryOp::Star,
            AssignOp::Div => BinaryOp::Slash,
            AssignOp::Rem => BinaryOp::Percent,
            AssignOp::Shl => BinaryOp::LtLt,
            AssignOp::Shr => BinaryOp::GtGt,
            _ => return self.err_node(expr.syntax().id(), "compound assignment"),
        };
        let temps = self.b.temp_scope();
        match target {
            Expr::Atom(_) => {
                self.expr(target)?;
                let old = self.b.stage_acc(&temps);
                self.expr(value)?;
                let rhs = self.b.stage_acc(&temps);
                self.binary_regs(binary, old, rhs);
                self.store_target(target)
            }
            Expr::Get(get) => {
                let receiver = get
                    .receiver()
                    .ok_or_else(|| self.error(expr.syntax().id(), "missing receiver"))?;
                let name = get
                    .name()
                    .ok_or_else(|| self.error(expr.syntax().id(), "missing name"))?;
                self.expr(receiver)?;
                let receiver_reg = self.b.stage_acc(&temps);
                let name_const = self.b.name(name.text().as_bytes());
                let feedback = self.b.new_feedback();
                self.b
                    .load_named_property(receiver_reg, name_const, feedback);
                let old = self.b.stage_acc(&temps);
                self.expr(value)?;
                let rhs = self.b.stage_acc(&temps);
                self.binary_regs(binary, old, rhs);
                let feedback = self.b.new_feedback();
                self.b
                    .store_named_property_no_shadow(receiver_reg, name_const, feedback);
                Ok(())
            }
            Expr::Index(index) => {
                let receiver = index
                    .receiver()
                    .ok_or_else(|| self.error(expr.syntax().id(), "missing receiver"))?;
                self.expr(receiver)?;
                let receiver_reg = self.b.stage_acc(&temps);
                if let Some(number) = index.number() {
                    self.b.load_smi(number.text().parse::<i32>().unwrap_or(0));
                } else if let Some(index_expr) = index.index() {
                    self.expr(index_expr)?;
                } else {
                    return self.err_node(expr.syntax().id(), "missing index");
                }
                let key = self.b.stage_acc(&temps);
                let feedback = self.b.new_feedback();
                self.b.load_indexed(receiver_reg, feedback);
                let old = self.b.stage_acc(&temps);
                self.expr(value)?;
                let rhs = self.b.stage_acc(&temps);
                self.binary_regs(binary, old, rhs);
                let feedback = self.b.new_feedback();
                self.b.store_indexed(receiver_reg, key, feedback);
                Ok(())
            }
            _ => self.err_node(expr.syntax().id(), "assignment target"),
        }
    }

    fn logical_assign(
        &mut self,
        target: Expr<'a>,
        value: Expr<'a>,
        and: bool,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let skip = self.b.new_label();
        let end = self.b.new_label();
        match target {
            Expr::Atom(_) => {
                self.expr(target)?;
                let old = self.b.stage_acc(&temps);
                if and {
                    self.b.jump_if_falsy(skip);
                } else {
                    self.b.jump_if_truthy(skip);
                }
                self.expr(value)?;
                self.store_target(target)?;
                self.b.jump(end);
                self.b.bind(skip);
                self.b.load(old);
                self.b.bind(end);
                Ok(())
            }
            Expr::Get(get) => {
                let receiver = get
                    .receiver()
                    .ok_or_else(|| self.error(target.syntax().id(), "missing receiver"))?;
                let name = get
                    .name()
                    .ok_or_else(|| self.error(target.syntax().id(), "missing name"))?;
                self.expr(receiver)?;
                let receiver = self.b.stage_acc(&temps);
                let name_const = self.b.name(name.text().as_bytes());
                let feedback = self.b.new_feedback();
                self.b.load_named_property(receiver, name_const, feedback);
                let old = self.b.stage_acc(&temps);
                if and {
                    self.b.jump_if_falsy(skip);
                } else {
                    self.b.jump_if_truthy(skip);
                }
                self.expr(value)?;
                let feedback = self.b.new_feedback();
                self.b
                    .store_named_property_no_shadow(receiver, name_const, feedback);
                self.b.jump(end);
                self.b.bind(skip);
                self.b.load(old);
                self.b.bind(end);
                Ok(())
            }
            Expr::Index(index) => {
                let receiver = index
                    .receiver()
                    .ok_or_else(|| self.error(target.syntax().id(), "missing receiver"))?;
                self.expr(receiver)?;
                let receiver = self.b.stage_acc(&temps);
                if let Some(number) = index.number() {
                    self.b.load_smi(number.text().parse::<i32>().unwrap_or(0));
                } else if let Some(key) = index.index() {
                    self.expr(key)?;
                } else {
                    return self.err_node(target.syntax().id(), "missing index");
                }
                let key = self.b.stage_acc(&temps);
                let feedback = self.b.new_feedback();
                self.b.load_indexed(receiver, feedback);
                let old = self.b.stage_acc(&temps);
                if and {
                    self.b.jump_if_falsy(skip);
                } else {
                    self.b.jump_if_truthy(skip);
                }
                self.expr(value)?;
                let feedback = self.b.new_feedback();
                self.b.store_indexed(receiver, key, feedback);
                self.b.jump(end);
                self.b.bind(skip);
                self.b.load(old);
                self.b.bind(end);
                Ok(())
            }
            _ => self.err_node(target.syntax().id(), "assignment target"),
        }
    }

    fn assign(&mut self, target: Expr<'a>, value: Expr<'a>) -> Result<(), CompileError> {
        match target {
            Expr::Atom(_) => {
                self.expr(value)?;
                self.store_target(target)
            }
            Expr::Get(get) => {
                let Some(receiver) = get.receiver() else {
                    return self.err_node(target.syntax().id(), "missing receiver");
                };
                let Some(name) = get.name() else {
                    return self.err_node(target.syntax().id(), "missing name");
                };
                let temps = self.b.temp_scope();
                self.expr(receiver)?;
                let receiver = self.b.stage_acc(&temps);
                self.expr(value)?;
                let name = self.b.name(name.text().as_bytes());
                let feedback = self.b.new_feedback();
                self.b
                    .store_named_property_no_shadow(receiver, name, feedback);
                Ok(())
            }
            Expr::Index(index) => {
                let Some(receiver) = index.receiver() else {
                    return self.err_node(target.syntax().id(), "missing receiver");
                };
                let temps = self.b.temp_scope();
                self.expr(receiver)?;
                let receiver = self.b.stage_acc(&temps);
                if let Some(number) = index.number() {
                    self.b.load_smi(number.text().parse::<i32>().unwrap_or(0));
                } else if let Some(index_expr) = index.index() {
                    self.expr(index_expr)?;
                } else {
                    return self.err_node(target.syntax().id(), "missing index");
                }
                let key = self.b.stage_acc(&temps);
                self.expr(value)?;
                let feedback = self.b.new_feedback();
                self.b.store_indexed(receiver, key, feedback);
                Ok(())
            }
            _ => self.err_node(
                target.syntax().id(),
                format!("assignment target {:?}", target.syntax().kind()),
            ),
        }
    }

    fn store_target(&mut self, target: Expr<'a>) -> Result<(), CompileError> {
        match target {
            Expr::Atom(atom) => self.store_resolution(atom.syntax().id()),
            Expr::Get(get) => {
                let Some(receiver) = get.receiver() else {
                    return self.err_node(target.syntax().id(), "missing receiver");
                };
                let Some(name) = get.name() else {
                    return self.err_node(target.syntax().id(), "missing name");
                };
                let temps = self.b.temp_scope();
                let value = self.b.stage_acc(&temps);
                self.expr(receiver)?;
                let receiver = self.b.stage_acc(&temps);
                self.b.load(value);
                let name = self.b.name(name.text().as_bytes());
                let feedback = self.b.new_feedback();
                self.b
                    .store_named_property_no_shadow(receiver, name, feedback);
                Ok(())
            }
            _ => self.err_node(
                target.syntax().id(),
                format!("assignment target {:?}", target.syntax().kind()),
            ),
        }
    }

    fn object(&mut self, object: Object<'a>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let has_elements = object.slots().any(|slot| {
            matches!(
                slot,
                SlotEntry::Slot(slot) if slot.kind() == Some(SlotKind::Numeric)
            )
        });
        if object.is_callable() {
            let frame = self.plan.function_of_node[object.syntax().id().0 as usize]
                .ok_or_else(|| self.error(object.syntax().id(), "missing object function"))?;
            let index = self.b.constant(Constant::Callable(FunctionId(frame.0)));
            self.b.create_closure(index);
        } else if has_elements {
            self.b.create_empty_array_literal();
        } else {
            self.b.create_empty_object_literal();
        }
        let object_reg = self.b.stage_acc(&temps);
        for slot in object.slots() {
            self.emit_slot(slot, object_reg, &temps)?;
        }
        self.b.load(object_reg);
        Ok(())
    }

    fn emit_slot(
        &mut self,
        slot: SlotEntry<'a>,
        object: Reg,
        temps: &TempScope,
    ) -> Result<(), CompileError> {
        match slot {
            SlotEntry::Multi(multi) => self.emit_multi_slot(multi, object, temps),
            SlotEntry::Slot(slot) => {
                let Some(kind) = slot.kind() else {
                    return self.err_node(slot.syntax().id(), "invalid slot");
                };
                let Some(value) = slot.value() else {
                    return self.err_node(slot.syntax().id(), "missing slot value");
                };
                match kind {
                    SlotKind::Named | SlotKind::Quoted => {
                        let name = slot.key_text().unwrap_or_default().as_bytes().to_vec();
                        let name = self.b.name(&name);
                        self.expr(value)?;
                        let feedback = self.b.new_feedback();
                        self.b.define_named_own_property(object, name, 0, feedback);
                    }
                    SlotKind::Parent => {
                        let name = slot.key_text().unwrap_or_default().as_bytes().to_vec();
                        let name = self.b.name(&name);
                        self.expr(value)?;
                        self.b.add_parent(object, name);
                    }
                    SlotKind::Getter | SlotKind::Setter => {
                        let name = slot.key_text().unwrap_or_default().as_bytes().to_vec();
                        let name = self.b.name(&name);
                        self.expr(value)?;
                        self.b.call_native_staged(
                            self.install_accessor,
                            &[
                                RtArg::Reg(object),
                                RtArg::Const(name),
                                RtArg::Acc,
                                RtArg::Smi(u32::from(kind == SlotKind::Getter)),
                            ],
                        );
                    }
                    SlotKind::Numeric => {
                        let key = slot.key_text().unwrap_or("0").parse::<i32>().unwrap_or(0);
                        self.b.load_smi(key);
                        let key = self.b.stage_acc(temps);
                        self.expr(value)?;
                        let feedback = self.b.new_feedback();
                        self.b.define_keyed_own_property(object, key, 0, feedback);
                    }
                }
                Ok(())
            }
        }
    }

    fn emit_multi_slot(
        &mut self,
        slot: SlotMulti<'a>,
        object: Reg,
        temps: &TempScope,
    ) -> Result<(), CompileError> {
        let Some(value) = slot.value() else {
            return self.err_node(slot.syntax().id(), "missing slot value");
        };
        self.expr(value)?;
        let value = self.b.stage_acc(temps);
        for key in slot.key_texts() {
            let name = self.b.name(key.as_bytes());
            self.b.load(value);
            let feedback = self.b.new_feedback();
            self.b.define_named_own_property(object, name, 0, feedback);
        }
        Ok(())
    }

    fn module(&mut self, node: NodeId) -> Result<(), CompileError> {
        let module = kette_syntax::ast::ModDecl::cast(SyntaxNode::new(self.tree, node))
            .expect("module statement");
        let frame = self.plan.function_of_node[node.0 as usize]
            .ok_or_else(|| self.error(node, "missing module function"))?;
        let path = module
            .path()
            .map(|path| {
                path.segments()
                    .map(|token| token.text().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let temps = self.b.temp_scope();
        let registry_feedback = self.b.new_feedback();
        self.load_global("ModuleRegistry");
        let registry = self.b.stage_acc(&temps);
        let method = if module.extend().is_some() {
            "extend"
        } else {
            "declare"
        };
        let method = self.b.name(method.as_bytes());
        self.b
            .load_named_property(registry, method, registry_feedback);
        let callee = self.b.stage_acc(&temps);
        let index = self.b.constant(Constant::Callable(FunctionId(frame.0)));
        self.b.create_closure(index);
        let closure = self.b.stage_acc(&temps);
        let args = if let Some(extension) = module.extend() {
            let target = self.modules_path(&path, &temps)?;
            let extension = extension.text().as_bytes().to_vec();
            let extension = self
                .b
                .constant(Constant::String(extension.into_boxed_slice()));
            self.b.load_constant(extension);
            let extension = self.b.stage_acc(&temps);
            vec![target, extension, closure]
        } else {
            let name = path.join(".").into_bytes();
            let name = self.b.constant(Constant::String(name.into_boxed_slice()));
            self.b.load_constant(name);
            let name = self.b.stage_acc(&temps);
            vec![name, closure]
        };
        self.call_regs(&temps, callee, registry, &args)
    }

    fn modules_path(&mut self, path: &[String], temps: &TempScope) -> Result<Reg, CompileError> {
        self.load_global("Modules");
        let mut current = self.b.stage_acc(temps);
        for segment in path {
            let name = self.b.name(segment.as_bytes());
            let feedback = self.b.new_feedback();
            self.b.load_named_property(current, name, feedback);
            current = self.b.stage_acc(temps);
        }
        Ok(current)
    }

    fn export(&mut self, export: ExportDecl<'a>) -> Result<(), CompileError> {
        let Some(name) = export.name() else {
            return self.err_node(export.syntax().id(), "missing export name");
        };
        let Some(value) = export.value() else {
            return self.err_node(export.syntax().id(), "missing export value");
        };
        if !matches!(self.frame.kind, FrameKind::Module(_)) {
            return self.err_node(export.syntax().id(), "export outside module");
        }
        let Some(staging) = self.frame.staging else {
            return self.err_node(export.syntax().id(), "missing module staging");
        };
        let name = self.b.name(name.text().as_bytes());
        self.expr(value)?;
        let feedback = self.b.new_feedback();
        self.b
            .store_named_property_no_shadow(staging, name, feedback);
        Ok(())
    }

    fn use_decl(&mut self, declaration: UseDecl<'a>) -> Result<(), CompileError> {
        let path = declaration
            .path()
            .map(|path| {
                path.segments()
                    .map(|token| token.text().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(tree) = declaration.tree() {
            return self.use_tree(&path, tree);
        }
        let alias = declaration
            .alias()
            .or_else(|| declaration.path().and_then(|p| p.last_segment()));
        let Some(alias) = alias else {
            return self.err_node(declaration.syntax().id(), "missing import name");
        };
        self.import(&path, alias)
    }

    fn use_tree(&mut self, prefix: &[String], tree: UseTree<'a>) -> Result<(), CompileError> {
        for item in tree.items() {
            let mut full = prefix.to_vec();
            full.extend(item.path.segments().map(|token| token.text().to_string()));
            if let Some(nested) = item.tree
                && item.alias.is_none()
            {
                self.use_tree(&full, nested)?;
                continue;
            }
            let alias = item.alias.or_else(|| item.path.last_segment());
            let Some(alias) = alias else {
                return self.err_node(tree.syntax().id(), "missing import name");
            };
            self.import(&full, alias)?;
        }
        Ok(())
    }

    fn import(&mut self, path: &[String], alias: SyntaxToken<'a>) -> Result<(), CompileError> {
        let binding = self.binding(alias)?;
        let temps = self.b.temp_scope();
        let value = self.modules_path(path, &temps)?;
        self.b.load(value);
        self.store_binding(binding)
    }

    fn bind_pattern_acc(&mut self, pattern: Pattern<'a>) -> Result<(), CompileError> {
        if let Pattern::Name(name) = pattern
            && let Some(token) = name.name()
        {
            let binding = self.binding(token)?;
            self.store_binding(binding)?;
            return Ok(());
        }
        let temps = self.b.temp_scope();
        let value = self.b.stage_acc(&temps);
        self.bind_pattern(pattern, value)
    }

    fn bind_pattern(&mut self, pattern: Pattern<'a>, value: Reg) -> Result<(), CompileError> {
        match pattern {
            Pattern::Name(pattern) => {
                let Some(name) = pattern.name() else {
                    return self.err_node(pattern.syntax().id(), "missing binding name");
                };
                let binding = self.binding(name)?;
                self.b.load(value);
                self.store_binding(binding)
            }
            Pattern::Fields(fields) => {
                for field in fields.fields() {
                    self.bind_field(value, field)?;
                }
                Ok(())
            }
            Pattern::Array(array) => {
                let elements: Vec<_> = array.elements().collect();
                self.bind_indexed(value, &elements, array.rest())?;
                Ok(())
            }
            Pattern::Tuple(tuple) => {
                let elements: Vec<_> = tuple.elements().collect();
                self.bind_indexed(value, &elements, None)?;
                Ok(())
            }
            Pattern::Alias(alias) => {
                if let Some(inner) = alias.pattern() {
                    self.bind_pattern(inner, value)?;
                }
                let Some(token) = alias.alias() else {
                    return self.err_node(alias.syntax().id(), "missing pattern alias");
                };
                let binding = self.binding(token)?;
                self.b.load(value);
                self.store_binding(binding)
            }
            Pattern::Literal(_) | Pattern::Wildcard(_) => Ok(()),
            Pattern::Object(_) | Pattern::Type(_) => self.err_node(
                pattern.syntax().id(),
                "unsupported pattern in this position",
            ),
        }
    }

    fn bind_field(&mut self, value: Reg, field: Field<'a>) -> Result<(), CompileError> {
        let token = field.alias.unwrap_or(field.name);
        let binding = self.binding(token)?;
        let name = self.b.name(field.name.text().as_bytes());
        let feedback = self.b.new_feedback();
        self.b.load_named_property(value, name, feedback);
        self.store_binding(binding)
    }

    fn bind_indexed(
        &mut self,
        value: Reg,
        elements: &[Pattern<'a>],
        rest: Option<SyntaxToken<'a>>,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let size = self.send_reg0(value, "size", &temps)?;
        for (index, element) in elements.iter().enumerate() {
            let missing = self.b.new_label();
            let have = self.b.new_label();
            self.b.load_smi(index as i32);
            self.b.less_than(size);
            self.b.jump_if_falsy(missing);
            self.b.load_smi(index as i32);
            let feedback = self.b.new_feedback();
            self.b.load_indexed(value, feedback);
            self.b.jump(have);
            self.b.bind(missing);
            self.b.load_undefined();
            self.b.bind(have);
            let element_value = self.b.stage_acc(&temps);
            self.bind_pattern(*element, element_value)?;
        }
        if let Some(rest) = rest {
            let tail = temps.temp();
            self.b.create_empty_array_literal();
            self.b.store(tail);
            let index = temps.temp();
            self.b.load_smi(elements.len() as i32);
            self.b.store(index);
            let start = self.b.new_label();
            let end = self.b.new_label();
            self.b.bind(start);
            self.b.load(index);
            self.b.less_than(size);
            self.b.jump_if_falsy(end);
            let key = temps.temp();
            self.b.load_smi(elements.len() as i32);
            self.b.sub(index);
            self.b.store(key);
            self.b.load(index);
            let load_feedback = self.b.new_feedback();
            self.b.load_indexed(value, load_feedback);
            let store_feedback = self.b.new_feedback();
            self.b.store_indexed(tail, key, store_feedback);
            self.b.load_smi(1);
            self.b.add(index);
            self.b.store(index);
            self.b.jump_loop(start);
            self.b.bind(end);
            let binding = self.binding(rest)?;
            self.b.load(tail);
            self.store_binding(binding)?;
        }
        Ok(())
    }

    fn pattern_needs_hole(pattern: Pattern<'_>) -> bool {
        match pattern {
            Pattern::Object(_) | Pattern::Fields(_) => true,
            Pattern::Array(array) => array
                .elements()
                .any(|element| Self::pattern_needs_hole(element)),
            Pattern::Tuple(tuple) => tuple
                .elements()
                .any(|element| Self::pattern_needs_hole(element)),
            Pattern::Alias(alias) => alias
                .pattern()
                .is_some_and(|inner| Self::pattern_needs_hole(inner)),
            Pattern::Name(_) | Pattern::Literal(_) | Pattern::Wildcard(_) | Pattern::Type(_) => {
                false
            }
        }
    }

    fn match_expr(&mut self, expr: MatchExpr<'a>, tail: bool) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let subject = temps.temp();
        if let Some(scrutinee) = expr.scrutinee() {
            self.expr(scrutinee)?;
            self.b.store(subject);
        } else {
            if self.frame.params.len() != 1 {
                return self.err_node(
                    expr.syntax().id(),
                    "a match closure must take exactly one parameter",
                );
            }
            let binding = self.frame.params[0];
            self.load_binding(binding)?;
            self.b.store(subject);
        }
        let result = temps.temp();
        self.b.load_null();
        self.b.store(result);
        let arms: Vec<MatchArm<'a>> = expr.arms().collect();
        let hole = temps.temp();
        if arms.iter().any(|arm| {
            arm.pattern()
                .is_some_and(|pattern| Self::pattern_needs_hole(pattern))
        }) {
            self.b.load_hole();
            self.b.store(hole);
        }
        let end = self.b.new_label();
        let mut cases = Vec::new();
        for arm in &arms {
            if arm.guard().is_some() {
                break;
            }
            let Some(pattern) = arm.pattern() else {
                break;
            };
            let Some(value) = Self::integer_pattern_value(pattern) else {
                break;
            };
            if cases.iter().any(|(existing, _)| *existing == value) {
                break;
            }
            cases.push((value, *arm));
        }
        let mut consumed = 0;
        if let Some(min) = Self::switch_min(&cases) {
            let labels: Vec<Label> = cases.iter().map(|_| self.b.new_label()).collect();
            let fallthrough = self.b.new_label();
            let targets: Vec<(i64, Label)> = cases
                .iter()
                .zip(&labels)
                .map(|((value, _), label)| (*value, *label))
                .collect();
            self.b
                .switch_int(subject, min, &targets, fallthrough)
                .map_err(|error| self.error(expr.syntax().id(), error.to_string()))?;
            for ((_, arm), label) in cases.iter().zip(labels) {
                self.b.bind(label);
                self.match_arm_body(*arm, result, tail, end)?;
            }
            self.b.bind(fallthrough);
            consumed = cases.len();
        }
        for arm in arms.into_iter().skip(consumed) {
            let fail = self.b.new_label();
            let Some(pattern) = arm.pattern() else {
                return self.err_node(arm.syntax().id(), "match arm without a pattern");
            };
            self.pattern_match(pattern, subject, hole, fail)?;
            if let Some(guard) = arm.guard() {
                self.expr(guard)?;
                self.b.jump_if_falsy(fail);
            }
            self.match_arm_body(arm, result, tail, end)?;
            self.b.bind(fail);
        }
        let message = Constant::String(b"match: no pattern matched".to_vec().into_boxed_slice());
        let message = self.b.constant(message);
        self.b.load_constant(message);
        self.b.throw();
        self.b.bind(end);
        self.b.load(result);
        Ok(())
    }

    fn match_arm_body(
        &mut self,
        arm: MatchArm<'a>,
        result: Reg,
        tail: bool,
        end: Label,
    ) -> Result<(), CompileError> {
        if let Some(body) = arm.body() {
            match body {
                Expr::Object(object)
                    if object.params().is_none()
                        && !object.has_slots()
                        && object.statements().next().is_some() =>
                {
                    self.inline_body_with(object, result, tail)?;
                }
                other => {
                    let mut tailed = false;
                    if tail {
                        tailed = self.tail_expr(other)?;
                    }
                    if !tailed {
                        self.expr(other)?;
                        self.b.store(result);
                    }
                }
            }
        }
        self.b.jump(end);
        Ok(())
    }

    fn integer_pattern_value(pattern: Pattern<'a>) -> Option<i64> {
        let Pattern::Literal(literal) = pattern else {
            return None;
        };
        let mut tokens = literal
            .syntax()
            .tokens()
            .filter(|token| !token.kind().is_trivia());
        let first = tokens.next()?;
        let negative = first.kind() == SyntaxKind::Minus;
        let token = if negative { tokens.next()? } else { first };
        if token.kind() != SyntaxKind::Number {
            return None;
        }
        let value = token.text().parse::<f64>().ok()?;
        let value = if negative { -value } else { value };
        Smi::from_f64(value).map(|smi| smi.value())
    }

    fn switch_min(cases: &[(i64, MatchArm<'a>)]) -> Option<i64> {
        if cases.len() < 4 {
            return None;
        }
        let min = cases.iter().map(|(value, _)| *value).min()?;
        let max = cases.iter().map(|(value, _)| *value).max()?;
        let range = max.checked_sub(min)?.checked_add(1)?;
        let count = cases.len() as i64;
        (range <= count * 2 && range <= 1024).then_some(min)
    }

    fn pattern_match(
        &mut self,
        pattern: Pattern<'a>,
        value: Reg,
        hole: Reg,
        fail: Label,
    ) -> Result<(), CompileError> {
        match pattern {
            Pattern::Name(name) => {
                let Some(token) = name.name() else {
                    return self.err_node(name.syntax().id(), "missing binding name");
                };
                self.bind_token(token, value)
            }
            Pattern::Wildcard(_) => Ok(()),
            Pattern::Literal(literal) => self.literal_pattern(literal, value, fail),
            Pattern::Alias(alias) => {
                if let Some(inner) = alias.pattern() {
                    self.pattern_match(inner, value, hole, fail)?;
                }
                if let Some(token) = alias.alias() {
                    self.bind_token(token, value)?;
                }
                Ok(())
            }
            Pattern::Type(typed) => {
                self.load_resolution(typed.syntax().id())?;
                let temps = self.b.temp_scope();
                let type_value = self.b.stage_acc(&temps);
                let feedback = self.b.new_feedback();
                self.b
                    .test_type(value, type_value, TestTypeKind::User as u32, feedback);
                self.b.jump_if_falsy(fail);
                if let Some(token) = typed.binding()
                    && token.text() != "_"
                {
                    self.bind_token(token, value)?;
                }
                Ok(())
            }
            Pattern::Fields(fields) => {
                self.test_object(value, fail);
                let temps = self.b.temp_scope();
                for field in fields.fields() {
                    let token = field.alias.unwrap_or(field.name);
                    let field_value = self.load_named_field(
                        value,
                        field.name.text().as_bytes(),
                        hole,
                        fail,
                        &temps,
                    )?;
                    self.bind_token(token, field_value)?;
                }
                Ok(())
            }
            Pattern::Object(object) => {
                self.test_object(value, fail);
                let temps = self.b.temp_scope();
                for field in object.fields() {
                    let field_value = self.load_pattern_field(value, field, hole, fail, &temps)?;
                    if let Some(inner) = field.pattern() {
                        self.pattern_match(inner, field_value, hole, fail)?;
                    } else {
                        let Some(token) = field.alias().or_else(|| field.key()) else {
                            return self.err_node(field.syntax().id(), "missing field binding");
                        };
                        self.bind_token(token, field_value)?;
                    }
                }
                Ok(())
            }
            Pattern::Array(array) => {
                let elements: Vec<_> = array.elements().collect();
                self.pattern_sequence(value, &elements, array.rest(), hole, fail)
            }
            Pattern::Tuple(tuple) => {
                let elements: Vec<_> = tuple.elements().collect();
                self.pattern_sequence(value, &elements, None, hole, fail)
            }
        }
    }

    fn literal_pattern(
        &mut self,
        literal: kette_syntax::ast::PatternLiteral<'a>,
        value: Reg,
        fail: Label,
    ) -> Result<(), CompileError> {
        let mut tokens = literal
            .syntax()
            .tokens()
            .filter(|token| !token.kind().is_trivia());
        let Some(first) = tokens.next() else {
            return self.err_node(literal.syntax().id(), "empty literal pattern");
        };
        let negative = first.kind() == SyntaxKind::Minus;
        let Some(token) = (if negative { tokens.next() } else { Some(first) }) else {
            return self.err_node(literal.syntax().id(), "empty literal pattern");
        };
        match token.kind() {
            SyntaxKind::Number => {
                let mut number = token.text().parse::<f64>().unwrap_or(0.0);
                if negative {
                    number = -number;
                }
                self.number(number);
                self.b.equal_strict(value);
            }
            SyntaxKind::String => {
                let text = decode_string(token.text());
                let index = self.b.constant(Constant::String(text.into_boxed_slice()));
                self.b.load_constant(index);
                self.b.equal_strict(value);
            }
            SyntaxKind::True => {
                self.b.load_true();
                self.b.test_reference_equal(value);
            }
            SyntaxKind::False => {
                self.b.load_false();
                self.b.test_reference_equal(value);
            }
            SyntaxKind::Null => {
                self.b.load_null();
                self.b.test_reference_equal(value);
            }
            SyntaxKind::Ident if token.text() == "undefined" => {
                self.b.load_undefined();
                self.b.test_reference_equal(value);
            }
            _ => return self.err_node(literal.syntax().id(), "unsupported literal pattern"),
        }
        self.b.jump_if_falsy(fail);
        Ok(())
    }

    fn pattern_sequence(
        &mut self,
        value: Reg,
        elements: &[Pattern<'a>],
        rest: Option<SyntaxToken<'a>>,
        hole: Reg,
        fail: Label,
    ) -> Result<(), CompileError> {
        self.test_object(value, fail);
        let temps = self.b.temp_scope();
        let length = self.sequence_length(value, &temps)?;
        self.b.load_smi(elements.len() as i32);
        if rest.is_some() {
            self.b.greater_than(length);
            self.b.jump_if_truthy(fail);
        } else {
            self.b.equal_strict(length);
            self.b.jump_if_falsy(fail);
        }
        for (index, element) in elements.iter().enumerate() {
            self.b.load_smi(index as i32);
            let feedback = self.b.new_feedback();
            self.b.load_indexed(value, feedback);
            let element_value = self.b.stage_acc(&temps);
            self.pattern_match(*element, element_value, hole, fail)?;
        }
        if let Some(rest) = rest {
            self.bind_rest(value, elements.len(), length, rest, &temps)?;
        }
        Ok(())
    }

    fn sequence_length(&mut self, value: Reg, temps: &TempScope) -> Result<Reg, CompileError> {
        let length = temps.temp();
        let feedback = self.b.new_feedback();
        self.b.load_length(value, feedback);
        self.b.store(length);
        let call = self.b.new_label();
        let have = self.b.new_label();
        self.b.load(length);
        let feedback = self.b.new_feedback();
        self.b
            .test_type(length, length, TestTypeKind::Function as u32, feedback);
        self.b.jump_if_truthy(call);
        self.b.jump(have);
        self.b.bind(call);
        self.send_reg0(value, "size", temps)?;
        self.b.store(length);
        self.b.bind(have);
        Ok(length)
    }

    fn bind_rest(
        &mut self,
        value: Reg,
        start: usize,
        length: Reg,
        token: SyntaxToken<'a>,
        temps: &TempScope,
    ) -> Result<(), CompileError> {
        let copy = self.b.new_label();
        let done = self.b.new_label();
        if start <= u16::MAX as usize {
            let feedback = self.b.new_feedback();
            self.b
                .test_type(value, value, TestTypeKind::Array as u32, feedback);
            self.b.jump_if_falsy(copy);
            self.b.array_slice(value, start as u32);
            let sliced = temps.temp();
            self.b.store(sliced);
            self.b.load_hole();
            self.b.test_reference_equal(sliced);
            self.b.jump_if_truthy(copy);
            self.b.load(sliced);
            let binding = self.binding(token)?;
            self.store_binding(binding)?;
            self.b.jump(done);
        }
        self.b.bind(copy);
        let tail = temps.temp();
        self.b.create_empty_array_literal();
        self.b.store(tail);
        let index = temps.temp();
        self.b.load_smi(start as i32);
        self.b.store(index);
        let begin = self.b.new_label();
        let end = self.b.new_label();
        self.b.bind(begin);
        self.b.load(index);
        self.b.less_than(length);
        self.b.jump_if_falsy(end);
        let key = temps.temp();
        self.b.load_smi(start as i32);
        self.b.sub(index);
        self.b.store(key);
        self.b.load(index);
        let feedback = self.b.new_feedback();
        self.b.load_indexed(value, feedback);
        let feedback = self.b.new_feedback();
        self.b.store_indexed(tail, key, feedback);
        self.b.load_smi(1);
        self.b.add(index);
        self.b.store(index);
        self.b.jump_loop(begin);
        self.b.bind(end);
        self.bind_token(token, tail)?;
        self.b.bind(done);
        Ok(())
    }

    fn load_pattern_field(
        &mut self,
        value: Reg,
        field: PatternField<'a>,
        hole: Reg,
        fail: Label,
        temps: &TempScope,
    ) -> Result<Reg, CompileError> {
        let Some(key) = field.key() else {
            return self.err_node(field.syntax().id(), "missing pattern field key");
        };
        match key.kind() {
            SyntaxKind::Number => {
                let index = key.text().parse::<i32>().unwrap_or(0);
                self.b.load_smi(index);
                let feedback = self.b.new_feedback();
                self.b.load_keyed_property_or_hole(value, feedback);
            }
            SyntaxKind::String => {
                let name = decode_string(key.text());
                self.load_named_or_hole(value, &name);
            }
            _ => self.load_named_or_hole(value, key.text().as_bytes()),
        }
        let field_value = self.b.stage_acc(temps);
        self.b.test_reference_equal(hole);
        self.b.jump_if_truthy(fail);
        Ok(field_value)
    }

    fn load_named_field(
        &mut self,
        value: Reg,
        name: &[u8],
        hole: Reg,
        fail: Label,
        temps: &TempScope,
    ) -> Result<Reg, CompileError> {
        self.load_named_or_hole(value, name);
        let field_value = self.b.stage_acc(temps);
        self.b.test_reference_equal(hole);
        self.b.jump_if_truthy(fail);
        Ok(field_value)
    }

    fn load_named_or_hole(&mut self, value: Reg, name: &[u8]) {
        let name = self.b.name(name);
        let feedback = self.b.new_feedback();
        self.b.load_named_property_or_hole(value, name, feedback);
    }

    fn test_object(&mut self, value: Reg, fail: Label) {
        let feedback = self.b.new_feedback();
        self.b
            .test_type(value, value, TestTypeKind::Object as u32, feedback);
        self.b.jump_if_falsy(fail);
    }

    fn bind_token(&mut self, token: SyntaxToken<'a>, value: Reg) -> Result<(), CompileError> {
        let binding = self.binding(token)?;
        self.b.load(value);
        self.store_binding(binding)
    }

    fn if_expr(&mut self, expr: IfExpr<'a>, tail: bool) -> Result<(), CompileError> {
        let Some(condition) = expr.condition() else {
            return self.err_node(expr.syntax().id(), "missing condition");
        };
        let Some(then_branch) = expr.then_branch() else {
            return self.err_node(expr.syntax().id(), "missing then branch");
        };
        let temps = self.b.temp_scope();
        let result = temps.temp();
        self.b.load_null();
        self.b.store(result);
        self.expr(condition)?;
        let else_label = self.b.new_label();
        let end = self.b.new_label();
        self.b.jump_if_falsy(else_label);
        self.inline_body_with(then_branch, result, tail)?;
        self.b.jump(end);
        self.b.bind(else_label);
        if let Some(else_branch) = expr.else_branch() {
            match else_branch {
                Expr::Object(object) => self.inline_body_with(object, result, tail)?,
                other => {
                    let mut tailed = false;
                    if tail {
                        tailed = self.tail_expr(other)?;
                    }
                    if !tailed {
                        self.expr(other)?;
                    }
                    self.b.store(result);
                }
            }
        }
        self.b.bind(end);
        self.b.load(result);
        Ok(())
    }

    fn while_expr(&mut self, expr: WhileExpr<'a>) -> Result<(), CompileError> {
        let Some(condition) = expr.condition() else {
            return self.err_node(expr.syntax().id(), "missing condition");
        };
        let Some(body) = expr.body() else {
            return self.err_node(expr.syntax().id(), "missing loop body");
        };
        let temps = self.b.temp_scope();
        let sink = temps.temp();
        let result = temps.temp();
        let start = self.b.new_label();
        let end = self.b.new_label();
        self.b.load_null();
        self.b.store(result);
        self.loops.push(LoopCtx {
            break_label: end,
            result,
        });
        self.b.bind(start);
        self.expr(condition)?;
        self.b.jump_if_falsy(end);
        self.inline_body(body, sink)?;
        if self.b.is_live() {
            self.b.jump(start);
        }
        self.loops.pop();
        self.b.bind(end);
        self.b.load(result);
        Ok(())
    }

    fn for_expr(&mut self, expr: ForExpr<'a>) -> Result<(), CompileError> {
        let Some(iterable) = expr.iterable() else {
            return self.err_node(expr.syntax().id(), "missing iterable");
        };
        let Some(body) = expr.body() else {
            return self.err_node(expr.syntax().id(), "missing loop body");
        };
        let temps = self.b.temp_scope();
        let sink = temps.temp();
        let result = temps.temp();
        let start = self.b.new_label();
        let end = self.b.new_label();
        let keyed = matches!(
            expr.pattern(),
            Some(Pattern::Tuple(tuple)) if tuple.elements().count() >= 2
        );
        self.expr(iterable)?;
        let source = self.b.stage_acc(&temps);
        let iterator = self.send_reg0(source, "iterator", &temps)?;
        self.b.load_null();
        self.b.store(result);
        self.loops.push(LoopCtx {
            break_label: end,
            result,
        });
        self.b.bind(start);
        let has_next = self.send_reg0(iterator, "hasNext", &temps)?;
        self.b.load(has_next);
        self.b.jump_if_falsy(end);
        let next = if keyed { "nextKeyed" } else { "next" };
        let value = self.send_reg0(iterator, next, &temps)?;
        if body.is_callable()
            && let Some(pattern) = expr.pattern()
        {
            self.bind_pattern(pattern, value)?;
        }
        self.inline_body(body, sink)?;
        if self.b.is_live() {
            self.b.jump_loop(start);
        }
        self.loops.pop();
        self.b.bind(end);
        self.b.load(result);
        Ok(())
    }

    fn try_expr(&mut self, expr: TryExpr<'a>) -> Result<(), CompileError> {
        let Some(body) = expr.body() else {
            return self.err_node(expr.syntax().id(), "missing try body");
        };
        let Some(handler) = expr.catch() else {
            return self.err_node(expr.syntax().id(), "missing catch body");
        };
        let temps = self.b.temp_scope();
        let result = temps.temp();
        self.b.load_undefined();
        self.b.store(result);
        let block = self.b.begin_try();
        self.b.load_undefined();
        self.b.store(result);
        self.try_depth += 1;
        self.inline_body(body, result)?;
        self.try_depth -= 1;
        self.b.end_try(block);
        let end = self.b.new_label();
        self.b.jump(end);
        self.b.handler_entry(block);
        if let Some(name) = expr.catch_name()
            && let Ok(binding) = self.binding(name)
        {
            self.store_binding(binding)?;
        }
        self.try_depth += 1;
        self.inline_body(handler, result)?;
        self.try_depth -= 1;
        self.b.bind(end);
        self.b.load(result);
        Ok(())
    }

    fn throw_expr(&mut self, expr: ThrowExpr<'a>) -> Result<(), CompileError> {
        let Some(value) = expr.value() else {
            return self.err_node(expr.syntax().id(), "missing throw value");
        };
        self.expr(value)?;
        self.b.throw();
        Ok(())
    }

    fn error(&self, node: NodeId, message: impl Into<String>) -> CompileError {
        CompileError {
            range: self.tree.node(node).range,
            message: message.into(),
        }
    }
}

fn decode_string(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let body = if bytes.first().is_some_and(|b| *b == b'"' || *b == b'\'') {
        &bytes[1..bytes.len().saturating_sub(1)]
    } else {
        bytes
    };
    let mut out = Vec::with_capacity(body.len());
    let mut index = 0;
    while index < body.len() {
        if body[index] != b'\\' {
            out.push(body[index]);
            index += 1;
            continue;
        }
        index += 1;
        if index >= body.len() {
            break;
        }
        match body[index] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b'v' => out.push(0x0B),
            b'0' => out.push(0),
            b'x' => {
                index += 1;
                if index + 2 <= body.len()
                    && let Ok(hex) = std::str::from_utf8(&body[index..index + 2])
                    && let Ok(value) = u8::from_str_radix(hex, 16)
                {
                    out.push(value);
                }
                index += 1;
            }
            b'u' => {
                index += 1;
                if index + 4 <= body.len()
                    && let Ok(hex) = std::str::from_utf8(&body[index..index + 4])
                    && let Ok(value) = u32::from_str_radix(hex, 16)
                    && let Some(ch) = char::from_u32(value)
                {
                    let mut buffer = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                }
                index += 3;
            }
            b'\n' | b'\r' => {}
            other => out.push(other),
        }
        index += 1;
    }
    out
}
