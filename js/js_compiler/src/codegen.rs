use std::collections::HashMap;

use oxc_ast::ast::*;
use oxc_semantic::Scoping;
use oxc_span::{GetSpan, Span};
use oxc_syntax::node::NodeId;
use oxc_syntax::scope::{ScopeFlags, ScopeId};
use oxc_syntax::symbol::{SymbolFlags, SymbolId};

use bytecode::{
    BuiltinFn, CallableKind, ConstIdx, Constant, FnBuilder, FunctionId, FunctionMeta,
    GlobalDeclarations, GlobalFunctionDecl, Label, Opcode, Program, PropertyFlags, Reg, RegList,
    RtArg, ScopeLayout, ScopeLocal, TempScope,
};
use vm_core::{
    INFO_GLOBAL, MODE_CLASS, MODE_CONST, MODE_LET, MODE_VAR, SCOPE_DECLARATION, SCOPE_REPL,
    SCOPE_SCRIPT,
};

use crate::analysis::{
    ClassIdx, Facts, Fid, FnBody, FnKind, Home, MemberKind, Mode, RootBinding, Special,
    root_binding,
};

/// A construct the materializer does not support yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub span: Span,
    pub feature: &'static str,
}

impl CompileError {
    fn new(span: Span, feature: &'static str) -> Self {
        Self { span, feature }
    }
}

impl core::fmt::Display for CompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "unsupported: {} at {}..{}",
            self.feature, self.span.start, self.span.end
        )
    }
}

/// A breakable statement: loops add a continue target, switches don't.
struct Breakable {
    labels: Vec<String>,
    breaks: Label,
    continues: Option<Label>,
    /// active context depth at the target
    context_depth: usize,
}

/// A store target ready for the final store. `_temps` keeps the staged
/// operand registers alive until the target is dropped; super receivers are
/// staged in the caller's temp scope so they can outlive the home/key parts.
enum StoreTarget {
    Named {
        obj: Reg,
        name_idx: ConstIdx,
        _temps: TempScope,
    },
    Keyed {
        obj: Reg,
        key: Reg,
        _temps: TempScope,
    },
    /// `this.#x = v`
    PrivateKeyed {
        obj: Reg,
        key: Reg,
        _temps: TempScope,
    },
    SuperNamed {
        recv: Reg,
        home: Reg,
        name_idx: ConstIdx,
        _temps: TempScope,
    },
    SuperKeyed {
        recv: Reg,
        home: Reg,
        key: Reg,
        _temps: TempScope,
    },
}

/// Name hint for NamedEvaluation in pattern defaults (ES 8.4.3).
enum NameHint {
    Const(ConstIdx),
    Reg(Reg),
    None,
}

/// The slot decision for a declared symbol, shared between its store and
/// load sites (assigned in the owning function's prologue, or lazily for
/// class / for-head context slots).
#[derive(Clone, Copy, Debug)]
enum Slot {
    Param {
        index: u32,
        hole_check: bool,
    },
    Local {
        reg: u32,
        hole_check: bool,
    },
    /// context slot in the owning function's frame context; the depth is
    /// a property of each use site
    Ctx {
        slot: u32,
        hole_check: bool,
    },
    /// context slot in a class / for-head block context; the depth is a
    /// property of each use site (precomputed per reference, or derived
    /// from the active context chain for declaration stores)
    CtxAt {
        slot: u32,
        hole_check: bool,
    },
    /// Root var/function binding on the global object
    Global,
    /// REPL root lexical: name lookup through the script-context table so
    /// redeclarations share the first script context's slot
    Repl,
}

/// The AST construct owning one runtime context. The function frame context
/// is the bottom of the chain; class / for-head / object-home contexts are
/// pushed above it while their construct executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextHost {
    Function(Fid),
    Block(ScopeId),
    Class(ClassIdx),
    For(NodeId),
    Object(NodeId),
}

/// Per-function frame layout, built at the prologue.
struct Layout {
    register_count: u32,
    this_slot: Option<u32>,
    new_target_slot: Option<u32>,
    this_function_slot: Option<u32>,
    slot_names: Vec<ScopeLocal>,
    /// block scopes that own a runtime context
    block_slots: HashMap<ScopeId, Vec<ScopeLocal>>,
    /// per scope: register locals that start in the TDZ hole
    hole_fills: HashMap<ScopeId, Vec<u32>>,
    /// whether the prologue must create/push a function context: any
    /// context-allocated binding, or a direct `eval` that may add
    /// bindings at runtime
    creates_context: bool,
}

/// How an identifier reference resolves at its use site.
enum IdRes {
    Global,
    Dynamic,
    Slot(Slot, u32),
}

/// A destructuring view over either pattern flavor.
#[derive(Clone, Copy)]
enum PatTarget<'r, 'a> {
    Binding(&'r BindingPattern<'a>),
    Assign(&'r AssignmentTarget<'a>),
    /// shorthand assignment-property leaf: `{a}` / `{a = 1}`
    AssignIdent(&'r IdentifierReference<'a>),
}

impl PatTarget<'_, '_> {
    fn span(&self) -> Span {
        match self {
            PatTarget::Binding(p) => p.span(),
            PatTarget::Assign(t) => t.span(),
            PatTarget::AssignIdent(i) => i.span,
        }
    }
}

enum PatElement<'r, 'a> {
    Hole,
    Item {
        target: PatTarget<'r, 'a>,
        default: Option<&'r Expression<'a>>,
    },
    Rest(PatTarget<'r, 'a>),
}

enum PatProperty<'r, 'a> {
    Prop {
        key: &'r PropertyKey<'a>,
        computed: bool,
        target: PatTarget<'r, 'a>,
        default: Option<&'r Expression<'a>>,
    },
    /// shorthand `{a}` assignment property: the key is the binding name
    Named {
        name: Vec<u8>,
        target: PatTarget<'r, 'a>,
        default: Option<&'r Expression<'a>>,
    },
    Rest(PatTarget<'r, 'a>),
}

fn array_binding_elements<'r, 'a>(a: &'r ArrayPattern<'a>) -> Vec<PatElement<'r, 'a>> {
    let mut els = Vec::new();
    for el in a.elements.iter() {
        match el {
            None => els.push(PatElement::Hole),
            Some(p) => els.push(binding_item(p)),
        }
    }
    if let Some(rest) = &a.rest {
        els.push(PatElement::Rest(PatTarget::Binding(&rest.argument)));
    }
    els
}

fn object_binding_props<'r, 'a>(o: &'r ObjectPattern<'a>) -> Vec<PatProperty<'r, 'a>> {
    let mut props = Vec::new();
    for prop in &o.properties {
        let (target, default) = split_binding_default(&prop.value);
        props.push(PatProperty::Prop {
            key: &prop.key,
            computed: prop.computed,
            target,
            default,
        });
    }
    if let Some(rest) = &o.rest {
        props.push(PatProperty::Rest(PatTarget::Binding(&rest.argument)));
    }
    props
}

fn array_assign_elements<'r, 'a>(a: &'r ArrayAssignmentTarget<'a>) -> Vec<PatElement<'r, 'a>> {
    let mut els = Vec::new();
    for el in a.elements.iter() {
        match el {
            None => els.push(PatElement::Hole),
            Some(AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d)) => {
                els.push(PatElement::Item {
                    target: PatTarget::Assign(&d.binding),
                    default: Some(&d.init),
                });
            }
            Some(other) => {
                els.push(PatElement::Item {
                    target: PatTarget::Assign(other.as_assignment_target().expect("plain target")),
                    default: None,
                });
            }
        }
    }
    if let Some(rest) = &a.rest {
        els.push(PatElement::Rest(PatTarget::Assign(&rest.target)));
    }
    els
}

fn object_assign_props<'r, 'a>(o: &'r ObjectAssignmentTarget<'a>) -> Vec<PatProperty<'r, 'a>> {
    let mut props = Vec::new();
    for prop in &o.properties {
        match prop {
            AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                let p = &**p;
                props.push(PatProperty::Named {
                    name: p.binding.name.as_bytes().to_vec(),
                    target: PatTarget::AssignIdent(&p.binding),
                    default: p.init.as_ref(),
                });
            }
            AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                let p = &**p;
                let (target, default) = match &p.binding {
                    AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                        (PatTarget::Assign(&d.binding), Some(&d.init))
                    }
                    other => (
                        PatTarget::Assign(other.as_assignment_target().expect("plain target")),
                        None,
                    ),
                };
                props.push(PatProperty::Prop {
                    key: &p.name,
                    computed: p.computed,
                    target,
                    default,
                });
            }
        }
    }
    if let Some(rest) = &o.rest {
        props.push(PatProperty::Rest(PatTarget::Assign(&rest.target)));
    }
    props
}

/// One binding-pattern array element, defaults unwrapped.
fn binding_item<'r, 'a>(p: &'r BindingPattern<'a>) -> PatElement<'r, 'a> {
    match p {
        BindingPattern::AssignmentPattern(a) => PatElement::Item {
            target: PatTarget::Binding(&a.left),
            default: Some(&a.right),
        },
        other => PatElement::Item {
            target: PatTarget::Binding(other),
            default: None,
        },
    }
}

fn is_simple(p: &BindingPattern<'_>) -> bool {
    matches!(p, BindingPattern::BindingIdentifier(_))
}

/// The immediate form of a register arithmetic opcode (`Add` -> `AddImmediate`).
fn immediate_variant(op: Opcode) -> Option<Opcode> {
    Some(match op {
        Opcode::Add => Opcode::AddImmediate,
        Opcode::Sub => Opcode::SubImmediate,
        Opcode::Mul => Opcode::MulImmediate,
        Opcode::Div => Opcode::DivImmediate,
        Opcode::Mod => Opcode::ModImmediate,
        Opcode::Exp => Opcode::ExpImmediate,
        Opcode::BitwiseOr => Opcode::BitwiseOrImmediate,
        Opcode::BitwiseXor => Opcode::BitwiseXorImmediate,
        Opcode::BitwiseAnd => Opcode::BitwiseAndImmediate,
        Opcode::ShiftLeft => Opcode::ShiftLeftImmediate,
        Opcode::ShiftRight => Opcode::ShiftRightImmediate,
        Opcode::ShiftRightLogical => Opcode::ShiftRightLogicalImmediate,
        _ => return None,
    })
}

/// Whether `op` is numeric-commutative (`imm OP x` == `x OP imm`).
/// Addition is excluded: JS `+` concatenates in operand order.
fn is_commutative(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::Mul | Opcode::BitwiseOr | Opcode::BitwiseXor | Opcode::BitwiseAnd
    )
}

/// Whether evaluating `e` can write a non-captured local (and thus whether
/// a register left operand may be read after it). A call runs in its own
/// frame and cannot reach our locals, but the callee/argument expressions
/// can contain assignments: any assignment or update in the subtree makes
/// this conservative.
fn rhs_cannot_clobber_locals(e: &Expression<'_>) -> bool {
    struct Scan {
        assigns: bool,
    }
    impl<'a> oxc_ast_visit::Visit<'a> for Scan {
        fn visit_assignment_expression(&mut self, _it: &oxc_ast::ast::AssignmentExpression<'a>) {
            self.assigns = true;
        }
        fn visit_update_expression(&mut self, _it: &oxc_ast::ast::UpdateExpression<'a>) {
            self.assigns = true;
        }
    }
    use oxc_ast_visit::Visit as _;
    let mut scan = Scan { assigns: false };
    scan.visit_expression(e);
    !scan.assigns
}

/// The integer value of a numeric literal that fits a 2-byte immediate
/// operand, if `e` is one.
fn int_literal(e: &Expression<'_>) -> Option<i32> {
    let Expression::NumericLiteral(n) = e else {
        return None;
    };
    let v = n.value;
    (v.fract() == 0.0 && v >= i16::MIN as f64 && v <= i16::MAX as f64).then_some(v as i32)
}

fn arith_opcode(op: BinaryOperator) -> Option<Opcode> {
    use BinaryOperator as Op;
    Some(match op {
        Op::Addition => Opcode::Add,
        Op::Subtraction => Opcode::Sub,
        Op::Multiplication => Opcode::Mul,
        Op::Division => Opcode::Div,
        Op::Remainder => Opcode::Mod,
        Op::Exponential => Opcode::Exp,
        Op::ShiftLeft => Opcode::ShiftLeft,
        Op::ShiftRight => Opcode::ShiftRight,
        Op::ShiftRightZeroFill => Opcode::ShiftRightLogical,
        Op::BitwiseOR => Opcode::BitwiseOr,
        Op::BitwiseXOR => Opcode::BitwiseXor,
        Op::BitwiseAnd => Opcode::BitwiseAnd,
        _ => return None,
    })
}

/// A borrowed member expression: `Expression` flattens the member kinds,
/// so helpers consume this view instead.
#[derive(Clone, Copy)]
enum MemberRef<'a> {
    Static(&'a StaticMemberExpression<'a>),
    Computed(&'a ComputedMemberExpression<'a>),
    Private(&'a PrivateFieldExpression<'a>),
}

impl<'a> MemberRef<'a> {
    fn of(e: &'a Expression<'a>) -> Option<Self> {
        Some(match e {
            Expression::StaticMemberExpression(m) => Self::Static(m),
            Expression::ComputedMemberExpression(m) => Self::Computed(m),
            Expression::PrivateFieldExpression(m) => Self::Private(m),
            _ => return None,
        })
    }

    fn span(&self) -> Span {
        match self {
            Self::Static(m) => m.span,
            Self::Computed(m) => m.span,
            Self::Private(m) => m.span,
        }
    }

    fn node_id(&self) -> NodeId {
        match self {
            Self::Static(m) => m.node_id.get(),
            Self::Computed(m) => m.node_id.get(),
            Self::Private(m) => m.node_id.get(),
        }
    }
}

fn is_super_member(m: MemberRef<'_>) -> bool {
    match m {
        MemberRef::Static(e) => matches!(e.object, Expression::Super(_)),
        MemberRef::Computed(e) => matches!(e.object, Expression::Super(_)),
        MemberRef::Private(_) => false,
    }
}

/// `this.<name> = ...` stores count toward the constructor-instance presize
/// hint. Only plain assignments count (compound/update stores are absorbed
/// by slack).
fn is_this_member(m: MemberRef<'_>) -> bool {
    match m {
        MemberRef::Static(e) => matches!(e.object, Expression::ThisExpression(_)),
        MemberRef::Computed(e) => matches!(e.object, Expression::ThisExpression(_)),
        MemberRef::Private(_) => false,
    }
}

/// A member expression inside a simple assignment target.
fn assign_member_ref_for_left<'a>(t: &'a ForStatementLeft<'a>) -> Option<MemberRef<'a>> {
    match t {
        ForStatementLeft::StaticMemberExpression(m) => Some(MemberRef::Static(m)),
        ForStatementLeft::ComputedMemberExpression(m) => Some(MemberRef::Computed(m)),
        ForStatementLeft::PrivateFieldExpression(m) => Some(MemberRef::Private(m)),
        _ => None,
    }
}

fn assign_member_ref<'a>(t: &'a AssignmentTarget<'a>) -> Option<MemberRef<'a>> {
    match t {
        AssignmentTarget::StaticMemberExpression(m) => Some(MemberRef::Static(m)),
        AssignmentTarget::ComputedMemberExpression(m) => Some(MemberRef::Computed(m)),
        AssignmentTarget::PrivateFieldExpression(m) => Some(MemberRef::Private(m)),
        _ => None,
    }
}

