//! Hand-written recursive descent over the token stream — no lookahead
//! beyond the current token.
//!
//! The one interesting production is the brace literal:
//!
//! ```text
//! object := '{' entry* '}'
//! entry  := slot | statement
//! slot   := name ':' expr          (named)
//!         | name '*' ':' expr      (parent)
//!         | '[' expr ']' ':' expr  (element)
//! ```
//!
//! Each entry is parsed as a statement first; a `:` directly after it
//! reclassifies it as a slot (a `[key]` expression unwraps to the key).
//! The single exception is `name * :` — there the `*` is consumed before
//! the `:` is visible, so a name (and possibly the star) is consumed
//! speculatively and ordinary expression parsing resumes when no `:`
//! follows. Slots must precede `|params|`, which must precede the body.

use crate::{
    CharStream, Scanner, Symbol, SymbolTable, ast::Ast, ast::BinaryOp, ast::Node, ast::NodeId,
    ast::NodeList, ast::ObjectParts, ast::SlotKind, ast::UnaryOp, plumbing::ParseError,
    token::ByteSpan, token::Token, token::TokenKind, token::TokenValue,
};
use std::collections::VecDeque;

fn is_name_kind(kind: TokenKind) -> bool {
    kind == TokenKind::Identifier || kind.is_keyword()
}

/// `*` binds tighter than every other binary operator (its precedence
/// in `TOKEN_INFO`).
const MUL_PREC: u8 = 6;

fn binary_op(kind: TokenKind) -> BinaryOp {
    match kind {
        TokenKind::OrOr => BinaryOp::Or,
        TokenKind::AmpAmp => BinaryOp::And,
        TokenKind::EqEq => BinaryOp::Eq,
        TokenKind::NotEq => BinaryOp::Ne,
        TokenKind::Lt => BinaryOp::Lt,
        TokenKind::Gt => BinaryOp::Gt,
        TokenKind::LtEq => BinaryOp::Le,
        TokenKind::GtEq => BinaryOp::Ge,
        TokenKind::Plus => BinaryOp::Add,
        TokenKind::Minus => BinaryOp::Sub,
        TokenKind::Star => BinaryOp::Mul,
        TokenKind::Slash => BinaryOp::Div,
        TokenKind::Percent => BinaryOp::Mod,
        _ => unreachable!("not a binary operator"),
    }
}

fn unary_op(kind: TokenKind) -> UnaryOp {
    match kind {
        TokenKind::Minus => UnaryOp::Neg,
        TokenKind::Bang => UnaryOp::Not,
        _ => unreachable!("not a unary operator"),
    }
}

enum Entry {
    Slot(NodeId),
    Stmt(NodeId),
}

pub struct Parser<S: CharStream> {
    scanner: Scanner<S>,
    ast: Ast,
    buffer: VecDeque<Token>,
    allow_mod: bool,
    export_target: Option<Symbol>,
    staging_symbol: Symbol,
    module_registry_symbol: Symbol,
    modules_symbol: Symbol,
    declare_symbol: Symbol,
    extend_symbol: Symbol,
    export_symbol: Symbol,
    use_symbol: Symbol,
}

impl<S: CharStream> Parser<S> {
    pub fn new(stream: S) -> Self {
        let mut scanner = Scanner::new(stream);
        let self_symbol = scanner.symbols_mut().intern(b"self");
        let iterator_symbol = scanner.symbols_mut().intern(b"iterator");
        let has_next_symbol = scanner.symbols_mut().intern(b"hasNext");
        let next_symbol = scanner.symbols_mut().intern(b"next");
        let next_keyed_symbol = scanner.symbols_mut().intern(b"nextKeyed");
        let size_symbol = scanner.symbols_mut().intern(b"size");
        let staging_symbol = scanner.symbols_mut().intern(b"$staging");
        let module_registry_symbol = scanner.symbols_mut().intern(b"ModuleRegistry");
        let modules_symbol = scanner.symbols_mut().intern(b"Modules");
        let declare_symbol = scanner.symbols_mut().intern(b"declare");
        let extend_symbol = scanner.symbols_mut().intern(b"extend");
        let export_symbol = scanner.symbols_mut().intern(b"export");
        let use_symbol = scanner.symbols_mut().intern(b"use");
        let mut ast = Ast::new();
        ast.set_self_symbol(self_symbol);
        ast.set_iteration_symbols(
            iterator_symbol,
            has_next_symbol,
            next_symbol,
            next_keyed_symbol,
            size_symbol,
        );
        Self {
            scanner,
            ast,
            buffer: VecDeque::new(),
            allow_mod: true,
            export_target: None,
            staging_symbol,
            module_registry_symbol,
            modules_symbol,
            declare_symbol,
            extend_symbol,
            export_symbol,
            use_symbol,
        }
    }

    pub fn ast(&self) -> &Ast {
        &self.ast
    }

    pub fn symbols(&self) -> &SymbolTable {
        self.scanner.symbols()
    }

    pub fn into_ast(mut self) -> Ast {
        self.ast.set_symbol_table(self.scanner.take_symbols());
        self.ast
    }

    pub fn parse_script(&mut self) -> Result<NodeId, ParseError> {
        let _span = trace::info_span!("kette::parse").entered();
        let start = self.peek()?.span.start;
        let stmts = self.parse_statement_list(TokenKind::Eof)?;
        let end = self.peek()?.span.end;
        let list = self.ast.list(&stmts);
        let root = self
            .ast
            .add(Node::StmtList { stmts: list }, ByteSpan::new(start, end));
        self.ast.set_root(root);
        Ok(root)
    }

