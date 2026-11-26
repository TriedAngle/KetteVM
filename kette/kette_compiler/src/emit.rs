use std::collections::{HashMap, HashSet};

use bytecode::{
    CallableKind, ConstIdx, Constant, FnBuilder, Function, FunctionId, FunctionMeta,
    GlobalDeclarations, Label, Program, Reg, RegList, RtArg, ScopeLayout, ScopeLocal, SourceMode,
    TempScope, TestTypeKind,
};
use kette_syntax::ast::{
    AssignExpr, AstNode, AtomLiteral, BinaryExpr, BitString, CallExpr, ExportDecl, Expr, ForExpr,
    GetExpr, IfExpr, IndexExpr, LetStmt, MatchArm, MatchExpr, Object, ObjectEntry, Pattern,
    PatternValue, SendExpr, SlotEntry, SlotKind, SlotMulti, Stmt, ThrowExpr, TryExpr, UnaryExpr,
    UseDecl, UseTree, WhileExpr,
};
use kette_syntax::ops::{AssignOp, BinaryOp};
use kette_syntax::resolve::{Binding, BindingId, BindingKind, Resolution, Resolved};
use kette_syntax::{NodeId, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, Tree};
use vm_core::{
    INFO_GLOBAL, MODE_LET, MODE_VAR, NativeIndex, SCOPE_DECLARATION, SCOPE_REPL, SCOPE_SCRIPT, Smi,
    bit_program,
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
    continue_label: Label,
    result: Reg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TailMode {
    None,
    Guard,
    Loop,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MatchAccess {
    Root(u32),
    Slot(ConstIdx),
    Index(usize),
    ArraySource,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MatchOccurrence {
    base: Option<u32>,
    access: MatchAccess,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MatchLiteral {
    Smi(i64),
    Float(u64),
    String(ConstIdx),
    Atom(ConstIdx),
    True,
    False,
    Null,
    Undefined,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MatchTestKind {
    IsObject,
    ArraySource,
    LengthEq(usize),
    LengthAtLeast(usize),
    OwnSlotCountEq(usize),
    Present,
    Literal(MatchLiteral),
    UserType(NodeId),
    Value(u32),
    BitMatch(ConstIdx, u32),
}

struct MatchValue {
    base: NodeId,
    names: Vec<ConstIdx>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MatchTest {
    occurrence: u32,
    kind: MatchTestKind,
}

#[derive(Clone)]
struct MatchPlan<'a> {
    demands: Vec<u32>,
    bindings: Vec<(BindingId, u32)>,
    bit_bindings: Vec<(BindingId, u32, u32)>,
    rests: Vec<(BindingId, u32, usize)>,
    guard: Option<Expr<'a>>,
    arm: Option<MatchArm<'a>>,
    group: usize,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct MatchState {
    facts: Vec<(u32, bool)>,
    remaining: Vec<usize>,
}

#[derive(Clone, Copy)]
enum MatchNode<'a> {
    Fail,
    Test {
        test: u32,
        yes: u32,
        no: u32,
    },
    Switch {
        occurrence: u32,
        switch: u32,
        default: u32,
    },
    AtomSwitch {
        occurrence: u32,
        switch: u32,
        default: u32,
    },
    Guard {
        plan: usize,
        expr: Expr<'a>,
        pass: u32,
        fail: u32,
    },
    Leaf {
        plan: usize,
        bound: bool,
    },
}

struct MatchBuild<'a, 'f> {
    function: &'f mut FunctionGen<'a>,
    temps: TempScope,
    hole: Reg,
    plans: Vec<MatchPlan<'a>>,
    fail_message: ConstIdx,
    result: Option<Reg>,
    tail: bool,
    end: Label,
    bind_done: Option<Label>,
    occurrences: Vec<MatchOccurrence>,
    occurrence_ids: HashMap<MatchOccurrence, u32>,
    occurrence_regs: Vec<Reg>,
    subjects: usize,
    tests: Vec<MatchTest>,
    test_ids: HashMap<MatchTest, u32>,
    capture_regs: Vec<Reg>,
    bit_args: Vec<Vec<Reg>>,
    values: Vec<MatchValue>,
    value_ids: HashMap<Vec<ConstIdx>, u32>,
    nodes: Vec<MatchNode<'a>>,
    switches: Vec<Vec<(i64, u32)>>,
    atom_switches: Vec<Vec<(Box<[u8]>, u32)>>,
    next_group: usize,
    memo: HashMap<MatchState, u32>,
    labels: Vec<Option<Label>>,
    emitted: Vec<bool>,
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
            Expr::AtomLiteral(atom) => {
                self.atom_literal(atom);
                Ok(())
            }
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
            Expr::Continue(statement) => {
                let Some(context) = self.loops.last().copied() else {
                    return self.err_node(statement.syntax().id(), "continue outside a loop");
                };
                self.b.jump(context.continue_label);
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
            Expr::BitString(bits) => self.bit_string_expr(bits),
        }
    }

    fn bit_string_expr(&mut self, bits: BitString<'a>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        enum BitArg<'a> {
            Expr(Expr<'a>),
            Bytes(ConstIdx),
        }
        let mut segments: Vec<bit_program::Segment> = Vec::new();
        let mut args: Vec<BitArg<'a>> = Vec::new();
        let mut next_arg = 1u32;
        for segment in bits.segments() {
            let Some(value) = segment.value() else {
                return self.err_node(segment.syntax().id(), "missing bit segment value");
            };
            let type_tokens = segment.type_tokens();
            let (mut kind, signed, mut little, mut unit) =
                self.bit_type(segment.syntax().id(), type_tokens.clone())?;
            let string_literal = match value {
                Expr::Atom(atom) => atom
                    .token()
                    .filter(|token| token.kind() == SyntaxKind::String)
                    .map(|token| decode_string(token.text())),
                _ => None,
            };
            let mut string_literal = string_literal;
            if let Some(bytes) = string_literal.as_deref()
                && matches!(
                    kind,
                    bit_program::KIND_UTF8 | bit_program::KIND_UTF16 | bit_program::KIND_UTF32
                )
            {
                let Some(encoded) = encode_utf_string(bytes, kind, little) else {
                    return self.err_node(segment.syntax().id(), "string is not valid UTF-8");
                };
                string_literal = Some(encoded);
                kind = bit_program::KIND_BINARY;
                unit = 8;
                little = false;
            }
            if string_literal.is_some() && type_tokens.is_empty() {
                kind = bit_program::KIND_BINARY;
                unit = 8;
                little = false;
            }
            let mut descriptor = bit_program::Segment {
                kind,
                flags: 0,
                size_form: bit_program::SIZE_DEFAULT,
                size: 0,
                unit,
                literal: 0,
            };
            if signed {
                descriptor.flags |= bit_program::FLAG_SIGNED;
            }
            if little {
                descriptor.flags |= bit_program::FLAG_LITTLE;
            }
            match string_literal {
                Some(bytes) => {
                    if kind != bit_program::KIND_BINARY && kind != bit_program::KIND_BITSTRING {
                        return self
                            .err_node(segment.syntax().id(), "a string segment is binary data");
                    }
                    let bytes = self.b.constant(Constant::Bytes(bytes.into_boxed_slice()));
                    args.push(BitArg::Bytes(bytes));
                }
                None => args.push(BitArg::Expr(value)),
            }
            next_arg += 1;
            if let Some(size) = segment.size() {
                descriptor.size_form = bit_program::SIZE_ARG;
                descriptor.size = next_arg;
                args.push(BitArg::Expr(size));
                next_arg += 1;
            }
            segments.push(descriptor);
        }
        let program = self.b.constant(Constant::Bytes(
            bit_program::encode(&segments).into_boxed_slice(),
        ));
        self.b.load_constant(program);
        let program = self.b.stage_acc(&temps);
        self.load_global("BitArray");
        let receiver = self.b.stage_acc(&temps);
        let name = self.b.name(b"build");
        let feedback = self.b.new_feedback();
        self.b.load_named_property(receiver, name, feedback);
        let callee = self.b.stage_acc(&temps);
        let mut regs = Vec::with_capacity(args.len() + 1);
        regs.push(program);
        for arg in args {
            match arg {
                BitArg::Expr(expr) => self.expr(expr)?,
                BitArg::Bytes(index) => self.b.load_constant(index),
            }
            regs.push(self.b.stage_acc(&temps));
        }
        self.call_regs(&temps, callee, receiver, &regs)
    }

    fn bit_type(
        &mut self,
        node: NodeId,
        tokens: Vec<SyntaxToken<'a>>,
    ) -> Result<(u8, bool, bool, u32), CompileError> {
        let mut kind = bit_program::KIND_INTEGER;
        let mut signed = false;
        let mut little = false;
        let mut unit = 1u32;
        let mut unit_explicit = false;
        let mut index = 0;
        while index < tokens.len() {
            let token = tokens[index];
            match token.kind() {
                SyntaxKind::Minus => {
                    index += 1;
                    continue;
                }
                SyntaxKind::Ident => {}
                _ => {
                    return Err(self.error(node, "invalid bit type specifier"));
                }
            }
            match token.text() {
                "integer" => kind = bit_program::KIND_INTEGER,
                "float" => kind = bit_program::KIND_FLOAT,
                "binary" | "bytes" => {
                    kind = bit_program::KIND_BINARY;
                    if !unit_explicit {
                        unit = 8;
                    }
                }
                "bitstring" | "bits" => {
                    kind = bit_program::KIND_BITSTRING;
                    if !unit_explicit {
                        unit = 1;
                    }
                }
                "utf8" => kind = bit_program::KIND_UTF8,
                "utf16" => kind = bit_program::KIND_UTF16,
                "utf32" => kind = bit_program::KIND_UTF32,
                "signed" => signed = true,
                "unsigned" => signed = false,
                "big" => little = false,
                "little" => little = true,
                "native" => little = cfg!(target_endian = "little"),
                "unit" => {
                    index += 1;
                    if tokens.get(index).map(|token| token.kind()) != Some(SyntaxKind::Colon) {
                        return Err(self.error(node, "expected `unit:<n>`"));
                    }
                    index += 1;
                    let Some(value) = tokens.get(index) else {
                        return Err(self.error(node, "expected a unit value"));
                    };
                    let parsed = value.text().parse::<u32>().unwrap_or(0);
                    if parsed == 0 || parsed > 256 {
                        return Err(self.error(node, "unit must be 1..256"));
                    }
                    unit = parsed;
                    unit_explicit = true;
                }
                other => {
                    return Err(self.error(node, format!("unknown bit type `{other}`")));
                }
            }
            index += 1;
        }
        if kind == bit_program::KIND_BITSTRING && unit != 1 {
            return Err(self.error(node, "bitstring segments only support unit 1"));
        }
        if matches!(
            kind,
            bit_program::KIND_FLOAT
                | bit_program::KIND_UTF8
                | bit_program::KIND_UTF16
                | bit_program::KIND_UTF32
        ) && unit != 1
        {
            return Err(self.error(node, "this bit type only supports unit 1"));
        }
        Ok((kind, signed, little, unit))
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

    fn atom_literal(&mut self, atom: AtomLiteral<'a>) {
        let Some(token) = atom.name() else {
            return;
        };
        let name = match token.kind() {
            SyntaxKind::String => decode_string(token.text()),
            _ => token.text().as_bytes().to_vec(),
        };
        let index = self.b.constant(Constant::Atom(name.into_boxed_slice()));
        self.b.load_constant(index);
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
        if op == BinaryOp::Pipe {
            return self.pipe_expr(lhs, rhs);
        }
        if matches!(op, BinaryOp::RangeExclusive | BinaryOp::RangeInclusive) {
            return self.range_expr(lhs, rhs, op == BinaryOp::RangeInclusive);
        }
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
        self.binary_regs(op, lhs, rhs, &temps);
        Ok(())
    }

    fn binary_regs(&mut self, op: BinaryOp, lhs: Reg, rhs: Reg, temps: &TempScope) {
        debug_assert!(op.is_dispatch(), "short-circuit operators are control flow");
        let callee = temps.reserve(1);
        let name = self.b.name(op.text().as_bytes());
        let feedback = self.b.new_feedback();
        self.b.binary_op(callee, lhs, rhs, name, feedback);
    }

    fn range_expr(
        &mut self,
        lhs: Expr<'a>,
        rhs: Expr<'a>,
        inclusive: bool,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        self.expr(lhs)?;
        let start = self.b.stage_acc(&temps);
        self.expr(rhs)?;
        let end = self.b.stage_acc(&temps);
        self.load_global("Range");
        let receiver = self.b.stage_acc(&temps);
        let name = self.b.name(b"new");
        let feedback = self.b.new_feedback();
        self.b.load_named_property(receiver, name, feedback);
        let callee = self.b.stage_acc(&temps);
        if inclusive {
            self.b.load_true();
        } else {
            self.b.load_false();
        }
        let flag = self.b.stage_acc(&temps);
        self.call_regs(&temps, callee, receiver, &[start, end, flag])
    }

    fn pipe_expr(&mut self, lhs: Expr<'a>, rhs: Expr<'a>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        self.expr(lhs)?;
        let prefix = self.b.stage_acc(&temps);
        match rhs {
            Expr::Match(match_expr) => {
                if match_expr.scrutinee().is_some() {
                    return self.err_node(
                        match_expr.syntax().id(),
                        "pipe into match requires a subjectless match",
                    );
                }
                self.match_expr_with_subject(match_expr, Some(prefix), false)
            }
            Expr::Call(call) => {
                let Some(callee_expr) = call.receiver() else {
                    return self.err_node(call.syntax().id(), "missing callee");
                };
                let Some(args) = call.args() else {
                    return self.err_node(call.syntax().id(), "missing arguments");
                };
                let args: Vec<_> = args.args().collect();
                self.expr(callee_expr)?;
                let callee = self.b.stage_acc(&temps);
                self.call_args_with_prefix(&temps, callee, None, prefix, &args)
            }
            Expr::Send(send) => {
                let Some(receiver_expr) = send.receiver() else {
                    return self.err_node(send.syntax().id(), "missing receiver");
                };
                let Some(name) = send.name() else {
                    return self.err_node(send.syntax().id(), "missing method name");
                };
                let Some(args) = send.args() else {
                    return self.err_node(send.syntax().id(), "missing arguments");
                };
                let args: Vec<_> = args.args().collect();
                self.expr(receiver_expr)?;
                let receiver = self.b.stage_acc(&temps);
                let name = self.b.name(name.text().as_bytes());
                let feedback = self.b.new_feedback();
                self.b.load_named_property(receiver, name, feedback);
                let callee = self.b.stage_acc(&temps);
                self.call_args_with_prefix(&temps, callee, Some(receiver), prefix, &args)
            }
            rhs => {
                self.expr(rhs)?;
                let callee = self.b.stage_acc(&temps);
                self.call_args_with_prefix(&temps, callee, None, prefix, &[])
            }
        }
    }

    fn call_args_with_prefix(
        &mut self,
        temps: &TempScope,
        callee: Reg,
        receiver: Option<Reg>,
        prefix: Reg,
        args: &[Expr<'a>],
    ) -> Result<(), CompileError> {
        let receiver = receiver.unwrap_or(callee);
        let feedback = self.b.new_feedback();
        match args.len() {
            0 => self.b.call_method1(callee, receiver, prefix, feedback),
            1 => {
                self.expr(args[0])?;
                let arg = self.b.stage_acc(temps);
                self.b.call_method2(callee, receiver, prefix, arg, feedback)
            }
            count => {
                let count = count + 1;
                let base = temps.reserve(count as u32 + 2);
                let top = base.index() + count as i32;
                let window_callee = Reg::new(top + 1);
                self.b.move_reg(window_callee, callee);
                self.b.move_reg(Reg::new(top), receiver);
                self.b.load(prefix);
                self.b.store(Reg::new(top - 1));
                for (index, arg) in args.iter().enumerate() {
                    self.expr(*arg)?;
                    self.b.store(Reg::new(top - 2 - index as i32));
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

    fn unary_expr(&mut self, expr: UnaryExpr<'a>) -> Result<(), CompileError> {
        let Some(operand) = expr.operand() else {
            return self.err_node(expr.syntax().id(), "missing operand");
        };
        let Some(op) = expr.op() else {
            return self.err_node(expr.syntax().id(), "unsupported unary operator");
        };
        let temps = self.b.temp_scope();
        self.expr(operand)?;
        let receiver = self.b.stage_acc(&temps);
        let callee = temps.reserve(1);
        let name = self.b.name(op.text().as_bytes());
        let feedback = self.b.new_feedback();
        self.b.unary_op(callee, receiver, name, feedback);
        Ok(())
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
            AssignOp::Shl => BinaryOp::ShiftLeft,
            AssignOp::Shr => BinaryOp::ShiftRight,
            AssignOp::BitAnd => BinaryOp::BitAnd,
            AssignOp::BitOr => BinaryOp::BitOr,
            AssignOp::BitXor => BinaryOp::BitXor,
            _ => return self.err_node(expr.syntax().id(), "compound assignment"),
        };
        let temps = self.b.temp_scope();
        match target {
            Expr::Atom(_) => {
                self.expr(target)?;
                let old = self.b.stage_acc(&temps);
                self.expr(value)?;
                let rhs = self.b.stage_acc(&temps);
                self.binary_regs(binary, old, rhs, &temps);
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
                self.binary_regs(binary, old, rhs, &temps);
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
                self.binary_regs(binary, old, rhs, &temps);
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
        let prefix = declaration.path().and_then(|path| path.last_segment());
        if let Some(tree) = declaration.tree() {
            return self.use_tree(&path, prefix, tree);
        }
        let alias = declaration
            .alias()
            .or_else(|| declaration.path().and_then(|p| p.last_segment()));
        let Some(alias) = alias else {
            return self.err_node(declaration.syntax().id(), "missing import name");
        };
        self.import(&path, alias)
    }

    fn use_tree(
        &mut self,
        prefix: &[String],
        prefix_token: Option<SyntaxToken<'a>>,
        tree: UseTree<'a>,
    ) -> Result<(), CompileError> {
        if tree.has_self() {
            let Some(alias) = prefix_token else {
                return self.err_node(tree.syntax().id(), "self import needs a module path");
            };
            self.import(prefix, alias)?;
        }
        for item in tree.items() {
            let mut full = prefix.to_vec();
            full.extend(item.path.segments().map(|token| token.text().to_string()));
            let last = item.path.last_segment();
            if let Some(nested) = item.tree
                && item.alias.is_none()
            {
                self.use_tree(&full, last, nested)?;
                continue;
            }
            let alias = item.alias.or(last);
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
        let done = self.b.new_label();
        let message = Constant::String(b"pattern did not match value".to_vec().into_boxed_slice());
        let message = self.b.constant(message);
        {
            let mut build =
                MatchBuild::new(self, vec![value], message, None, false, done, Some(done));
            build.add_bind(pattern)?;
            let root = build.build()?;
            build.emit(root)?;
        }
        self.b.bind(done);
        Ok(())
    }

    fn match_expr(&mut self, expr: MatchExpr<'a>, tail: bool) -> Result<(), CompileError> {
        self.match_expr_with_subject(expr, None, tail)
    }

    fn match_expr_with_subject(
        &mut self,
        expr: MatchExpr<'a>,
        subject: Option<Reg>,
        tail: bool,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let arms: Vec<MatchArm<'a>> = expr.arms().collect();
        let mut subjects: Vec<Reg> = Vec::new();
        if let Some(subject) = subject {
            subjects.push(subject);
        } else if let Some(scrutinee) = expr.scrutinee() {
            let mut multi = false;
            if let Expr::Array(array) = scrutinee {
                let elements: Vec<Expr<'a>> = array.elements().collect();
                if elements.len() > 1
                    && arms.iter().all(|arm| {
                        arm.pattern()
                            .is_some_and(|pattern| supports_multi_pattern(pattern, elements.len()))
                    })
                {
                    for element in elements {
                        let reg = temps.temp();
                        self.expr(element)?;
                        self.b.store(reg);
                        subjects.push(reg);
                    }
                    multi = true;
                }
            }
            if !multi {
                let subject = temps.temp();
                self.expr(scrutinee)?;
                self.b.store(subject);
                subjects.push(subject);
            }
        } else {
            if self.frame.params.len() != 1 {
                return self.err_node(
                    expr.syntax().id(),
                    "a match closure must take exactly one parameter",
                );
            }
            let binding = self.frame.params[0];
            let subject = temps.temp();
            self.load_binding(binding)?;
            self.b.store(subject);
            subjects.push(subject);
        }
        let result = temps.temp();
        self.b.load_null();
        self.b.store(result);
        let end = self.b.new_label();
        let message = Constant::String(b"match: no pattern matched".to_vec().into_boxed_slice());
        let message = self.b.constant(message);
        {
            let mut build = MatchBuild::new(self, subjects, message, Some(result), tail, end, None);
            for arm in arms {
                build.add_arm(arm)?;
            }
            let root = build.build()?;
            build.emit(root)?;
        }
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
            continue_label: start,
            result,
        });
        self.b.bind(start);
        self.expr(condition)?;
        self.b.jump_if_falsy(end);
        self.inline_body(body, sink)?;
        if self.b.is_live() {
            self.b.jump_loop(start);
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
        self.expr(iterable)?;
        let source = self.b.stage_acc(&temps);
        let iterator = self.send_reg0(source, "iterator", &temps)?;
        self.b.load_null();
        self.b.store(result);
        self.loops.push(LoopCtx {
            break_label: end,
            continue_label: start,
            result,
        });
        self.b.bind(start);
        let has_next = self.send_reg0(iterator, "hasNext", &temps)?;
        self.b.load(has_next);
        self.b.jump_if_falsy(end);
        let value = self.send_reg0(iterator, "next", &temps)?;
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

impl<'a, 'f> MatchBuild<'a, 'f> {
    fn new(
        function: &'f mut FunctionGen<'a>,
        subjects: Vec<Reg>,
        fail_message: ConstIdx,
        result: Option<Reg>,
        tail: bool,
        end: Label,
        bind_done: Option<Label>,
    ) -> Self {
        let temps = function.b.temp_scope();
        let hole = temps.temp();
        let mut occurrences = Vec::with_capacity(subjects.len());
        let mut occurrence_ids = HashMap::new();
        for (index, _) in subjects.iter().enumerate() {
            let root = MatchOccurrence {
                base: None,
                access: MatchAccess::Root(index as u32),
            };
            occurrence_ids.insert(root, index as u32);
            occurrences.push(root);
        }
        let subject_count = subjects.len();
        Self {
            function,
            temps,
            hole,
            plans: Vec::new(),
            fail_message,
            result,
            tail,
            end,
            bind_done,
            occurrences,
            occurrence_ids,
            occurrence_regs: subjects,
            subjects: subject_count,
            tests: Vec::new(),
            test_ids: HashMap::new(),
            capture_regs: Vec::new(),
            bit_args: Vec::new(),
            values: Vec::new(),
            value_ids: HashMap::new(),
            nodes: Vec::new(),
            switches: Vec::new(),
            atom_switches: Vec::new(),
            next_group: 0,
            memo: HashMap::new(),
            labels: Vec::new(),
            emitted: Vec::new(),
        }
    }

    fn add_arm(&mut self, arm: MatchArm<'a>) -> Result<(), CompileError> {
        let Some(pattern) = arm.pattern() else {
            return self
                .function
                .err_node(arm.syntax().id(), "match arm without a pattern");
        };
        let plan = MatchPlan {
            demands: Vec::new(),
            bindings: Vec::new(),
            bit_bindings: Vec::new(),
            rests: Vec::new(),
            guard: arm.guard(),
            arm: Some(arm),
            group: 0,
        };
        self.add_plans(pattern, plan)?;
        Ok(())
    }

    fn add_bind(&mut self, pattern: Pattern<'a>) -> Result<(), CompileError> {
        let plan = MatchPlan {
            demands: Vec::new(),
            bindings: Vec::new(),
            bit_bindings: Vec::new(),
            rests: Vec::new(),
            guard: None,
            arm: None,
            group: 0,
        };
        self.add_plans(pattern, plan)?;
        Ok(())
    }

    fn add_plans(&mut self, pattern: Pattern<'a>, plan: MatchPlan<'a>) -> Result<(), CompileError> {
        let group = self.next_group;
        self.next_group += 1;
        let mut plans = if self.subjects > 1 {
            self.lower_multi_pattern(pattern, vec![plan])?
        } else {
            self.lower_pattern(pattern, 0, vec![plan])?
        };
        let mut expected: Option<Vec<BindingId>> = None;
        for plan in &mut plans {
            plan.group = group;
            let mut bindings: Vec<BindingId> = plan
                .bindings
                .iter()
                .map(|(binding, _)| *binding)
                .chain(plan.bit_bindings.iter().map(|(binding, _, _)| *binding))
                .collect();
            bindings.sort();
            if let Some(expected) = &expected {
                if *expected != bindings {
                    return self.function.err_node(
                        pattern.syntax().id(),
                        "or-pattern alternatives must bind the same names",
                    );
                }
            } else {
                expected = Some(bindings);
            }
        }
        self.plans.extend(plans);
        Ok(())
    }

    fn lower_multi_pattern(
        &mut self,
        pattern: Pattern<'a>,
        mut plans: Vec<MatchPlan<'a>>,
    ) -> Result<Vec<MatchPlan<'a>>, CompileError> {
        match pattern {
            Pattern::Wildcard(_) => Ok(plans),
            Pattern::Array(array) => {
                let elements: Vec<Pattern<'a>> = array.elements().collect();
                if array.rest().is_some() || elements.len() != self.subjects {
                    return self.function.err_node(
                        array.syntax().id(),
                        "multi-match array pattern has the wrong shape",
                    );
                }
                for (index, element) in elements.into_iter().enumerate() {
                    plans = self.lower_pattern(element, index as u32, plans)?;
                }
                Ok(plans)
            }
            Pattern::Or(or) => {
                let mut result = Vec::new();
                for alternative in or.alternatives() {
                    let branch = plans.clone();
                    let mut lowered = self.lower_multi_pattern(alternative, branch)?;
                    result.append(&mut lowered);
                }
                Ok(result)
            }
            _ => self.function.err_node(
                pattern.syntax().id(),
                "multi-match arms must be array patterns",
            ),
        }
    }

    fn lower_pattern(
        &mut self,
        pattern: Pattern<'a>,
        occurrence: u32,
        mut plans: Vec<MatchPlan<'a>>,
    ) -> Result<Vec<MatchPlan<'a>>, CompileError> {
        match pattern {
            Pattern::Name(name) => {
                let Some(token) = name.name() else {
                    return self
                        .function
                        .err_node(name.syntax().id(), "missing binding name");
                };
                if token.text() != "_" {
                    let binding = self.function.binding(token)?;
                    for plan in &mut plans {
                        plan.bindings.push((binding, occurrence));
                    }
                }
                Ok(plans)
            }
            Pattern::Wildcard(_) => Ok(plans),
            Pattern::Literal(literal) => {
                let literal = self.literal_key(literal)?;
                let test = self.intern_test(occurrence, MatchTestKind::Literal(literal));
                for plan in &mut plans {
                    Self::push_demand(plan, test);
                }
                Ok(plans)
            }
            Pattern::Alias(alias) => {
                let mut plans = if let Some(inner) = alias.pattern() {
                    self.lower_pattern(inner, occurrence, plans)?
                } else {
                    plans
                };
                if let Some(token) = alias.alias() {
                    let binding = self.function.binding(token)?;
                    for plan in &mut plans {
                        plan.bindings.push((binding, occurrence));
                    }
                }
                Ok(plans)
            }
            Pattern::Type(typed) => {
                let test =
                    self.intern_test(occurrence, MatchTestKind::UserType(typed.syntax().id()));
                for plan in &mut plans {
                    Self::push_demand(plan, test);
                }
                if let Some(token) = typed.binding()
                    && token.text() != "_"
                {
                    let binding = self.function.binding(token)?;
                    for plan in &mut plans {
                        plan.bindings.push((binding, occurrence));
                    }
                }
                Ok(plans)
            }
            Pattern::Object(object) => {
                let test = self.intern_test(occurrence, MatchTestKind::IsObject);
                for plan in &mut plans {
                    Self::push_demand(plan, test);
                }
                if !object.has_rest() {
                    let named = object
                        .fields()
                        .filter(|field| {
                            field
                                .key()
                                .is_some_and(|key| key.kind() != SyntaxKind::Number)
                        })
                        .count();
                    let test = self.intern_test(occurrence, MatchTestKind::OwnSlotCountEq(named));
                    for plan in &mut plans {
                        Self::push_demand(plan, test);
                    }
                }
                for field in object.fields() {
                    let Some(key) = field.key() else {
                        return self
                            .function
                            .err_node(field.syntax().id(), "missing pattern field key");
                    };
                    let child = match key.kind() {
                        SyntaxKind::Number => {
                            let index = key.text().parse::<usize>().unwrap_or(0);
                            self.intern_occurrence(occurrence, MatchAccess::Index(index))
                        }
                        SyntaxKind::String => {
                            let name = decode_string(key.text());
                            let name = self.function.b.name(&name);
                            self.intern_occurrence(occurrence, MatchAccess::Slot(name))
                        }
                        _ => {
                            let name = self.function.b.name(key.text().as_bytes());
                            self.intern_occurrence(occurrence, MatchAccess::Slot(name))
                        }
                    };
                    let present = self.intern_test(child, MatchTestKind::Present);
                    for plan in &mut plans {
                        Self::push_demand(plan, present);
                    }
                    if let Some(inner) = field.pattern() {
                        plans = self.lower_pattern(inner, child, plans)?;
                    } else {
                        let Some(token) = field.alias().or_else(|| field.key()) else {
                            return self
                                .function
                                .err_node(field.syntax().id(), "missing field binding");
                        };
                        if token.kind().is_name() && token.text() != "_" {
                            let binding = self.function.binding(token)?;
                            for plan in &mut plans {
                                plan.bindings.push((binding, child));
                            }
                        }
                    }
                }
                Ok(plans)
            }
            Pattern::Array(array) => {
                let source = self.intern_occurrence(occurrence, MatchAccess::ArraySource);
                let test = self.intern_test(source, MatchTestKind::ArraySource);
                for plan in &mut plans {
                    Self::push_demand(plan, test);
                }
                let elements: Vec<Pattern<'a>> = array.elements().collect();
                let rest = array.rest();
                if rest.is_some() {
                    if !elements.is_empty() {
                        let test =
                            self.intern_test(source, MatchTestKind::LengthAtLeast(elements.len()));
                        for plan in &mut plans {
                            Self::push_demand(plan, test);
                        }
                    }
                } else {
                    let test = self.intern_test(source, MatchTestKind::LengthEq(elements.len()));
                    for plan in &mut plans {
                        Self::push_demand(plan, test);
                    }
                }
                for (index, element) in elements.iter().enumerate() {
                    let child = self.intern_occurrence(source, MatchAccess::Index(index));
                    let present = self.intern_test(child, MatchTestKind::Present);
                    for plan in &mut plans {
                        Self::push_demand(plan, present);
                    }
                    plans = self.lower_pattern(*element, child, plans)?;
                }
                if let Some(rest) = rest {
                    if elements.len() > u16::MAX as usize {
                        return self
                            .function
                            .err_node(array.syntax().id(), "array rest start is too large");
                    }
                    let binding = self.function.binding(rest)?;
                    for plan in &mut plans {
                        plan.rests.push((binding, source, elements.len()));
                    }
                }
                Ok(plans)
            }
            Pattern::BitString(bits) => {
                let mut segments: Vec<bit_program::Segment> = Vec::new();
                let mut captures: Vec<(BindingId, u32)> = Vec::new();
                let mut earlier: Vec<(String, u32)> = Vec::new();
                let mut extra: Vec<Reg> = Vec::new();
                let mut declared: HashSet<String> = HashSet::new();
                for segment in bits.segments() {
                    if let Some(Pattern::Name(name)) = segment.pattern()
                        && let Some(token) = name.name()
                    {
                        declared.insert(token.text().to_string());
                    }
                }
                let mut capture_index = 0u32;
                for segment in bits.segments() {
                    let Some(pattern) = segment.pattern() else {
                        return self
                            .function
                            .err_node(segment.syntax().id(), "missing bit segment pattern");
                    };
                    let (kind, signed, little, unit) = self
                        .function
                        .bit_type(segment.syntax().id(), segment.type_tokens())?;
                    let mut descriptor = bit_program::Segment {
                        kind,
                        flags: 0,
                        size_form: bit_program::SIZE_DEFAULT,
                        size: 0,
                        unit,
                        literal: 0,
                    };
                    if signed {
                        descriptor.flags |= bit_program::FLAG_SIGNED;
                    }
                    if little {
                        descriptor.flags |= bit_program::FLAG_LITTLE;
                    }
                    match pattern {
                        Pattern::Name(name) => {
                            let Some(token) = name.name() else {
                                return self
                                    .function
                                    .err_node(name.syntax().id(), "missing bit segment name");
                            };
                            if token.text() != "_" {
                                let binding = self.function.binding(token)?;
                                descriptor.flags |= bit_program::FLAG_CAPTURE;
                                captures.push((binding, capture_index));
                                earlier.push((token.text().to_string(), capture_index));
                            }
                        }
                        Pattern::Wildcard(_) => {}
                        Pattern::Literal(literal) => {
                            let mut tokens = literal.syntax().tokens();
                            let mut negative = false;
                            let Some(token) = tokens.next() else {
                                return self
                                    .function
                                    .err_node(literal.syntax().id(), "empty segment literal");
                            };
                            let value = if token.kind() == SyntaxKind::Minus {
                                negative = true;
                                let Some(token) = tokens.next() else {
                                    return self
                                        .function
                                        .err_node(literal.syntax().id(), "empty segment literal");
                                };
                                token
                            } else {
                                token
                            };
                            if value.kind() != SyntaxKind::Number {
                                return self.function.err_node(
                                    literal.syntax().id(),
                                    "only numeric bit segment literals are supported",
                                );
                            }
                            if kind == bit_program::KIND_FLOAT {
                                let mut parsed = value.text().parse::<f64>().unwrap_or(f64::NAN);
                                if negative {
                                    parsed = -parsed;
                                }
                                descriptor.flags |= bit_program::FLAG_FLOAT_LITERAL;
                                descriptor.literal = parsed.to_bits();
                            } else {
                                let mut parsed = value.text().parse::<i64>().unwrap_or(0);
                                if negative {
                                    parsed = -parsed;
                                }
                                descriptor.flags |= bit_program::FLAG_LITERAL;
                                descriptor.literal = parsed as u64;
                            }
                        }
                        _ => {
                            return self.function.err_node(
                                segment.syntax().id(),
                                "unsupported bit segment pattern",
                            );
                        }
                    }
                    if let Some(size) = segment.size() {
                        match size {
                            Expr::Atom(atom) => {
                                let Some(token) = atom.token() else {
                                    return self
                                        .function
                                        .err_node(size.syntax().id(), "empty bit size");
                                };
                                match token.kind() {
                                    SyntaxKind::Number => {
                                        descriptor.size_form = bit_program::SIZE_LITERAL;
                                        descriptor.size = token.text().parse::<u32>().unwrap_or(0);
                                    }
                                    SyntaxKind::Ident => {
                                        if let Some((_, index)) =
                                            earlier.iter().find(|(name, _)| name == token.text())
                                        {
                                            descriptor.size_form = bit_program::SIZE_CAPTURE;
                                            descriptor.size = *index;
                                        } else if declared.contains(token.text()) {
                                            return self.function.err_node(
                                                size.syntax().id(),
                                                "bit size must reference an earlier segment",
                                            );
                                        } else {
                                            self.function.load_resolution(atom.syntax().id())?;
                                            let reg = self.function.b.stage_acc(&self.temps);
                                            descriptor.size_form = bit_program::SIZE_ARG;
                                            descriptor.size = 2 + extra.len() as u32;
                                            extra.push(reg);
                                        }
                                    }
                                    _ => {
                                        return self.function.err_node(
                                            size.syntax().id(),
                                            "unsupported bit size expression",
                                        );
                                    }
                                }
                            }
                            _ => {
                                return self.function.err_node(
                                    size.syntax().id(),
                                    "unsupported bit size expression",
                                );
                            }
                        }
                    }
                    if descriptor.flags & bit_program::FLAG_CAPTURE != 0 {
                        capture_index += 1;
                    }
                    segments.push(descriptor);
                }
                let program = self.function.b.constant(Constant::Bytes(
                    bit_program::encode(&segments).into_boxed_slice(),
                ));
                let args = self.bit_args.len() as u32;
                self.bit_args.push(extra);
                let test = self.intern_test(occurrence, MatchTestKind::BitMatch(program, args));
                for plan in &mut plans {
                    Self::push_demand(plan, test);
                    for (binding, index) in &captures {
                        plan.bit_bindings.push((*binding, test, *index));
                    }
                }
                Ok(plans)
            }
            Pattern::Atom(atom) => {
                let Some(token) = atom.name() else {
                    return self
                        .function
                        .err_node(atom.syntax().id(), "missing atom name");
                };
                let name = match token.kind() {
                    SyntaxKind::String => decode_string(token.text()),
                    _ => token.text().as_bytes().to_vec(),
                };
                let index = self
                    .function
                    .b
                    .constant(Constant::Atom(name.into_boxed_slice()));
                let test = self.intern_test(
                    occurrence,
                    MatchTestKind::Literal(MatchLiteral::Atom(index)),
                );
                for plan in &mut plans {
                    Self::push_demand(plan, test);
                }
                Ok(plans)
            }
            Pattern::Value(value) => {
                let value_id = self.intern_value(value.syntax().id())?;
                let test = self.intern_test(occurrence, MatchTestKind::Value(value_id));
                for plan in &mut plans {
                    Self::push_demand(plan, test);
                }
                Ok(plans)
            }
            Pattern::Or(or) => {
                let mut result = Vec::new();
                for alternative in or.alternatives() {
                    let branch = plans.clone();
                    let mut lowered = self.lower_pattern(alternative, occurrence, branch)?;
                    result.append(&mut lowered);
                }
                Ok(result)
            }
        }
    }

    fn literal_key(
        &mut self,
        literal: kette_syntax::ast::PatternLiteral<'a>,
    ) -> Result<MatchLiteral, CompileError> {
        let mut tokens = literal
            .syntax()
            .tokens()
            .filter(|token| !token.kind().is_trivia());
        let Some(first) = tokens.next() else {
            return self
                .function
                .err_node(literal.syntax().id(), "empty literal pattern");
        };
        let negative = first.kind() == SyntaxKind::Minus;
        let Some(token) = (if negative { tokens.next() } else { Some(first) }) else {
            return self
                .function
                .err_node(literal.syntax().id(), "empty literal pattern");
        };
        match token.kind() {
            SyntaxKind::Number => {
                let mut number = token.text().parse::<f64>().unwrap_or(0.0);
                if negative {
                    number = -number;
                }
                if let Some(smi) = Smi::from_f64(number) {
                    Ok(MatchLiteral::Smi(smi.value()))
                } else {
                    Ok(MatchLiteral::Float(number.to_bits()))
                }
            }
            SyntaxKind::String => {
                let text = decode_string(token.text());
                let index = self
                    .function
                    .b
                    .constant(Constant::String(text.into_boxed_slice()));
                Ok(MatchLiteral::String(index))
            }
            SyntaxKind::True => Ok(MatchLiteral::True),
            SyntaxKind::False => Ok(MatchLiteral::False),
            SyntaxKind::Null => Ok(MatchLiteral::Null),
            SyntaxKind::Ident if token.text() == "undefined" => Ok(MatchLiteral::Undefined),
            _ => self
                .function
                .err_node(literal.syntax().id(), "unsupported literal pattern"),
        }
    }

    fn intern_value(&mut self, node: NodeId) -> Result<u32, CompileError> {
        let syntax = SyntaxNode::new(self.function.tree, node);
        let Some(value) = PatternValue::cast(syntax) else {
            return self
                .function
                .err_node(node, "invalid runtime value pattern");
        };
        let names: Vec<ConstIdx> = value
            .segments()
            .map(|token| self.function.b.name(token.text().as_bytes()))
            .collect();
        if names.len() < 2 {
            return self
                .function
                .err_node(node, "runtime value pattern needs a path");
        }
        if let Some(id) = self.value_ids.get(&names) {
            return Ok(*id);
        }
        let id = self.values.len() as u32;
        self.values.push(MatchValue {
            base: node,
            names: names.clone(),
        });
        self.value_ids.insert(names, id);
        Ok(id)
    }

    fn intern_occurrence(&mut self, base: u32, access: MatchAccess) -> u32 {
        let occurrence = MatchOccurrence {
            base: Some(base),
            access,
        };
        if let Some(id) = self.occurrence_ids.get(&occurrence) {
            return *id;
        }
        let id = self.occurrences.len() as u32;
        self.occurrences.push(occurrence);
        self.occurrence_ids.insert(occurrence, id);
        self.occurrence_regs.push(self.temps.temp());
        id
    }

    fn intern_test(&mut self, occurrence: u32, kind: MatchTestKind) -> u32 {
        let test = MatchTest { occurrence, kind };
        if let Some(id) = self.test_ids.get(&test) {
            return *id;
        }
        let id = self.tests.len() as u32;
        self.capture_regs
            .push(if matches!(kind, MatchTestKind::BitMatch(..)) {
                self.temps.temp()
            } else {
                Reg::new(0)
            });
        self.tests.push(test);
        self.test_ids.insert(test, id);
        id
    }

    fn push_demand(plan: &mut MatchPlan<'a>, test: u32) {
        if !plan.demands.contains(&test) {
            plan.demands.push(test);
        }
    }

    fn build(&mut self) -> Result<u32, CompileError> {
        let state = MatchState {
            facts: Vec::new(),
            remaining: (0..self.plans.len()).collect(),
        };
        self.compile_state(&state)
    }

    fn compile_state(&mut self, state: &MatchState) -> Result<u32, CompileError> {
        if let Some(node) = self.memo.get(state) {
            return Ok(*node);
        }
        if state.remaining.is_empty() {
            let node = self.new_node(MatchNode::Fail);
            self.memo.insert(state.clone(), node);
            return Ok(node);
        }
        let plan = state.remaining[0];
        if self.plan_complete(state, plan) {
            let node = if let Some(expr) = self.plans[plan].guard {
                let pass = self.new_node(MatchNode::Leaf { plan, bound: true });
                let group = self.plans[plan].group;
                let mut fallback = state.clone();
                fallback
                    .remaining
                    .retain(|&remaining| self.plans[remaining].group != group);
                let fail = self.compile_state(&fallback)?;
                self.new_node(MatchNode::Guard {
                    plan,
                    expr,
                    pass,
                    fail,
                })
            } else {
                self.new_node(MatchNode::Leaf { plan, bound: false })
            };
            self.memo.insert(state.clone(), node);
            return Ok(node);
        }
        let mut selected = None;
        for &demand in &self.plans[plan].demands {
            if self.fact(state, demand).is_none() {
                selected = Some(demand);
                break;
            }
        }
        let Some(test) = selected else {
            let node = self.new_node(MatchNode::Fail);
            self.memo.insert(state.clone(), node);
            return Ok(node);
        };
        if let Some(node) = self.try_switch(state, test)? {
            self.memo.insert(state.clone(), node);
            return Ok(node);
        }
        let mut yes = state.clone();
        self.add_fact(&mut yes, test, true);
        let mut no = state.clone();
        self.add_fact(&mut no, test, false);
        let yes = self.compile_state(&yes)?;
        let no = self.compile_state(&no)?;
        let node = self.new_node(MatchNode::Test { test, yes, no });
        self.memo.insert(state.clone(), node);
        Ok(node)
    }

    fn try_switch(&mut self, state: &MatchState, test: u32) -> Result<Option<u32>, CompileError> {
        let selected = self.tests[test as usize];
        let MatchTestKind::Literal(selected_literal) = selected.kind else {
            return Ok(None);
        };
        if !matches!(
            selected_literal,
            MatchLiteral::Smi(_) | MatchLiteral::Atom(_)
        ) {
            return Ok(None);
        }
        let mut values: Vec<(MatchLiteral, u32)> = Vec::new();
        for &plan in &state.remaining {
            for &demand in &self.plans[plan].demands {
                if self.fact(state, demand).is_some() {
                    continue;
                }
                let candidate = self.tests[demand as usize];
                if candidate.occurrence != selected.occurrence {
                    continue;
                }
                let MatchTestKind::Literal(candidate_literal) = candidate.kind else {
                    continue;
                };
                if !same_switch_literal_kind(selected_literal, candidate_literal) {
                    continue;
                }
                if !values
                    .iter()
                    .any(|(existing, _)| *existing == candidate_literal)
                {
                    values.push((candidate_literal, demand));
                }
            }
        }
        if values.len() < 4 {
            return Ok(None);
        }
        values.sort_by_key(|(value, _)| switch_literal_key(*value));
        if let MatchLiteral::Smi(_) = selected_literal {
            let min = switch_literal_key(values[0].0);
            let max = switch_literal_key(values[values.len() - 1].0);
            let range = max.checked_sub(min).and_then(|range| range.checked_add(1));
            let Some(range) = range else {
                return Ok(None);
            };
            if range > (values.len() as i64) * 2 || range > 1024 {
                return Ok(None);
            }
        }
        let mut cases: Vec<(MatchLiteral, u32)> = Vec::with_capacity(values.len());
        for (value, _) in &values {
            let mut branch = state.clone();
            for (candidate_value, candidate) in &values {
                self.add_fact(&mut branch, *candidate, candidate_value == value);
            }
            let node = self.compile_state(&branch)?;
            cases.push((*value, node));
        }
        let mut default = state.clone();
        for (_, candidate) in &values {
            self.add_fact(&mut default, *candidate, false);
        }
        let default = self.compile_state(&default)?;
        match selected_literal {
            MatchLiteral::Smi(_) => {
                let cases: Vec<(i64, u32)> = cases
                    .into_iter()
                    .map(|(value, node)| (switch_literal_key(value), node))
                    .collect();
                let switch = self.switches.len() as u32;
                self.switches.push(cases);
                Ok(Some(self.new_node(MatchNode::Switch {
                    occurrence: selected.occurrence,
                    switch,
                    default,
                })))
            }
            MatchLiteral::Atom(_) => {
                let mut names: Vec<(Box<[u8]>, u32)> = Vec::with_capacity(cases.len());
                for (value, node) in cases {
                    let MatchLiteral::Atom(index) = value else {
                        unreachable!("atom switch case");
                    };
                    let Some(Constant::Atom(name)) = self.function.b.constant_ref(index) else {
                        return Err(self.function.error(
                            self.function.tree.root_id(),
                            "atom switch constant vanished",
                        ));
                    };
                    names.push((name.clone(), node));
                }
                let switch = self.atom_switches.len() as u32;
                self.atom_switches.push(names);
                Ok(Some(self.new_node(MatchNode::AtomSwitch {
                    occurrence: selected.occurrence,
                    switch,
                    default,
                })))
            }
            _ => unreachable!("switch literal kind"),
        }
    }

    fn plan_complete(&self, state: &MatchState, plan: usize) -> bool {
        self.plans[plan]
            .demands
            .iter()
            .all(|&test| self.fact(state, test) == Some(true))
    }

    fn fact(&self, state: &MatchState, test: u32) -> Option<bool> {
        state
            .facts
            .binary_search_by_key(&test, |(id, _)| *id)
            .ok()
            .map(|index| state.facts[index].1)
    }

    fn add_fact(&self, state: &mut MatchState, test: u32, value: bool) {
        match state.facts.binary_search_by_key(&test, |(id, _)| *id) {
            Ok(index) => state.facts[index].1 = value,
            Err(index) => state.facts.insert(index, (test, value)),
        }
        if !value {
            state
                .remaining
                .retain(|&plan| !self.plans[plan].demands.contains(&test));
        }
    }

    fn new_node(&mut self, node: MatchNode<'a>) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(node);
        id
    }

    fn emit(&mut self, root: u32) -> Result<(), CompileError> {
        if self
            .tests
            .iter()
            .any(|test| matches!(test.kind, MatchTestKind::Present))
        {
            self.function.b.load_hole();
            self.function.b.store(self.hole);
        }
        self.labels = vec![None; self.nodes.len()];
        self.emitted = vec![false; self.nodes.len()];
        self.emit_node(root)
    }

    fn label_for(&mut self, node: u32) -> Label {
        if let Some(label) = self.labels[node as usize] {
            return label;
        }
        let label = self.function.b.new_label();
        self.labels[node as usize] = Some(label);
        label
    }

    fn emit_node(&mut self, node: u32) -> Result<(), CompileError> {
        if self.emitted[node as usize] {
            return Ok(());
        }
        self.emitted[node as usize] = true;
        let label = self.label_for(node);
        self.function.b.bind(label);
        match self.nodes[node as usize] {
            MatchNode::Fail => {
                self.function.b.load_constant(self.fail_message);
                self.function.b.throw();
            }
            MatchNode::Test { test, yes, no } => {
                let no_label = self.label_for(no);
                self.emit_test(test, no_label)?;
                let yes_label = self.label_for(yes);
                self.function.b.jump(yes_label);
                self.emit_node(yes)?;
                self.emit_node(no)?;
            }
            MatchNode::Switch {
                occurrence,
                switch,
                default,
            } => {
                let cases = self.switches[switch as usize].clone();
                let default_label = self.label_for(default);
                let targets: Vec<(i64, Label)> = cases
                    .iter()
                    .map(|(value, node)| (*value, self.label_for(*node)))
                    .collect();
                let value = self.occurrence_regs[occurrence as usize];
                self.function
                    .b
                    .switch_int(value, targets[0].0, &targets, default_label)
                    .map_err(|error| {
                        self.function
                            .error(self.function.tree.root_id(), error.to_string())
                    })?;
                for (_, node) in cases {
                    self.emit_node(node)?;
                }
                self.emit_node(default)?;
            }
            MatchNode::AtomSwitch {
                occurrence,
                switch,
                default,
            } => {
                let cases = self.atom_switches[switch as usize].clone();
                let default_label = self.label_for(default);
                let targets: Vec<(Box<[u8]>, Label)> = cases
                    .iter()
                    .map(|(name, node)| (name.clone(), self.label_for(*node)))
                    .collect();
                let value = self.occurrence_regs[occurrence as usize];
                self.function
                    .b
                    .switch_atom(value, &targets, default_label)
                    .map_err(|error| {
                        self.function
                            .error(self.function.tree.root_id(), error.to_string())
                    })?;
                for (_, node) in cases {
                    self.emit_node(node)?;
                }
                self.emit_node(default)?;
            }
            MatchNode::Guard {
                plan,
                expr,
                pass,
                fail,
            } => {
                self.emit_bindings(plan)?;
                self.function.expr(expr)?;
                let fail_label = self.label_for(fail);
                self.function.b.jump_if_falsy(fail_label);
                let pass_label = self.label_for(pass);
                self.function.b.jump(pass_label);
                self.emit_node(pass)?;
                self.emit_node(fail)?;
            }
            MatchNode::Leaf { plan, bound } => {
                self.emit_leaf(plan, bound)?;
            }
        }
        Ok(())
    }

    fn emit_test(&mut self, test: u32, no: Label) -> Result<(), CompileError> {
        let test_id = test as usize;
        let test = self.tests[test_id];
        let occurrence = self.occurrence_regs[test.occurrence as usize];
        match test.kind {
            MatchTestKind::IsObject => {
                let feedback = self.function.b.new_feedback();
                self.function.b.test_type(
                    occurrence,
                    occurrence,
                    TestTypeKind::Object as u32,
                    feedback,
                );
                self.function.b.jump_if_falsy(no);
            }
            MatchTestKind::ArraySource => {
                let source = self.occurrences[test.occurrence as usize];
                let base = source.base.expect("array source has a base");
                let base = self.occurrence_regs[base as usize];
                let array = self.function.b.new_label();
                let done = self.function.b.new_label();
                let feedback = self.function.b.new_feedback();
                self.function
                    .b
                    .test_type(base, base, TestTypeKind::Array as u32, feedback);
                self.function.b.jump_if_truthy(array);
                let feedback = self.function.b.new_feedback();
                self.function
                    .b
                    .test_type(base, base, TestTypeKind::IntoArray as u32, feedback);
                self.function.b.jump_if_falsy(no);
                let converted = self.function.send_reg0(base, "intoArray", &self.temps)?;
                self.function.b.load(converted);
                self.function.b.store(occurrence);
                self.function.b.jump(done);
                self.function.b.bind(array);
                self.function.b.load(base);
                self.function.b.store(occurrence);
                self.function.b.bind(done);
            }
            MatchTestKind::LengthEq(length) => {
                let feedback = self.function.b.new_feedback();
                self.function.b.load_length(occurrence, feedback);
                let current = self.function.b.stage_acc(&self.temps);
                self.function.b.load_smi(length as i32);
                self.function.b.equal_strict(current);
                self.function.b.jump_if_falsy(no);
            }
            MatchTestKind::LengthAtLeast(length) => {
                let feedback = self.function.b.new_feedback();
                self.function.b.load_length(occurrence, feedback);
                let current = self.function.b.stage_acc(&self.temps);
                self.function.b.load_smi(length as i32);
                self.function.b.less_than_or_equal(current);
                self.function.b.jump_if_falsy(no);
            }
            MatchTestKind::OwnSlotCountEq(count) => {
                self.function.b.own_slot_count(occurrence);
                let current = self.function.b.stage_acc(&self.temps);
                self.function.b.load_smi(count as i32);
                self.function.b.equal_strict(current);
                self.function.b.jump_if_falsy(no);
            }
            MatchTestKind::Present => {
                let present = self.occurrences[test.occurrence as usize];
                let base = present.base.expect("present occurrence has a base");
                let base = self.occurrence_regs[base as usize];
                match present.access {
                    MatchAccess::Slot(name) => {
                        self.function.b.load_own_slot_or_hole(base, name);
                    }
                    MatchAccess::Index(index) => {
                        self.function.b.load_smi(index as i32);
                        let feedback = self.function.b.new_feedback();
                        self.function.b.load_indexed_or_hole(base, feedback);
                    }
                    MatchAccess::Root(_) | MatchAccess::ArraySource => {
                        unreachable!("only slots and indexes are present tests")
                    }
                }
                self.function.b.store(occurrence);
                self.function.b.test_reference_equal(self.hole);
                self.function.b.jump_if_truthy(no);
            }
            MatchTestKind::Literal(literal) => {
                self.function.b.load(occurrence);
                self.load_literal(literal);
                match literal {
                    MatchLiteral::Smi(_) | MatchLiteral::Float(_) => {
                        self.function.b.equal_strict(occurrence);
                    }
                    MatchLiteral::String(_) => {
                        self.function.b.equal_strict(occurrence);
                    }
                    MatchLiteral::Atom(_) => {
                        self.function.b.test_reference_equal(occurrence);
                    }
                    MatchLiteral::True
                    | MatchLiteral::False
                    | MatchLiteral::Null
                    | MatchLiteral::Undefined => {
                        self.function.b.test_reference_equal(occurrence);
                    }
                }
                self.function.b.jump_if_falsy(no);
            }
            MatchTestKind::BitMatch(program, args) => {
                self.function.b.load_constant(program);
                let program_reg = self.function.b.stage_acc(&self.temps);
                self.function.load_global("BitArray");
                let receiver = self.function.b.stage_acc(&self.temps);
                let name = self.function.b.name(b"match");
                let feedback = self.function.b.new_feedback();
                self.function
                    .b
                    .load_named_property(receiver, name, feedback);
                let callee = self.function.b.stage_acc(&self.temps);
                let extra = self.bit_args[args as usize].clone();
                let mut call_args = Vec::with_capacity(extra.len() + 2);
                call_args.push(occurrence);
                call_args.push(program_reg);
                call_args.extend(extra);
                self.function
                    .call_regs(&self.temps, callee, receiver, &call_args)?;
                self.function.b.jump_if_nullish(no);
                self.function.b.store(self.capture_regs[test_id]);
            }
            MatchTestKind::UserType(node) => {
                self.function.load_resolution(node)?;
                let ty = self.function.b.stage_acc(&self.temps);
                let feedback = self.function.b.new_feedback();
                self.function
                    .b
                    .test_type(occurrence, ty, TestTypeKind::User as u32, feedback);
                self.function.b.jump_if_falsy(no);
            }
            MatchTestKind::Value(id) => {
                let value = &self.values[id as usize];
                let base = value.base;
                let names = value.names.clone();
                self.function.load_resolution(base)?;
                let mut current = self.function.b.stage_acc(&self.temps);
                for name in names.iter().skip(1) {
                    let feedback = self.function.b.new_feedback();
                    self.function
                        .b
                        .load_named_property(current, *name, feedback);
                    current = self.function.b.stage_acc(&self.temps);
                }
                self.function.b.load(current);
                self.function.b.equal_strict(occurrence);
                self.function.b.jump_if_falsy(no);
            }
        }
        Ok(())
    }

    fn load_literal(&mut self, literal: MatchLiteral) {
        match literal {
            MatchLiteral::Smi(value) => self.function.number(value as f64),
            MatchLiteral::Float(bits) => self.function.number(f64::from_bits(bits)),
            MatchLiteral::String(index) => self.function.b.load_constant(index),
            MatchLiteral::Atom(index) => self.function.b.load_constant(index),
            MatchLiteral::True => self.function.b.load_true(),
            MatchLiteral::False => self.function.b.load_false(),
            MatchLiteral::Null => self.function.b.load_null(),
            MatchLiteral::Undefined => self.function.b.load_undefined(),
        }
    }

    fn emit_bindings(&mut self, plan: usize) -> Result<(), CompileError> {
        let bindings = self.plans[plan].bindings.clone();
        let rests = self.plans[plan].rests.clone();
        for (binding, occurrence) in bindings {
            self.function
                .b
                .load(self.occurrence_regs[occurrence as usize]);
            self.function.store_binding(binding)?;
        }
        for (binding, occurrence, start) in rests {
            let source = self.occurrence_regs[occurrence as usize];
            self.function.b.array_slice(source, start as u32);
            self.function.store_binding(binding)?;
        }
        for (binding, test, index) in self.plans[plan].bit_bindings.clone() {
            let capture = self.capture_regs[test as usize];
            self.function.b.load(capture);
            self.function.b.load_smi(index as i32);
            let feedback = self.function.b.new_feedback();
            self.function.b.load_indexed(capture, feedback);
            self.function.store_binding(binding)?;
        }
        Ok(())
    }

    fn emit_leaf(&mut self, plan: usize, bound: bool) -> Result<(), CompileError> {
        if !bound {
            self.emit_bindings(plan)?;
        }
        let arm = self.plans[plan].arm;
        if let Some(arm) = arm {
            let Some(result) = self.result else {
                return Err(self
                    .function
                    .error(arm.syntax().id(), "missing match result register"));
            };
            self.function
                .match_arm_body(arm, result, self.tail, self.end)?;
        } else if let Some(done) = self.bind_done {
            self.function.b.jump(done);
        }
        Ok(())
    }
}

fn supports_multi_pattern(pattern: Pattern<'_>, count: usize) -> bool {
    match pattern {
        Pattern::Wildcard(_) => true,
        Pattern::Array(array) => array.rest().is_none() && array.elements().count() == count,
        Pattern::Or(or) => or
            .alternatives()
            .all(|alternative| supports_multi_pattern(alternative, count)),
        _ => false,
    }
}

fn same_switch_literal_kind(left: MatchLiteral, right: MatchLiteral) -> bool {
    matches!(
        (left, right),
        (MatchLiteral::Smi(_), MatchLiteral::Smi(_))
            | (MatchLiteral::Atom(_), MatchLiteral::Atom(_))
    )
}

fn switch_literal_key(literal: MatchLiteral) -> i64 {
    match literal {
        MatchLiteral::Smi(value) => value,
        MatchLiteral::Atom(_) => 0,
        _ => 0,
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

fn encode_utf_string(bytes: &[u8], kind: u8, little: bool) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut out = Vec::new();
    match kind {
        bit_program::KIND_UTF8 => out.extend_from_slice(bytes),
        bit_program::KIND_UTF16 => {
            for unit in text.encode_utf16() {
                let encoded = if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                };
                out.extend_from_slice(&encoded);
            }
        }
        bit_program::KIND_UTF32 => {
            for ch in text.chars() {
                let encoded = if little {
                    (ch as u32).to_le_bytes()
                } else {
                    (ch as u32).to_be_bytes()
                };
                out.extend_from_slice(&encoded);
            }
        }
        _ => return None,
    }
    Some(out)
}