fn simple_member_ref<'a>(t: &'a SimpleAssignmentTarget<'a>) -> Option<MemberRef<'a>> {
    match t {
        SimpleAssignmentTarget::StaticMemberExpression(m) => Some(MemberRef::Static(m)),
        SimpleAssignmentTarget::ComputedMemberExpression(m) => Some(MemberRef::Computed(m)),
        SimpleAssignmentTarget::PrivateFieldExpression(m) => Some(MemberRef::Private(m)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// shared compiler state: slot memo + scope queries
// ---------------------------------------------------------------------------

pub struct Compiler<'a, 'p> {
    scoping: &'p Scoping,
    facts: &'p Facts<'a>,
    /// scopes allocating from each function's register/context space
    /// (class / for contexts excluded), in scope-id order
    scopes_by_fid: Vec<Vec<ScopeId>>,
    /// context-free for-head scopes allocating function registers
    for_scopes_by_fid: Vec<Vec<ScopeId>>,
    /// scopes nested under a for scope that the function hosts
    nested_scopes_by_fid: Vec<Vec<ScopeId>>,
    /// symbol → slot, the single source of agreement between stores/loads
    slots: HashMap<SymbolId, Slot>,
    /// per-fid layouts, filled as functions are compiled (reads only ever
    /// target already-compiled ancestors)
    layouts: Vec<Layout>,
    /// fid → its oxc function scope (eval detection)
    fid_scope: HashMap<Fid, ScopeId>,
    /// any sloppy direct eval in the program (fast path for identifier
    /// resolution: without one, static bindings cannot be shadowed)
    any_sloppy_eval: bool,
    /// for-head scopes: next slot index (declaration-order assignment)
    for_counters: HashMap<ScopeId, u32>,
}

pub fn generate<'a>(scoping: &Scoping, facts: &Facts<'a>) -> Result<Program, CompileError> {
    let n = facts.functions.len();
    let mut scopes_by_fid: Vec<Vec<ScopeId>> = (0..n).map(|_| Vec::new()).collect();
    for sid in 0..scoping.scopes_len() {
        let sid = ScopeId::from_usize(sid);
        if let Some(&fid) = facts.scope_owner.get(&sid) {
            if fid != Fid(u32::MAX) {
                scopes_by_fid[fid.0 as usize].push(sid);
            }
        }
    }
    // register-allocated for heads (no context) and bindings nested under
    // a for scope that the function hosts
    let mut for_scopes_by_fid: Vec<Vec<ScopeId>> = (0..n).map(|_| Vec::new()).collect();
    let mut nested_scopes_by_fid: Vec<Vec<ScopeId>> = (0..n).map(|_| Vec::new()).collect();
    for sid in 0..scoping.scopes_len() {
        let sid = ScopeId::from_usize(sid);
        if facts.class_of_scope.contains_key(&sid) || facts.fn_scope_to_fid.contains_key(&sid) {
            continue;
        }
        if let Some(head) = facts.for_heads.get(&sid) {
            if !head.creates_context() {
                let mut cur = scoping.scope_parent_id(sid);
                while let Some(s) = cur {
                    if let Some(&fid) = facts.fn_scope_to_fid.get(&s) {
                        for_scopes_by_fid[fid.0 as usize].push(sid);
                        break;
                    }
                    cur = scoping.scope_parent_id(s);
                }
            }
            continue;
        }
        let mut crossed_for = false;
        let mut owner = None;
        let mut cur = scoping.scope_parent_id(sid);
        while let Some(s) = cur {
            if let Some(&fid) = facts.fn_scope_to_fid.get(&s) {
                owner = Some(fid);
                break;
            }
            if facts.class_of_scope.contains_key(&s) {
                break;
            }
            if facts.for_heads.contains_key(&s) {
                crossed_for = true;
            }
            cur = scoping.scope_parent_id(s);
        }
        if crossed_for && let Some(fid) = owner {
            nested_scopes_by_fid[fid.0 as usize].push(sid);
        }
    }
    let any_sloppy_eval = facts.fn_scope_to_fid.iter().any(|(&scope, &fid)| {
        !facts.functions[fid.0 as usize].strict
            && scoping.scope_flags(scope).contains(ScopeFlags::DirectEval)
    });
    let mut compiler = Compiler {
        scoping,
        facts,
        scopes_by_fid,
        for_scopes_by_fid,
        nested_scopes_by_fid,
        slots: HashMap::new(),
        layouts: (0..n)
            .map(|_| Layout {
                register_count: 0,
                this_slot: None,
                new_target_slot: None,
                this_function_slot: None,
                slot_names: Vec::new(),
                block_slots: HashMap::new(),
                hole_fills: HashMap::new(),
                creates_context: true,
            })
            .collect(),
        fid_scope: facts
            .fn_scope_to_fid
            .iter()
            .map(|(&s, &f)| (f, s))
            .collect(),
        any_sloppy_eval,
        for_counters: HashMap::new(),
    };
    // Frame layouts decide both register counts and whether a function
    // pushes a context; context depths are baked into the bytecode, so
    // they must be settled before any body (including nested depth
    // calculations) is emitted.
    for fid in 0..n as u32 {
        let layout = compiler.assign_function_slots(Fid(fid));
        compiler.layouts[fid as usize] = layout;
    }
    let mut program = Program::with_capacity(n);
    for fid in 0..n as u32 {
        let _span = trace::debug_span!("js::function", fid).entered();
        let mut fgen = FunctionGen::new(&mut compiler, Fid(fid));
        fgen.emit_function_body()?;
        program.add_function(fgen.finish());
    }
    #[cfg(debug_assertions)]
    if let Err(err) = bytecode::validate(&program) {
        panic!("invalid bytecode: {err:?}");
    }
    Ok(program)
}

impl<'a, 'p> Compiler<'a, 'p> {
    fn scope_creates_ctx(&self, scope: ScopeId) -> bool {
        if let Some(&fid) = self.facts.fn_scope_to_fid.get(&scope) {
            return self.layouts[fid.0 as usize].creates_context;
        }
        if self.facts.block_contexts.contains(&scope) {
            return true;
        }
        if let Some(&idx) = self.facts.class_of_scope.get(&scope) {
            return self.facts.classes[idx.0 as usize].slot_count > 0;
        }
        self.facts
            .for_heads
            .get(&scope)
            .is_some_and(|head| head.creates_context())
    }

    /// Whether `host` is the context-creating scope at or above
    /// `decl_scope` that hosts its slots.
    fn hosts(&self, host: ScopeId, decl_scope: ScopeId) -> bool {
        let mut cur = decl_scope;
        loop {
            if cur == host && self.scope_creates_ctx(cur) {
                return true;
            }
            if self.scope_creates_ctx(cur) {
                return false;
            }
            cur = self
                .scoping
                .scope_parent_id(cur)
                .expect("scope chain ends at the root");
        }
    }

    /// ContextObject hops from the use site's scope to the context hosting
    /// `decl_scope`'s slots (the use site's own context-creating
    /// ancestor is depth 0).
    fn depth_to(&self, use_scope: ScopeId, decl_scope: ScopeId) -> u32 {
        // depth = index of the hosting context in the use site's context
        // chain, where the innermost (use site's own) context is index 0
        let mut passed_use = false;
        let mut hops = 0u32;
        let mut cur = use_scope;
        loop {
            if self.scope_creates_ctx(cur) {
                if !passed_use {
                    passed_use = true;
                } else {
                    hops += 1;
                }
                if self.hosts(cur, decl_scope) {
                    return hops;
                }
            }
            cur = self
                .scoping
                .scope_parent_id(cur)
                .expect("scope chain ends at the root");
        }
    }

    /// The construct owning the context that hosts `sym`'s slots. For a
    /// declaration store this is always on the active emission-time context
    /// chain (function frame or an enclosing class / for-head / home object).
    fn context_host_of(&self, sym: SymbolId) -> ContextHost {
        let mut cur = self.scoping.symbol_scope_id(sym);
        loop {
            if self.facts.block_contexts.contains(&cur) {
                return ContextHost::Block(cur);
            }
            if let Some(&idx) = self.facts.class_of_scope.get(&cur) {
                return ContextHost::Class(idx);
            }
            if let Some(head) = self.facts.for_heads.get(&cur) {
                return ContextHost::For(head.node);
            }
            if let Some(&fid) = self.facts.fn_scope_to_fid.get(&cur) {
                return ContextHost::Function(fid);
            }
            cur = self
                .scoping
                .scope_parent_id(cur)
                .expect("scope chain ends at the root");
        }
    }

    /// The memoized slot decision for a symbol.
    fn slot_of(&mut self, sym: SymbolId) -> Slot {
        if let Some(&slot) = self.slots.get(&sym) {
            return slot;
        }
        let scope = self.scoping.symbol_scope_id(sym);
        // only class / for-head symbols reach the lazy path:
        // function-owned symbols are assigned eagerly at their
        // function's prologue
        let slot = if let Some(&idx) = self.facts.class_of_scope.get(&scope) {
            Slot::CtxAt {
                slot: self.facts.classes[idx.0 as usize].name_slot.unwrap_or(0),
                hole_check: true,
            }
        } else {
            let slot = self.for_next_entry(scope);
            Slot::CtxAt {
                slot,
                hole_check: true,
            }
        };
        self.slots.insert(sym, slot);
        slot
    }

    /// Next context-slot index for a for-head scope (declaration order).
    fn for_next_entry(&mut self, scope: ScopeId) -> u32 {
        let n = self.scoping.iter_bindings_in(scope).count() as u32;
        let next = self.for_counters.get(&scope).copied().unwrap_or(0);
        self.for_counters.insert(scope, next + 1);
        debug_assert!(next < n, "for-head slot overflow");
        next
    }

    fn scope_local_info(&self, sym: SymbolId) -> u32 {
        let flags = self.scoping.symbol_flags(sym);
        if flags.contains(SymbolFlags::ConstVariable) {
            MODE_CONST as u32
        } else if flags.contains(SymbolFlags::Class) {
            MODE_CLASS as u32
        } else if flags.contains(SymbolFlags::BlockScopedVariable) {
            MODE_LET as u32
        } else {
            MODE_VAR as u32
        }
    }

    /// Assign every slot of `fid`'s scopes (its prologue's layout).
    fn assign_function_slots(&mut self, fid: Fid) -> Layout {
        let non_simple = self.facts.functions[fid.0 as usize]
            .params
            .iter()
            .any(|p| p.rest || p.default.is_some() || !is_simple(p.pattern));
        // oxc propagates DirectEval to every ancestor scope, so the
        // function scope flag covers calls anywhere inside it
        let calls_eval = self
            .fid_scope
            .get(&fid)
            .is_some_and(|&s| self.scoping.scope_flags(s).contains(ScopeFlags::DirectEval));

        let mut next_reg = 0u32;
        let mut next_ctx = 0u32;
        let mut slot_names: Vec<ScopeLocal> = Vec::new();
        let mut block_slots: HashMap<ScopeId, Vec<ScopeLocal>> = HashMap::new();
        let mut block_next: HashMap<ScopeId, u32> = HashMap::new();
        let mut hole_fills: HashMap<ScopeId, Vec<u32>> = HashMap::new();
        let mut this_slot = None;
        let mut push_special = |name: &[u8]| {
            let info = 0;
            slot_names.push(ScopeLocal {
                name: name.into(),
                info,
            });
            next_ctx += 1;
            next_ctx - 1
        };
        if self.facts.captures_this.contains(&fid) {
            this_slot = Some(push_special(b"this"));
        }
        let mut new_target_slot = None;
        if self.facts.captures_new_target.contains(&fid) {
            new_target_slot = Some(push_special(b".new.target"));
        }
        let mut this_function_slot = None;
        if self.facts.needs_this_function.contains(&fid) {
            this_function_slot = Some(push_special(b".this_function"));
        }

        for &sid in &self.scopes_by_fid[fid.0 as usize] {
            for sym in self.scoping.iter_bindings_in(sid) {
                let flags = self.scoping.symbol_flags(sym);
                let block_context = self.facts.block_contexts.contains(&sid)
                    && flags.intersects(SymbolFlags::BlockScopedVariable | SymbolFlags::Class);
                match root_binding(self.facts.mode, self.facts.root, sid, flags) {
                    Some(RootBinding::GlobalObject) => {
                        self.slots.insert(sym, Slot::Global);
                        continue;
                    }
                    Some(RootBinding::GlobalLexical) => {
                        let slot = next_ctx;
                        next_ctx += 1;
                        slot_names.push(ScopeLocal {
                            name: self.scoping.symbol_name(sym).as_bytes().into(),
                            info: self.scope_local_info(sym) | INFO_GLOBAL as u32,
                        });
                        let slot = if self.facts.mode == Mode::Repl {
                            Slot::Repl
                        } else {
                            Slot::Ctx {
                                slot,
                                hole_check: true,
                            }
                        };
                        self.slots.insert(sym, slot);
                        continue;
                    }
                    None => {}
                }
                let is_param = self.facts.param_symbols.contains_key(&sym);
                let is_pattern_param = self.facts.pattern_params.contains(&sym);
                let hole_check = if is_pattern_param {
                    true
                } else if is_param {
                    non_simple
                } else {
                    flags.intersects(SymbolFlags::BlockScopedVariable | SymbolFlags::Class)
                };
                let forced = calls_eval || self.facts.captured.contains(&sym);
                let slot = if block_context {
                    let slot = *block_next.entry(sid).or_default();
                    block_next.insert(sid, slot + 1);
                    block_slots.entry(sid).or_default().push(ScopeLocal {
                        name: self.scoping.symbol_name(sym).as_bytes().into(),
                        info: self.scope_local_info(sym),
                    });
                    Slot::CtxAt { slot, hole_check }
                } else if is_param && !is_pattern_param && !forced {
                    Slot::Param {
                        index: self.facts.param_symbols[&sym],
                        hole_check,
                    }
                } else if forced {
                    let slot = next_ctx;
                    next_ctx += 1;
                    slot_names.push(ScopeLocal {
                        name: self.scoping.symbol_name(sym).as_bytes().into(),
                        info: self.scope_local_info(sym),
                    });
                    Slot::Ctx { slot, hole_check }
                } else {
                    let reg = next_reg;
                    next_reg += 1;
                    if hole_check {
                        hole_fills.entry(sid).or_default().push(reg);
                    }
                    Slot::Local { reg, hole_check }
                };
                self.slots.insert(sym, slot);
            }
        }

        for i in 0..self.for_scopes_by_fid[fid.0 as usize].len() {
            let sid = self.for_scopes_by_fid[fid.0 as usize][i];
            for sym in self.scoping.iter_bindings_in(sid) {
                let reg = next_reg;
                next_reg += 1;
                hole_fills.entry(sid).or_default().push(reg);
                self.slots.insert(
                    sym,
                    Slot::Local {
                        reg,
                        hole_check: true,
                    },
                );
            }
        }
        for i in 0..self.nested_scopes_by_fid[fid.0 as usize].len() {
            let sid = self.nested_scopes_by_fid[fid.0 as usize][i];
            for sym in self.scoping.iter_bindings_in(sid) {
                let flags = self.scoping.symbol_flags(sym);
                let block_context = self.facts.block_contexts.contains(&sid)
                    && flags.intersects(SymbolFlags::BlockScopedVariable | SymbolFlags::Class);
                if block_context {
                    let slot = *block_next.entry(sid).or_default();
                    block_next.insert(sid, slot + 1);
                    block_slots.entry(sid).or_default().push(ScopeLocal {
                        name: self.scoping.symbol_name(sym).as_bytes().into(),
                        info: self.scope_local_info(sym),
                    });
                    self.slots.insert(
                        sym,
                        Slot::CtxAt {
                            slot,
                            hole_check: true,
                        },
                    );
                    continue;
                }
                if calls_eval || self.facts.captured.contains(&sym) {
                    continue;
                }
                let hole_check = self
                    .scoping
                    .symbol_flags(sym)
                    .intersects(SymbolFlags::BlockScopedVariable | SymbolFlags::Class);
                let reg = next_reg;
                next_reg += 1;
                if hole_check {
                    hole_fills.entry(sid).or_default().push(reg);
                }
                self.slots.insert(sym, Slot::Local { reg, hole_check });
            }
        }

        let creates_context = !slot_names.is_empty() || !block_slots.is_empty() || calls_eval;
        debug_assert_eq!(
            creates_context,
            self.facts.creates_context.contains(&fid),
            "context predicate desynced from the slot layout"
        );
        Layout {
            register_count: next_reg,
            this_slot,
            new_target_slot,
            this_function_slot,
            creates_context,
            slot_names,
            block_slots,
            hole_fills,
        }
    }
}

// ---------------------------------------------------------------------------
// per-function emission
// ---------------------------------------------------------------------------

struct FunctionGen<'c, 'a, 'p> {
    c: &'c mut Compiler<'a, 'p>,
    fid: Fid,
    b: FnBuilder,
    /// register holding the pushed-over context for prologue/epilogue
    ctx_save: Reg,
    /// whether the prologue created/pushed a function context
    frame_context: bool,
    /// script completion-value register (scripts/eval only)
    completion: Option<Reg>,
    breakables: Vec<Breakable>,
    /// labels forwarded into an enclosing loop/switch by LabeledStatements
    nested_labels: Vec<String>,
    /// Active runtime context chain, outermost first. The function frame
    /// context is present when the prologue created one; block / class /
    /// for-head / object-home contexts are pushed and popped around their
    /// construct. Declaration stores use this to find the hosting depth.
    contexts: Vec<ContextHost>,
    /// save registers parallel to `contexts`
    context_saves: Vec<Reg>,
}

/// RAII guard for one structural context pushed by a class / for-head /
/// object-home construct. Construction emits `PushContext`; drop emits the
/// matching `PopContext` and removes the host from the emission-time chain.
///
/// # Safety contract
///
/// The guard borrows the generator through a raw pointer so the generator can
/// keep emitting while the guard lives (the borrow checker cannot express
/// "borrow only for the destructor"). Consequently:
///
/// - the `FunctionGen` must not move while a guard is alive,
/// - guards must be dropped in LIFO order (the normal Rust drop order does
///   this for lexically nested scopes),
/// - no other code may pop a context while its guard is alive; per-iteration
///   replacement goes through [`ContextScope::replace_block`].
struct ContextScope<'c, 'a, 'p> {
    generator: *mut FunctionGen<'c, 'a, 'p>,
    save: Reg,
    host: ContextHost,
}

impl<'c, 'a, 'p> ContextScope<'c, 'a, 'p> {
    /// Replace this context with a fresh sibling (same host and depth),
    /// keeping the guard alive. Used by per-iteration lexical loop heads.
    fn replace_block(&self, slots: u32, save_into: Option<Reg>) {
        // SAFETY: the guard's generator is alive and not moved; this is the
        // same exclusive access the emitting code already holds.
        unsafe {
            (*self.generator).replace_context_block(self.save, self.host, slots, save_into);
        }
    }
}

impl<'c, 'a, 'p> Drop for ContextScope<'c, 'a, 'p> {
    fn drop(&mut self) {
        // SAFETY: the generator outlives the guard (see the type contract).
        unsafe {
            (*self.generator).pop_context_checked(self.save, self.host);
        }
    }
}

impl<'c, 'a, 'p> FunctionGen<'c, 'a, 'p> {
    fn new(c: &'c mut Compiler<'a, 'p>, fid: Fid) -> Self {
        let arity = c.facts.functions[fid.0 as usize].params.len() as u32;
        Self {
            c,
            fid,
            b: FnBuilder::new(arity),
            ctx_save: Reg::new(0),
            frame_context: false,
            completion: None,
            breakables: Vec::new(),
            nested_labels: Vec::new(),
            contexts: Vec::new(),
            context_saves: Vec::new(),
        }
    }

    fn finish(self) -> bytecode::Function {
        let info = &self.c.facts.functions[self.fid.0 as usize];
        let meta = FunctionMeta {
            name: info.name.clone().map(|n| n.into_bytes().into()),
            kind: callable_kind(info.kind),
            length: info.formal_length,
            strict: info.strict,
        };
        // build failures (unbalanced temps, unbound labels) are compiler
        // bugs: every emit path pairs its temp marks and binds its labels
        self.b.finish(meta).expect("js function builder invariants")
    }

    fn err<T>(&self, span: Span, feature: &'static str) -> Result<T, CompileError> {
        Err(CompileError::new(span, feature))
    }

    fn global_declarations(&self) -> GlobalDeclarations {
        let fns = self
            .c
            .facts
            .hoist_fns
            .get(&self.fid)
            .cloned()
            .unwrap_or_default();
        let mut funcs: Vec<GlobalFunctionDecl> = Vec::new();
        for (sym, fid) in fns {
            let name: Box<[u8]> = self.c.scoping.symbol_name(sym).as_bytes().into();
            if let Some(existing) = funcs.iter_mut().find(|d| d.name == name) {
                existing.function = FunctionId(fid.0);
            } else {
                funcs.push(GlobalFunctionDecl {
                    name,
                    function: FunctionId(fid.0),
                });
            }
        }
        let mut vars: Vec<Box<[u8]>> = Vec::new();
        for sym in self.c.facts.hoist_vars.get(&self.fid).into_iter().flatten() {
            let name: Box<[u8]> = self.c.scoping.symbol_name(*sym).as_bytes().into();
            if funcs.iter().any(|d| d.name == name) || vars.iter().any(|n| *n == name) {
                continue;
            }
            vars.push(name);
        }
        GlobalDeclarations {
            vars: vars.into_boxed_slice(),
            funcs: funcs.into_boxed_slice(),
        }
    }

    // -- staging helpers ----------------------------------------------------

    fn with_temps<F, T>(&mut self, f: F) -> Result<T, CompileError>
    where
        F: FnOnce(&mut Self, &TempScope) -> Result<T, CompileError>,
    {
        let temps = self.b.temp_scope();
        f(self, &temps)
    }

    /// Push the function's own frame context. Unlike structural contexts this
    /// has one entry and many runtime exits (every `return`), so it cannot be
    /// a scoped guard; [`Self::pop_frame_context`] is its single exit path.
    fn push_frame_context(&mut self) {
        self.b.push_context(self.ctx_save);
        self.contexts.push(ContextHost::Function(self.fid));
        self.context_saves.push(self.ctx_save);
    }

    /// Leave the function-context frame, if the prologue pushed one.
    fn pop_frame_context(&mut self) {
        if self.frame_context {
            self.b.pop_context(self.ctx_save);
        }
    }

