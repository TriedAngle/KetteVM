use std::collections::HashMap;

use crate::ast::{
    AstNode, BinaryExpr, CallExpr, ExportDecl, Expr, ForExpr, IfExpr, IndexExpr, LetStmt, Object,
    ObjectEntry, SendExpr, Slot, SlotEntry, SlotKind, SlotMulti, Stmt, ThrowExpr, TryExpr,
    UnaryExpr, WhileExpr,
};
use crate::{TextRange, Tree};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: TextRange,
    pub message: String,
}

pub fn validate(tree: &Tree) -> Vec<Diagnostic> {
    let mut validator = Validator {
        diagnostics: Vec::new(),
    };
    validator.statements(tree.script().statements(), false);
    validator
        .diagnostics
        .sort_by_key(|diagnostic| diagnostic.range.start);
    validator
        .diagnostics
        .dedup_by(|a, b| a.range == b.range && a.message == b.message);
    validator.diagnostics
}

#[derive(Default)]
struct SlotUsage {
    data: u32,
    parent: u32,
    getter: u32,
    setter: u32,
}

struct Validator {
    diagnostics: Vec<Diagnostic>,
}

impl Validator {
    fn error(&mut self, range: TextRange, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            range,
            message: message.into(),
        });
    }

    fn statements<'a>(&mut self, statements: crate::AstChildren<'a, Stmt<'a>>, in_module: bool) {
        for statement in statements {
            self.statement(statement, in_module);
        }
    }

    fn statement(&mut self, statement: Stmt<'_>, in_module: bool) {
        match statement {
            Stmt::Let(statement) => self.let_statement(statement, in_module),
            Stmt::Expr(statement) => {
                if let Some(expr) = statement.expr() {
                    self.expr(expr, in_module);
                }
            }
            Stmt::Mod(module) => {
                self.statements(module.statements(), true);
            }
            Stmt::Use(_) => {}
            Stmt::Export(export) => self.export(export, in_module),
        }
    }

    fn let_statement(&mut self, statement: LetStmt<'_>, in_module: bool) {
        if let Some(value) = statement.value() {
            self.expr(value, in_module);
        }
    }

    fn export(&mut self, export: ExportDecl<'_>, in_module: bool) {
        if !in_module {
            self.error(
                export.syntax().text_range(),
                "`export` is only valid inside a module",
            );
        }
        if let Some(value) = export.value() {
            self.expr(value, in_module);
        }
    }

    fn expr(&mut self, expr: Expr<'_>, in_module: bool) {
        match expr {
            Expr::Atom(_) => {}
            Expr::Paren(paren) => {
                if let Some(inner) = paren.expr() {
                    self.expr(inner, in_module);
                }
            }
            Expr::Object(object) => self.object(object, false, in_module),
            Expr::Array(array) => {
                for element in array.elements() {
                    self.expr(element, in_module);
                }
            }
            Expr::If(expr) => self.if_expr(expr, in_module),
            Expr::While(expr) => self.while_expr(expr, in_module),
            Expr::For(expr) => self.for_expr(expr, in_module),
            Expr::Try(expr) => self.try_expr(expr, in_module),
            Expr::Throw(expr) => self.throw_expr(expr, in_module),
            Expr::Return(statement) => {
                if let Some(value) = statement.value() {
                    self.expr(value, in_module);
                }
            }
            Expr::Break(statement) => {
                if let Some(value) = statement.value() {
                    self.expr(value, in_module);
                }
            }
            Expr::Unary(expr) => self.unary_expr(expr, in_module),
            Expr::Binary(expr) => self.binary_expr(expr, in_module),
            Expr::Assign(expr) => {
                if let Some(place) = expr.place() {
                    self.expr(place, in_module);
                }
                if let Some(value) = expr.value() {
                    self.expr(value, in_module);
                }
            }
            Expr::Get(expr) => {
                if let Some(receiver) = expr.receiver() {
                    self.expr(receiver, in_module);
                }
            }
            Expr::Index(expr) => self.index_expr(expr, in_module),
            Expr::Send(expr) => self.send_expr(expr, in_module),
            Expr::Call(expr) => self.call_expr(expr, in_module),
        }
    }

    fn unary_expr(&mut self, expr: UnaryExpr<'_>, in_module: bool) {
        if let Some(operand) = expr.operand() {
            self.expr(operand, in_module);
        }
    }

    fn binary_expr(&mut self, expr: BinaryExpr<'_>, in_module: bool) {
        if let Some(lhs) = expr.lhs() {
            self.expr(lhs, in_module);
        }
        if let Some(rhs) = expr.rhs() {
            self.expr(rhs, in_module);
        }
    }

    fn index_expr(&mut self, expr: IndexExpr<'_>, in_module: bool) {
        if let Some(receiver) = expr.receiver() {
            self.expr(receiver, in_module);
        }
        if let Some(index) = expr.index() {
            self.expr(index, in_module);
        }
    }

    fn send_expr(&mut self, expr: SendExpr<'_>, in_module: bool) {
        if let Some(receiver) = expr.receiver() {
            self.expr(receiver, in_module);
        }
        if let Some(args) = expr.args() {
            for arg in args.args() {
                self.expr(arg, in_module);
            }
        }
    }

    fn call_expr(&mut self, expr: CallExpr<'_>, in_module: bool) {
        if let Some(receiver) = expr.receiver() {
            self.expr(receiver, in_module);
        }
        if let Some(args) = expr.args() {
            for arg in args.args() {
                self.expr(arg, in_module);
            }
        }
    }

    fn throw_expr(&mut self, expr: ThrowExpr<'_>, in_module: bool) {
        if let Some(value) = expr.value() {
            self.expr(value, in_module);
        }
    }

    fn if_expr(&mut self, expr: IfExpr<'_>, in_module: bool) {
        if let Some(condition) = expr.condition() {
            self.expr(condition, in_module);
        }
        if let Some(then_branch) = expr.then_branch() {
            self.object(then_branch, true, in_module);
        }
        if let Some(else_branch) = expr.else_branch() {
            match else_branch {
                Expr::Object(object) => self.object(object, true, in_module),
                other => self.expr(other, in_module),
            }
        }
    }

    fn while_expr(&mut self, expr: WhileExpr<'_>, in_module: bool) {
        if let Some(condition) = expr.condition() {
            self.expr(condition, in_module);
        }
        if let Some(body) = expr.body() {
            self.object(body, true, in_module);
        }
    }

    fn for_expr(&mut self, expr: ForExpr<'_>, in_module: bool) {
        if let Some(iterable) = expr.iterable() {
            self.expr(iterable, in_module);
        }
        if let Some(body) = expr.body() {
            self.object(body, true, in_module);
        }
    }

    fn try_expr(&mut self, expr: TryExpr<'_>, in_module: bool) {
        if let Some(body) = expr.body() {
            self.object(body, true, in_module);
        }
        if let Some(handler) = expr.catch() {
            self.object(handler, true, in_module);
        }
    }

    fn object(&mut self, object: Object<'_>, control: bool, in_module: bool) {
        let mut seen_params = false;
        let mut seen_body = false;
        let mut names: HashMap<String, SlotUsage> = HashMap::new();
        let mut indices: HashMap<u32, SlotUsage> = HashMap::new();

        for entry in object.entries() {
            match entry {
                ObjectEntry::Slot(slot) => {
                    if control {
                        self.error(
                            slot.syntax().text_range(),
                            "slots are not allowed in control bodies",
                        );
                    }
                    if seen_params {
                        self.error(slot.syntax().text_range(), "slots must precede parameters");
                    }
                    if seen_body {
                        self.error(slot.syntax().text_range(), "slots must precede the body");
                    }
                    self.slot_entry(slot, &mut names, &mut indices, in_module);
                }
                ObjectEntry::Params(params) => {
                    if seen_params {
                        self.error(params.syntax().text_range(), "duplicate parameter list");
                    }
                    if seen_body {
                        self.error(
                            params.syntax().text_range(),
                            "parameters must precede the body",
                        );
                    }
                    seen_params = true;
                }
                ObjectEntry::Let(statement) => {
                    seen_body = true;
                    self.let_statement(statement, in_module);
                }
                ObjectEntry::Export(export) => {
                    seen_body = true;
                    self.export(export, in_module);
                }
                ObjectEntry::Expr(statement) => {
                    seen_body = true;
                    if let Some(expr) = statement.expr() {
                        self.expr(expr, in_module);
                    }
                }
            }
        }
    }

    fn slot_entry(
        &mut self,
        entry: SlotEntry<'_>,
        names: &mut HashMap<String, SlotUsage>,
        indices: &mut HashMap<u32, SlotUsage>,
        in_module: bool,
    ) {
        match entry {
            SlotEntry::Slot(slot) => self.slot(slot, names, indices, in_module),
            SlotEntry::Multi(multi) => self.multi_slot(multi, names, in_module),
        }
    }

    fn slot(
        &mut self,
        slot: Slot<'_>,
        names: &mut HashMap<String, SlotUsage>,
        indices: &mut HashMap<u32, SlotUsage>,
        in_module: bool,
    ) {
        let Some(kind) = slot.kind() else {
            self.error(slot.syntax().text_range(), "invalid slot head");
            return;
        };
        let Some(key) = slot.key_text() else {
            self.error(slot.syntax().text_range(), "slot has no key");
            return;
        };
        let range = slot.syntax().text_range();
        match kind {
            SlotKind::Named | SlotKind::Quoted => {
                let usage = names.entry(key.to_string()).or_default();
                usage.data += 1;
                if usage.data > 1 {
                    self.error(range, format!("duplicate slot `{key}`"));
                }
                if usage.getter + usage.setter > 0 {
                    self.error(range, format!("slot `{key}` conflicts with an accessor"));
                }
            }
            SlotKind::Parent => {
                let usage = names.entry(key.to_string()).or_default();
                usage.parent += 1;
                if usage.parent > 1 {
                    self.error(range, format!("duplicate parent slot `{key}`"));
                }
            }
            SlotKind::Getter | SlotKind::Setter => {
                let usage = names.entry(key.to_string()).or_default();
                if usage.data > 0 {
                    self.error(range, format!("accessor `{key}` conflicts with a slot"));
                }
                match kind {
                    SlotKind::Getter => usage.getter += 1,
                    SlotKind::Setter => usage.setter += 1,
                    _ => {}
                }
                if usage.getter > 1 {
                    self.error(range, format!("duplicate getter `{key}`"));
                }
                if usage.setter > 1 {
                    self.error(range, format!("duplicate setter `{key}`"));
                }
            }
            SlotKind::Numeric => match key.parse::<u32>() {
                Ok(index) => {
                    let usage = indices.entry(index).or_default();
                    usage.data += 1;
                    if usage.data > 1 {
                        self.error(range, format!("duplicate numeric slot `{key}`"));
                    }
                }
                Err(_) => self.error(range, format!("invalid numeric slot key `{key}`")),
            },
        }
        if let Some(value) = slot.value() {
            self.expr(value, in_module);
        }
    }

    fn multi_slot(
        &mut self,
        slot: SlotMulti<'_>,
        names: &mut HashMap<String, SlotUsage>,
        in_module: bool,
    ) {
        let range = slot.syntax().text_range();
        let mut count = 0;
        for key in slot.key_texts() {
            count += 1;
            let usage = names.entry(key.to_string()).or_default();
            usage.data += 1;
            if usage.data > 1 {
                self.error(range, format!("duplicate slot `{key}`"));
            }
            if usage.getter + usage.setter > 0 {
                self.error(range, format!("slot `{key}` conflicts with an accessor"));
            }
        }
        if count < 2 {
            self.error(range, "a multi slot needs at least two keys");
        }
        if let Some(value) = slot.value() {
            self.expr(value, in_module);
        }
    }
}