    // -- token plumbing ------------------------------------------------------

    fn peek(&mut self) -> Result<Token, ParseError> {
        if let Some(token) = self.buffer.front() {
            return Ok(*token);
        }
        self.scanner.peek().clone()
    }

    fn next(&mut self) -> Result<Token, ParseError> {
        if let Some(token) = self.buffer.pop_front() {
            return Ok(token);
        }
        self.scanner.next_token()
    }

    fn peek_nth(&mut self, n: usize) -> Result<Token, ParseError> {
        if self.buffer.is_empty() {
            self.buffer.push_back(self.scanner.next_token()?);
        }
        while self.buffer.len() <= n {
            self.buffer.push_back(self.scanner.next_token()?);
        }
        Ok(self.buffer[n])
    }

    fn eat(&mut self, kind: TokenKind) -> Result<bool, ParseError> {
        if self.peek()?.kind == kind {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token, ParseError> {
        let t = self.peek()?;
        if t.kind != kind {
            return Err(ParseError::new(
                t.span,
                format!(
                    "expected `{}`, found `{}`",
                    kind.describe(),
                    t.kind.describe()
                ),
            ));
        }
        self.next()
    }

    fn expect_semicolon(&mut self) -> Result<(), ParseError> {
        let t = self.peek()?;
        if t.kind == TokenKind::Semicolon {
            self.next()?;
            return Ok(());
        }
        if t.kind == TokenKind::RBrace || t.kind == TokenKind::Eof || t.after_newline {
            return Ok(());
        }
        Err(ParseError::new(
            t.span,
            format!("expected `;` or newline, found `{}`", t.kind.describe()),
        ))
    }

    fn token_name(&mut self, t: Token) -> Result<Symbol, ParseError> {
        match t.value {
            TokenValue::Symbol(s) => Ok(Symbol(s)),
            _ if t.kind.is_keyword() => {
                Ok(self.scanner.symbols_mut().intern(t.kind.text().as_bytes()))
            }
            _ => Err(ParseError::new(t.span, "expected a name")),
        }
    }

    fn number_node(&mut self, t: Token) -> Result<NodeId, ParseError> {
        let n = t
            .value
            .number()
            .ok_or_else(|| ParseError::new(t.span, "expected a number"))?;
        Ok(self.ast.add(Node::Number(n), t.span))
    }

    /// The primary node for a consumed name-kind token.
    fn name_primary(&mut self, t: Token) -> Result<NodeId, ParseError> {
        let node = match t.kind {
            TokenKind::Identifier => Node::Ident(Symbol(
                t.value.symbol().expect("identifier carries a symbol"),
            )),
            TokenKind::SelfKw => Node::Self_,
            TokenKind::True => Node::Bool(true),
            TokenKind::False => Node::Bool(false),
            TokenKind::Null => Node::Null,
            _ => return Err(ParseError::new(t.span, "expected an expression")),
        };
        Ok(self.ast.add(node, t.span))
    }

    // -- statements ----------------------------------------------------------

    fn parse_statement_list(&mut self, terminator: TokenKind) -> Result<Vec<NodeId>, ParseError> {
        let mut stmts = Vec::new();
        loop {
            if self.peek()?.kind == terminator {
                break;
            }
            if self.eat(TokenKind::Semicolon)? {
                continue;
            }
            if self.peek()?.kind == TokenKind::Eof {
                break;
            }
            let stmt = self.parse_statement()?;
            stmts.push(stmt);
            self.expect_semicolon()?;
        }
        Ok(stmts)
    }

    fn parse_statement(&mut self) -> Result<NodeId, ParseError> {
        if self.peek()?.kind == TokenKind::Mod {
            if !self.allow_mod {
                return Err(ParseError::new(
                    self.peek()?.span,
                    "`mod` is only allowed at the top level or directly inside another `mod`",
                ));
            }
            return self.parse_mod();
        }
        if self.peek()?.kind == TokenKind::Let {
            return self.parse_let();
        }
        if self.at_word(self.use_symbol)? && self.next_is_name()? {
            return self.parse_use();
        }
        if self.export_target.is_some() && self.at_word(self.export_symbol)? {
            return self.parse_export();
        }
        let expr = self.parse_expression()?;
        let span = self.ast.span(expr);
        Ok(self.ast.add(Node::ExprStmt { expr }, span))
    }

    fn at_word(&mut self, word: Symbol) -> Result<bool, ParseError> {
        let token = self.peek()?;
        Ok(token.kind == TokenKind::Identifier && token.value.symbol() == Some(word.0))
    }

    fn next_is_name(&mut self) -> Result<bool, ParseError> {
        Ok(is_name_kind(self.peek_nth(1)?.kind))
    }

    fn parse_module_path(&mut self) -> Result<(Vec<Symbol>, ByteSpan), ParseError> {
        let first = self.next()?;
        if !is_name_kind(first.kind) {
            return Err(ParseError::new(first.span, "expected a module name"));
        }
        let start = first.span.start;
        let mut end = first.span.end;
        let mut parts = vec![self.token_name(first)?];
        while self.eat(TokenKind::Period)? {
            let token = self.next()?;
            if !is_name_kind(token.kind) {
                return Err(ParseError::new(token.span, "expected a module name"));
            }
            end = token.span.end;
            parts.push(self.token_name(token)?);
        }
        Ok((parts, ByteSpan::new(start, end)))
    }

    fn module_name_symbol(&mut self, parts: &[Symbol]) -> Symbol {
        let mut bytes = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if index != 0 {
                bytes.push(b'.');
            }
            bytes.extend_from_slice(self.scanner.symbols().get(*part));
        }
        self.scanner.symbols_mut().intern(&bytes)
    }

    fn modules_path(&mut self, parts: &[Symbol], span: ByteSpan) -> NodeId {
        let mut node = self.ast.add(Node::Ident(self.modules_symbol), span);
        for part in parts {
            node = self.ast.add(
                Node::Get {
                    recv: node,
                    name: *part,
                },
                span,
            );
        }
        node
    }

    fn parse_mod(&mut self) -> Result<NodeId, ParseError> {
        let keyword = self.expect(TokenKind::Mod)?;
        let (parts, path_span) = self.parse_module_path()?;
        let extension = if self.at_word(self.extend_symbol)? {
            self.next()?;
            let token = self.next()?;
            if !is_name_kind(token.kind) {
                return Err(ParseError::new(token.span, "expected an extension name"));
            }
            let name = self.token_name(token)?;
            Some((name, token.span))
        } else {
            None
        };
        let open = self.expect(TokenKind::LBrace)?;

        let saved_export = self.export_target;
        let saved_allow = self.allow_mod;
        self.export_target = Some(self.staging_symbol);
        self.allow_mod = false;

        let mut body_stmts = Vec::new();
        let mut nested = Vec::new();
        loop {
            if self.peek()?.kind == TokenKind::RBrace {
                break;
            }
            if self.eat(TokenKind::Semicolon)? {
                continue;
            }
            if self.peek()?.kind == TokenKind::Mod {
                if extension.is_some() {
                    return Err(ParseError::new(
                        self.peek()?.span,
                        "`mod` is not allowed inside an extension",
                    ));
                }
                nested.push(self.parse_mod()?);
                self.expect_semicolon()?;
                continue;
            }
            body_stmts.push(self.parse_statement()?);
            self.expect_semicolon()?;
        }

        self.export_target = saved_export;
        self.allow_mod = saved_allow;
        let close = self.expect(TokenKind::RBrace)?;

        let body_span = ByteSpan::new(open.span.end, close.span.start);
        let body_list = self.ast.list(&body_stmts);
        let body = self.ast.add(Node::StmtList { stmts: body_list }, body_span);
        let staging_ident = self.ast.add(Node::Ident(self.staging_symbol), path_span);
        let params = self.ast.list(&[staging_ident]);
        let slots = self.ast.list(&[]);
        let closure = self.ast.add(
            Node::Object(ObjectParts {
                slots,
                params: Some(params),
                rest: None,
                body,
            }),
            ByteSpan::new(open.span.start, close.span.end),
        );

        let registry = self
            .ast
            .add(Node::Ident(self.module_registry_symbol), path_span);
        let expr = if let Some((extension_name, extension_span)) = extension {
            let target = self.modules_path(&parts, path_span);
            let extension_name = self.ast.add(Node::String(extension_name), extension_span);
            let args = self.ast.list(&[target, extension_name, closure]);
            self.ast.add(
                Node::Send {
                    recv: registry,
                    name: self.extend_symbol,
                    args,
                },
                path_span,
            )
        } else {
            let name = self.module_name_symbol(&parts);
            let name = self.ast.add(Node::String(name), path_span);
            let args = self.ast.list(&[name, closure]);
            self.ast.add(
                Node::Send {
                    recv: registry,
                    name: self.declare_symbol,
                    args,
                },
                path_span,
            )
        };

        let span = ByteSpan::new(keyword.span.start, close.span.end);
        let statement = self.ast.add(Node::ExprStmt { expr }, span);
        nested.push(statement);
        let stmts = self.ast.list(&nested);
        Ok(self.ast.add(Node::StmtList { stmts }, span))
    }

    fn parse_export(&mut self) -> Result<NodeId, ParseError> {
        let keyword = self.expect(TokenKind::Identifier)?;
        let Some(staging) = self.export_target else {
            return Err(ParseError::new(keyword.span, "`export` outside a module"));
        };
        let name_token = self.next()?;
        if !is_name_kind(name_token.kind) {
            return Err(ParseError::new(name_token.span, "expected an export name"));
        }
        let name = self.token_name(name_token)?;
        self.expect(TokenKind::Assign)?;
        let value = self.parse_expression()?;
        let recv = self.ast.add(Node::Ident(staging), keyword.span);
        let target = self.ast.add(Node::Get { recv, name }, name_token.span);
        let span = ByteSpan::new(keyword.span.start, self.ast.span(value).end);
        let assign = self.ast.add(Node::Assign { target, value }, span);
        Ok(self.ast.add(Node::ExprStmt { expr: assign }, span))
    }

    fn parse_use(&mut self) -> Result<NodeId, ParseError> {
        let keyword = self.expect(TokenKind::Identifier)?;
        let (parts, path_span) = self.parse_module_path()?;
        if self.peek()?.kind == TokenKind::LBrace {
            let (stmts, tree_span) = self.parse_use_tree(&parts)?;
            let list = self.ast.list(&stmts);
            let span = ByteSpan::new(keyword.span.start, tree_span.end);
            return Ok(self.ast.add(Node::StmtList { stmts: list }, span));
        }
        let alias = if self.eat(TokenKind::As)? {
            let token = self.next()?;
            if !is_name_kind(token.kind) {
                return Err(ParseError::new(token.span, "expected a use alias"));
            }
            Some((self.token_name(token)?, token.span))
        } else {
            None
        };
        Ok(self.import_binding(&[], &parts, alias, path_span))
    }

    fn parse_use_tree(&mut self, parent: &[Symbol]) -> Result<(Vec<NodeId>, ByteSpan), ParseError> {
        let open = self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        loop {
            if self.peek()?.kind == TokenKind::RBrace {
                break;
            }
            let (parts, path_span) = self.parse_module_path()?;
            let alias = if self.eat(TokenKind::As)? {
                let token = self.next()?;
                if !is_name_kind(token.kind) {
                    return Err(ParseError::new(token.span, "expected an import alias"));
                }
                Some((self.token_name(token)?, token.span))
            } else {
                None
            };
            if alias.is_none() && self.peek()?.kind == TokenKind::LBrace {
                let mut full = parent.to_vec();
                full.extend_from_slice(&parts);
                let (mut nested, _) = self.parse_use_tree(&full)?;
                stmts.append(&mut nested);
            } else {
                stmts.push(self.import_binding(parent, &parts, alias, path_span));
            }
            if !self.eat(TokenKind::Comma)? {
                break;
            }
        }
        let close = self.expect(TokenKind::RBrace)?;
        if stmts.is_empty() {
            return Err(ParseError::new(
                close.span,
                "expected at least one import name",
            ));
        }
        Ok((stmts, ByteSpan::new(open.span.start, close.span.end)))
    }

    fn import_binding(
        &mut self,
        parent: &[Symbol],
        parts: &[Symbol],
        alias: Option<(Symbol, ByteSpan)>,
        path_span: ByteSpan,
    ) -> NodeId {
        let mut full = parent.to_vec();
        full.extend_from_slice(parts);
        let (bound, bound_span) =
            alias.unwrap_or_else(|| (*full.last().expect("module path has a segment"), path_span));
        let pattern = self.ast.add(Node::Ident(bound), bound_span);
        let init = self.modules_path(&full, path_span);
        let span = ByteSpan::new(path_span.start, self.ast.span(init).end);
        self.ast.add(Node::Let { pattern, init }, span)
    }

    fn parse_let(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::Let)?;
        let pattern = self.parse_pattern()?;
        self.expect(TokenKind::Assign)?;
        let init = self.parse_expression()?;
        let span = ByteSpan::new(kw.span.start, self.ast.span(init).end);
        Ok(self.ast.add(Node::Let { pattern, init }, span))
    }