    /// Push a structural context, returning its RAII guard. Control-flow
    /// restores (`PopContext` with a saved context register) are not
    /// structural and continue to be emitted directly: they return to an
    /// existing context instead of removing one from the chain.
    fn context_scope(&mut self, save: Reg, host: ContextHost) -> ContextScope<'c, 'a, 'p> {
        self.b.push_context(save);
        self.contexts.push(host);
        self.context_saves.push(save);
        ContextScope {
            generator: self,
            save,
            host,
        }
    }

    fn enter_block_context(
        &mut self,
        scope: Option<ScopeId>,
        save: Reg,
    ) -> Option<ContextScope<'c, 'a, 'p>> {
        let scope = scope?;
        let locals = self.c.layouts[self.fid.0 as usize]
            .block_slots
            .get(&scope)?
            .clone();
        let idx = self.b.constant(Constant::ScopeInfo(ScopeLayout {
            locals: locals.into_boxed_slice(),
            flags: 0,
        }));
        self.b.create_function_context(idx);
        Some(self.context_scope(save, ContextHost::Block(scope)))
    }

    /// Drop-time half of [`Self::context_scope`].
    fn pop_context_checked(&mut self, save: Reg, host: ContextHost) {
        debug_assert_eq!(
            self.contexts.last(),
            Some(&host),
            "context guards must drop in LIFO order"
        );
        self.b.pop_context(save);
        self.contexts.pop().expect("context stack underflow");
        self.context_saves
            .pop()
            .expect("context save stack underflow");
    }

    /// Replace the guarded context with a fresh sibling. The logical chain
    /// entry stays the same; only the runtime context object changes.
    fn replace_context_block(
        &mut self,
        save: Reg,
        host: ContextHost,
        slots: u32,
        save_into: Option<Reg>,
    ) {
        debug_assert_eq!(self.contexts.last(), Some(&host));
        self.b.pop_context(save);
        self.b.create_block_context(slots);
        if let Some(reg) = save_into {
            self.b.store(reg);
        }
        self.b.push_context(save);
    }

    fn emit_this_initialized_check(&mut self) {
        self.b
            .call_runtime_staged(BuiltinFn::ThrowSuperNotCalledIfHole, &[RtArg::Acc]);
    }

    fn name_constant(&mut self, key: &PropertyKey<'_>) -> Result<ConstIdx, CompileError> {
        match key {
            PropertyKey::StaticIdentifier(i) => Ok(self.b.name(i.name.as_bytes())),
            PropertyKey::StringLiteral(s) => Ok(self.b.name(s.value.as_bytes())),
            _ => self.err(key.span(), "non-string property names"),
        }
    }

    /// ES 8.4.2 IsAnonymousFunctionDefinition.
    fn is_anon_function(&self, e: &Expression<'_>) -> bool {
        match e {
            Expression::FunctionExpression(f) => f.id.is_none(),
            Expression::ClassExpression(c) => c.id.is_none(),
            _ => false,
        }
    }

    fn emit_set_name_const(&mut self, bytes: &[u8]) {
        let idx = self.b.name(bytes);
        self.emit_set_name_by_const(idx);
    }

    fn emit_set_name_by_const(&mut self, idx: ConstIdx) {
        self.b.call_runtime_staged(
            BuiltinFn::SetFunctionName,
            &[RtArg::Acc, RtArg::Const(idx), RtArg::Smi(0)],
        );
    }

    fn emit_set_name_by_reg(&mut self, key: Reg, prefix: u32) {
        self.b.call_runtime_staged(
            BuiltinFn::SetFunctionName,
            &[RtArg::Acc, RtArg::Reg(key), RtArg::Smi(prefix)],
        );
    }

    fn emit_set_name_for_key_node(&mut self, key: &PropertyKey<'_>) {
        match key {
            PropertyKey::StaticIdentifier(i) => {
                self.emit_set_name_const(i.name.as_bytes());
            }
            PropertyKey::StringLiteral(s) => {
                self.emit_set_name_const(s.value.as_bytes());
            }
            _ => {}
        }
    }

    // -- variable access -------------------------------------------------------

    /// Load a resolved slot into the accumulator (hole checks included).
    fn load_slot(&mut self, slot: Slot, depth: u32) {
        match slot {
            Slot::Param { index, hole_check } => {
                let reg = self.b.param(index);
                self.b.load(reg);
                if hole_check {
                    self.b.throw_reference_error_if_hole();
                }
            }
            Slot::Local { reg, hole_check } => {
                self.b.load(Reg::new(reg as i32));
                if hole_check {
                    self.b.throw_reference_error_if_hole();
                }
            }
            Slot::Ctx { slot, hole_check } | Slot::CtxAt { slot, hole_check } => {
                self.b.load_context_slot(slot, depth);
                if hole_check {
                    self.b.throw_reference_error_if_hole();
                }
            }
            Slot::Global => unreachable!("global loads carry a name"),
            Slot::Repl => unreachable!("REPL globals load by name"),
        }
    }

    /// Store the accumulator into a resolved slot. `depth` is relative to
    /// the current frame context.
    fn store_slot(&mut self, slot: Slot, depth: u32) {
        match slot {
            Slot::Param { index, .. } => {
                let reg = self.b.param(index);
                self.b.store(reg);
            }
            Slot::Local { reg, .. } => self.b.store(Reg::new(reg as i32)),
            Slot::Ctx { slot, .. } | Slot::CtxAt { slot, .. } => {
                self.b.store_context_slot(slot, depth);
            }
            Slot::Global => unreachable!("global stores carry a name"),
            Slot::Repl => unreachable!("REPL globals store by name"),
        }
    }

    /// Depth of a declared symbol's hosting context from the current frame
    /// context. Declaration stores execute wherever the declaration appears,
    /// which may be inside an enclosing for-head / class / object-home
    /// context, so this must come from the active chain rather than assuming
    /// zero hops.
    fn decl_depth(&self, sym: SymbolId, slot: Slot) -> u32 {
        let host = match slot {
            // Ctx slots are allocated in the current function's frame
            // context (eagerly, at its prologue).
            Slot::Ctx { .. } => ContextHost::Function(self.fid),
            // CtxAt slots belong to the class / for-head scope that declared
            // the binding.
            Slot::CtxAt { .. } => self.c.context_host_of(sym),
            _ => return 0,
        };
        self.contexts
            .iter()
            .rev()
            .position(|&ctx| ctx == host)
            .unwrap_or_else(|| {
                panic!(
                    "declaration host {host:?} is not on the active context chain {:?}",
                    self.contexts
                )
            }) as u32
    }

    /// Store the accumulator into a symbol's declared slot.
    fn store_symbol(&mut self, sym: SymbolId, name: &str) -> Result<(), CompileError> {
        let slot = self.c.slot_of(sym);
        if matches!(slot, Slot::Global) {
            let idx = self.b.name(name.as_bytes());
            let feedback = self.b.new_feedback();
            self.b.store_global(idx, feedback);
            return Ok(());
        }
        if matches!(slot, Slot::Repl) {
            let idx = self.b.name(name.as_bytes());
            self.b.call_runtime_staged(
                BuiltinFn::StoreReplGlobalInit,
                &[RtArg::Acc, RtArg::Const(idx)],
            );
            return Ok(());
        }
        let depth = self.decl_depth(sym, slot);
        self.store_slot(slot, depth);
        Ok(())
    }

    /// Store a declaration's initial value.
    fn store_declaration(&mut self, sym: SymbolId, name: &str) -> Result<(), CompileError> {
        self.store_symbol(sym, name)
    }

    fn ident_symbol_flags(&self, ident: &IdentifierReference<'_>) -> Option<SymbolFlags> {
        let rid = ident.reference_id.get()?;
        let sym = self.c.scoping.get_reference(rid).symbol_id()?;
        Some(self.c.scoping.symbol_flags(sym))
    }

    /// A named function expression's own name is an immutable binding.
    fn is_immutable_ident(&self, ident: &IdentifierReference<'_>) -> bool {
        self.ident_symbol_flags(ident).is_some_and(|flags| {
            flags.intersects(SymbolFlags::ConstVariable | SymbolFlags::FunctionExpression)
        })
    }

    /// Store the accumulator into an identifier assignment target.
    fn store_name(&mut self, ident: &IdentifierReference<'_>) -> Result<(), CompileError> {
        let flags = self.ident_symbol_flags(ident);
        let is_const = flags.is_some_and(|f| f.contains(SymbolFlags::ConstVariable));
        let is_nfe = flags.is_some_and(|f| f.contains(SymbolFlags::FunctionExpression));
        // Sloppy assignment to a named function expression's own name is
        // silently ignored; strict mode throws like a const assignment.
        if is_nfe && !self.c.facts.functions[self.fid.0 as usize].strict {
            return Ok(());
        }
        let is_immutable = is_const || is_nfe;
        match self.identifier_resolution(ident)? {
            IdRes::Global => {
                if is_immutable {
                    self.b.call_runtime_staged(BuiltinFn::ThrowConstAssign, &[]);
                } else {
                    let idx = self.b.name(ident.name.as_bytes());
                    let feedback = self.b.new_feedback();
                    self.b.store_global(idx, feedback);
                }
            }
            IdRes::Dynamic => {
                let idx = self.b.name(ident.name.as_bytes());
                self.b.call_runtime_staged(
                    BuiltinFn::StoreDynamicName,
                    &[RtArg::Acc, RtArg::Const(idx)],
                );
            }
            IdRes::Slot(slot, depth) => {
                if is_immutable {
                    self.load_slot(slot, depth);
                    self.b.call_runtime_staged(BuiltinFn::ThrowConstAssign, &[]);
                } else {
                    self.store_slot(slot, depth);
                }
            }
        }
        Ok(())
    }

    fn emit_identifier(&mut self, ident: &IdentifierReference<'_>) -> Result<(), CompileError> {
        match self.identifier_resolution(ident)? {
            IdRes::Global => {
                let idx = self.b.name(ident.name.as_bytes());
                let feedback = self.b.new_feedback();
                self.b.load_global(idx, feedback);
            }
            IdRes::Dynamic => {
                let idx = self.b.name(ident.name.as_bytes());
                self.b
                    .call_runtime_staged(BuiltinFn::LoadDynamicName, &[RtArg::Const(idx)]);
            }
            IdRes::Slot(slot, depth) => self.load_slot(slot, depth),
        }
        Ok(())
    }

    /// Whether an identifier resolves to a plain global-object reference
    /// (the `typeof` no-throw path).
    fn resolves_global(&mut self, ident: &IdentifierReference<'_>) -> bool {
        matches!(self.identifier_resolution(ident), Ok(IdRes::Global))
    }

    /// Whether `sym` is a top-level `var` in a sloppy direct-eval program.
    /// Such declarations belong to the caller's variable environment, not
    /// the eval frame.
    fn is_eval_dynamic_var(&self, sym: SymbolId) -> bool {
        if !matches!(self.c.facts.mode, Mode::Eval | Mode::EvalGlobal) || self.fid != Fid(0) {
            return false;
        }
        // Strict eval keeps its vars local; only sloppy direct eval extends
        // the caller's variable environment.
        if self.c.facts.functions[self.fid.0 as usize].strict {
            return false;
        }
        let flags = self.c.scoping.symbol_flags(sym);
        self.c.scoping.symbol_scope_id(sym) == self.c.facts.root
            && flags.intersects(SymbolFlags::FunctionScopedVariable | SymbolFlags::Function)
    }

    /// Whether the reference's enclosing scope chain contains a direct eval.
    /// oxc propagates `DirectEval` to ancestor scopes; a sloppy eval there may
    /// introduce names that no static binding resolved.
    fn scope_has_direct_eval(&self, scope: ScopeId) -> bool {
        let mut current = Some(scope);
        while let Some(scope) = current {
            if self
                .c
                .scoping
                .scope_flags(scope)
                .contains(ScopeFlags::DirectEval)
            {
                return true;
            }
            current = self.c.scoping.scope_parent_id(scope);
        }
        false
    }

    /// The function owning a resolved symbol's declaration scope.
    fn symbol_owner_fn(&self, sym: SymbolId) -> Option<Fid> {
        let mut current = Some(self.c.scoping.symbol_scope_id(sym));
        while let Some(scope) = current {
            if let Some(&fid) = self.c.facts.fn_scope_to_fid.get(&scope) {
                return Some(fid);
            }
            current = self.c.scoping.scope_parent_id(scope);
        }
        None
    }

    /// A static binding outside an eval-extendable function is not stable:
    /// the eval may add a same-named binding in the eval function's
    /// environment, which shadows the outer one for code inside it.
    fn eval_may_shadow(&self, sym: SymbolId, reference_scope: ScopeId) -> bool {
        if !self.c.any_sloppy_eval {
            return false;
        }
        let Some(owner) = self.symbol_owner_fn(sym) else {
            return false;
        };
        let mut current = Some(reference_scope);
        while let Some(scope) = current {
            if let Some(&fid) = self.c.facts.fn_scope_to_fid.get(&scope) {
                // Reached the declaration function: within its own
                // environment a var redeclaration updates the same slot.
                if fid == owner {
                    return false;
                }
                if !self.c.facts.functions[fid.0 as usize].strict
                    && self
                        .c
                        .scoping
                        .scope_flags(scope)
                        .contains(ScopeFlags::DirectEval)
                {
                    return true;
                }
            }
            current = self.c.scoping.scope_parent_id(scope);
        }
        false
    }

    fn identifier_resolution(
        &mut self,
        ident: &IdentifierReference<'_>,
    ) -> Result<IdRes, CompileError> {
        let Some(rid) = ident.reference_id.get() else {
            return Err(CompileError::new(ident.span, "unresolved identifier"));
        };
        let reference = self.c.scoping.get_reference(rid);
        match reference.symbol_id() {
            None => {
                let dynamic = matches!(
                    self.c.facts.mode,
                    Mode::Eval | Mode::EvalStrict | Mode::EvalGlobal
                ) || self.scope_has_direct_eval(reference.scope_id());
                Ok(if dynamic {
                    IdRes::Dynamic
                } else {
                    IdRes::Global
                })
            }
            Some(sym) => {
                if self.is_eval_dynamic_var(sym) || self.eval_may_shadow(sym, reference.scope_id())
                {
                    return Ok(IdRes::Dynamic);
                }
                let skip_hole = self.c.facts.ref_skip_hole.contains(&rid);
                let check = |hole: bool| hole && !skip_hole;
                let slot = self.c.slot_of(sym);
                match slot {
                    Slot::Global | Slot::Repl => Ok(IdRes::Global),
                    // registers carry no depth
                    Slot::Param { index, hole_check } => Ok(IdRes::Slot(
                        Slot::Param {
                            index,
                            hole_check: check(hole_check),
                        },
                        0,
                    )),
                    Slot::Local { reg, hole_check } => Ok(IdRes::Slot(
                        Slot::Local {
                            reg,
                            hole_check: check(hole_check),
                        },
                        0,
                    )),
                    Slot::CtxAt { slot, hole_check } | Slot::Ctx { slot, hole_check } => {
                        // depths are precomputed over node ancestry
                        // (synthesized field-initializer frames count);
                        // the scope-tree walk is only a fallback
                        let depth =
                            self.c
                                .facts
                                .ref_depth
                                .get(&rid)
                                .copied()
                                .unwrap_or_else(|| {
                                    let decl_scope = self.c.scoping.symbol_scope_id(sym);
                                    self.c.depth_to(reference.scope_id(), decl_scope)
                                });
                        Ok(IdRes::Slot(
                            Slot::Ctx {
                                slot,
                                hole_check: check(hole_check),
                            },
                            depth,
                        ))
                    }
                }
            }
        }
    }

    fn emit_not(&mut self) {
        let is_truthy = self.b.new_label();
        self.b.jump_if_truthy(is_truthy);
        self.b.load_true();
        let end = self.b.new_label();
        self.b.jump(end);
        self.b.bind(is_truthy);
        self.b.load_false();
        self.b.bind(end);
    }

    /// Load a private-name symbol from its class-context slot.
    fn emit_private_key_load(&mut self, node: NodeId, span: Span) -> Result<(), CompileError> {
        match self.c.facts.special.get(&node) {
            Some(Special::Private { slot, depth }) => {
                self.b.load_context_slot(*slot, *depth);
                Ok(())
            }
            _ => self.err(span, "private name outside its class"),
        }
    }

    /// Load `this` of `owner`. Inside a derived constructor the value may
    /// still be the hole: every access must throw "super not called".
    fn emit_this_for(&mut self, owner: Fid, depth: u32) {
        if owner != self.fid {
            let slot = self.c.layouts[owner.0 as usize]
                .this_slot
                .expect("owner captures this");
            self.b.load_context_slot(slot, depth);
        } else if let Some(slot) = self.c.layouts[self.fid.0 as usize].this_slot {
            self.b.load_context_slot(slot, 0);
        } else {
            self.b.load(self.b.this_reg());
        }
        if self.c.facts.functions[owner.0 as usize]
            .kind
            .is_derived_class_constructor()
        {
            self.emit_this_initialized_check();
        }
    }

    // -- expressions ------------------------------------------------------------

    fn expr(&mut self, e: &Expression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        match e {
            Expression::NumericLiteral(n) => {
                // (2^53 − 1: largest exactly-representable integer)
                const MAX_EXACT_INT: f64 = 9007199254740991.0;
                let f = n.value;
                let is_int = f.fract() == 0.0 && !(f == 0.0 && f.is_sign_negative());
                if is_int && f >= i16::MIN as f64 && f <= i16::MAX as f64 {
                    self.b.load_smi(f as i32);
                } else if is_int && f.abs() <= MAX_EXACT_INT {
                    let c = self.b.constant(Constant::Smi(f as i64));
                    self.b.load_constant(c);
                } else {
                    let c = self.b.constant(Constant::Float(f));
                    self.b.load_constant(c);
                }
                Ok(())
            }
            Expression::StringLiteral(s) => {
                let c = self.b.constant(Constant::String(s.value.as_bytes().into()));
                self.b.load_constant(c);
                Ok(())
            }
            Expression::BigIntLiteral(_) => self.err(e.span(), "BigInt literals"),
            Expression::RegExpLiteral(_) => self.err(e.span(), "regular expressions"),
            Expression::BooleanLiteral(b) => {
                if b.value {
                    self.b.load_true();
                } else {
                    self.b.load_false();
                }
                Ok(())
            }
            Expression::NullLiteral(_) => {
                self.b.load_null();
                Ok(())
            }
            Expression::Identifier(i) => self.emit_identifier(i),
            Expression::ThisExpression(t) => {
                if self.fid == Fid(0) && !matches!(self.c.facts.mode, Mode::Eval | Mode::EvalStrict)
                {
                    let idx = self.b.constant(Constant::GlobalObject);
                    self.b.load_constant(idx);
                } else {
                    match self.c.facts.special.get(&t.node_id.get()) {
                        Some(Special::This { owner, depth }) => self.emit_this_for(*owner, *depth),
                        _ => self.emit_this_for(self.fid, 0),
                    }
                }
                Ok(())
            }
            Expression::UnaryExpression(u) => self.emit_unary(u),
            Expression::UpdateExpression(u) => self.emit_update(u),
            Expression::BinaryExpression(b) => self.emit_binary(b),
            Expression::LogicalExpression(l) => self.emit_logical(l),
            Expression::ConditionalExpression(c) => {
                self.expr(&c.test)?;
                let else_l = self.b.new_label();
                self.b.jump_if_falsy(else_l);
                self.expr(&c.consequent)?;
                let end = self.b.new_label();
                self.b.jump(end);
                self.b.bind(else_l);
                self.expr(&c.alternate)?;
                self.b.bind(end);
                Ok(())
            }
            Expression::AssignmentExpression(a) => self.emit_assign(a),
            Expression::SequenceExpression(s) => {
                let [rest @ .., last] = s.expressions.as_slice() else {
                    return self.err(s.span, "empty sequence");
                };
                for x in rest {
                    self.expr(x)?;
                }
                self.expr(last)
            }
            Expression::CallExpression(c) => self.emit_call(c),
            Expression::NewExpression(n) => self.emit_new(n),
            Expression::StaticMemberExpression(m) => self.emit_property_load(MemberRef::Static(m)),
            Expression::ComputedMemberExpression(m) => {
                self.emit_property_load(MemberRef::Computed(m))
            }
            Expression::PrivateFieldExpression(m) => self.emit_property_load(MemberRef::Private(m)),
            Expression::ArrayExpression(a) => self.emit_array_literal(a),
            Expression::ObjectExpression(o) => self.emit_object_literal(o),
            Expression::FunctionExpression(f) => {
                let fid = self.c.facts.fn_of_node[&f.node_id.get()];
                let idx = self.b.constant(Constant::Callable(FunctionId(fid.0)));
                self.b.create_closure(idx);
                Ok(())
            }
            Expression::ArrowFunctionExpression(f) => {
                let fid = self.c.facts.fn_of_node[&f.node_id.get()];
                let idx = self.b.constant(Constant::Callable(FunctionId(fid.0)));
                self.b.create_closure(idx);
                Ok(())
            }
            Expression::ClassExpression(c) => {
                let idx = self.c.facts.class_of_node[&c.node_id.get()];
                self.emit_class(idx)
            }
            Expression::NewTarget(m) => {
                match self.c.facts.special.get(&m.node_id.get()) {
                    Some(Special::NewTarget { owner, depth }) if *owner != self.fid => {
                        let slot = self.c.layouts[owner.0 as usize]
                            .new_target_slot
                            .expect("arrow new.target forces the slot");
                        self.b.load_context_slot(slot, *depth);
                    }
                    _ => {
                        self.b.load_new_target();
                    }
                }
                Ok(())
            }
            Expression::ImportMeta(_) => self.err(e.span(), "import.meta"),
            Expression::PrivateInExpression(p) => {
                // `#x in obj`: (key, obj) -> bool — key rides the top slot
                let base = temps.reserve(2);
                self.emit_private_key_load(p.left.node_id.get(), p.left.span)?;
                self.b.store(Reg::new(base.index() + 1));
                self.expr(&p.right)?;
                self.b.store(base);
                self.b
                    .call_runtime(BuiltinFn::PrivateIn, RegList::new(base, 2));
                Ok(())
            }
            Expression::ParenthesizedExpression(_) => {
                // parse options drop parens; defensive
                self.err(e.span(), "parenthesized expressions")
            }
            Expression::ChainExpression(_) => self.err(e.span(), "optional chaining"),
            Expression::TemplateLiteral(_) => self.err(e.span(), "template literals"),
            Expression::TaggedTemplateExpression(_) => self.err(e.span(), "template literals"),
            Expression::AwaitExpression(_) => self.err(e.span(), "await expressions"),
            Expression::YieldExpression(_) => self.err(e.span(), "generator functions"),
            Expression::ImportExpression(_) => self.err(e.span(), "dynamic import"),
            Expression::Super(_) => self.err(e.span(), "super outside a call or member"),
            _ => self.err(e.span(), "expression"),
        }
    }

    fn emit_unary(&mut self, u: &UnaryExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        match u.operator {
            UnaryOperator::LogicalNot => {
                self.expr(&u.argument)?;
                self.emit_not();
                Ok(())
            }
            UnaryOperator::UnaryNegation => {
                self.expr(&u.argument)?;
                self.b.negate();
                Ok(())
            }
            UnaryOperator::UnaryPlus => {
                // ToNumber(arg) via `arg - 0`
                self.expr(&u.argument)?;
                let arg = self.b.stage_acc(&temps);
                self.b.load_zero();
                self.b.raw(Opcode::Sub, &[arg.operand()]);
                Ok(())
            }
            UnaryOperator::Typeof => {
                // `typeof` on an unresolved global yields "undefined"
                // instead of throwing
                if let Expression::Identifier(i) = &u.argument
                    && self.resolves_global(i)
                {
                    let idx = self.b.name(i.name.as_bytes());
                    let feedback = self.b.new_feedback();
                    self.b.load_global_no_throw(idx, feedback);
                    self.b.test_typeof();
                    return Ok(());
                }
                self.expr(&u.argument)?;
                self.b.test_typeof();
                Ok(())
            }
            UnaryOperator::Void => {
                self.expr(&u.argument)?;
                self.b.load_undefined();
                Ok(())
            }
            UnaryOperator::BitwiseNot => self.err(u.span, "bitwise not"),
            UnaryOperator::Delete => self.emit_delete(u),
        }
    }

    fn emit_delete(&mut self, u: &UnaryExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        match &u.argument {
            e if MemberRef::of(e).is_some_and(is_super_member) => {
                let m = MemberRef::of(e).unwrap();
                // ReferenceError in both language modes (ES 13.5.1.2 step
                // 4.c); reference evaluation runs first
                let Some(store) = self.prepare_super_parts(m, &temps)? else {
                    return self.err(u.span, "super property");
                };
                drop(store);
                self.b.call_runtime(
                    BuiltinFn::DeleteSuperProperty,
                    RegList::new(self.b.this_reg(), 0),
                );
                Ok(())
            }
            e if MemberRef::of(e).is_some() => {
                let m = MemberRef::of(e).unwrap();
                if let MemberRef::Private(p) = m {
                    return self.err(p.span, "delete of a private name");
                }
                let object = match m {
                    MemberRef::Static(s) => &s.object,
                    MemberRef::Computed(c) => &c.object,
                    MemberRef::Private(_) => unreachable!(),
                };
                self.expr(object)?;
                let base = temps.reserve(2);
                self.b.store(Reg::new(base.index() + 1));
                match m {
                    MemberRef::Static(s) => {
                        let idx = self.b.name(s.property.name.as_bytes());
                        self.b.load_constant(idx);
                    }
                    MemberRef::Computed(c) => {
                        self.expr(&c.expression)?;
                    }
                    MemberRef::Private(_) => unreachable!(),
                }
                self.b.store(base);
                // one opcode, one handler: sloppy returns false on a
                // refused delete, strict throws
                self.b.delete_property(RegList::new(base, 2));
                Ok(())
            }
            Expression::Identifier(i) => {
                // strict sites are rejected at parse time; sloppy
                // declarative bindings cannot be deleted (false), free
                // names are global-object properties
                match self.identifier_resolution(i)? {
                    IdRes::Global | IdRes::Dynamic => {
                        let idx = self.b.name(i.name.as_bytes());
                        self.b.load_constant(idx);
                        let name = self.b.stage_acc(&temps);
                        self.b
                            .call_runtime(BuiltinFn::DeleteIdentifierSloppy, RegList::new(name, 1));
                    }
                    _ => self.b.load_false(),
                }
                Ok(())
            }
            _ => {
                // not a reference: side effects only, the result is true
                self.expr(&u.argument)?;
                self.b.load_true();
                Ok(())
            }
        }
    }

    fn emit_binary(&mut self, b: &BinaryExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        use BinaryOperator as Op;
        match b.operator {
            Op::StrictInequality => {
                self.binary_arith(&b.left, &b.right, Opcode::EqualStrict)?;
                self.emit_not();
                Ok(())
            }
            Op::Inequality => {
                self.binary_arith(&b.left, &b.right, Opcode::Equal)?;
                self.emit_not();
                Ok(())
            }
            Op::Instanceof => {
                // acc = lhs instanceof rhs: evaluate rhs first
                self.expr(&b.right)?;
                let r = self.b.stage_acc(&temps);
                self.expr(&b.left)?;
                self.b.instance_of(r);
                Ok(())
            }
            Op::In => {
                // `key in obj`: (key, obj) -> bool — key rides the top slot
                let base = temps.reserve(2);
                self.expr(&b.left)?;
                self.b.store(Reg::new(base.index() + 1));
                self.expr(&b.right)?;
                self.b.store(base);
                self.b
                    .call_runtime(BuiltinFn::HasProperty, RegList::new(base, 2));
                Ok(())
            }
            _ => {
                let op = match b.operator {
                    Op::Addition => Opcode::Add,
                    Op::Subtraction => Opcode::Sub,
                    Op::Multiplication => Opcode::Mul,
                    Op::Division => Opcode::Div,
                    Op::Remainder => Opcode::Mod,
                    Op::Exponential => Opcode::Exp,
                    Op::BitwiseOR => Opcode::BitwiseOr,
                    Op::BitwiseXOR => Opcode::BitwiseXor,
                    Op::BitwiseAnd => Opcode::BitwiseAnd,
                    Op::ShiftLeft => Opcode::ShiftLeft,
                    Op::ShiftRight => Opcode::ShiftRight,
                    Op::ShiftRightZeroFill => Opcode::ShiftRightLogical,
                    Op::StrictEquality => Opcode::EqualStrict,
                    Op::Equality => Opcode::Equal,
                    Op::LessThan => Opcode::LessThan,
                    Op::GreaterThan => Opcode::GreaterThan,
                    Op::LessEqualThan => Opcode::LessThanOrEqual,
                    Op::GreaterEqualThan => Opcode::GreaterThanOrEqual,
                    _ => return self.err(b.span, "binary operator"),
                };
                self.binary_arith(&b.left, &b.right, op)
            }
        }
    }

    fn emit_logical(&mut self, l: &LogicalExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let truthy = match l.operator {
            LogicalOperator::And => false,
            LogicalOperator::Or => true,
            LogicalOperator::Coalesce => return self.err(l.span, "nullish coalescing"),
        };
        self.expr(&l.left)?;
        let t = self.b.stage_acc(&temps);
        let short = self.b.new_label();
        if truthy {
            self.b.jump_if_truthy(short);
        } else {
            self.b.jump_if_falsy(short);
        }
        self.expr(&l.right)?;
        let end = self.b.new_label();
        self.b.jump(end);
        self.b.bind(short);
        self.b.load(t);
        self.b.bind(end);
        Ok(())
    }

    /// The register behind a plain local/parameter operand whose read is
    /// side-effect free (no TDZ check), if `e` is one.
    fn simple_register(&mut self, e: &Expression<'_>) -> Option<Reg> {
        let Expression::Identifier(ident) = e else {
            return None;
        };
        self.simple_identifier_register(ident)
    }

    /// The receiver operand of the current frame (`this`, operand 0) when
    /// `e` is a `this` expression that resolves to this frame's receiver.
    /// `None` for arrows (lexical `this`), `super`-captured `this`, and
    /// derived constructors (whose `this` load carries an initialization
    /// check that must not be elided).
    fn this_operand_reg(&self, e: &Expression<'_>) -> Option<Reg> {
        let Expression::ThisExpression(t) = e else {
            return None;
        };
        if self.fid == Fid(0) && !matches!(self.c.facts.mode, Mode::Eval | Mode::EvalStrict) {
            return None;
        }
        if let Some(Special::This { owner, .. }) = self.c.facts.special.get(&t.node_id.get())
            && *owner != self.fid
        {
            return None;
        }
        if self.c.facts.functions[self.fid.0 as usize]
            .kind
            .is_derived_class_constructor()
        {
            return None;
        }
        Some(self.b.this_reg())
    }

    /// A register whose value is stable while sibling expressions (call
    /// arguments, an assignment's right-hand side) evaluate: the frame
    /// receiver, or a local/parameter this function never rebinds.
    /// Captured bindings and eval-scoped functions live in contexts, so a
    /// callee can only clobber such a binding through a direct assignment
    /// in this function — exactly what `symbol_is_mutated` detects.
    fn stable_register(&mut self, e: &Expression<'_>) -> Option<Reg> {
        if let Some(reg) = self.this_operand_reg(e) {
            return Some(reg);
        }
        let Expression::Identifier(ident) = e else {
            return None;
        };
        let rid = ident.reference_id.get()?;
        let sym = self.c.scoping.get_reference(rid).symbol_id()?;
        if self.c.scoping.symbol_is_mutated(sym) {
            return None;
        }
        self.simple_identifier_register(ident)
    }

    fn simple_identifier_register(&mut self, ident: &IdentifierReference<'_>) -> Option<Reg> {
        match self.identifier_resolution(ident).ok()? {
            IdRes::Slot(
                Slot::Param {
                    index,
                    hole_check: false,
                },
                0,
            ) => Some(self.b.param(index)),
            IdRes::Slot(
                Slot::Local {
                    reg,
                    hole_check: false,
                },
                0,
            ) => Some(Reg::new(reg as i32)),
            _ => None,
        }
    }

    fn binary_arith(
        &mut self,
        lhs: &Expression<'_>,
        rhs: &Expression<'_>,
        op: Opcode,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        if let Some(imm_op) = immediate_variant(op) {
            // `reg OP imm`
            if let Some(l) = self.simple_register(lhs)
                && let Some(imm) = int_literal(rhs)
            {
                self.b.raw(imm_op, &[l.operand(), imm as u32]);
                return Ok(());
            }
            // `lhs` is already a register: keep it as the register operand
            // and evaluate `rhs` into the accumulator. Evaluation order
            // (lhs then rhs) is preserved because a simple register has no
            // side effects.
            if let Some(l) = self.simple_register(lhs)
                && rhs_cannot_clobber_locals(rhs)
            {
                self.expr(rhs)?;
                self.b.raw(op, &[l.operand()]);
                return Ok(());
            }
            // `imm OP reg` for a commutative operator == `reg OP imm`
            if is_commutative(op)
                && let Some(imm) = int_literal(lhs)
                && let Some(r) = self.simple_register(rhs)
            {
                self.b.raw(imm_op, &[r.operand(), imm as u32]);
                return Ok(());
            }
            // complex `lhs`, immediate `rhs`: stage lhs, then `reg OP imm`
            if let Some(imm) = int_literal(rhs) {
                self.expr(lhs)?;
                let t = self.b.stage_acc(&temps);
                self.b.raw(imm_op, &[t.operand(), imm as u32]);
                return Ok(());
            }
            // general: `lhs` -> temp register, `rhs` -> accumulator
            self.expr(lhs)?;
            let t = self.b.stage_acc(&temps);
            self.expr(rhs)?;
            self.b.raw(op, &[t.operand()]);
            return Ok(());
        }
        // non-arithmetic operator (`acc OP reg`): comparisons/tests. When
        // the right operand is a plain register there is nothing to stage.
        if let Some(r) = self.simple_register(rhs) {
            self.expr(lhs)?;
            self.b.raw(op, &[r.operand()]);
            return Ok(());
        }
        self.expr(lhs)?;
        let a = self.b.stage_acc(&temps);
        self.expr(rhs)?;
        let b = self.b.stage_acc(&temps);
        self.b.load(a);
        self.b.raw(op, &[b.operand()]);
        Ok(())
    }

    fn emit_assign(&mut self, a: &AssignmentExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        if a.operator.is_assign()
            && matches!(
                &a.left,
                AssignmentTarget::ArrayAssignmentTarget(_)
                    | AssignmentTarget::ObjectAssignmentTarget(_)
            )
        {
            // destructuring assignment (ES 14.13.3): the RHS value, then
            // the pattern against it; the expression evaluates to the value
            self.expr(&a.right)?;
            let v = self.b.stage_acc(&temps);
            self.emit_assign_target_pattern(&a.left, v)?;
            self.b.load(v);
            return Ok(());
        }
        if a.operator.is_logical() {
            return self.err(a.span, "logical assignment");
        }
        if a.operator.is_assign() {
            self.emit_simple_assign(&a.left, &a.right)
        } else {
            let op = a
                .operator
                .to_binary_operator()
                .and_then(arith_opcode)
                .ok_or_else(|| CompileError::new(a.span, "assignment operator"))?;
            self.emit_compound_assign(&a.left, &a.right, op)
        }
    }

    fn emit_simple_assign(
        &mut self,
        target: &AssignmentTarget<'_>,
        value: &Expression<'_>,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(i) => {
                if let Some(dst) = self.simple_identifier_register(i)
                    && let Expression::BinaryExpression(b) = value
                    && matches!(
                        b.operator,
                        BinaryOperator::Addition | BinaryOperator::Subtraction
                    )
                    && let Expression::Identifier(lhs) = &b.left
                    && !self.is_immutable_ident(i)
                    && self.simple_identifier_register(lhs).map(|r| r.operand())
                        == Some(dst.operand())
                    && let Some(src) = self.simple_register(&b.right)
                {
                    if b.operator == BinaryOperator::Addition {
                        self.b.add_loc(dst, src);
                    } else {
                        self.b.sub_loc(dst, src);
                    }
                    return Ok(());
                }
                self.expr(value)?;
                if self.is_anon_function(value) {
                    self.emit_set_name_const(i.name.as_bytes());
                }
                self.store_name(i)
            }
            t if assign_member_ref(t).is_some() => {
                let m = assign_member_ref(t).unwrap();
                if is_this_member(m) {
                    self.b.note_this_property();
                }
                let store = if is_super_member(m) {
                    self.prepare_super_store(m, &temps)?
                } else {
                    self.prepare_property_store(m)?
                };
                self.expr(value)?;
                // NamedEvaluation: `a.b = function () {}` names the closure "b"
                if let StoreTarget::Named { name_idx, .. } = &store
                    && self.is_anon_function(value)
                {
                    let name_idx = *name_idx;
                    self.emit_set_name_by_const(name_idx);
                }
                self.emit_property_store(&store);
                drop(store);
                Ok(())
            }
            _ => self.err(target.span(), "assignment target"),
        }
    }

    fn emit_compound_assign(
        &mut self,
        target: &AssignmentTarget<'_>,
        value: &Expression<'_>,
        op: Opcode,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(i) => {
                if let (Some(dst), Some(src)) = (
                    self.simple_identifier_register(i),
                    self.simple_register(value),
                ) && !self.is_immutable_ident(i)
                {
                    match op {
                        Opcode::Add => {
                            self.b.add_loc(dst, src);
                            return Ok(());
                        }
                        Opcode::Sub => {
                            self.b.sub_loc(dst, src);
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                self.emit_identifier(i)?;
                let orig = self.b.stage_acc(&temps);
                self.expr(value)?;
                self.b.raw(op, &[orig.operand()]);
                self.store_name(i)?;
                Ok(())
            }
            t if assign_member_ref(t).is_some() => {
                let m = assign_member_ref(t).unwrap();
                let store = if is_super_member(m) {
                    self.prepare_super_store(m, &temps)?
                } else {
                    self.prepare_property_store(m)?
                };
                self.emit_property_load_of(&store);
                let orig = self.b.stage_acc(&temps);
                self.expr(value)?;
                self.b.raw(op, &[orig.operand()]);
                self.emit_property_store(&store);
                drop(store);
                Ok(())
            }
            _ => self.err(target.span(), "assignment target"),
        }
    }

    /// With the old value staged in `orig` and the step in `d`: replace
    /// the accumulator with `ToNumber(orig) ± d` (`orig - 0` performs the
    /// ToNumber, ES 13.5.6.2).
    fn emit_add_delta(&mut self, arith: Opcode, orig: Reg, d: Reg) {
        let temps = self.b.temp_scope();
        // acc = ToNumber(orig) OP d
        self.b.load(orig);
        let t = self.b.stage_acc(&temps);
        self.b.load_zero();
        self.b.raw(Opcode::Sub, &[t.operand()]);
        let lhs = self.b.stage_acc(&temps);
        self.b.load(d);
        self.b.raw(arith, &[lhs.operand()]);
    }

    fn emit_update(&mut self, u: &UpdateExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let delta: i32 = match u.operator {
            UpdateOperator::Increment => 1,
            UpdateOperator::Decrement => -1,
        };
        let arith = if delta > 0 { Opcode::Add } else { Opcode::Sub };
        let delta = delta.unsigned_abs();

        if let SimpleAssignmentTarget::AssignmentTargetIdentifier(i) = &u.argument {
            // a plain local updates in place: `IncLoc`/`DecLoc` do the
            // ToNumeric + step and leave the numeric old value in acc
            if !self.is_immutable_ident(i)
                && let Some(reg) = self.simple_identifier_register(i)
            {
                let op = match u.operator {
                    UpdateOperator::Increment => Opcode::IncLoc,
                    UpdateOperator::Decrement => Opcode::DecLoc,
                };
                self.b.raw(op, &[reg.operand()]);
                if u.prefix {
                    self.b.load(reg);
                }
                return Ok(());
            }
            self.emit_identifier(i)?;
            let orig = self.b.stage_acc(&temps);
            self.b.load_smi(delta as i32);
            let d = self.b.stage_acc(&temps);
            self.emit_add_delta(arith, orig, d);
            self.store_name(i)?;
            if !u.prefix {
                self.b.load(orig);
            }
            return Ok(());
        }
        if let Some(m) = simple_member_ref(&u.argument) {
            let store = if is_super_member(m) {
                self.prepare_super_store(m, &temps)?
            } else {
                self.prepare_property_store(m)?
            };
            self.emit_property_load_of(&store);
            let orig = self.b.stage_acc(&temps);
            self.b.load_smi(delta as i32);
            let d = self.b.stage_acc(&temps);
            self.emit_add_delta(arith, orig, d);
            self.emit_property_store(&store);
            if !u.prefix {
                self.b.load(orig);
            }
            drop(store);
            return Ok(());
        }
        self.err(u.span, "update target")
    }
}

// ---------------------------------------------------------------------------
// destructuring, property stores, calls, classes, literals
// ---------------------------------------------------------------------------

impl<'c, 'a, 'p> FunctionGen<'c, 'a, 'p> {
    fn emit_binding_pattern(&mut self, p: &BindingPattern<'_>, v: Reg) -> Result<(), CompileError> {
        match p {
            BindingPattern::ObjectPattern(o) => {
                self.emit_object_pattern(object_binding_props(o), v, true)
            }
            BindingPattern::ArrayPattern(a) => {
                self.emit_array_pattern(array_binding_elements(a), v, true)
            }
            _ => self.err(p.span(), "destructuring pattern"),
        }
    }

    fn emit_assign_target_pattern(
        &mut self,
        t: &AssignmentTarget<'_>,
        v: Reg,
    ) -> Result<(), CompileError> {
        match t {
            AssignmentTarget::ArrayAssignmentTarget(a) => {
                self.emit_array_pattern(array_assign_elements(a), v, false)
            }
            AssignmentTarget::ObjectAssignmentTarget(o) => {
                self.emit_object_pattern(object_assign_props(o), v, false)
            }
            _ => self.err(t.span(), "destructuring pattern"),
        }
    }

    /// `{a, b: c = 1, ...rest}` (ES 14.13).
    fn emit_object_pattern(
        &mut self,
        props: Vec<PatProperty<'_, '_>>,
        value_reg: Reg,
        binding: bool,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        // RequireObjectCoercible runs even for the empty pattern
        self.b.call_runtime(
            BuiltinFn::RequireObjectCoercible,
            RegList::new(value_reg, 1),
        );
        self.b.store(value_reg);
        let has_rest = matches!(props.last(), Some(PatProperty::Rest(_)));
        // with a rest property, every earlier key stays live in contiguous
        // registers as the CopyDataProperties exclusion set
        let mut excl_base: Option<Reg> = None;
        let mut excluded = 0u32;
        for prop in &props {
            // With a rest target, keys are staged in the outer scope because
            // the exclusion set must outlive the property. Without one, all
            // per-property temps can be released each iteration.
            let prop_temps = (!has_rest).then(|| self.b.temp_scope());
            match prop {
                PatProperty::Named {
                    name,
                    target,
                    default,
                } => {
                    let name_idx = self.b.name(name);
                    if has_rest {
                        self.b.load_constant(name_idx);
                        let r = self.b.stage_acc(&temps);
                        excl_base.get_or_insert(r);
                        excluded += 1;
                    }
                    let feedback = self.b.new_feedback();
                    self.b.load_named_property(value_reg, name_idx, feedback);
                    self.emit_pattern_element(
                        *target,
                        *default,
                        binding,
                        NameHint::Const(name_idx),
                    )?;
                }
                PatProperty::Rest(target) => {
                    // native layout: (excluded..., target, source) — element
                    // 0 (the first excluded key) rides the top slot
                    self.b.create_empty_object_literal();
                    let rest_obj = self.b.stage_acc(&temps);
                    self.b.load(value_reg);
                    self.b.stage_acc(&temps);
                    let base = excl_base.unwrap_or(rest_obj);
                    let count = excluded + 2;
                    // restage the window in element order: excluded keys
                    // ride the top (excluded[0] highest), then the target,
                    // the source at the bottom slot
                    let w = temps.reserve(count);
                    let top = w.index() + (count - 1) as i32;
                    for k in 0..excluded {
                        self.b.load(Reg::new(base.index() + k as i32));
                        self.b.store(Reg::new(top - k as i32));
                    }
                    self.b.load(rest_obj);
                    self.b.store(Reg::new(w.index() + 1));
                    self.b.load(Reg::new(base.index() + excluded as i32 + 1));
                    self.b.store(w);
                    self.b
                        .call_runtime(BuiltinFn::CopyDataProperties, RegList::new(w, count));
                    self.b.store(rest_obj);
                    self.emit_pattern_leaf(*target, rest_obj, binding)?;
                }
                PatProperty::Prop {
                    key,
                    computed,
                    target,
                    default,
                } => {
                    let key_temps = if has_rest {
                        &temps
                    } else {
                        prop_temps
                            .as_ref()
                            .expect("no-rest patterns own prop temps")
                    };
                    let key_reg = self.stage_key(key, *computed, key_temps)?;
                    let name_hint = match key_reg {
                        Some(r) => NameHint::Reg(r),
                        None => NameHint::Const(self.name_constant(key)?),
                    };
                    if has_rest {
                        match key_reg {
                            Some(r) => {
                                excl_base.get_or_insert(r);
                                excluded += 1;
                            }
                            None => {
                                // materialize the constant key for the
                                // exclusion set
                                if let NameHint::Const(idx) = name_hint {
                                    self.b.load_constant(idx);
                                    let r = self.b.stage_acc(&temps);
                                    excl_base.get_or_insert(r);
                                    excluded += 1;
                                }
                            }
                        }
                    }
                    // v = GetV(value, P); computed keys read the value
                    // straight from their register via the accumulator
                    match key_reg {
                        Some(k) => {
                            self.b.load(k);
                            let feedback = self.b.new_feedback();
                            self.b.load_keyed_property(value_reg, feedback);
                        }
                        None => {
                            let NameHint::Const(name_idx) = name_hint else {
                                unreachable!("constant keys have a constant hint")
                            };
                            let feedback = self.b.new_feedback();
                            self.b.load_named_property(value_reg, name_idx, feedback);
                        }
                    }
                    self.emit_pattern_element(*target, *default, binding, name_hint)?;
                }
            }
        }
        Ok(())
    }

    /// `[a, b = 1, , ...rest]` (ES 14.13).
    fn emit_array_pattern(
        &mut self,
        elements: Vec<PatElement<'_, '_>>,
        value_reg: Reg,
        binding: bool,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        // iterator = GetIterator(value)
        self.b
            .call_runtime(BuiltinFn::GetIterator, RegList::new(value_reg, 1));
        let iter = self.b.stage_acc(&temps);
        // done flag (ES 8.5.9: once done, later elements read undefined
        // without calling next again)
        self.b.load_zero();
        let done = self.b.stage_acc(&temps);
        let mut rest: Option<PatTarget<'_, '_>> = None;
        for el in &elements {
            match el {
                PatElement::Hole => {
                    // elision still consumes one iterator step
                    self.b.load(done);
                    let skip = self.b.new_label();
                    self.b.jump_if_truthy(skip);
                    self.b
                        .call_runtime(BuiltinFn::IteratorNext, RegList::new(iter, 1));
                    self.b.bind(skip);
                }
                PatElement::Rest(target) => {
                    rest = Some(*target);
                }
                PatElement::Item { target, default } => {
                    self.emit_iterator_element(*target, *default, iter, done, binding)?;
                }
            }
        }
        if let Some(target) = rest {
            // array ← remaining values (loop while !done)
            self.b.create_empty_array_literal();
            let arr = self.b.stage_acc(&temps);
            self.b.load_zero();
            let idx = self.b.stage_acc(&temps);
            let back = self.b.new_label();
            self.b.bind(back); // loop head
            self.b.load(done);
            let exit = self.b.new_label();
            self.b.jump_if_truthy(exit);
            self.b
                .call_runtime(BuiltinFn::IteratorNext, RegList::new(iter, 1));
            let result = self.b.stage_acc(&temps);
            self.b
                .call_runtime(BuiltinFn::IteratorDone, RegList::new(result, 1));
            let have = self.b.new_label();
            self.b.jump_if_falsy(have);
            self.b.load_smi(1);
            self.b.store(done);
            let after = self.b.new_label();
            self.b.jump(after);
            self.b.bind(have);
            self.b
                .call_runtime(BuiltinFn::IteratorValue, RegList::new(result, 1));
            let feedback = self.b.new_feedback();
            self.b.store_keyed_property_no_shadow(arr, idx, feedback);
            self.b.raw(Opcode::AddImmediate, &[idx.operand(), 1]); // acc = idx + 1
            self.b.store(idx);
            self.b.jump_loop(back);
            self.b.bind(after);
            // both loop exits converge here
            self.b.bind(exit);
            self.b.load(arr);
            self.emit_pattern_leaf(target, arr, binding)?;
        }
        Ok(())
    }

    /// Load a numeric literal key (small ints inline, floats via the pool).
    fn emit_number_key(&mut self, key: &PropertyKey<'_>) {
        if let PropertyKey::NumericLiteral(n) = key {
            let f = n.value;
            if f.fract() == 0.0 && f.is_sign_positive() && f <= i16::MAX as f64 {
                self.b.load_smi(f as i32);
            } else {
                let c = self.b.constant(Constant::Float(f));
                self.b.load_constant(c);
            }
        }
    }

    /// Stage a property key for the keyed paths: computed keys evaluate
    /// into a register, numeric literal keys load their literal; string
    /// keys stay pooled (`None`, addressed by name).
    fn stage_key(
        &mut self,
        key: &PropertyKey<'_>,
        computed: bool,
        temps: &TempScope,
    ) -> Result<Option<Reg>, CompileError> {
        if computed {
            if let Some(e) = key.as_expression() {
                self.expr(e)?;
            }
            Ok(Some(self.b.stage_acc(temps)))
        } else if matches!(key, PropertyKey::NumericLiteral(_)) {
            self.emit_number_key(key);
            Ok(Some(self.b.stage_acc(temps)))
        } else {
            Ok(None)
        }
    }

    /// One array-pattern element: v = done ? undefined : IteratorValue;
    /// then the shared default handling.
    fn emit_iterator_element(
        &mut self,
        target: PatTarget<'_, '_>,
        default: Option<&Expression<'_>>,
        iter: Reg,
        done: Reg,
        binding: bool,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        self.b.load_undefined();
        let v = self.b.stage_acc(&temps);
        self.b.load(done);
        let skip_next = self.b.new_label();
        self.b.jump_if_truthy(skip_next);
        self.b
            .call_runtime(BuiltinFn::IteratorNext, RegList::new(iter, 1));
        let result = self.b.stage_acc(&temps);
        self.b
            .call_runtime(BuiltinFn::IteratorDone, RegList::new(result, 1));
        let have = self.b.new_label();
        self.b.jump_if_falsy(have);
        self.b.load_smi(1);
        self.b.store(done);
        let after = self.b.new_label();
        self.b.jump(after);
        self.b.bind(have);
        self.b
            .call_runtime(BuiltinFn::IteratorValue, RegList::new(result, 1));
        self.b.store(v);
        self.b.bind(after);
        self.b.bind(skip_next);
        // array-element defaults name anonymous functions after the
        // binding identifier (ES 8.6.2: NamedEvaluation with bindingName)
        let bind_name: Option<Vec<u8>> = match target {
            PatTarget::Binding(BindingPattern::BindingIdentifier(b)) if binding => {
                Some(b.name.as_bytes().to_vec())
            }
            PatTarget::Assign(AssignmentTarget::AssignmentTargetIdentifier(i)) => {
                Some(i.name.as_bytes().to_vec())
            }
            _ => None,
        };
        let hint = if default.is_some_and(|d| self.is_anon_function(d)) && bind_name.is_some() {
            let idx = self.b.name(&bind_name.unwrap());
            NameHint::Const(idx)
        } else {
            NameHint::None
        };
        self.emit_pattern_element_with(target, default, v, binding, hint)?;
        Ok(())
    }

    /// Default application + target binding with the current value in
    /// `v` (the accumulator is not used).
    fn emit_pattern_element_with(
        &mut self,
        target: PatTarget<'_, '_>,
        default: Option<&Expression<'_>>,
        v: Reg,
        binding: bool,
        name_hint: NameHint,
    ) -> Result<(), CompileError> {
        if let Some(default) = default {
            self.b.load(v);
            let skip = self.b.new_label();
            self.b.jump_if_not_undefined(skip);
            self.expr(default)?;
            // NamedEvaluation: `{a = function(){}} = x` names the closure
            // after the property key / array index (ES 8.4.3). Binding
            // patterns always name; assignment patterns only when the
            // target is an identifier reference
            if self.is_anon_function(default) && {
                let is_ident = matches!(
                    target,
                    PatTarget::Assign(AssignmentTarget::AssignmentTargetIdentifier(_))
                        | PatTarget::AssignIdent(_)
                        | PatTarget::Binding(BindingPattern::BindingIdentifier(_))
                );
                binding || is_ident
            } {
                match name_hint {
                    NameHint::Const(idx) => self.emit_set_name_by_const(idx),
                    NameHint::Reg(r) => self.emit_set_name_by_reg(r, 0),
                    NameHint::None => {}
                }
            }
            self.b.store(v);
            self.b.bind(skip);
        }
        self.b.load(v);
        self.emit_pattern_leaf(target, v, binding)
    }

    /// Value in the accumulator: apply the default and bind into a fresh
    /// register.
    fn emit_pattern_element(
        &mut self,
        target: PatTarget<'_, '_>,
        default: Option<&Expression<'_>>,
        binding: bool,
        name_hint: NameHint,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let v = self.b.stage_acc(&temps);
        self.emit_pattern_element_with(target, default, v, binding, name_hint)?;
        Ok(())
    }

    /// Store the value in the accumulator into one pattern leaf: a
    /// declaration name, an assignment target, or a nested pattern.
    fn emit_pattern_leaf(
        &mut self,
        target: PatTarget<'_, '_>,
        value_reg: Reg,
        binding: bool,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        match target {
            PatTarget::Binding(BindingPattern::BindingIdentifier(b)) => {
                let Some(sym) = b.symbol_id.get() else {
                    return self.err(b.span, "unresolved binding");
                };
                self.store_declaration(sym, b.name.as_ref())
            }
            PatTarget::Assign(AssignmentTarget::AssignmentTargetIdentifier(i)) => {
                self.store_name(i)
            }
            PatTarget::AssignIdent(i) => self.store_name(i),
            PatTarget::Assign(t) if assign_member_ref(t).is_some() => {
                let m = assign_member_ref(t).unwrap();
                let value = self.b.stage_acc(&temps);
                let store = if is_super_member(m) {
                    self.prepare_super_store(m, &temps)?
                } else {
                    self.prepare_property_store(m)?
                };
                self.b.load(value);
                self.emit_property_store(&store);
                drop(store);
                Ok(())
            }
            PatTarget::Binding(BindingPattern::ObjectPattern(o)) => {
                self.emit_object_pattern(object_binding_props(o), value_reg, binding)
            }
            PatTarget::Binding(BindingPattern::ArrayPattern(arr)) => {
                self.emit_array_pattern(array_binding_elements(arr), value_reg, binding)
            }
            PatTarget::Assign(AssignmentTarget::ArrayAssignmentTarget(arr)) => {
                self.emit_array_pattern(array_assign_elements(arr), value_reg, binding)
            }
            PatTarget::Assign(AssignmentTarget::ObjectAssignmentTarget(o)) => {
                self.emit_object_pattern(object_assign_props(o), value_reg, binding)
            }
            _ => self.err(target.span(), "destructuring target"),
        }
    }

    // -- property stores ------------------------------------------------------

    fn emit_property_store(&mut self, store: &StoreTarget) {
        let temps = self.b.temp_scope();
        match store {
            StoreTarget::Named { obj, name_idx, .. } => {
                let feedback = self.b.new_feedback();
                self.b.store_named_property(*obj, *name_idx, feedback);
            }
            StoreTarget::Keyed { obj, key, .. } => {
                let feedback = self.b.new_feedback();
                self.b.store_keyed_property(*obj, *key, feedback);
            }
            StoreTarget::PrivateKeyed { obj, key: _, .. } => {
                // (obj, key, value): obj rides the top slot, the key one
                // above the pre-staged obj was staged by the target
                // preparation, the value is the accumulator
                let key = Reg::new(obj.index() + 1);
                let base = temps.reserve(3);
                self.b.store(base);
                self.b.load(key);
                self.b.store(Reg::new(base.index() + 1));
                self.b.load(*obj);
                self.b.store(Reg::new(base.index() + 2));
                self.b
                    .call_runtime(BuiltinFn::PrivateSet, RegList::new(base, 3));
            }
            // runtime(home, recv, key, value = acc, semantics: shadow)
            StoreTarget::SuperNamed {
                recv,
                home,
                name_idx,
                ..
            } => {
                self.b.call_runtime_staged(
                    BuiltinFn::SuperSetProperty,
                    &[
                        RtArg::Reg(*home),
                        RtArg::Reg(*recv),
                        RtArg::Const(*name_idx),
                        RtArg::Acc,
                        RtArg::Smi(0),
                    ],
                );
            }
            StoreTarget::SuperKeyed {
                recv, home, key, ..
            } => {
                self.b.call_runtime_staged(
                    BuiltinFn::SuperSetProperty,
                    &[
                        RtArg::Reg(*home),
                        RtArg::Reg(*recv),
                        RtArg::Reg(*key),
                        RtArg::Acc,
                        RtArg::Smi(0),
                    ],
                );
            }
        }
    }

    fn emit_property_load_of(&mut self, store: &StoreTarget) {
        let temps = self.b.temp_scope();
        match store {
            StoreTarget::Named { obj, name_idx, .. } => {
                let feedback = self.b.new_feedback();
                self.b.load_named_property(*obj, *name_idx, feedback);
            }
            StoreTarget::Keyed { obj, key, .. } => {
                let feedback = self.b.new_feedback();
                self.b.load_keyed_property_reg(*obj, *key, feedback);
            }
            StoreTarget::PrivateKeyed { obj, key, .. } => {
                // (obj, key): obj rides the top slot — restaged into a
                // fresh window (the prep stages obj below the key, the
                // old layout's order)
                let base = temps.reserve(2);
                self.b.load(*key);
                self.b.store(base);
                self.b.load(*obj);
                self.b.store(Reg::new(base.index() + 1));
                self.b
                    .call_runtime(BuiltinFn::PrivateGet, RegList::new(base, 2));
            }
            // runtime(home, recv, key) -> value
            StoreTarget::SuperNamed {
                recv,
                home,
                name_idx,
                ..
            } => {
                self.b.call_runtime_staged(
                    BuiltinFn::SuperGetProperty,
                    &[
                        RtArg::Reg(*home),
                        RtArg::Reg(*recv),
                        RtArg::Const(*name_idx),
                    ],
                );
            }
            StoreTarget::SuperKeyed {
                recv, home, key, ..
            } => {
                self.b.call_runtime_staged(
                    BuiltinFn::SuperGetProperty,
                    &[RtArg::Reg(*home), RtArg::Reg(*recv), RtArg::Reg(*key)],
                );
            }
        }
    }

    fn prepare_property_store(&mut self, m: MemberRef<'_>) -> Result<StoreTarget, CompileError> {
        let temps = self.b.temp_scope();
        match m {
            MemberRef::Private(p) => {
                self.expr(&p.object)?;
                let obj = self.b.stage_acc(&temps);
                self.emit_private_key_load(p.field.node_id.get(), p.field.span)?;
                let k = self.b.stage_acc(&temps);
                Ok(StoreTarget::PrivateKeyed {
                    obj,
                    key: k,
                    _temps: temps,
                })
            }
            MemberRef::Static(s) => {
                let name_idx = self.b.name(s.property.name.as_bytes());
                if let Some(obj) = self.this_operand_reg(&s.object) {
                    return Ok(StoreTarget::Named {
                        obj,
                        name_idx,
                        _temps: temps,
                    });
                }
                self.expr(&s.object)?;
                let obj = self.b.stage_acc(&temps);
                Ok(StoreTarget::Named {
                    obj,
                    name_idx,
                    _temps: temps,
                })
            }
            MemberRef::Computed(c) => {
                self.expr(&c.object)?;
                let obj = self.b.stage_acc(&temps);
                self.expr(&c.expression)?;
                let k = self.b.stage_acc(&temps);
                Ok(StoreTarget::Keyed {
                    obj,
                    key: k,
                    _temps: temps,
                })
            }
        }
    }

    /// The receiver and home-object registers of a `super.x` reference
    /// (assignment target or compound-access base). The receiver is staged
    /// in the caller's `temps` so it can outlive the home/key parts.
    fn prepare_super_parts(
        &mut self,
        m: MemberRef<'_>,
        temps: &TempScope,
    ) -> Result<Option<StoreTarget>, CompileError> {
        let node = m.node_id();
        let Some(Special::Super {
            home,
            depth,
            this_owner,
            this_depth,
        }) = self.c.facts.special.get(&node).copied()
        else {
            return Ok(None);
        };
        self.emit_this_for(this_owner, this_depth);
        let recv = self.b.stage_acc(temps);
        let parts = self.b.temp_scope();
        let (home_slot, home_depth) = match home {
            Home::Class(_, slot) => (slot, depth),
            Home::Object(_) => (0, depth),
        };
        match m {
            MemberRef::Computed(c) => {
                self.expr(&c.expression)?;
                let k = self.b.stage_acc(&parts);
                self.b.load_context_slot(home_slot, home_depth);
                let home = self.b.stage_acc(&parts);
                Ok(Some(StoreTarget::SuperKeyed {
                    recv,
                    home,
                    key: k,
                    _temps: parts,
                }))
            }
            MemberRef::Static(s) => {
                let name_idx = self.b.name(s.property.name.as_bytes());
                self.b.load_context_slot(home_slot, home_depth);
                let home = self.b.stage_acc(&parts);
                Ok(Some(StoreTarget::SuperNamed {
                    recv,
                    home,
                    name_idx,
                    _temps: parts,
                }))
            }
            MemberRef::Private(p) => {
                self.err(p.span, "private fields may not be accessed on 'super'")
            }
        }
    }

    fn prepare_super_store(
        &mut self,
        m: MemberRef<'_>,
        temps: &TempScope,
    ) -> Result<StoreTarget, CompileError> {
        self.prepare_super_parts(m, temps)?
            .ok_or_else(|| CompileError::new(m.span(), "super assignment target"))
    }

    fn emit_property_load(&mut self, m: MemberRef<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        if is_super_member(m) {
            let Some(store) = self.prepare_super_parts(m, &temps)? else {
                return self.err(m.span(), "super property");
            };
            self.emit_property_load_of(&store);
            drop(store);
            return Ok(());
        }
        match m {
            MemberRef::Private(p) => {
                // PrivateGet: `obj.#x` — own private field or TypeError
                let base = temps.reserve(2);
                self.emit_private_key_load(p.field.node_id.get(), p.field.span)?;
                self.b.store(base);
                self.expr(&p.object)?;
                self.b.store(Reg::new(base.index() + 1));
                self.b
                    .call_runtime(BuiltinFn::PrivateGet, RegList::new(base, 2));
                Ok(())
            }
            MemberRef::Computed(c) => {
                let feedback = self.b.new_feedback();
                if let (Some(obj), Some(key)) = (
                    self.simple_register(&c.object),
                    self.simple_register(&c.expression),
                ) {
                    self.b.load_keyed_property_reg(obj, key, feedback);
                    return Ok(());
                }
                self.expr(&c.object)?;
                let obj = self.b.stage_acc(&temps);
                if let Some(key) = self.simple_register(&c.expression) {
                    self.b.load_keyed_property_reg(obj, key, feedback);
                } else {
                    self.expr(&c.expression)?;
                    self.b.load_keyed_property(obj, feedback);
                }
                Ok(())
            }
            MemberRef::Static(s) => {
                let name_idx = self.b.name(s.property.name.as_bytes());
                let feedback = self.b.new_feedback();
                if let Some(obj) = self.this_operand_reg(&s.object) {
                    self.b.load_named_property(obj, name_idx, feedback);
                    return Ok(());
                }
                if let Some(obj) = self.simple_register(&s.object) {
                    self.b.load_named_property(obj, name_idx, feedback);
                    return Ok(());
                }
                self.expr(&s.object)?;
                let obj = self.b.stage_acc(&temps);
                self.b.load_named_property(obj, name_idx, feedback);
                Ok(())
            }
        }
    }

    // -- calls -----------------------------------------------------------------

    fn call_argument(&mut self, arg: &Argument<'_>) -> Result<(), CompileError> {
        match arg {
            Argument::SpreadElement(_) => self.err(arg.span(), "spread in calls"),
            other => self.expr(other.as_expression().expect("spread handled")),
        }
    }

    /// Stage a method-call window: `[recv, args..., callee]` with the
    /// receiver already staged and the callee in the accumulator.
    /// Returns `(callee, args_base, argc + 1, base_mark)` — callers drop
    /// back to `base_mark` (the depth before the window) after the call.
    /// A method call `recv.<method>(args...)`: `load_method` evaluates and
    /// stages the receiver, loads the method into the accumulator, and
    /// returns the receiver's register. The callee then sits in a fixed
    /// slot above the contiguous `[recv, args...]` window.
    fn emit_method_call(
        &mut self,
        c: &CallExpression<'_>,
        load_method: impl FnOnce(&mut Self, &TempScope) -> Result<Reg, CompileError>,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let argc = c.arguments.len();
        // zero to two arguments: the call opcode takes callee, receiver and
        // arguments as independent registers, so no window is built and the
        // receiver is never copied
        if argc <= 2 {
            let recv = load_method(self, &temps)?;
            let callee = self.b.stage_acc(&temps);
            let mut args = [Reg::new(0); 2];
            for (i, arg) in c.arguments.iter().enumerate() {
                // a plain local/parameter is already in a register: read it
                // in place instead of `Load`+`Store` through the
                // accumulator. The last argument needs no stability (nothing
                // evaluates after it); earlier ones must survive sibling
                // evaluation, which `stable_register` guarantees.
                let direct = match arg.as_expression() {
                    Some(inner) if i + 1 == argc => self.simple_register(inner),
                    Some(inner) => self.stable_register(inner),
                    None => None,
                };
                if let Some(reg) = direct {
                    args[i] = reg;
                } else {
                    self.call_argument(arg)?;
                    args[i] = self.b.stage_acc(&temps);
                }
            }
            match argc {
                0 => {
                    let fb = self.b.new_feedback();
                    self.b.call_method0(callee, recv, fb)
                }
                1 => {
                    let fb = self.b.new_feedback();
                    self.b.call_method1(callee, recv, args[0], fb)
                }
                _ => {
                    let fb = self.b.new_feedback();
                    self.b.call_method2(callee, recv, args[0], args[1], fb)
                }
            }
            return Ok(());
        }
        let argc = argc as u32;
        let recv = load_method(self, &temps)?;
        // the window is [args_base .. args_base+argc]: element 0 (the
        // receiver) rides the top slot, argument i sits argc-1-i slots in,
        // the callee rides above
        let args_base = temps.reserve(argc + 2);
        let top = args_base.index() + argc as i32;
        let callee = Reg::new(top + 1);
        self.b.store(callee);
        for (i, arg) in c.arguments.iter().enumerate() {
            let direct = match arg.as_expression() {
                Some(inner) if i + 1 == argc as usize => self.simple_register(inner),
                Some(inner) => self.stable_register(inner),
                None => None,
            };
            if let Some(reg) = direct {
                self.b.move_reg(Reg::new(top - 1 - i as i32), reg);
            } else {
                self.call_argument(arg)?;
                self.b.store(Reg::new(top - 1 - i as i32));
            }
        }
        self.b.move_reg(Reg::new(top), recv);
        let fb = self.b.new_feedback();
        self.b.call(callee, RegList::new(args_base, argc + 1), fb);
        Ok(())
    }

    fn emit_direct_eval_call(&mut self, c: &CallExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let argc = c.arguments.len();
        if argc <= 2 {
            self.expr(&c.callee)?;
            let original = self.b.stage_acc(&temps);
            let callee = temps.temp();
            self.b.load(original);
            self.b.store(callee);
            let mut args = [Reg::new(0); 2];
            for (i, arg) in c.arguments.iter().enumerate() {
                let direct = match arg.as_expression() {
                    Some(inner) if i + 1 == argc => self.simple_register(inner),
                    Some(inner) => self.stable_register(inner),
                    None => None,
                };
                if let Some(reg) = direct {
                    args[i] = reg;
                } else {
                    self.call_argument(arg)?;
                    args[i] = self.b.stage_acc(&temps);
                }
            }
            let source = if argc == 0 {
                self.b.load_undefined();
                self.b.stage_acc(&temps)
            } else {
                args[0]
            };
            self.b.call_runtime_staged(
                BuiltinFn::ResolveDirectEval,
                &[RtArg::Reg(callee), RtArg::Reg(source)],
            );
            self.b.store(callee);
            let plain = self.b.new_label();
            let end = self.b.new_label();
            self.b.load(original);
            self.b.equal_strict(callee);
            self.b.jump_if_truthy(plain);
            self.emit_this_for(self.fid, 0);
            let this = self.b.stage_acc(&temps);
            let fb = self.b.new_feedback();
            match argc {
                0 => self.b.call_method0(callee, this, fb),
                1 => self.b.call_method1(callee, this, args[0], fb),
                _ => self.b.call_method2(callee, this, args[0], args[1], fb),
            }
            self.b.jump(end);
            self.b.bind(plain);
            let fb = self.b.new_feedback();
            match argc {
                0 => self.b.call_function0(callee, fb),
                1 => self.b.call_function1(callee, args[0], fb),
                _ => self.b.call_function2(callee, args[0], args[1], fb),
            }
            self.b.bind(end);
            return Ok(());
        }

        self.expr(&c.callee)?;
        let original = self.b.stage_acc(&temps);
        let argc = argc as u32;
        let args_base = temps.reserve(argc + 2);
        let top = args_base.index() + argc as i32;
        let callee = Reg::new(top + 1);
        self.b.load(original);
        self.b.store(callee);
        self.b.load_undefined();
        self.b.store(Reg::new(top));
        for (i, arg) in c.arguments.iter().enumerate() {
            let direct = match arg.as_expression() {
                Some(inner) if i + 1 == argc as usize => self.simple_register(inner),
                Some(inner) => self.stable_register(inner),
                None => None,
            };
            if let Some(reg) = direct {
                self.b.move_reg(Reg::new(top - 1 - i as i32), reg);
            } else {
                self.call_argument(arg)?;
                self.b.store(Reg::new(top - 1 - i as i32));
            }
        }
        self.b.load(Reg::new(top - 1));
        let source = self.b.stage_acc(&temps);
        self.b.call_runtime_staged(
            BuiltinFn::ResolveDirectEval,
            &[RtArg::Reg(callee), RtArg::Reg(source)],
        );
        self.b.store(callee);
        let plain = self.b.new_label();
        self.b.load(original);
        self.b.equal_strict(callee);
        self.b.jump_if_truthy(plain);
        self.emit_this_for(self.fid, 0);
        let this = self.b.stage_acc(&temps);
        self.b.load(this);
        self.b.store(Reg::new(top));
        self.b.bind(plain);
        let fb = self.b.new_feedback();
        self.b.call(callee, RegList::new(args_base, argc + 1), fb);
        Ok(())
    }

    fn emit_call(&mut self, c: &CallExpression<'_>) -> Result<(), CompileError> {
        if !c.optional
            && let Expression::Identifier(i) = &c.callee
            && i.name == "eval"
        {
            return self.emit_direct_eval_call(c);
        }
        let temps = self.b.temp_scope();
        // super.m(...): the method comes from the home-object chain and
        // runs with the current `this`
        if let Some(m) = MemberRef::of(&c.callee)
            && is_super_member(m)
        {
            return self.emit_super_method_call(c, m);
        }
        if let Expression::Super(_) = &c.callee {
            return self.emit_super_call(c);
        }
        // method calls: the receiver is the first register of the argument
        // list (evaluation order: receiver, property get, then arguments)
        if let Expression::StaticMemberExpression(s) = &c.callee {
            let name_idx = self.b.name(s.property.name.as_bytes());
            return self.emit_method_call(c, |g, temps| {
                let recv = match g.stable_register(&s.object) {
                    Some(r) => r,
                    None => {
                        g.expr(&s.object)?;
                        g.b.stage_acc(&temps)
                    }
                };
                let feedback = g.b.new_feedback();
                g.b.load_named_property(recv, name_idx, feedback);
                Ok(recv)
            });
        }
        if let Expression::ComputedMemberExpression(cm) = &c.callee {
            return self.emit_method_call(c, |g, temps| {
                let recv = match g.stable_register(&cm.object) {
                    Some(r) => r,
                    None => {
                        g.expr(&cm.object)?;
                        g.b.stage_acc(&temps)
                    }
                };
                g.expr(&cm.expression)?;
                let feedback = g.b.new_feedback();
                g.b.load_keyed_property(recv, feedback);
                Ok(recv)
            });
        }
        if let Expression::PrivateFieldExpression(p) = &c.callee {
            return self.err(p.span, "calling a private method");
        }
        // plain call: receiver = undefined (slot 0), callee evaluated
        // first, then arguments
        let argc = c.arguments.len();
        if argc <= 2 {
            self.expr(&c.callee)?;
            let callee = self.b.stage_acc(&temps);
            let mut args = [Reg::new(0); 2];
            for (i, arg) in c.arguments.iter().enumerate() {
                // see `emit_method_call`: read a plain local/parameter
                // argument in place rather than copying through the
                // accumulator.
                let direct = match arg.as_expression() {
                    Some(inner) if i + 1 == argc => self.simple_register(inner),
                    Some(inner) => self.stable_register(inner),
                    None => None,
                };
                if let Some(reg) = direct {
                    args[i] = reg;
                } else {
                    self.call_argument(arg)?;
                    args[i] = self.b.stage_acc(&temps);
                }
            }
            match argc {
                0 => {
                    let fb = self.b.new_feedback();
                    self.b.call_function0(callee, fb)
                }
                1 => {
                    let fb = self.b.new_feedback();
                    self.b.call_function1(callee, args[0], fb)
                }
                _ => {
                    let fb = self.b.new_feedback();
                    self.b.call_function2(callee, args[0], args[1], fb)
                }
            }
            return Ok(());
        }
        let argc = argc as u32;
        self.expr(&c.callee)?;
        let args_base = temps.reserve(argc + 2);
        let top = args_base.index() + argc as i32;
        let callee = Reg::new(top + 1);
        self.b.store(callee);
        self.b.load_undefined();
        self.b.store(Reg::new(top));
        for (i, arg) in c.arguments.iter().enumerate() {
            let direct = match arg.as_expression() {
                Some(inner) if i + 1 == argc as usize => self.simple_register(inner),
                Some(inner) => self.stable_register(inner),
                None => None,
            };
            if let Some(reg) = direct {
                self.b.move_reg(Reg::new(top - 1 - i as i32), reg);
            } else {
                self.call_argument(arg)?;
                self.b.store(Reg::new(top - 1 - i as i32));
            }
        }
        let fb = self.b.new_feedback();
        self.b.call(callee, RegList::new(args_base, argc + 1), fb);
        Ok(())
    }

    fn emit_super_method_call(
        &mut self,
        c: &CallExpression<'_>,
        m: MemberRef<'_>,
    ) -> Result<(), CompileError> {
        self.emit_method_call(c, |g, temps| {
            let Some(store) = g.prepare_super_parts(m, temps)? else {
                return Err(CompileError::new(c.span, "super method call"));
            };
            g.emit_property_load_of(&store);
            let recv = match &store {
                StoreTarget::SuperNamed { recv, .. } => *recv,
                StoreTarget::SuperKeyed { recv, .. } => *recv,
                _ => unreachable!("super store parts"),
            };
            // the home/key parts above the receiver are dead once the
            // method is loaded; dropping the store's scope leaves the
            // receiver as the top temp so the call window stays contiguous.
            drop(store);
            Ok(recv)
        })
    }

    fn emit_new(&mut self, n: &NewExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let argc = n.arguments.len() as u32;
        self.expr(&n.callee)?;
        // window [args...] with argument 0 at the top slot, callee above
        let args_base = temps.reserve(argc + 1);
        let top = args_base.index() + argc as i32 - 1;
        let callee = Reg::new(top + 1);
        self.b.store(callee);
        for (i, arg) in n.arguments.iter().enumerate() {
            // like the call paths: a plain local/parameter is already in a
            // register, so move it directly instead of routing through the
            // accumulator; earlier arguments must survive sibling
            // evaluation (`stable_register`), the last needs no stability.
            let direct = match arg.as_expression() {
                Some(inner) if i + 1 == argc as usize => self.simple_register(inner),
                Some(inner) => self.stable_register(inner),
                None => None,
            };
            if let Some(reg) = direct {
                self.b.move_reg(Reg::new(top - i as i32), reg);
            } else {
                self.call_argument(arg)?;
                self.b.store(Reg::new(top - i as i32));
            }
        }
        self.b.construct(callee, RegList::new(args_base, argc));
        Ok(())
    }

    /// `super(...)`: construct the superclass with the constructor's
    /// new.target and initialize `this` with the result (ES 15.4.3).
    fn emit_super_call(&mut self, c: &CallExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let argc = c.arguments.len() as u32;
        let (owner, owner_depth) = match self.c.facts.special.get(&c.node_id.get()) {
            Some(Special::SuperCall { owner, depth }) => (*owner, *depth),
            _ => (self.fid, 0),
        };
        let direct = owner == self.fid;
        let this_slot = if direct {
            self.c.layouts[self.fid.0 as usize].this_slot
        } else {
            Some(
                self.c.layouts[owner.0 as usize]
                    .this_slot
                    .expect("delegated super() forces the this slot"),
            )
        };

        // reserve arguments + result (+ closure/new.target registers for
        // the delegated variant) so nested temps land above
        let reserved = argc + 1 + u32::from(!direct) * 2;
        let arg_base = temps.reserve(reserved);
        // the delegated window is (args..., closure, new_target): args
        // ride the top, closure/new.target the two bottom slots
        let arg_top = arg_base.index() + argc as i32 + i32::from(!direct) * 2 - 1;
        for (i, arg) in c.arguments.iter().enumerate() {
            let direct_reg = match arg.as_expression() {
                Some(inner) if i + 1 == argc as usize => self.simple_register(inner),
                Some(inner) => self.stable_register(inner),
                None => None,
            };
            if let Some(reg) = direct_reg {
                self.b.move_reg(Reg::new(arg_top - i as i32), reg);
            } else {
                self.call_argument(arg)?;
                self.b.store(Reg::new(arg_top - i as i32));
            }
        }
        if direct {
            self.b
                .call_runtime(BuiltinFn::ConstructSuper, RegList::new(arg_base, argc));
        } else {
            // .this_function and .new.target of the owning constructor
            // (runtime ABI: (args..., closure, new_target))
            let (this_function_slot, new_target_slot) = {
                let l = &self.c.layouts[owner.0 as usize];
                (
                    l.this_function_slot
                        .expect("delegated super() forces the closure slot"),
                    l.new_target_slot
                        .expect("delegated super() forces the new.target slot"),
                )
            };
            let new_target_reg = arg_base;
            let closure_reg = Reg::new(arg_base.index() + 1);
            self.b.load_context_slot(this_function_slot, owner_depth);
            self.b.store(closure_reg);
            self.b.load_context_slot(new_target_slot, owner_depth);
            self.b.store(new_target_reg);
            self.b.call_runtime(
                BuiltinFn::ConstructSuperVia,
                RegList::new(arg_base, argc + 2),
            );
        }
        // the constructed instance lands above the (contiguous) runtime
        // argument window
        let result = Reg::new(arg_base.index() + argc as i32 + i32::from(!direct) * 2);
        self.b.store(result);
        // InitializeThisBinding: this must still be uninitialized
        let super_once_check = |g: &mut Self| {
            let t = g.b.stage_acc(&temps);
            g.b.call_runtime(
                BuiltinFn::ThrowSuperAlreadyCalledIfNotHole,
                RegList::new(t, 1),
            );
        };
        match this_slot {
            Some(slot) => {
                let depth = if direct { 0 } else { owner_depth };
                self.b.load_context_slot(slot, depth);
                super_once_check(self);
                self.b.load(result);
                self.b.store_context_slot(slot, depth);
                if direct {
                    // mirror the bound `this` into the frame register: the
                    // interpreter's [[Construct]] return fixup reads it
                    let this = self.b.this_reg();
                    self.b.load(result);
                    self.b.store(this);
                }
            }
            None => {
                let this = self.b.this_reg();
                self.b.load(this);
                super_once_check(self);
                self.b.load(result);
                self.b.store(this);
            }
        }
        // InitializeInstanceElements (ES 7.3.33): the derived
        // constructor's own fields are defined on the freshly bound
        // instance (for arrow-delegated super(), the owner is the ctor)
        let field_owner = if direct { self.fid } else { owner };
        if self.fn_has_instance_fields(field_owner) {
            // (ctor, instance): ctor rides the top slot
            self.b.load(result);
            let instance = self.b.stage_acc(&temps);
            let ctor = temps.temp();
            if direct {
                self.b.load_current_closure();
            } else {
                let slot = self.c.layouts[owner.0 as usize]
                    .this_function_slot
                    .expect("delegated super() forces the closure slot");
                self.b.load_context_slot(slot, owner_depth);
            }
            self.b.store(ctor);
            self.b
                .call_runtime(BuiltinFn::InitInstanceFields, RegList::new(instance, 2));
        }
        self.b.load(result);
        Ok(())
    }

    // -- classes ------------------------------------------------------------------

    /// Class definitions (ES 15.7.14 ClassDefinitionEvaluation) as inline
    /// per-member emission. Leaves the class constructor in the
    /// accumulator.
    fn emit_class(&mut self, idx: ClassIdx) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let slot_count = self.c.facts.classes[idx.0 as usize].slot_count;
        let ctor = self.c.facts.classes[idx.0 as usize].ctor;
        let uses_super = self.c.facts.classes[idx.0 as usize].uses_super;
        let name_slot = self.c.facts.classes[idx.0 as usize].name_slot;
        let is_decl = self.c.facts.classes[idx.0 as usize].is_decl;
        let superclass = self.c.facts.classes[idx.0 as usize].superclass;
        let has_instance_fields = self.c.facts.classes[idx.0 as usize].has_instance_fields;
        let member_count = self.c.facts.classes[idx.0 as usize].members.len();
        let decl_symbol = self.c.facts.classes[idx.0 as usize].decl_symbol.clone();

        // class inner context: the name binding (TDZ until the class value
        // exists) and the super home objects; captured by member closures.
        // Pushed before the superclass evaluation (ES 15.7.14 step 8).
        let ctx_guard = if slot_count > 0 {
            self.b.create_block_context(slot_count);
            let save = temps.temp();
            Some(self.context_scope(save, ContextHost::Class(idx)))
        } else {
            None
        };

        // private names: one fresh Symbol per private name per class
        // evaluation, stored into the class context before any element
        // evaluates (methods and initializers reference them by slot)
        let privates = self.c.facts.classes[idx.0 as usize].privates.clone();
        let private_slots = self.c.facts.classes[idx.0 as usize].private_slots.clone();
        for (name, slot) in privates.iter().zip(&private_slots) {
            let private_temps = self.b.temp_scope();
            let desc = self.b.name(name.as_bytes());
            self.b.load_constant(desc);
            let desc_reg = self.b.stage_acc(&private_temps);
            self.b
                .call_runtime(BuiltinFn::CreatePrivateName, RegList::new(desc_reg, 1));
            self.b.store_context_slot(*slot, 0);
        }

        // superclass: must be null or a constructor
        let sup = if let Some(sup) = superclass {
            self.expr(sup)?;
            let sup = self.b.stage_acc(&temps);
            self.b
                .call_runtime_staged(BuiltinFn::ThrowIfNotConstructorOrNull, &[RtArg::Reg(sup)]);
            Some(sup)
        } else {
            None
        };

        // prototype/constructor parents; defaults are the no-extends case
        let obj_proto = self.b.constant(Constant::ObjectPrototype);
        self.b.load_constant(obj_proto);
        let pp = self.b.stage_acc(&temps);
        let fn_proto = self.b.constant(Constant::FunctionPrototype);
        self.b.load_constant(fn_proto);
        let cp = self.b.stage_acc(&temps);
        if let Some(sup) = sup {
            // null superclass → null-proto prototype; ctor parent stays
            // %Function.prototype% (objects are always truthy, so the
            // falsy test identifies null)
            self.b.load(sup);
            let null_extends = self.b.new_label();
            self.b.jump_if_falsy(null_extends);
            // protoParent = Get(superCtor, "prototype") (full [[Get]])
            let proto_name = self.b.name(b"prototype");
            let feedback = self.b.new_feedback();
            self.b.load_named_property(sup, proto_name, feedback);
            self.b.store(pp);
            self.b
                .call_runtime_staged(BuiltinFn::ThrowIfNotObjectOrNull, &[RtArg::Reg(pp)]);
            self.b.load(sup);
            self.b.store(cp);
            let done = self.b.new_label();
            self.b.jump(done);
            self.b.bind(null_extends);
            self.b.load_null();
            self.b.store(pp);
            self.b.bind(done);
        }

        // prototype: a fresh ordinary object with protoParent
        self.b.create_empty_object_literal();
        let proto = self.b.stage_acc(&temps);
        self.b.call_runtime_staged(
            BuiltinFn::SetPrototype,
            &[RtArg::Reg(proto), RtArg::Reg(pp)],
        );

        // constructor closure
        let ctor_idx = self.b.constant(Constant::Callable(FunctionId(ctor.0)));
        self.b.create_closure(ctor_idx);
        let ctor = self.b.stage_acc(&temps);

        // wiring before member installation (ES 15.7.14 steps 17–18
        // precede the element loop): computed `['constructor']` members
        // overwrite proto.constructor, computed static `['prototype']`
        // defines fail against the non-configurable ctor.prototype
        // proto.constructor → the class {w+, e−, c+}
        let ctor_name = self.b.name(b"constructor");
        self.b.call_runtime_staged(
            BuiltinFn::DefineOwnProperty,
            &[
                RtArg::Reg(proto),
                RtArg::Const(ctor_name),
                RtArg::Reg(ctor),
                RtArg::Smi(PropertyFlags::DontEnum.bits()),
            ],
        );
        // ctor.prototype → the prototype {w+, e−, c−}
        let proto_name = self.b.name(b"prototype");
        self.b.call_runtime_staged(
            BuiltinFn::DefineOwnProperty,
            &[
                RtArg::Reg(ctor),
                RtArg::Const(proto_name),
                RtArg::Reg(proto),
                RtArg::Smi(PropertyFlags::DontEnum.bits() | PropertyFlags::DontDelete.bits()),
            ],
        );
        // the class itself inherits from the superclass constructor
        self.b
            .call_runtime_staged(BuiltinFn::SetPrototype, &[RtArg::Reg(ctor), RtArg::Reg(cp)]);

        // instance field list: a JS array [key0, init0, key1, init1, ...]
        // attached to the constructor (its hidden fields slot)
        let fields_arr = if has_instance_fields {
            self.b.create_empty_array_literal();
            Some(self.b.stage_acc(&temps))
        } else {
            None
        };
        let mut field_index = 0u32;
        // static field keys (evaluated in element order) stay live until
        // the deferred initializer calls after the class is complete
        // (closure register, key register, constant-key name index)
        let mut static_fields: Vec<(Reg, Option<Reg>, Option<ConstIdx>)> = Vec::new();

        // members in declaration order; the constructor is already installed
        for mi in 0..member_count {
            let (kind, is_static, computed, fid, key) = {
                let m = &self.c.facts.classes[idx.0 as usize].members[mi];
                (m.kind, m.is_static, m.computed, m.fid, m.key)
            };
            // Static field keys/closures must outlive the member loop (they
            // run after the class is complete), so they use the outer scope.
            // Everything else is local to this member.
            let member_temps = if kind == MemberKind::Field && is_static {
                None
            } else {
                Some(self.b.temp_scope())
            };
            if kind == MemberKind::Field {
                let key_temps = if is_static {
                    &temps
                } else {
                    member_temps
                        .as_ref()
                        .expect("instance fields own member temps")
                };
                // field key: evaluated in element order (ES 15.7.14 step 27)
                let key_reg = if matches!(key, PropertyKey::PrivateIdentifier(_)) {
                    if let PropertyKey::PrivateIdentifier(p) = key {
                        self.emit_private_key_load(p.node_id.get(), p.span)?;
                    }
                    Some(self.b.stage_acc(key_temps))
                } else {
                    self.stage_key(key, computed, key_temps)?
                };
                let fn_idx = self.b.constant(Constant::Callable(FunctionId(fid.0)));
                self.b.create_closure(fn_idx);
                if is_static {
                    let closure = self.b.stage_acc(&temps);
                    let name_idx = match key_reg {
                        Some(_) => None,
                        None => Some(self.name_constant(key)?),
                    };
                    static_fields.push((closure, key_reg, name_idx));
                } else {
                    let member_temps = member_temps
                        .as_ref()
                        .expect("instance fields own member temps");
                    // append [key, initializer] to the instance field list
                    let arr = fields_arr.expect("instance fields allocate the list");
                    let closure = self.b.stage_acc(member_temps);
                    self.b.load_smi(field_index as i32);
                    let i = self.b.stage_acc(member_temps);
                    match key_reg {
                        Some(k) => {
                            self.b.load(k);
                            let feedback = self.b.new_feedback();
                            self.b.store_keyed_property_no_shadow(arr, i, feedback);
                        }
                        None => {
                            let name_idx = self.name_constant(key)?;
                            self.b.load_constant(name_idx);
                            let feedback = self.b.new_feedback();
                            self.b.store_keyed_property_no_shadow(arr, i, feedback);
                        }
                    }
                    // initializer at the next slot
                    self.b.load_smi(field_index as i32 + 1);
                    self.b.store(i);
                    self.b.load(closure);
                    let feedback = self.b.new_feedback();
                    self.b.store_keyed_property_no_shadow(arr, i, feedback);
                    field_index += 2;
                }
                continue;
            }
            let target = if is_static { ctor } else { proto };
            let member_temps = member_temps
                .as_ref()
                .expect("non-static-field members own member temps");
            // non-computed keys are strings or numbers; numbers go through
            // the keyed define with the literal loaded into a register
            let key_reg = self.stage_key(key, computed, member_temps)?;
            let fn_idx = self.b.constant(Constant::Callable(FunctionId(fid.0)));
            self.b.create_closure(fn_idx);
            // computed keys name the member after their ToPropertyKey value
            if let Some(k) = key_reg {
                let prefix = match kind {
                    MemberKind::Get => 1,
                    MemberKind::Set => 2,
                    _ => 0,
                };
                self.emit_set_name_by_reg(k, prefix);
            }
            let name_idx = match key_reg {
                Some(_) => None,
                None => Some(self.name_constant(key)?),
            };
            match kind {
                MemberKind::Method => {
                    // {w+, e−, c+}: runtime(obj, key, value = acc, flags)
                    let key = match key_reg {
                        Some(k) => RtArg::Reg(k),
                        None => RtArg::Const(name_idx.unwrap()),
                    };
                    self.b.call_runtime_staged(
                        BuiltinFn::DefineOwnProperty,
                        &[
                            RtArg::Reg(target),
                            key,
                            RtArg::Acc,
                            RtArg::Smi(PropertyFlags::DontEnum.bits()),
                        ],
                    );
                }
                MemberKind::Get | MemberKind::Set => {
                    // one accessor half; merges with an existing pair:
                    // runtime(target, key, closure = acc, flags)
                    let mut flags = PropertyFlags::DontEnum.bits();
                    if kind == MemberKind::Get {
                        flags |= 1;
                    }
                    let key = match key_reg {
                        Some(k) => RtArg::Reg(k),
                        None => RtArg::Const(name_idx.unwrap()),
                    };
                    self.b.call_runtime_staged(
                        BuiltinFn::InstallAccessor,
                        &[RtArg::Reg(target), key, RtArg::Acc, RtArg::Smi(flags)],
                    );
                }
                MemberKind::Field => unreachable!("fields handled above"),
            }
        }

        // home objects and the inner class-name binding
        if uses_super {
            let (home_slot, static_home_slot) = {
                let c = &self.c.facts.classes[idx.0 as usize];
                (c.home_slot.unwrap(), c.static_home_slot.unwrap())
            };
            // the class context is pushed here: zero hops
            self.b.load(proto);
            self.b.store_context_slot(home_slot, 0);
            self.b.load(ctor);
            self.b.store_context_slot(static_home_slot, 0);
        }
        if let Some(slot) = name_slot {
            self.b.load(ctor);
            self.b.store_context_slot(slot, 0);
        }

        // static fields: each initializer runs with the constructor as
        // `this` and its result is [[DefineOwnProperty]]'d on it, in
        // declaration order (ES 15.7.14 step 33)
        for (closure, key_reg, name_idx) in &static_fields {
            self.b.load(*closure);
            let fb = self.b.new_feedback();
            self.b.call(*closure, RegList::new(ctor, 1), fb);
            // runtime(obj = ctor, key, value = acc, flags 0)
            let key = match (key_reg, name_idx) {
                (Some(k), _) => RtArg::Reg(*k),
                (None, Some(name_idx)) => RtArg::Const(*name_idx),
                (None, None) => unreachable!("static field keys are reg or const"),
            };
            self.b.call_runtime_staged(
                BuiltinFn::DefineOwnProperty,
                &[RtArg::Reg(ctor), key, RtArg::Acc, RtArg::Smi(0)],
            );
        }
        // attach the instance field list to the constructor
        if let Some(arr) = fields_arr {
            // (ctor, fields): ctor rides the top slot
            self.b.load(arr);
            let arr_reg = self.b.stage_acc(&temps);
            let ctor_reg = temps.temp();
            self.b.load(ctor);
            self.b.store(ctor_reg);
            self.b
                .call_runtime(BuiltinFn::SetClassFields, RegList::new(arr_reg, 2));
        }

        // pop the class context; member closures already captured it
        if let Some(guard) = ctx_guard {
            drop(guard);
        }

        // outer binding for declarations (the class value in acc)
        self.b.load(ctor);
        if is_decl && let Some((sym, name)) = decl_symbol {
            self.store_declaration(sym, &name)?;
        }

        // LIFO: ctor, proto, cp, pp, superclass
        self.b.load(ctor);
        Ok(())
    }

    // -- literals -----------------------------------------------------------------

    fn emit_array_literal(&mut self, a: &ArrayExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        if a.elements.is_empty() {
            self.b.create_empty_array_literal();
        } else {
            // pre-size the backing store with the store-growth policy's
            // slack: element stores then append within headroom
            // (IC-fast, no reallocation)
            let len = a.elements.len();
            let capacity = len + (len >> 1) + 8;
            self.b.create_sized_array_literal(capacity as u32);
        }
        let arr = self.b.stage_acc(&temps);
        let mut i = 0u32;
        for el in &a.elements {
            match el {
                // an elision is a hole: it consumes an index but stores
                // nothing (the gap grow/holes below fill it in)
                ArrayExpressionElement::Elision(_) => i += 1,
                ArrayExpressionElement::SpreadElement(_) => {
                    return self.err(a.span, "spread in array literals");
                }
                other => {
                    let elem_temps = self.b.temp_scope();
                    let x = other.as_expression().expect("elision/spread handled");
                    self.b.load_smi(i as i32);
                    let idx = self.b.stage_acc(&elem_temps);
                    self.expr(x)?;
                    let feedback = self.b.new_feedback();
                    self.b.store_keyed_property_no_shadow(arr, idx, feedback);
                    i += 1;
                }
            }
        }
        // trailing elisions raise the length past the last store: the
        // elements store cannot do it (nothing is stored), so set it
        // explicitly (`length` is the array accessor)
        if matches!(a.elements.last(), Some(ArrayExpressionElement::Elision(_))) {
            self.b.load_smi(a.elements.len() as i32);
            let name = self.b.name(b"length");
            let feedback = self.b.new_feedback();
            self.b.store_named_property(arr, name, feedback);
        }
        self.b.load(arr);
        Ok(())
    }

    fn emit_object_literal(&mut self, o: &ObjectExpression<'_>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        // methods using `super` capture a per-literal home-object context
        // (the literal itself), mirroring class scopes
        let node = o.node_id.get();
        let needs_home = self.c.facts.obj_lit_home.contains(&node);
        let ctx_guard = if needs_home {
            self.b.create_block_context(1);
            let save = temps.temp();
            Some(self.context_scope(save, ContextHost::Object(node)))
        } else {
            None
        };

        self.b.create_empty_object_literal();
        let obj = self.b.stage_acc(&temps);
        for p in &o.properties {
            let prop_temps = self.b.temp_scope();
            let ObjectPropertyKind::ObjectProperty(p) = p else {
                return self.err(o.span, "spread in object literals");
            };
            // B.3.1 / ES 13.2.5.7: a non-computed `__proto__: value` sets
            // the literal's [[Prototype]] when value is an object or null
            // and is ignored otherwise; it never defines an own property.
            if p.kind == PropertyKind::Init
                && !p.computed
                && !p.shorthand
                && is_proto_literal_key(&p.key)
            {
                self.expr(&p.value)?;
                self.b
                    .call_runtime_staged(BuiltinFn::SetPrototype, &[RtArg::Reg(obj), RtArg::Acc]);
                continue;
            }
            // numeric literal keys ({ 1: x }) use the keyed path
            let key_reg = self.stage_key(&p.key, p.computed, &prop_temps)?;
            let is_method = p.method || matches!(p.value, Expression::FunctionExpression(_));
            match p.kind {
                PropertyKind::Init if !is_method => {
                    self.expr(&p.value)?;
                    match key_reg {
                        None => {
                            // NamedEvaluation: { m: function () {} }
                            let name_idx = self.name_constant(&p.key)?;
                            if self.is_anon_function(&p.value) {
                                self.emit_set_name_by_const(name_idx);
                            }
                            // literal properties are own data properties,
                            // never stores through an inherited setter
                            self.b.call_runtime_staged(
                                BuiltinFn::DefineOwnProperty,
                                &[
                                    RtArg::Reg(obj),
                                    RtArg::Const(name_idx),
                                    RtArg::Acc,
                                    RtArg::Smi(0),
                                ],
                            );
                        }
                        Some(k) => {
                            if self.is_anon_function(&p.value) {
                                self.emit_set_name_by_reg(k, 0);
                            }
                            self.b.call_runtime_staged(
                                BuiltinFn::DefineOwnProperty,
                                &[RtArg::Reg(obj), RtArg::Reg(k), RtArg::Acc, RtArg::Smi(0)],
                            );
                        }
                    }
                }
                PropertyKind::Init => {
                    // method shorthand `{ foo() {} }`: the closure is named
                    // after its key
                    self.expr(&p.value)?;
                    if let Some(k) = key_reg {
                        self.emit_set_name_by_reg(k, 0);
                        self.b.call_runtime_staged(
                            BuiltinFn::DefineOwnProperty,
                            &[RtArg::Reg(obj), RtArg::Reg(k), RtArg::Acc, RtArg::Smi(0)],
                        );
                    } else {
                        let name_idx = self.name_constant(&p.key)?;
                        self.b.call_runtime_staged(
                            BuiltinFn::DefineOwnProperty,
                            &[
                                RtArg::Reg(obj),
                                RtArg::Const(name_idx),
                                RtArg::Acc,
                                RtArg::Smi(0),
                            ],
                        );
                    }
                }
                PropertyKind::Get | PropertyKind::Set => {
                    // enumerable accessor halves, merged pairs; bit 0 marks
                    // the getter half
                    let prefix = match p.kind {
                        PropertyKind::Get => 1u32,
                        _ => 2,
                    };
                    self.expr(&p.value)?;
                    if let Some(k) = key_reg {
                        self.emit_set_name_by_reg(k, prefix);
                    }
                    let flags: u32 = match p.kind {
                        PropertyKind::Get => 1,
                        _ => 0,
                    };
                    let name_idx = match key_reg {
                        Some(_) => None,
                        None => Some(self.name_constant(&p.key)?),
                    };
                    let key = match key_reg {
                        Some(k) => RtArg::Reg(k),
                        None => RtArg::Const(name_idx.unwrap()),
                    };
                    self.b.call_runtime_staged(
                        BuiltinFn::InstallAccessor,
                        &[RtArg::Reg(obj), key, RtArg::Acc, RtArg::Smi(flags)],
                    );
                }
            }
        }
        // the home object is the literal itself; methods already captured
        // the context, so the store is visible to them
        if needs_home {
            self.b.load(obj);
            self.b.store_context_slot(0, 0);
        }
        if let Some(guard) = ctx_guard {
            drop(guard);
        }
        self.b.load(obj);
        Ok(())
    }
}

/// B.3.1 / ES 13.2.5.7: a non-computed `__proto__` property name in an
/// object initializer (bare identifier or string literal; shorthand and
/// method definitions are ordinary properties).
fn is_proto_literal_key(key: &PropertyKey<'_>) -> bool {
    match key {
        PropertyKey::StaticIdentifier(i) => i.name.as_bytes() == b"__proto__",
        PropertyKey::StringLiteral(s) => s.value.as_bytes() == b"__proto__",
        _ => false,
    }
}

/// Split a binding pattern's optional default: `{a = 1}` wraps the target
/// in an AssignmentPattern.
fn split_binding_default<'r, 'a>(
    p: &'r BindingPattern<'a>,
) -> (PatTarget<'r, 'a>, Option<&'r Expression<'a>>) {
    match p {
        BindingPattern::AssignmentPattern(a) => (PatTarget::Binding(&a.left), Some(&a.right)),
        other => (PatTarget::Binding(other), None),
    }
}

// ---------------------------------------------------------------------------
// statements + function bodies
// ---------------------------------------------------------------------------

fn callable_kind(kind: FnKind) -> CallableKind {
    match kind {
        FnKind::Script | FnKind::Normal => CallableKind::Normal,
        FnKind::Arrow => CallableKind::Arrow,
        FnKind::Method => CallableKind::Method,
        FnKind::Getter => CallableKind::Getter,
        FnKind::Setter => CallableKind::Setter,
        FnKind::BaseClassCtor => CallableKind::BaseClassConstructor,
        FnKind::DerivedClassCtor => CallableKind::DerivedClassConstructor,
        FnKind::DefaultDerivedCtor => CallableKind::DefaultDerivedConstructor,
    }
}