    /// A binding pattern: a name, `(a, b as c, ...)`, or `[a, b, ...rest]`.
    fn parse_pattern(&mut self) -> Result<NodeId, ParseError> {
        match self.peek()?.kind {
            TokenKind::LParen => self.parse_field_pattern(),
            TokenKind::LBracket => self.parse_array_pattern(),
            _ => {
                let name_tok = self.next()?;
                let name = self.token_name(name_tok)?;
                Ok(self.ast.add(Node::Ident(name), name_tok.span))
            }
        }
    }

    /// `(a, b as c, ...)` — read named fields.
    fn parse_field_pattern(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LParen)?;
        let mut sources = Vec::new();
        let mut names = Vec::new();
        loop {
            if self.peek()?.kind == TokenKind::RParen {
                break;
            }
            let source_tok = self.next()?;
            let source_span = source_tok.span;
            let source = self.token_name(source_tok)?;
            sources.push(self.ast.add(Node::Ident(source), source_span));
            let (bound, bound_span) = if self.eat(TokenKind::As)? {
                let bound_tok = self.next()?;
                let span = bound_tok.span;
                (self.token_name(bound_tok)?, span)
            } else {
                (source, source_span)
            };
            names.push(self.ast.add(Node::Ident(bound), bound_span));
            if !self.eat(TokenKind::Comma)? {
                break;
            }
        }
        let close = self.expect(TokenKind::RParen)?;
        if names.is_empty() {
            return Err(ParseError::new(close.span, "expected at least one name"));
        }
        let sources = self.ast.list(&sources);
        let names = self.ast.list(&names);
        let span = ByteSpan::new(open.span.start, close.span.end);
        Ok(self.ast.add(Node::PatternFields { sources, names }, span))
    }

    /// `[a, b as c, ...rest]` — bind positions, rest as a fresh array.
    fn parse_array_pattern(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LBracket)?;
        let mut elements = Vec::new();
        let mut rest = None;
        loop {
            if self.peek()?.kind == TokenKind::RBracket {
                break;
            }
            if self.peek()?.kind == TokenKind::Period {
                self.next()?;
                self.expect(TokenKind::Period)?;
                self.expect(TokenKind::Period)?;
                let name_tok = self.next()?;
                let name = self.token_name(name_tok)?;
                rest = Some(self.ast.add(Node::Ident(name), name_tok.span));
                break;
            }
            let element = self.parse_pattern()?;
            let element = if matches!(self.ast.node(element), Node::Ident(_)) {
                if self.eat(TokenKind::As)? {
                    let alias_tok = self.next()?;
                    let alias = self.token_name(alias_tok)?;
                    self.ast.add(Node::Ident(alias), alias_tok.span)
                } else {
                    element
                }
            } else {
                if self.peek()?.kind == TokenKind::As {
                    return Err(ParseError::new(
                        self.ast.span(element),
                        "as is only allowed after a name",
                    ));
                }
                element
            };
            elements.push(element);
            if !self.eat(TokenKind::Comma)? {
                break;
            }
        }
        let close = self.expect(TokenKind::RBracket)?;
        let elements = self.ast.list(&elements);
        let span = ByteSpan::new(open.span.start, close.span.end);
        Ok(self.ast.add(Node::PatternArray { elements, rest }, span))
    }

    // -- expressions ---------------------------------------------------------

    fn parse_expression(&mut self) -> Result<NodeId, ParseError> {
        if self.peek()?.kind == TokenKind::Throw {
            return self.parse_throw();
        }
        self.parse_assignment()
    }

    fn parse_throw(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::Throw)?;
        let value = self.parse_expression()?;
        let span = ByteSpan::new(kw.span.start, self.ast.span(value).end);
        Ok(self.ast.add(Node::Throw { value }, span))
    }

    fn parse_assignment(&mut self) -> Result<NodeId, ParseError> {
        let lhs = self.parse_binary(1)?;
        self.parse_assignment_from(lhs)
    }

    /// `= value`, when the parsed left side turns out to be the target.
    fn parse_assignment_from(&mut self, lhs: NodeId) -> Result<NodeId, ParseError> {
        let t = self.peek()?;
        if t.kind == TokenKind::Assign && !t.after_newline {
            self.next()?;
            self.check_assign_target(lhs)?;
            let value = self.parse_assignment()?;
            let span = ByteSpan::new(self.ast.span(lhs).start, self.ast.span(value).end);
            return Ok(self.ast.add(Node::Assign { target: lhs, value }, span));
        }
        Ok(lhs)
    }

    fn check_assign_target(&self, node: NodeId) -> Result<(), ParseError> {
        match self.ast.node(node) {
            Node::Ident(_) | Node::Get { .. } | Node::Index { .. } => Ok(()),
            _ => Err(ParseError::new(
                self.ast.span(node),
                "invalid assignment target",
            )),
        }
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<NodeId, ParseError> {
        let lhs = self.parse_unary()?;
        self.parse_binary_from(lhs, min_prec)
    }

    /// Continue the precedence climb over an already-parsed left side.
    /// A binary operator only continues an expression on the same line:
    /// a leading operator starts a new statement (and lets `||` open the
    /// parameter list on its own line).
    fn parse_binary_from(&mut self, mut lhs: NodeId, min_prec: u8) -> Result<NodeId, ParseError> {
        loop {
            let t = self.peek()?;
            if t.after_newline {
                break;
            }
            let prec = t.kind.precedence();
            if prec == 0 || prec < min_prec {
                break;
            }
            let op_tok = self.next()?;
            let rhs = self.parse_binary(prec + 1)?;
            let span = ByteSpan::new(self.ast.span(lhs).start, self.ast.span(rhs).end);
            lhs = self.ast.add(
                Node::Binary {
                    op: binary_op(op_tok.kind),
                    lhs,
                    rhs,
                },
                span,
            );
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<NodeId, ParseError> {
        let t = self.peek()?;
        match t.kind {
            TokenKind::Minus | TokenKind::Bang => {
                self.next()?;
                let expr = self.parse_unary()?;
                let span = ByteSpan::new(t.span.start, self.ast.span(expr).end);
                Ok(self.ast.add(
                    Node::Unary {
                        op: unary_op(t.kind),
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Return => {
                self.next()?;
                let value = self.parse_unary()?;
                let span = ByteSpan::new(t.span.start, self.ast.span(value).end);
                Ok(self.ast.add(Node::Return { value }, span))
            }
            TokenKind::Break => {
                self.next()?;
                let next = self.peek()?;
                let empty = next.after_newline
                    || matches!(
                        next.kind,
                        TokenKind::RBrace
                            | TokenKind::RParen
                            | TokenKind::RBracket
                            | TokenKind::Semicolon
                            | TokenKind::Else
                            | TokenKind::Eof
                    );
                if empty {
                    return Ok(self.ast.add(
                        Node::Break { value: None },
                        ByteSpan::new(t.span.start, t.span.end),
                    ));
                }
                let value = self.parse_expression()?;
                let span = ByteSpan::new(t.span.start, self.ast.span(value).end);
                Ok(self.ast.add(Node::Break { value: Some(value) }, span))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<NodeId, ParseError> {
        let expr = self.parse_primary()?;
        self.parse_postfix_from(expr)
    }

    /// Continue postfix parsing over an already-parsed primary.
    fn parse_postfix_from(&mut self, mut expr: NodeId) -> Result<NodeId, ParseError> {
        loop {
            let t = self.peek()?;
            match t.kind {
                TokenKind::Period => {
                    self.next()?;
                    let nt = self.peek()?;
                    if nt.kind == TokenKind::Number {
                        self.next()?;
                        let key = self.number_node(nt)?;
                        let span = ByteSpan::new(self.ast.span(expr).start, nt.span.end);
                        expr = self.ast.add(Node::Index { recv: expr, key }, span);
                    } else if is_name_kind(nt.kind) {
                        let name_tok = self.next()?;
                        let name = self.token_name(name_tok)?;
                        expr = self.finish_named(expr, name, name_tok.span.end)?;
                    } else {
                        return Err(ParseError::new(
                            nt.span,
                            format!("expected a name after `.`, found `{}`", nt.kind.describe()),
                        ));
                    }
                }
                TokenKind::LBracket if !t.after_newline => {
                    self.next()?;
                    let key = self.parse_expression()?;
                    let close = self.expect(TokenKind::RBracket)?;
                    let span = ByteSpan::new(self.ast.span(expr).start, close.span.end);
                    expr = self.ast.add(Node::Index { recv: expr, key }, span);
                }
                TokenKind::LParen if !t.after_newline => {
                    // `f(x)` — call shorthand; the callee is the receiver
                    let (args, end) = self.parse_args()?;
                    let span = ByteSpan::new(self.ast.span(expr).start, end);
                    expr = self.ast.add(Node::Call { callee: expr, args }, span);
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn finish_named(
        &mut self,
        recv: NodeId,
        name: Symbol,
        name_end: u32,
    ) -> Result<NodeId, ParseError> {
        let start = self.ast.span(recv).start;
        let next = self.peek()?;
        if next.kind == TokenKind::LParen && !next.after_newline {
            let (args, end) = self.parse_args()?;
            Ok(self
                .ast
                .add(Node::Send { recv, name, args }, ByteSpan::new(start, end)))
        } else {
            Ok(self
                .ast
                .add(Node::Get { recv, name }, ByteSpan::new(start, name_end)))
        }
    }

    fn parse_args(&mut self) -> Result<(NodeList, u32), ParseError> {
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if self.peek()?.kind != TokenKind::RParen {
            loop {
                args.push(self.parse_expression()?);
                if !self.eat(TokenKind::Comma)? {
                    break;
                }
                if self.peek()?.kind == TokenKind::RParen {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::RParen)?;
        Ok((self.ast.list(&args), close.span.end))
    }

    fn parse_primary(&mut self) -> Result<NodeId, ParseError> {
        let t = self.peek()?;
        match t.kind {
            TokenKind::Number => {
                self.next()?;
                self.number_node(t)
            }
            TokenKind::String => {
                self.next()?;
                let sym = Symbol(t.value.symbol().expect("string token carries a symbol"));
                Ok(self.ast.add(Node::String(sym), t.span))
            }
            TokenKind::True | TokenKind::False | TokenKind::Null | TokenKind::SelfKw => {
                self.next()?;
                self.name_primary(t)
            }
            TokenKind::SelfContext => {
                self.next()?;
                Ok(self.ast.add(Node::SelfContext, t.span))
            }
            TokenKind::Identifier => {
                self.next()?;
                self.name_primary(t)
            }
            TokenKind::LParen => {
                self.next()?;
                let expr = self.parse_expression()?;
                self.expect(TokenKind::RParen)?;
                Ok(expr)
            }
            TokenKind::LBrace => self.parse_object(),
            TokenKind::LBracket => self.parse_array(),
            TokenKind::Pipe | TokenKind::OrOr => self.parse_prefixed_function(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Try => self.parse_try(),
            _ => Err(ParseError::new(
                t.span,
                format!("expected an expression, found `{}`", t.kind.describe()),
            )),
        }
    }

    // -- the brace literal ---------------------------------------------------

    /// `{ slots | params | body }`, single pass, entry by entry.
    fn parse_object(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LBrace)?;
        let saved_export = self.export_target;
        self.export_target = None;
        let mut slots = Vec::new();
        let mut stmts = Vec::new();
        let mut params = None;
        let mut rest = None;
        let mut saw_stmt = false;
        loop {
            let t = self.peek()?;
            if t.kind == TokenKind::RBrace {
                break;
            }
            // `|params|` — or `||`, the empty marker (an `||` on its own
            // at an entry boundary cannot continue an expression)
            if t.kind == TokenKind::Pipe || t.kind == TokenKind::OrOr {
                if saw_stmt || params.is_some() {
                    return Err(ParseError::new(t.span, "params must precede the body"));
                }
                let (list, rest_param) = self.parse_params()?;
                params = Some(list);
                rest = rest_param;
                continue;
            }
            if self.eat(TokenKind::Comma)? || self.eat(TokenKind::Semicolon)? {
                continue;
            }
            match self.parse_entry()? {
                Entry::Slot(slot) => {
                    if saw_stmt || params.is_some() {
                        return Err(ParseError::new(
                            self.ast.span(slot),
                            "slots must precede the body",
                        ));
                    }
                    slots.push(slot);
                }
                Entry::Stmt(stmt) => {
                    saw_stmt = true;
                    stmts.push(stmt);
                    self.expect_semicolon()?;
                }
            }
        }
        let close = self.expect(TokenKind::RBrace)?;
        let body_span = ByteSpan::new(open.span.end, close.span.start);
        let body_list = self.ast.list(&stmts);
        let body = self.ast.add(Node::StmtList { stmts: body_list }, body_span);
        let slots = self.ast.list(&slots);
        self.export_target = saved_export;
        let span = ByteSpan::new(open.span.start, close.span.end);
        Ok(self.ast.add(
            Node::Object(ObjectParts {
                slots,
                params,
                rest,
                body,
            }),
            span,
        ))
    }

    /// One entry inside braces: a slot (revealed by a trailing `:`) or a
    /// statement.
    fn parse_entry(&mut self) -> Result<Entry, ParseError> {
        let t = self.peek()?;
        // `let` is never an expression, and never a slot key
        if t.kind == TokenKind::Let {
            return Ok(Entry::Stmt(self.parse_let()?));
        }
        // `if` / `return` parse as (odd) expressions; they are keywords,
        // so they can never begin a slot
        if t.kind == TokenKind::If
            || t.kind == TokenKind::While
            || t.kind == TokenKind::For
            || t.kind == TokenKind::Return
            || t.kind == TokenKind::Break
            || t.kind == TokenKind::Try
            || t.kind == TokenKind::Throw
            || t.kind == TokenKind::SelfContext
        {
            let expr = self.parse_expression()?;
            return Ok(Entry::Stmt(self.expr_stmt(expr)?));
        }
        if is_name_kind(t.kind)
            && let Some(slot) = self.multi_slot()?
        {
            return Ok(Entry::Slot(slot));
        }
        if is_name_kind(t.kind) {
            let name_tok = self.next()?;
            // `name >> value` / `name << value` — an accessor slot
            let accessor = match self.peek()?.kind {
                TokenKind::GtGt => Some(SlotKind::Getter),
                TokenKind::LtLt => Some(SlotKind::Setter),
                _ => None,
            };
            if let Some(kind) = accessor {
                self.next()?;
                let name = self.token_name(name_tok)?;
                let key = self.ast.add(Node::Ident(name), name_tok.span);
                let value = self.parse_expression()?;
                let span = ByteSpan::new(name_tok.span.start, self.ast.span(value).end);
                return Ok(Entry::Slot(
                    self.ast.add(Node::Slot { kind, key, value }, span),
                ));
            }
            // `name :` — a named slot
            if self.peek()?.kind == TokenKind::Colon {
                return Ok(Entry::Slot(self.name_slot(name_tok, SlotKind::Named)?));
            }
            // `name * :` — a parent slot; any other token after `*` means
            // the star was the multiply operator
            if self.peek()?.kind == TokenKind::Star {
                self.next()?;
                if self.peek()?.kind == TokenKind::Colon {
                    return Ok(Entry::Slot(self.name_slot(name_tok, SlotKind::Parent)?));
                }
                let lhs = self.name_primary(name_tok)?;
                let rhs = self.parse_binary(MUL_PREC + 1)?;
                let span = ByteSpan::new(name_tok.span.start, self.ast.span(rhs).end);
                let lhs = self.ast.add(
                    Node::Binary {
                        op: BinaryOp::Mul,
                        lhs,
                        rhs,
                    },
                    span,
                );
                let expr = self.parse_binary_from(lhs, 1)?;
                let expr = self.parse_assignment_from(expr)?;
                return Ok(Entry::Stmt(self.expr_stmt(expr)?));
            }
            // an ordinary expression beginning with the name
            let prim = self.name_primary(name_tok)?;
            let expr = self.parse_postfix_from(prim)?;
            let expr = self.parse_binary_from(expr, 1)?;
            let expr = self.parse_assignment_from(expr)?;
            return Ok(Entry::Stmt(self.expr_stmt(expr)?));
        }
        // everything else: parse the statement; a trailing `:` turns a
        // `[key]` expression into an element slot
        let expr = self.parse_expression()?;
        if self.eat(TokenKind::Colon)? {
            let key = self.element_key(expr)?;
            let value = self.parse_expression()?;
            let span = ByteSpan::new(self.ast.span(key).start, self.ast.span(value).end);
            let slot = self.ast.add(
                Node::Slot {
                    kind: SlotKind::Element,
                    key,
                    value,
                },
                span,
            );
            return Ok(Entry::Slot(slot));
        }
        Ok(Entry::Stmt(self.expr_stmt(expr)?))
    }

    /// `a, b, c: value` — several named slots sharing one initializer.
    fn multi_slot(&mut self) -> Result<Option<NodeId>, ParseError> {
        let first = self.peek_nth(0)?;
        if !is_name_kind(first.kind) {
            return Ok(None);
        }
        let mut count = 1;
        let mut saw_comma = false;
        loop {
            let separator = self.peek_nth(count)?;
            if separator.kind == TokenKind::Colon {
                if !saw_comma {
                    return Ok(None);
                }
                break;
            }
            if separator.kind != TokenKind::Comma {
                return Ok(None);
            }
            saw_comma = true;
            let next = self.peek_nth(count + 1)?;
            if !is_name_kind(next.kind) {
                return Ok(None);
            }
            count += 2;
        }
        let mut keys = Vec::with_capacity((count + 1) / 2);
        for offset in 0..count {
            if offset % 2 == 0 {
                let token = self.next()?;
                let name = self.token_name(token)?;
                keys.push(self.ast.add(Node::Ident(name), token.span));
            } else {
                self.expect(TokenKind::Comma)?;
            }
        }
        self.expect(TokenKind::Colon)?;
        let value = self.parse_expression()?;
        let span = ByteSpan::new(first.span.start, self.ast.span(value).end);
        let keys = self.ast.list(&keys);
        Ok(Some(self.ast.add(
            Node::SlotMulti {
                kind: SlotKind::Named,
                keys,
                value,
            },
            span,
        )))
    }

    fn expr_stmt(&mut self, expr: NodeId) -> Result<NodeId, ParseError> {
        let span = self.ast.span(expr);
        Ok(self.ast.add(Node::ExprStmt { expr }, span))
    }

    /// `name` (already consumed) `:` value.
    fn name_slot(&mut self, name_tok: Token, kind: SlotKind) -> Result<NodeId, ParseError> {
        let name = self.token_name(name_tok)?;
        let key = self.ast.add(Node::Ident(name), name_tok.span);
        self.expect(TokenKind::Colon)?;
        let value = self.parse_expression()?;
        let span = ByteSpan::new(name_tok.span.start, self.ast.span(value).end);
        Ok(self.ast.add(Node::Slot { kind, key, value }, span))
    }

    /// The `[key]` of an element slot, unwrapped from the array literal
    /// the expression parser produced.
    fn element_key(&self, expr: NodeId) -> Result<NodeId, ParseError> {
        if let Node::Array { elements } = self.ast.node(expr) {
            let items = self.ast.list_items(*elements);
            if let [key] = items {
                return Ok(*key);
            }
        }
        Err(ParseError::new(
            self.ast.span(expr),
            "an element slot key is a single `[expr]`",
        ))
    }

    /// Parse `|params|`; a trailing `...name` is the rest parameter.
    fn parse_params(&mut self) -> Result<(NodeList, Option<NodeId>), ParseError> {
        if self.eat(TokenKind::OrOr)? {
            return Ok((self.ast.list(&[]), None));
        }
        self.expect(TokenKind::Pipe)?;
        let mut params = Vec::new();
        let mut rest = None;
        if self.peek()?.kind != TokenKind::Pipe {
            loop {
                if self.peek()?.kind == TokenKind::Period {
                    for _ in 0..3 {
                        self.expect(TokenKind::Period)?;
                    }
                    let t = self.expect(TokenKind::Identifier)?;
                    let sym = Symbol(t.value.symbol().expect("identifier carries a symbol"));
                    rest = Some(self.ast.add(Node::Ident(sym), t.span));
                    if self.peek()?.kind != TokenKind::Pipe {
                        return Err(ParseError::new(
                            self.peek()?.span,
                            "rest parameter must be last",
                        ));
                    }
                    break;
                }
                let t = self.expect(TokenKind::Identifier)?;
                let sym = Symbol(t.value.symbol().expect("identifier carries a symbol"));
                params.push(self.ast.add(Node::Ident(sym), t.span));
                if !self.eat(TokenKind::Comma)? {
                    break;
                }
            }
        }
        self.expect(TokenKind::Pipe)?;
        Ok((self.ast.list(&params), rest))
    }

    fn parse_array(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LBracket)?;
        let mut elements = Vec::new();
        if self.peek()?.kind != TokenKind::RBracket {
            loop {
                elements.push(self.parse_expression()?);
                if !self.eat(TokenKind::Comma)? {
                    break;
                }
                if self.peek()?.kind == TokenKind::RBracket {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::RBracket)?;
        let elements = self.ast.list(&elements);
        let span = ByteSpan::new(open.span.start, close.span.end);
        Ok(self.ast.add(Node::Array { elements }, span))
    }

    // -- control flow --------------------------------------------------------

    fn parse_if(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::If)?;
        let cond = self.parse_expression()?;
        let then = self.parse_branch()?;
        let mut else_ = None;
        if self.peek()?.kind == TokenKind::Else {
            self.next()?;
            else_ = Some(if self.peek()?.kind == TokenKind::If {
                self.parse_if()?
            } else {
                self.parse_branch()?
            });
        }
        let end = else_
            .map(|e| self.ast.span(e).end)
            .unwrap_or_else(|| self.ast.span(then).end);
        let span = ByteSpan::new(kw.span.start, end);
        Ok(self.ast.add(Node::If { cond, then, else_ }, span))
    }

    fn parse_while(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::While)?;
        let cond = self.parse_expression()?;
        let body = self.parse_branch()?;
        let span = ByteSpan::new(kw.span.start, self.ast.span(body).end);
        Ok(self.ast.add(Node::While { cond, body }, span))
    }

    /// `for pattern in iterable { body }`
    fn parse_for(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::For)?;
        let first = self.parse_pattern()?;
        let (pattern, keyed) = if self.eat(TokenKind::Comma)? {
            let start = self.ast.span(first).start;
            let mut end;
            let mut elements = vec![first];
            loop {
                let element = self.parse_pattern()?;
                end = self.ast.span(element).end;
                elements.push(element);
                if !self.eat(TokenKind::Comma)? {
                    break;
                }
            }
            let elements = self.ast.list(&elements);
            let span = ByteSpan::new(start, end);
            (
                self.ast.add(
                    Node::PatternArray {
                        elements,
                        rest: None,
                    },
                    span,
                ),
                true,
            )
        } else {
            (first, false)
        };
        self.expect(TokenKind::In)?;
        let iterable = self.parse_expression()?;
        let body = self.parse_branch()?;
        let span = ByteSpan::new(kw.span.start, self.ast.span(body).end);
        Ok(self.ast.add(
            Node::For {
                keyed,
                pattern,
                iterable,
                body,
            },
            span,
        ))
    }

    fn parse_prefixed_function(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek()?.span.start;
        let (params, rest) = self.parse_params()?;
        let saved_export = self.export_target;
        self.export_target = None;
        let body = self.parse_branch()?;
        self.export_target = saved_export;
        let slots = self.ast.list(&[]);
        let span = ByteSpan::new(start, self.ast.span(body).end);
        Ok(self.ast.add(
            Node::Object(ObjectParts {
                slots,
                params: Some(params),
                rest,
                body,
            }),
            span,
        ))
    }

    fn parse_try(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::Try)?;
        let body = self.parse_branch()?;
        self.expect(TokenKind::Catch)?;
        self.expect(TokenKind::LParen)?;
        let param_tok = self.expect(TokenKind::Identifier)?;
        let param = self.name_primary(param_tok)?;
        self.expect(TokenKind::RParen)?;
        let handler = self.parse_branch()?;
        let span = ByteSpan::new(kw.span.start, self.ast.span(handler).end);
        Ok(self.ast.add(
            Node::Try {
                body,
                param,
                handler,
            },
            span,
        ))
    }

    /// `{ statements }` compiled inline: a plain statement list, not an
    /// object. The leading `{` has not been eaten.
    fn parse_branch(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LBrace)?;
        let saved = self.allow_mod;
        self.allow_mod = false;
        let stmts = self.parse_statement_list(TokenKind::RBrace)?;
        self.allow_mod = saved;
        let close = self.expect(TokenKind::RBrace)?;
        let list = self.ast.list(&stmts);
        Ok(self.ast.add(
            Node::StmtList { stmts: list },
            ByteSpan::new(open.span.start, close.span.end),
        ))
    }
}