impl<'c, 'a, 'p> FunctionGen<'c, 'a, 'p> {
    fn stmt(&mut self, stmt: &Statement<'_>) -> Result<(), CompileError> {
        match stmt {
            Statement::ExpressionStatement(s) => {
                self.expr(&s.expression)?;
                // a script-level value-producing statement records the
                // completion value (non-value statements leave it)
                if let Some(completion) = self.completion {
                    self.b.store(completion);
                }
                Ok(())
            }
            Statement::VariableDeclaration(d) => self.emit_var_decl(d),
            Statement::BlockStatement(b) => {
                let temps = self.b.temp_scope();
                let guard = self.enter_block_context(b.scope_id.get(), temps.temp());
                self.emit_scope_hole_fills(b.scope_id.get());
                for s in &b.body {
                    self.stmt(s)?;
                }
                drop(guard);
                Ok(())
            }
            Statement::IfStatement(s) => {
                self.expr(&s.test)?;
                let else_l = self.b.new_label();
                self.b.jump_if_falsy(else_l);
                self.stmt(&s.consequent)?;
                match &s.alternate {
                    Some(e) => {
                        let end = self.b.new_label();
                        self.b.jump(end);
                        self.b.bind(else_l);
                        self.stmt(e)?;
                        self.b.bind(end);
                    }
                    None => {
                        self.b.bind(else_l);
                    }
                }
                Ok(())
            }
            Statement::WhileStatement(s) => {
                let labels = self.take_labels();
                self.emit_while(&s.test, &s.body, labels)
            }
            Statement::DoWhileStatement(s) => {
                let labels = self.take_labels();
                self.emit_do_while(&s.test, &s.body, labels)
            }
            Statement::ForStatement(s) => self.emit_for(s),
            Statement::ForInStatement(s) => self.emit_for_in(s),
            // TODO: for-of only needs the iterator protocol here; the
            // ForHead per-iteration context and assignment paths are shared
            // with for-in.
            Statement::ForOfStatement(s) => self.err(s.span, "for-of loops"),
            Statement::ReturnStatement(s) => {
                // derived constructors: `return v` returns v only when it
                // is an object; `undefined` (and fallthrough) return `this`
                // (ES 9.2.2.1)
                if self.is_derived_ctor() {
                    return self.emit_derived_return(s.argument.as_ref());
                }
                match &s.argument {
                    Some(v) => self.expr(v)?,
                    None => self.b.load_undefined(),
                }
                self.pop_frame_context();
                self.b.ret();
                Ok(())
            }
            Statement::ThrowStatement(s) => {
                self.expr(&s.argument)?;
                self.b.throw();
                Ok(())
            }
            Statement::TryStatement(s) => self.emit_try_catch(s),
            Statement::SwitchStatement(s) => self.emit_switch(s),
            Statement::FunctionDeclaration(f) => {
                // direct-body declarations were hoisted into the prologue;
                // block-level ones initialize at their position
                let id = f.id.as_ref().expect("function declaration");
                let Some(sym) = id.symbol_id.get() else {
                    return self.err(f.span, "function declaration without a binding");
                };
                let hoisted = self
                    .c
                    .facts
                    .hoist_fns
                    .get(&self.fid)
                    .is_some_and(|v| v.iter().any(|(s, _)| *s == sym));
                if !hoisted {
                    let fid = self.c.facts.fn_of_node[&f.node_id.get()];
                    let idx = self.b.constant(Constant::Callable(FunctionId(fid.0)));
                    let sloppy_block_function = self
                        .c
                        .facts
                        .sloppy_block_functions
                        .iter()
                        .any(|(s, _)| *s == sym);
                    if sloppy_block_function {
                        let temps = self.b.temp_scope();
                        self.b.create_closure(idx);
                        let value = self.b.stage_acc(&temps);
                        self.b.load(value);
                        self.store_symbol(sym, id.name.as_ref())?;
                        self.b.load(value);
                        let name_idx = self.b.name(id.name.as_bytes());
                        let feedback = self.b.new_feedback();
                        self.b.store_global(name_idx, feedback);
                    } else {
                        self.b.create_closure(idx);
                        self.store_symbol(sym, id.name.as_ref())?;
                    }
                }
                Ok(())
            }
            Statement::LabeledStatement(s) => self.emit_labeled(s),
            Statement::BreakStatement(s) => {
                self.emit_break_continue(s.span, s.label.as_ref().map(|l| l.name.as_ref()), true)
            }
            Statement::ContinueStatement(s) => {
                self.emit_break_continue(s.span, s.label.as_ref().map(|l| l.name.as_ref()), false)
            }
            Statement::EmptyStatement(_) => Ok(()),
            Statement::ClassDeclaration(c) => {
                let idx = self.c.facts.class_of_node[&c.node_id.get()];
                self.emit_class(idx)
            }
            Statement::DebuggerStatement(s) => self.err(s.span, "debugger statements"),
            Statement::WithStatement(s) => self.err(s.span, "with statements"),
            _ => self.err(stmt.span(), "statement"),
        }
    }

    /// Outer labels (from enclosing LabeledStatements) forwarded to the
    /// innermost loop/switch.
    fn take_labels(&mut self) -> Vec<String> {
        let mut labels = std::mem::take(&mut self.nested_labels);
        labels.reverse(); // outermost first
        labels
    }

    /// Labeled statements around loops forward their label set into the
    /// loop emitter; anything else is a break-only breakable (ES 14.13).
    fn emit_labeled(&mut self, s: &LabeledStatement<'_>) -> Result<(), CompileError> {
        let label = s.label.name.to_string();
        match &s.body {
            Statement::WhileStatement(_)
            | Statement::DoWhileStatement(_)
            | Statement::ForStatement(_)
            | Statement::ForInStatement(_)
            | Statement::SwitchStatement(_)
            | Statement::LabeledStatement(_) => {
                self.nested_labels.push(label);
                let r = self.stmt(&s.body);
                self.nested_labels.pop();
                r
            }
            _ => {
                let breaks = self.b.new_label();
                self.breakables.push(Breakable {
                    labels: vec![label],
                    breaks,
                    continues: None,
                    context_depth: self.contexts.len(),
                });
                let result = self.stmt(&s.body);
                let (breaks, _) = self.end_breakable();
                self.b.bind(breaks);
                result
            }
        }
    }

    fn emit_var_decl(&mut self, d: &VariableDeclaration<'_>) -> Result<(), CompileError> {
        for decl in &d.declarations {
            match &decl.id {
                BindingPattern::ObjectPattern(_) | BindingPattern::ArrayPattern(_) => {
                    let decl_temps = self.b.temp_scope();
                    let Some(init) = &decl.init else {
                        return self
                            .err(decl.span, "destructuring declaration needs an initializer");
                    };
                    self.expr(init)?;
                    let value = self.b.stage_acc(&decl_temps);
                    self.emit_binding_pattern(&decl.id, value)?;
                }
                BindingPattern::BindingIdentifier(b) => {
                    let Some(sym) = b.symbol_id.get() else {
                        return self.err(b.span, "declaration without a binding");
                    };
                    let eval_dynamic = self.is_eval_dynamic_var(sym);
                    if let Some(init) = &decl.init {
                        self.expr(init)?;
                        if self.is_anon_function(init) {
                            self.emit_set_name_const(b.name.as_bytes());
                        }
                        if eval_dynamic {
                            let idx = self.b.name(b.name.as_bytes());
                            self.b.call_runtime_staged(
                                BuiltinFn::StoreDynamicName,
                                &[RtArg::Acc, RtArg::Const(idx)],
                            );
                        } else {
                            self.store_declaration(sym, b.name.as_ref())?;
                        }
                    } else if d.kind == VariableDeclarationKind::Let {
                        // ES InitializeBinding: `let x;` initializes x to
                        // undefined. `var` is already undefined from frame
                        // initialization and must not reset on redeclaration.
                        self.b.load_undefined();
                        self.store_declaration(sym, b.name.as_ref())?;
                    }
                }
                _ => return self.err(decl.span, "var declarator target"),
            }
        }
        Ok(())
    }

    /// `for (left in object) body` (ES 14.7.5): the subject evaluates in
    /// the head scope (lexical bindings are in TDZ there), null/undefined
    /// enumerate nothing, and the loop pulls one key per iteration from
    /// the hidden enumerator. The assignment target re-evaluates per
    /// iteration.
    fn emit_for_in(&mut self, s: &ForInStatement<'_>) -> Result<(), CompileError> {
        let node = s.node_id.get();
        let per_iteration = self.c.facts.per_iteration.contains(&node);
        let slots = self
            .c
            .facts
            .for_head(s.scope_id.get())
            .map_or(0, |head| head.slots);
        let labels = self.take_labels();
        self.with_temps(|g, temps| {
            // lexical head bindings re-enter their TDZ on every execution of
            // the loop statement
            g.emit_scope_hole_fills(s.scope_id.get());
            // lexical heads (`for (let/const k in …)`) own a block
            // context; captured heads get a fresh copy per iteration so
            // closures in the body capture per-iteration bindings
            let mut loop_ctx: Option<Reg> = None;
            let ctx_guard = if slots > 0 {
                g.b.create_block_context(slots);
                let save = temps.temp();
                let lc = temps.temp();
                g.b.store(lc);
                let guard = g.context_scope(save, ContextHost::For(node));
                loop_ctx = Some(lc);
                Some(guard)
            } else {
                None
            };
            // head: subject → enumerator (undefined for nullish subjects;
            // ForInNext(undefined) is immediately done)
            g.expr(&s.right)?;
            let subject = g.b.stage_acc(&temps);
            g.b.call_runtime(BuiltinFn::ForInEnumerate, RegList::new(subject, 1));
            let enumerator = g.b.stage_acc(&temps);

            // loop: next key or undefined
            let back = g.b.new_label();
            g.b.bind(back); // loop head
            let breaks = g.b.new_label();
            let continues = g.b.new_label();
            g.breakables.push(Breakable {
                labels,
                breaks,
                continues: Some(continues),
                context_depth: g.contexts.len(),
            });
            g.b.call_runtime(BuiltinFn::ForInNext, RegList::new(enumerator, 1));
            let have_key = g.b.new_label();
            g.b.jump_if_not_undefined(have_key);
            g.b.jump(breaks);
            g.b.bind(have_key);
            let key = g.b.stage_acc(&temps);

            // per iteration with a lexical head: replace the context with
            // a fresh sibling (same outer) and initialize the binding
            // there — a fresh binding per iteration
            if per_iteration {
                ctx_guard
                    .as_ref()
                    .expect("per-iteration loops own a context")
                    .replace_block(slots, None);
            }

            // assign the key to the target (per iteration), then the body
            g.emit_for_in_assign(&s.left, key)?;
            g.stmt(&s.body)?;

            let continues = g.breakables.last().unwrap().continues.unwrap();
            g.b.bind(continues);
            // absolute restore: a labelled continue may arrive from a
            // nested construct still holding its context (per-iteration
            // heads self-heal at the top of the next iteration)
            if !per_iteration && let Some(lc) = loop_ctx {
                g.b.pop_context(lc);
            }
            g.b.jump_loop(back);
            // absolute restore: break may arrive from arbitrary context
            // depth (labelled jumps past nested loop pops)
            let (breaks, _) = g.end_breakable();
            g.b.bind(breaks);
            if let Some(guard) = ctx_guard {
                drop(guard);
            }

            Ok(())
        })
    }

    /// Store the enumeration key (in register `key`) into the for-in
    /// assignment target. Declaration heads assign their single binding
    /// (patterns destructure); expression targets are stored through the
    /// normal assignment machinery.
    fn emit_for_in_assign(
        &mut self,
        left: &ForStatementLeft<'_>,
        key: Reg,
    ) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        match left {
            ForStatementLeft::VariableDeclaration(d) => {
                let [declarator] = d.declarations.as_slice() else {
                    return self.err(d.span, "for-in declarator");
                };
                match &declarator.id {
                    BindingPattern::BindingIdentifier(b) => {
                        self.b.load(key);
                        let Some(sym) = b.symbol_id.get() else {
                            return self.err(b.span, "for-in binding");
                        };
                        self.store_symbol(sym, b.name.as_ref())
                    }
                    BindingPattern::ObjectPattern(_) | BindingPattern::ArrayPattern(_) => {
                        self.emit_binding_pattern(&declarator.id, key)
                    }
                    _ => self.err(d.span, "for-in binding pattern"),
                }
            }
            ForStatementLeft::AssignmentTargetIdentifier(i) => {
                self.b.load(key);
                self.store_name(i)
            }
            t if assign_member_ref_for_left(t).is_some() => {
                let m = assign_member_ref_for_left(t).unwrap();
                let store = if is_super_member(m) {
                    self.prepare_super_store(m, &temps)?
                } else {
                    self.prepare_property_store(m)?
                };
                self.b.load(key);
                self.emit_property_store(&store);
                drop(store);
                Ok(())
            }
            _ => self.err(left.span(), "for-in assignment target"),
        }
    }

    fn emit_while(
        &mut self,
        test: &Expression<'_>,
        body: &Statement<'_>,
        labels: Vec<String>,
    ) -> Result<(), CompileError> {
        // head: cond; JumpIfFalsy breaks; body; continues; back-edge
        let back = self.b.new_label();
        self.b.bind(back); // loop head
        self.expr(test)?;
        let breaks = self.b.new_label();
        let continues = self.b.new_label();
        self.breakables.push(Breakable {
            labels,
            breaks,
            continues: Some(continues),
            context_depth: self.contexts.len(),
        });
        self.b.jump_if_falsy(breaks);
        self.stmt(body)?;
        let continues = self.breakables.last().unwrap().continues.unwrap();
        self.b.bind(continues);
        self.b.jump_loop(back);
        let (breaks, _) = self.end_breakable();
        self.b.bind(breaks);
        Ok(())
    }

    /// `do body while (test)`: the body runs once before the first test;
    /// `continue` targets the test, and the back-edge stays a `JumpLoop`
    /// so a long loop still reaches safepoints.
    fn emit_do_while(
        &mut self,
        test: &Expression<'_>,
        body: &Statement<'_>,
        labels: Vec<String>,
    ) -> Result<(), CompileError> {
        let back = self.b.new_label();
        self.b.bind(back); // loop head, entered without a test
        let breaks = self.b.new_label();
        let continues = self.b.new_label();
        self.breakables.push(Breakable {
            labels,
            breaks,
            continues: Some(continues),
            context_depth: self.contexts.len(),
        });
        self.stmt(body)?;
        let continues = self.breakables.last().unwrap().continues.unwrap();
        self.b.bind(continues);
        self.expr(test)?;
        self.b.jump_if_falsy(breaks);
        self.b.jump_loop(back);
        let (breaks, _) = self.end_breakable();
        self.b.bind(breaks);
        Ok(())
    }

    /// `for (init; cond; next) body`: lexical heads own a block context;
    /// captured bindings require a fresh environment per iteration with
    /// the values copied forward (ES 14.7.5.4).
    fn emit_for(&mut self, s: &ForStatement<'_>) -> Result<(), CompileError> {
        let node = s.node_id.get();
        let per_iteration = self.c.facts.per_iteration.contains(&node);
        let slots = self
            .c
            .facts
            .for_head(s.scope_id.get())
            .map_or(0, |head| head.slots);
        let labels = self.take_labels();
        self.with_temps(|g, temps| {
            // lexical head bindings re-enter their TDZ on every execution of
            // the loop statement
            g.emit_scope_hole_fills(s.scope_id.get());
            let mut loop_ctx: Option<Reg> = None;
            let ctx_guard = if slots > 0 {
                g.b.create_block_context(slots);
                let save = temps.temp();
                let lc = temps.temp();
                g.b.store(lc);
                let guard = g.context_scope(save, ContextHost::For(node));
                loop_ctx = Some(lc);
                Some(guard)
            } else {
                None
            };
            match &s.init {
                Some(ForStatementInit::VariableDeclaration(d)) => g.emit_var_decl(d)?,
                Some(other) => {
                    g.expr(other.as_expression().expect("init expression"))?;
                }
                None => {}
            }
            // per-iteration state: copy registers for the head bindings
            // and the current iteration's context (merge points restore
            // absolutely — a labelled continue may bypass the pops of
            // nested loops still holding contexts)
            let copies: Vec<Reg>;
            let mut iter_ctx: Option<Reg> = None;
            if per_iteration {
                copies = (0..slots).map(|_| temps.temp()).collect();
                let iter_ctx_reg = temps.temp();
                // the first iteration starts from a copy of the head
                // context: values copied out, fresh sibling pushed
                g.emit_iteration_context_copy(
                    ctx_guard
                        .as_ref()
                        .expect("per-iteration loops own a context"),
                    Some(iter_ctx_reg),
                    slots,
                    &copies,
                );
                iter_ctx = Some(iter_ctx_reg);
            } else {
                copies = Vec::new();
            }
            // head: cond?; JumpIfFalsy breaks; body; continues; update; back-edge
            let back = g.b.new_label();
            g.b.bind(back); // loop head
            let breaks = g.b.new_label();
            let continues = g.b.new_label();
            g.breakables.push(Breakable {
                labels,
                breaks,
                continues: Some(continues),
                context_depth: g.contexts.len(),
            });
            if let Some(cond) = &s.test {
                g.expr(cond)?;
                g.b.jump_if_falsy(breaks);
            }
            g.stmt(&s.body)?;
            let continues = g.breakables.last().unwrap().continues.unwrap();
            g.b.bind(continues);
            if let Some(iter_ctx) = iter_ctx {
                // absolute restore to this iteration's context, then copy
                // its values into a fresh sibling for the next iteration
                // (ES 14.7.5.4: the copy precedes the update)
                g.b.pop_context(iter_ctx);
                g.emit_iteration_context_copy(
                    ctx_guard
                        .as_ref()
                        .expect("per-iteration loops own a context"),
                    Some(iter_ctx),
                    slots,
                    &copies,
                );
            } else if let Some(lc) = loop_ctx {
                g.b.pop_context(lc);
            }
            if let Some(next) = &s.update {
                g.expr(next)?;
            }
            g.b.jump_loop(back);
            // absolute restore: break may arrive from arbitrary context
            // depth (labelled jumps past nested loop pops)
            let (breaks, _) = g.end_breakable();
            g.b.bind(breaks);
            if let Some(guard) = ctx_guard {
                drop(guard);
            }
            Ok(())
        })
    }

    /// Copy a loop head's bindings into a fresh sibling context: read the
    /// slots out of the current context, replace the guarded context with a
    /// fresh sibling (holes) and write the values back in.
    fn emit_iteration_context_copy(
        &mut self,
        guard: &ContextScope<'c, 'a, 'p>,
        iter_ctx: Option<Reg>,
        count: u32,
        copies: &[Reg],
    ) {
        for slot in 0..count {
            self.b.load_context_slot(slot, 0);
            self.b.store(copies[slot as usize]);
        }
        guard.replace_block(count, iter_ctx);
        for slot in 0..count {
            self.b.load(copies[slot as usize]);
            self.b.store_context_slot(slot, 0);
        }
    }

    fn end_breakable(&mut self) -> (Label, Option<Label>) {
        let state = self.breakables.pop().expect("breakable state");
        (state.breaks, state.continues)
    }

    fn emit_break_continue(
        &mut self,
        span: Span,
        label: Option<&str>,
        is_break: bool,
    ) -> Result<(), CompileError> {
        if is_break {
            let idx = match label {
                None => self.breakables.len().checked_sub(1),
                Some(name) => self
                    .breakables
                    .iter()
                    .rposition(|b| b.labels.iter().any(|l| l == name)),
            };
            let Some(idx) = idx else {
                return self.err(span, "break outside a breakable statement");
            };
            let depth = self.breakables[idx].context_depth;
            for i in (depth..self.contexts.len()).rev() {
                self.b.pop_context(self.context_saves[i]);
            }
            let target = self.breakables[idx].breaks;
            self.b.jump(target);
            return Ok(());
        }
        let idx = match label {
            None => self.breakables.iter().rposition(|b| b.continues.is_some()),
            Some(name) => self
                .breakables
                .iter()
                .rposition(|b| b.labels.iter().any(|l| l == name) && b.continues.is_some()),
        };
        let Some(idx) = idx else {
            return self.err(span, "continue outside a loop");
        };
        let depth = self.breakables[idx].context_depth;
        for i in (depth..self.contexts.len()).rev() {
            self.b.pop_context(self.context_saves[i]);
        }
        let target = self.breakables[idx].continues.unwrap();
        self.b.jump(target);
        Ok(())
    }

    fn emit_switch(&mut self, s: &SwitchStatement<'_>) -> Result<(), CompileError> {
        // all case clauses share one lexical scope; its bindings restart in
        // the TDZ on each execution of the switch
        self.emit_scope_hole_fills(s.scope_id.get());
        let temps = self.b.temp_scope();
        let labels = self.take_labels();
        // evaluate the discriminant once into a temp
        self.expr(&s.discriminant)?;
        let d = self.b.stage_acc(&temps);
        let guard = self.enter_block_context(s.scope_id.get(), temps.temp());
        let breaks = self.b.new_label();
        self.breakables.push(Breakable {
            labels,
            breaks,
            continues: None,
            context_depth: self.contexts.len(),
        });

        let cases = &s.cases;
        let bodies: Vec<Label> = (0..cases.len()).map(|_| self.b.new_label()).collect();
        let mut default_idx = None;

        for (i, case) in cases.iter().enumerate() {
            match &case.test {
                Some(test) => {
                    let case_temps = self.b.temp_scope();
                    self.expr(test)?;
                    let t = self.b.stage_acc(&case_temps);
                    self.b.load(d);
                    self.b.equal_strict(t);
                    self.b.jump_if_truthy(bodies[i]);
                }
                None => default_idx = Some(i),
            }
        }

        // no case matched: the default body, or past the switch
        let end = self.b.new_label();
        match default_idx {
            Some(i) => self.b.jump(bodies[i]),
            None => self.b.jump(end),
        }

        // bodies execute in order; fallthrough is just sequential layout
        for (i, case) in cases.iter().enumerate() {
            self.b.bind(bodies[i]);
            for st in &case.consequent {
                self.stmt(st)?;
            }
        }

        let (breaks, _) = self.end_breakable();
        self.b.bind(breaks);
        drop(guard);
        self.b.bind(end);
        Ok(())
    }

    fn emit_try_catch(&mut self, s: &TryStatement<'_>) -> Result<(), CompileError> {
        if s.finalizer.is_some() {
            return self.err(s.span, "finally blocks");
        }
        if s.block.body.is_empty() {
            return Ok(());
        }
        self.with_temps(|g, temps| {
            // snapshot the current context: an exception may unwind out of
            // context-owning constructs (lexical loop heads, class
            // evaluation) whose PushContext the handler entry bypasses;
            // the handler restores absolutely so the catch block's
            // context-slot accesses see the context of the enclosing
            // statement
            let try_ctx = temps.temp();
            g.b.load_context();
            g.b.store(try_ctx);
            // try-block lexicals restart in the TDZ on every execution
            g.emit_scope_hole_fills(s.block.scope_id.get());
            let t = g.b.begin_try();
            for st in &s.block.body {
                g.stmt(st)?;
            }
            g.b.end_try(t);

            let end = g.b.new_label();
            g.b.jump(end);

            // handler entry: the exception arrives in the accumulator
            g.b.handler_entry(t);
            let thrown = g.b.stage_acc(&temps);
            g.b.pop_context(try_ctx);
            if let Some(h) = &s.handler {
                let catch_guard = g.enter_block_context(h.scope_id.get(), temps.temp());
                let body_scope = h.body.scope_id.get();
                let body_guard = if body_scope != h.scope_id.get() {
                    g.enter_block_context(body_scope, temps.temp())
                } else {
                    None
                };
                // catch bindings restart in the TDZ on every catch
                g.emit_scope_hole_fills(body_scope);
                if let Some(param) = &h.param {
                    g.b.load(thrown);
                    match &param.pattern {
                        BindingPattern::ObjectPattern(_) | BindingPattern::ArrayPattern(_) => {
                            let value = g.b.stage_acc(&temps);
                            g.emit_binding_pattern(&param.pattern, value)?;
                        }
                        BindingPattern::BindingIdentifier(b) => {
                            let Some(sym) = b.symbol_id.get() else {
                                return Err(CompileError::new(b.span, "catch param declared"));
                            };
                            g.store_symbol(sym, b.name.as_ref())?;
                        }
                        _ => return g.err(param.span, "catch parameter"),
                    }
                }
                for st in &h.body.body {
                    g.stmt(st)?;
                }
                drop(body_guard);
                drop(catch_guard);
            }

            g.b.bind(end);
            Ok(())
        })
    }

    // -- function body ---------------------------------------------------------

    fn is_derived_ctor(&self) -> bool {
        self.c.facts.functions[self.fid.0 as usize]
            .kind
            .is_derived_class_constructor()
    }

    fn fn_has_instance_fields(&self, fid: Fid) -> bool {
        self.c.facts.functions[fid.0 as usize]
            .class
            .is_some_and(|c| self.c.facts.classes[c.0 as usize].has_instance_fields)
    }

    /// Load this function's own `this`: the context slot when captured
    /// (the bind target of super(), shared with nested arrows), else the
    /// receiver register.
    fn emit_this_load_own(&mut self) {
        match self.c.layouts[self.fid.0 as usize].this_slot {
            Some(slot) => self.b.load_context_slot(slot, 0),
            None => self.b.load(self.b.this_reg()),
        }
    }

    /// `return [expr]` inside a derived constructor: an object result
    /// wins, `undefined` (and missing) return `this` (initialization
    /// checked), other primitives make the [[Construct]] throw (ES
    /// 9.2.2.1 — the primitive escapes and the Construct opcode rejects
    /// it).
    fn emit_derived_return(&mut self, value: Option<&Expression<'_>>) -> Result<(), CompileError> {
        let temps = self.b.temp_scope();
        let Some(v) = value else {
            // `return;` → return this (loaded while the frame context is
            // still pushed: the captured-this slot lives in it)
            self.emit_this_load_own();
            self.emit_this_initialized_check();
            self.pop_frame_context();
            self.b.ret();
            return Ok(());
        };
        self.expr(v)?;
        let t = self.b.stage_acc(&temps);
        // acc === undefined → return this
        self.b.load_undefined();
        let u = self.b.stage_acc(&temps);
        self.b.load(t);
        self.b.equal_strict(u);
        let is_obj = self.b.new_label();
        self.b.jump_if_falsy(is_obj);
        // undefined → return this (context still pushed for the slot read)
        self.emit_this_load_own();
        self.emit_this_initialized_check();
        self.pop_frame_context();
        self.b.ret();
        self.b.bind(is_obj);
        self.b.load(t);
        self.pop_frame_context();
        self.b.ret();
        Ok(())
    }

    /// Fill the register-resident lexical bindings belonging to `scope` with
    /// the hole. Called at every entry to the scope, so a block executed more
    /// than once (a loop body, a repeated switch, a re-entered nested loop)
    /// starts its TDZ again.
    fn emit_scope_hole_fills(&mut self, scope: Option<ScopeId>) {
        let Some(scope) = scope else {
            return;
        };
        let fills = self.c.layouts[self.fid.0 as usize]
            .hole_fills
            .get(&scope)
            .cloned()
            .unwrap_or_default();
        for reg in fills {
            self.b.load_hole();
            self.b.store(Reg::new(reg as i32));
        }
    }

    fn emit_function_body(&mut self) -> Result<(), CompileError> {
        let layout = &self.c.layouts[self.fid.0 as usize];
        // the script (and each eval compilation) tracks its completion
        // value in a dedicated register between ctx_save and the temps
        self.completion = (self.fid.0 == 0).then(|| Reg::new(layout.register_count as i32 + 1));
        self.ctx_save = Reg::new(layout.register_count as i32);
        self.b
            .set_temp_base(layout.register_count + 1 + u32::from(self.completion.is_some()));

        // prologue: functions with context-allocated bindings get a fresh
        // context pushed onto the frame context. Fresh frames are born
        // `undefined` (so `var`s need no setup); lexical registers get the
        // hole explicitly before any parameter/body code can observe them.
        // Functions with no context reuse the closure's context (outer slots
        // resolve through it), and the depth baked into their nested accesses
        // already skips this frame.
        self.frame_context = layout.creates_context;
        if self.frame_context {
            let func = &self.c.facts.functions[self.fid.0 as usize];
            // A function/script context is a var-declaration environment.
            // A sloppy eval root is not: its vars hoist to the caller.
            let eval_root = matches!(
                self.c.facts.mode,
                Mode::Eval | Mode::EvalStrict | Mode::EvalGlobal
            ) && self.fid == Fid(0);
            let declaration = !(eval_root && !func.strict);
            let script = func.kind == FnKind::Script && !eval_root;
            let mut flags = 0;
            if declaration {
                flags |= SCOPE_DECLARATION;
            }
            if script {
                flags |= SCOPE_SCRIPT;
                if self.c.facts.mode == Mode::Repl {
                    flags |= SCOPE_REPL;
                }
            }
            let ctx_info = self.b.constant(Constant::ScopeInfo(ScopeLayout {
                locals: layout.slot_names.clone().into_boxed_slice(),
                flags,
            }));
            self.b.create_function_context(ctx_info);
            self.push_frame_context();
        }

        // lexical declarations start in their TDZ, including pattern-param
        // locals that destructuring will initialize below
        let fn_scope = self.c.fid_scope.get(&self.fid).copied();
        self.emit_scope_hole_fills(fn_scope);

        // A named function expression binds its own name to the closure.
        // oxc already models it as a FunctionExpression binding in the
        // function scope; if a parameter/var/lexical shadows the name, that
        // binding has different flags and there is nothing to initialize.
        let self_name = self.c.fid_scope.get(&self.fid).copied().and_then(|scope| {
            self.c.scoping.iter_bindings_in(scope).find(|&sym| {
                self.c
                    .scoping
                    .symbol_flags(sym)
                    .contains(SymbolFlags::FunctionExpression)
            })
        });
        if let Some(sym) = self_name {
            let name = self.c.scoping.symbol_name(sym).to_string();
            self.b.load_current_closure();
            self.store_symbol(sym, &name)?;
        }

        // parameters: non-simple lists (any default / pattern / rest)
        // stage the incoming arguments, hole-fill the parameter registers,
        // and initialize each binding in order (TDZ until its turn, ES
        // 10.2.11); simple lists only copy context-allocated (captured)
        // parameters into their slots
        let params: Vec<(PatTarget<'_, '_>, Option<&Expression<'_>>, bool)> =
            self.c.facts.functions[self.fid.0 as usize]
                .params
                .iter()
                .map(|p| (PatTarget::Binding(p.pattern), p.default, p.rest))
                .collect();
        let non_simple = params.iter().any(|(t, d, rest)| {
            *rest
                || d.is_some()
                || !matches!(t, PatTarget::Binding(BindingPattern::BindingIdentifier(_)))
        });
        if non_simple {
            let param_temps = self.b.temp_scope();
            let n = params.len() as u32;
            let staged_base = param_temps.reserve(n);
            // stage the incoming arguments (missing ones arrive as
            // undefined through frame padding)
            for i in 0..n {
                let param = self.b.param(i);
                self.b.load(param);
                self.b.store(Reg::new(staged_base.index() + i as i32));
            }
            // rest arrays capture the frame's raw argument list — build
            // them before the registers are hole-filled
            for (i, (_, _, rest)) in params.iter().enumerate() {
                if *rest {
                    let i = i as u32;
                    self.b
                        .call_runtime_staged(BuiltinFn::CreateRestParameter, &[RtArg::Smi(i)]);
                    self.b.store(Reg::new(staged_base.index() + i as i32));
                }
            }
            // parameters start in their TDZ
            for i in 0..n {
                let param = self.b.param(i);
                self.b.load_hole();
                self.b.store(param);
            }
            // left-to-right initialization
            for (i, (target, default, _)) in params.iter().enumerate() {
                let reg = self.b.param(i as u32);
                let staged = Reg::new(staged_base.index() + i as i32);
                if let Some(default) = default {
                    self.b.load(staged);
                    let skip = self.b.new_label();
                    self.b.jump_if_not_undefined(skip);
                    self.expr(default)?;
                    self.b.store(staged);
                    self.b.bind(skip);
                }
                // InitializeBinding: the register (and, when captured, the
                // context slot) receives the value
                self.b.load(staged);
                self.b.store(reg);
                if let PatTarget::Binding(BindingPattern::BindingIdentifier(b)) = target
                    && let Some(sym) = b.symbol_id.get()
                    && let Some(Slot::Ctx { slot, .. }) = self.c.slots.get(&sym)
                {
                    let slot = *slot;
                    self.b.load(reg);
                    self.b.store_context_slot(slot, 0);
                }
                // pattern parameters destructure the bound value
                if matches!(
                    target,
                    PatTarget::Binding(BindingPattern::ArrayPattern(_))
                        | PatTarget::Binding(BindingPattern::ObjectPattern(_))
                ) {
                    self.emit_binding_pattern(
                        match target {
                            PatTarget::Binding(p) => p,
                            _ => unreachable!(),
                        },
                        staged,
                    )?;
                }
            }
        } else {
            // context-allocated parameters: copy the argument into its
            // slot (captured params and direct-eval scopes force params
            // to contexts)
            for (target, _, _) in &params {
                let PatTarget::Binding(BindingPattern::BindingIdentifier(b)) = target else {
                    unreachable!("simple lists have identifier params")
                };
                let Some(sym) = b.symbol_id.get() else {
                    continue;
                };
                if let Some(Slot::Ctx { slot, .. }) = self.c.slots.get(&sym) {
                    let slot = *slot;
                    let index = self.c.facts.param_symbols[&sym];
                    let reg = self.b.param(index);
                    self.b.load(reg);
                    self.b.store_context_slot(slot, 0);
                }
            }
        }

        // store the receiver into the hidden this-slot when a nested
        // arrow captures it
        if let Some(slot) = self.c.layouts[self.fid.0 as usize].this_slot {
            self.b.load(self.b.this_reg());
            self.b.store_context_slot(slot, 0);
        }
        // expose new.target / the running closure to nested arrows
        // (arrow-delegated super() and arrow new.target reads)
        if let Some(slot) = self.c.layouts[self.fid.0 as usize].new_target_slot {
            self.b.load_new_target();
            self.b.store_context_slot(slot, 0);
        }
        if let Some(slot) = self.c.layouts[self.fid.0 as usize].this_function_slot {
            self.b.load_current_closure();
            self.b.store_context_slot(slot, 0);
        }

        let kind = self.c.facts.functions[self.fid.0 as usize].kind;

        // the synthesized default derived constructor forwards every
        // argument to super() and returns the bound this (ES 15.7.13)
        if kind == FnKind::DefaultDerivedCtor {
            self.b.call_runtime(
                BuiltinFn::ConstructSuperAllArgs,
                RegList::new(self.b.this_reg(), 0),
            );
            self.b.store(self.b.this_reg());
            if self.fn_has_instance_fields(self.fid) {
                // InitializeInstanceElements on the bound this:
                // native(ctor, instance)
                self.with_temps(|g, temps| {
                    // (ctor, instance): ctor rides the top slot
                    g.emit_this_load_own();
                    let instance = g.b.stage_acc(&temps);
                    g.b.load_current_closure();
                    let ctor = temps.temp();
                    g.b.store(ctor);
                    g.b.call_runtime(BuiltinFn::InitInstanceFields, RegList::new(instance, 2));
                    Ok(())
                })?;
            }
            self.pop_frame_context();
            self.b.load(self.b.this_reg());
            self.b.ret();
            return Ok(());
        }

        // base class constructors run their instance field initializers
        // right after the receiver exists (ES 7.3.33, before the body)
        if kind == FnKind::BaseClassCtor && self.fn_has_instance_fields(self.fid) {
            self.with_temps(|g, temps| {
                // (ctor, instance): ctor rides the top slot
                g.emit_this_load_own();
                let instance = g.b.stage_acc(&temps);
                g.b.load_current_closure();
                let ctor = temps.temp();
                g.b.store(ctor);
                g.b.call_runtime(BuiltinFn::InitInstanceFields, RegList::new(instance, 2));
                Ok(())
            })?;
        }

        let root_script =
            self.fid == Fid(0) && matches!(self.c.facts.mode, Mode::Script | Mode::Repl);
        if root_script {
            let idx = self
                .b
                .constant(Constant::GlobalDeclarations(self.global_declarations()));
            self.b
                .call_runtime_staged(BuiltinFn::DeclareGlobals, &[RtArg::Const(idx)]);
        } else if self.fid == Fid(0) && matches!(self.c.facts.mode, Mode::Eval | Mode::EvalGlobal) {
            let declarations = self.global_declarations();
            if !declarations.vars.is_empty() || !declarations.funcs.is_empty() {
                let idx = self.b.constant(Constant::GlobalDeclarations(declarations));
                self.b
                    .call_runtime_staged(BuiltinFn::ValidateEvalDeclarations, &[RtArg::Const(idx)]);
            }
        }

        // hoisting: `var`s initialize to undefined and top-level function
        // declarations become closures before any body code runs
        if let Some(vars) = self
            .c
            .facts
            .hoist_vars
            .get(&self.fid)
            .cloned()
            .filter(|_| !root_script)
        {
            for sym in vars {
                let name = self.c.scoping.symbol_name(sym).to_string();
                if self.is_eval_dynamic_var(sym) {
                    let idx = self.b.name(name.as_bytes());
                    self.b
                        .call_runtime_staged(BuiltinFn::DeclareEvalVar, &[RtArg::Const(idx)]);
                    continue;
                }
                match self.c.slot_of(sym) {
                    Slot::Global => unreachable!("root globals are declared in bulk"),
                    Slot::Param { .. } => {
                        // simple-list params are already undefined when
                        // omitted (frame padding); a `var` bound to a
                        // parameter keeps its incoming value
                    }
                    Slot::Local { .. } => {
                        // fresh frames start as undefined
                    }
                    Slot::Ctx { slot, .. } => {
                        self.b.load_undefined();
                        self.b.store_context_slot(slot, 0);
                    }
                    Slot::CtxAt { .. } => unreachable!("vars never live in class/for contexts"),
                    Slot::Repl => unreachable!("REPL lexicals are not hoisted vars"),
                }
            }
        }
        if let Some(fns) = self
            .c
            .facts
            .hoist_fns
            .get(&self.fid)
            .cloned()
            .filter(|_| !root_script)
        {
            for (sym, fid) in fns {
                let name = self.c.scoping.symbol_name(sym).to_string();
                let idx = self.b.constant(Constant::Callable(FunctionId(fid.0)));
                self.b.create_closure(idx);
                if self.is_eval_dynamic_var(sym) {
                    let name_idx = self.b.name(name.as_bytes());
                    self.b.call_runtime_staged(
                        BuiltinFn::DeclareEvalFunction,
                        &[RtArg::Const(name_idx), RtArg::Acc],
                    );
                } else if matches!(self.c.slot_of(sym), Slot::Global) {
                    unreachable!("root globals are declared in bulk");
                } else {
                    self.store_symbol(sym, &name)?;
                }
            }
        }

        // the script's completion value starts as undefined; only
        // value-producing statements overwrite it (see `stmt`)
        if let Some(completion) = self.completion {
            self.b.load_undefined();
            self.b.store(completion);
        }

        // field-initializer functions (`return <init>;`): an anonymous
        // function value is named after the field key (ES 15.7.19)
        let field_key = self.c.facts.functions[self.fid.0 as usize].field_key;
        if field_key.is_some() {
            match &self.c.facts.functions[self.fid.0 as usize].body {
                FnBody::FieldInit(e) => {
                    self.expr(e)?;
                    if self.is_anon_function(e) {
                        self.emit_set_name_for_key_node(field_key.unwrap());
                    }
                    self.pop_frame_context();
                    self.b.ret();
                    return Ok(());
                }
                FnBody::Empty => {
                    self.b.load_undefined();
                    self.pop_frame_context();
                    self.b.ret();
                    return Ok(());
                }
                _ => {}
            }
        }

        match &self.c.facts.functions[self.fid.0 as usize].body {
            FnBody::Script(p) => {
                for stmt in &p.body {
                    self.stmt(stmt)?;
                }
            }
            FnBody::Function(b) => {
                for stmt in &b.statements {
                    self.stmt(stmt)?;
                }
            }
            FnBody::ArrowExpr(e) => {
                self.expr(e)?;
                self.pop_frame_context();
                self.b.ret();
                return Ok(());
            }
            FnBody::FieldInit(_) | FnBody::Empty => {}
        }

        // fallthrough: the script yields its completion value; ordinary
        // functions return undefined; derived constructors return `this`
        // (initialization checked — super() must have run)
        match self.completion {
            Some(completion) => {
                self.pop_frame_context();
                self.b.load(completion);
            }
            None if self.is_derived_ctor() => {
                self.emit_this_load_own();
                self.emit_this_initialized_check();
                self.pop_frame_context();
            }
            None => {
                self.pop_frame_context();
                self.b.load_undefined();
            }
        }
        self.b.ret();
        Ok(())
    }
}
