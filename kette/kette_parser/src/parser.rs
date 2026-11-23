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
//!         | quoted ':' expr        (operator/index name: `+`, `[]`, ...)
//! ```
//!
//! Each entry is parsed as a statement first; a `:` directly after it
//! reclassifies it as a slot (a `[key]` expression unwraps to the key).
//! The single exception is `name * :` — there the `*` is consumed before
//! the `:` is visible, so a name (and possibly the star) is consumed
//! speculatively and ordinary expression parsing resumes when no `:`
//! follows. Slots must precede `|params|`, which must precede the body.

use crate::{
    ByteSpan, CharStream, Diagnostic, LineIndex, ParseError, Scanner, Symbol, SyntaxToken,
    ast::Ast, ast::BinaryOp, ast::Node, ast::NodeId, ast::NodeList, ast::ObjectParts,
    ast::SlotKind, ast::UnaryOp, token::Token, token::TokenKind, token::TokenValue,
};
use std::collections::VecDeque;

/// The result of a parse. Errors never abort the parse: the AST contains
/// every statement that could be recovered, and `tokens` is lossless.
pub struct ParseOutput {
    pub ast: Ast,
    pub tokens: Vec<SyntaxToken>,
    pub errors: Vec<ParseError>,
    pub line_index: LineIndex,
}

impl ParseOutput {
    /// Resolve byte spans to 1-based line/column diagnostics.
    pub fn diagnostics(&self, source: &str) -> Vec<Diagnostic> {
        self.errors
            .iter()
            .map(|error| error.diagnostic(&self.line_index, source))
            .collect()
    }
}

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

struct SlotHead {
    kind: SlotKind,
    key: NodeId,
    start: u32,
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
    errors: Vec<ParseError>,
    tokens_consumed: u64,
    last_end: u32,
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
            errors: Vec::new(),
            tokens_consumed: 0,
            last_end: 0,
        }
    }

    /// Parse the whole source. Recovery is unconditional: a malformed
    /// statement never stops later statements from being parsed.
    pub fn parse(mut self) -> ParseOutput {
        let _span = trace::info_span!("kette::parse").entered();
        let start = self.peek().span.start;
        let stmts = self.parse_statement_list(TokenKind::Eof);
        let end = self.peek().span.end;
        let list = self.ast.list(&stmts);
        let root = self
            .ast
            .add(Node::StmtList { stmts: list }, ByteSpan::new(start, end));
        self.ast.set_root(root);

        let tokens = self.scanner.take_tokens();
        let line_starts = self.scanner.take_line_starts();
        let mut errors = self.scanner.take_errors();
        errors.append(&mut self.errors);
        errors.sort_by_key(|error| error.span.start);
        errors.dedup_by(|a, b| a.span == b.span);
        errors.truncate(64);

        self.ast.set_symbol_table(self.scanner.take_symbols());
        ParseOutput {
            ast: self.ast,
            tokens,
            errors,
            line_index: LineIndex::from_line_starts(line_starts),
        }
    }

    // -- token access --------------------------------------------------------

    fn peek(&mut self) -> Token {
        if let Some(token) = self.buffer.front() {
            return *token;
        }
        *self.scanner.peek()
    }

    fn next(&mut self) -> Token {
        self.tokens_consumed += 1;
        let token = if let Some(token) = self.buffer.pop_front() {
            token
        } else {
            self.scanner.next_token()
        };
        self.last_end = token.span.end;
        token
    }

    fn finish_node(&mut self, start: u32, node: Node) -> NodeId {
        self.ast.add(node, ByteSpan::new(start, self.last_end))
    }

    fn peek_nth(&mut self, n: usize) -> Token {
        if self.buffer.is_empty() {
            self.buffer.push_back(self.scanner.next_token());
        }
        while self.buffer.len() <= n {
            self.buffer.push_back(self.scanner.next_token());
        }
        self.buffer[n]
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.peek().kind == kind {
            self.next();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token, ParseError> {
        let t = self.peek();
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
        Ok(self.next())
    }

    fn expect_semicolon(&mut self) -> Result<(), ParseError> {
        let t = self.peek();
        if t.kind == TokenKind::Semicolon {
            self.next();
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

    fn parse_statement_list(&mut self, terminator: TokenKind) -> Vec<NodeId> {
        let mut stmts = Vec::new();
        loop {
            let token = self.peek();
            if token.kind == terminator || token.kind == TokenKind::Eof {
                break;
            }
            if token.kind == TokenKind::Semicolon {
                self.next();
                continue;
            }
            self.parse_statement_into(&mut stmts, terminator);
        }
        stmts
    }

    fn parse_statement_into(&mut self, stmts: &mut Vec<NodeId>, terminator: TokenKind) {
        let before = self.tokens_consumed;
        match self.parse_statement() {
            Ok(stmt) => {
                stmts.push(stmt);
                if let Err(error) = self.expect_semicolon() {
                    self.push_error(error);
                    self.recover_statement();
                }
            }
            Err(error) => {
                self.push_error(error);
                self.recover_statement();
            }
        }
        if self.tokens_consumed == before {
            let next = self.peek();
            if next.kind != terminator && next.kind != TokenKind::Eof {
                self.next();
            }
        }
    }

    fn push_error(&mut self, error: ParseError) {
        if self.errors.len() < 64 {
            self.errors.push(error);
        }
    }

    fn recover_statement(&mut self) {
        let mut depth = 0u32;
        loop {
            let token = self.peek();
            match token.kind {
                TokenKind::Eof => return,
                TokenKind::RBrace if depth == 0 => return,
                TokenKind::Semicolon if depth == 0 => {
                    self.next();
                    return;
                }
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => {
                    depth += 1;
                    self.next();
                }
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                    depth = depth.saturating_sub(1);
                    self.next();
                }
                _ if depth == 0 && token.after_newline => return,
                _ => {
                    self.next();
                }
            }
        }
    }

    fn recover_entry(&mut self) {
        let mut depth = 0u32;
        loop {
            let token = self.peek();
            match token.kind {
                TokenKind::Eof => return,
                TokenKind::RBrace if depth == 0 => return,
                TokenKind::Comma | TokenKind::Semicolon if depth == 0 => {
                    self.next();
                    return;
                }
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => {
                    depth += 1;
                    self.next();
                }
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                    depth = depth.saturating_sub(1);
                    self.next();
                }
                _ if depth == 0 && token.after_newline => return,
                _ => {
                    self.next();
                }
            }
        }
    }

    fn parse_statement(&mut self) -> Result<NodeId, ParseError> {
        if self.peek().kind == TokenKind::Mod {
            if !self.allow_mod {
                return Err(ParseError::new(
                    self.peek().span,
                    "`mod` is only allowed at the top level or directly inside another `mod`",
                ));
            }
            return self.parse_mod();
        }
        if self.peek().kind == TokenKind::Let {
            return self.parse_let();
        }
        if self.at_word(self.use_symbol) && self.next_is_name() {
            return self.parse_use();
        }
        if self.export_target.is_some() && self.at_word(self.export_symbol) {
            return self.parse_export();
        }
        let expr = self.parse_expression()?;
        let span = self.ast.span(expr);
        Ok(self.ast.add(Node::ExprStmt { expr }, span))
    }

    fn at_word(&mut self, word: Symbol) -> bool {
        let token = self.peek();
        token.kind == TokenKind::Identifier && token.value.symbol() == Some(word.0)
    }

    fn next_is_name(&mut self) -> bool {
        is_name_kind(self.peek_nth(1).kind)
    }

    fn parse_module_path(&mut self) -> Result<(Vec<(Symbol, ByteSpan)>, ByteSpan), ParseError> {
        let first = self.next();
        if !is_name_kind(first.kind) {
            return Err(ParseError::new(first.span, "expected a module name"));
        }
        let start = first.span.start;
        let mut end = first.span.end;
        let mut parts = vec![(self.token_name(first)?, first.span)];
        while self.eat(TokenKind::Period) {
            let token = self.next();
            if !is_name_kind(token.kind) {
                return Err(ParseError::new(token.span, "expected a module name"));
            }
            end = token.span.end;
            parts.push((self.token_name(token)?, token.span));
        }
        Ok((parts, ByteSpan::new(start, end)))
    }

    fn module_name_symbol(&mut self, parts: &[(Symbol, ByteSpan)]) -> Symbol {
        let mut bytes = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if index != 0 {
                bytes.push(b'.');
            }
            bytes.extend_from_slice(self.scanner.symbols().get(part.0));
        }
        self.scanner.symbols_mut().intern(&bytes)
    }

    fn modules_path(&mut self, parts: &[(Symbol, ByteSpan)], span: ByteSpan) -> NodeId {
        let mut node = self.ast.add(
            Node::Ident(self.modules_symbol),
            ByteSpan::new(span.start, span.start),
        );
        for &(symbol, part_span) in parts {
            let start = self.ast.span(node).start;
            let name = self.ast.add(Node::Name(symbol), part_span);
            node = self.ast.add(
                Node::Get { recv: node, name },
                ByteSpan::new(start, part_span.end),
            );
        }
        node
    }

    fn parse_mod(&mut self) -> Result<NodeId, ParseError> {
        let keyword = self.expect(TokenKind::Mod)?;
        let (parts, path_span) = self.parse_module_path()?;
        let extension = if self.at_word(self.extend_symbol) {
            self.next();
            let token = self.next();
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
            if self.peek().kind == TokenKind::RBrace {
                break;
            }
            if self.eat(TokenKind::Semicolon) {
                continue;
            }
            if self.peek().kind == TokenKind::Mod {
                if extension.is_some() {
                    let span = self.peek().span;
                    self.push_error(ParseError::new(
                        span,
                        "`mod` is not allowed inside an extension",
                    ));
                }
                match self.parse_mod() {
                    Ok(module) => nested.push(module),
                    Err(error) => {
                        self.push_error(error);
                        self.recover_statement();
                    }
                }
                if let Err(error) = self.expect_semicolon() {
                    self.push_error(error);
                    self.recover_statement();
                }
                continue;
            }
            self.parse_statement_into(&mut body_stmts, TokenKind::RBrace);
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

        let span = ByteSpan::new(keyword.span.start, close.span.end);
        let registry = self
            .ast
            .add(Node::Ident(self.module_registry_symbol), path_span);
        let zero = ByteSpan::new(path_span.start, path_span.start);
        let expr = if let Some((extension_name, extension_span)) = extension {
            let target = self.modules_path(&parts, path_span);
            let extension_name = self.ast.add(Node::String(extension_name), extension_span);
            let name = self.ast.add(Node::Name(self.extend_symbol), zero);
            let args = self.ast.list(&[target, extension_name, closure]);
            self.ast.add(
                Node::Send {
                    recv: registry,
                    name,
                    args,
                },
                span,
            )
        } else {
            let name = self.module_name_symbol(&parts);
            let name = self.ast.add(Node::String(name), path_span);
            let args = self.ast.list(&[name, closure]);
            let send_name = self.ast.add(Node::Name(self.declare_symbol), zero);
            self.ast.add(
                Node::Send {
                    recv: registry,
                    name: send_name,
                    args,
                },
                span,
            )
        };

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
        let name_token = self.next();
        if !is_name_kind(name_token.kind) {
            return Err(ParseError::new(name_token.span, "expected an export name"));
        }
        let symbol = self.token_name(name_token)?;
        self.expect(TokenKind::Assign)?;
        let value = self.parse_expression()?;
        let recv = self.ast.add(
            Node::Ident(staging),
            ByteSpan::new(name_token.span.start, name_token.span.start),
        );
        let name = self.ast.add(Node::Name(symbol), name_token.span);
        let target = self.ast.add(Node::Get { recv, name }, name_token.span);
        let start = keyword.span.start;
        let assign = self.finish_node(start, Node::Assign { target, value });
        Ok(self.finish_node(start, Node::ExprStmt { expr: assign }))
    }

    fn parse_use(&mut self) -> Result<NodeId, ParseError> {
        let keyword = self.expect(TokenKind::Identifier)?;
        let (parts, path_span) = self.parse_module_path()?;
        if self.peek().kind == TokenKind::LBrace {
            let stmts = self.parse_use_tree(&parts)?;
            let list = self.ast.list(&stmts);
            return Ok(self.finish_node(keyword.span.start, Node::StmtList { stmts: list }));
        }
        let alias = if self.eat(TokenKind::As) {
            let token = self.next();
            if !is_name_kind(token.kind) {
                return Err(ParseError::new(token.span, "expected a use alias"));
            }
            Some((self.token_name(token)?, token.span))
        } else {
            None
        };
        Ok(self.import_binding(&[], &parts, alias, path_span))
    }

    fn parse_use_tree(&mut self, parent: &[(Symbol, ByteSpan)]) -> Result<Vec<NodeId>, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        loop {
            if self.peek().kind == TokenKind::RBrace {
                break;
            }
            let (parts, path_span) = self.parse_module_path()?;
            let alias = if self.eat(TokenKind::As) {
                let token = self.next();
                if !is_name_kind(token.kind) {
                    return Err(ParseError::new(token.span, "expected an import alias"));
                }
                Some((self.token_name(token)?, token.span))
            } else {
                None
            };
            if alias.is_none() && self.peek().kind == TokenKind::LBrace {
                let mut full = parent.to_vec();
                full.extend_from_slice(&parts);
                let mut nested = self.parse_use_tree(&full)?;
                stmts.append(&mut nested);
            } else {
                stmts.push(self.import_binding(parent, &parts, alias, path_span));
            }
            if !self.eat(TokenKind::Comma) {
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
        Ok(stmts)
    }

    fn import_binding(
        &mut self,
        parent: &[(Symbol, ByteSpan)],
        parts: &[(Symbol, ByteSpan)],
        alias: Option<(Symbol, ByteSpan)>,
        path_span: ByteSpan,
    ) -> NodeId {
        let mut full = parent.to_vec();
        full.extend_from_slice(parts);
        let (bound, bound_span) =
            alias.unwrap_or_else(|| *full.last().expect("module path has a segment"));
        let pattern = self.ast.add(Node::Ident(bound), bound_span);
        let init = self.modules_path(&full, path_span);
        self.finish_node(path_span.start, Node::Let { pattern, init })
    }

    fn parse_let(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::Let)?;
        let pattern = self.parse_pattern()?;
        self.expect(TokenKind::Assign)?;
        let init = self.parse_expression()?;
        Ok(self.finish_node(kw.span.start, Node::Let { pattern, init }))
    }

    /// A binding pattern: a name, `(a, b as c, ...)`, or `[a, b, ...rest]`.
    fn parse_pattern(&mut self) -> Result<NodeId, ParseError> {
        match self.peek().kind {
            TokenKind::LParen => self.parse_field_pattern(),
            TokenKind::LBracket => self.parse_array_pattern(),
            _ => {
                let name_tok = self.next();
                Ok(self.named_key(name_tok)?)
            }
        }
    }

    /// `(a, b as c, ...)` — read named fields.
    fn parse_field_pattern(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LParen)?;
        let (items, close) = self.parse_comma_list(TokenKind::RParen, |parser| {
            let source_tok = parser.next();
            let source_span = source_tok.span;
            let source = parser.token_name(source_tok)?;
            let source_node = parser.ast.add(Node::Ident(source), source_span);
            let (bound, bound_span) = if parser.eat(TokenKind::As) {
                let bound_tok = parser.next();
                let span = bound_tok.span;
                (parser.token_name(bound_tok)?, span)
            } else {
                (source, source_span)
            };
            let bound_node = parser.ast.add(Node::Ident(bound), bound_span);
            Ok((source_node, bound_node))
        })?;
        if items.is_empty() {
            return Err(ParseError::new(close.span, "expected at least one name"));
        }
        let (sources, names): (Vec<_>, Vec<_>) = items.into_iter().unzip();
        let sources = self.ast.list(&sources);
        let names = self.ast.list(&names);
        Ok(self.finish_node(open.span.start, Node::PatternFields { sources, names }))
    }

    /// `[a, b as c, ...rest]` — bind positions, rest as a fresh array.
    fn parse_array_pattern(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LBracket)?;
        let mut elements = Vec::new();
        let mut rest = None;
        loop {
            if self.peek().kind == TokenKind::RBracket {
                break;
            }
            if self.peek().kind == TokenKind::Period {
                rest = Some(self.parse_rest_name()?);
                break;
            }
            let element = self.parse_pattern()?;
            let element = if matches!(self.ast.node(element), Node::Ident(_)) {
                if self.eat(TokenKind::As) {
                    let alias = self.next();
                    self.named_key(alias)?
                } else {
                    element
                }
            } else {
                if self.peek().kind == TokenKind::As {
                    return Err(ParseError::new(
                        self.ast.span(element),
                        "as is only allowed after a name",
                    ));
                }
                element
            };
            elements.push(element);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBracket)?;
        let elements = self.ast.list(&elements);
        Ok(self.finish_node(open.span.start, Node::PatternArray { elements, rest }))
    }

    // -- expressions ---------------------------------------------------------

    fn parse_expression(&mut self) -> Result<NodeId, ParseError> {
        if self.peek().kind == TokenKind::Throw {
            return self.parse_throw();
        }
        self.parse_assignment()
    }

    fn parse_throw(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::Throw)?;
        let value = self.parse_expression()?;
        Ok(self.finish_node(kw.span.start, Node::Throw { value }))
    }

    fn parse_assignment(&mut self) -> Result<NodeId, ParseError> {
        let lhs = self.parse_binary(1)?;
        self.parse_assignment_from(lhs)
    }

    /// `= value`, when the parsed left side turns out to be the target.
    fn parse_assignment_from(&mut self, lhs: NodeId) -> Result<NodeId, ParseError> {
        let t = self.peek();
        if t.kind == TokenKind::Assign && !t.after_newline {
            self.next();
            self.check_assign_target(lhs)?;
            let value = self.parse_assignment()?;
            let start = self.ast.span(lhs).start;
            return Ok(self.finish_node(start, Node::Assign { target: lhs, value }));
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
            let t = self.peek();
            if t.after_newline {
                break;
            }
            let prec = t.kind.precedence();
            if prec == 0 || prec < min_prec {
                break;
            }
            let op_tok = self.next();
            let rhs = self.parse_binary(prec + 1)?;
            let start = self.ast.span(lhs).start;
            lhs = self.finish_node(
                start,
                Node::Binary {
                    op: binary_op(op_tok.kind),
                    op_span: op_tok.span,
                    lhs,
                    rhs,
                },
            );
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<NodeId, ParseError> {
        let t = self.peek();
        match t.kind {
            TokenKind::Minus | TokenKind::Bang => {
                self.next();
                let expr = self.parse_unary()?;
                Ok(self.finish_node(
                    t.span.start,
                    Node::Unary {
                        op: unary_op(t.kind),
                        op_span: t.span,
                        expr,
                    },
                ))
            }
            TokenKind::Return => {
                self.next();
                let value = self.parse_unary()?;
                Ok(self.finish_node(t.span.start, Node::Return { value }))
            }
            TokenKind::Break => {
                self.next();
                let next = self.peek();
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
                    return Ok(self.finish_node(t.span.start, Node::Break { value: None }));
                }
                let value = self.parse_expression()?;
                Ok(self.finish_node(t.span.start, Node::Break { value: Some(value) }))
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
            let t = self.peek();
            match t.kind {
                TokenKind::Period => {
                    self.next();
                    let nt = self.peek();
                    if nt.kind == TokenKind::Number {
                        self.next();
                        let key = self.number_node(nt)?;
                        let start = self.ast.span(expr).start;
                        expr = self.finish_node(start, Node::Index { recv: expr, key });
                    } else if is_name_kind(nt.kind) {
                        let name_tok = self.next();
                        expr = self.finish_named(expr, name_tok)?;
                    } else {
                        return Err(ParseError::new(
                            nt.span,
                            format!("expected a name after `.`, found `{}`", nt.kind.describe()),
                        ));
                    }
                }
                TokenKind::LBracket if !t.after_newline => {
                    self.next();
                    let key = self.parse_expression()?;
                    self.expect(TokenKind::RBracket)?;
                    let start = self.ast.span(expr).start;
                    expr = self.finish_node(start, Node::Index { recv: expr, key });
                }
                TokenKind::LParen if !t.after_newline => {
                    // `f(x)` — call shorthand; the callee is the receiver
                    let args = self.parse_args()?;
                    let start = self.ast.span(expr).start;
                    expr = self.finish_node(start, Node::Call { callee: expr, args });
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn finish_named(&mut self, recv: NodeId, name_tok: Token) -> Result<NodeId, ParseError> {
        let symbol = self.token_name(name_tok)?;
        let name = self.ast.add(Node::Name(symbol), name_tok.span);
        let start = self.ast.span(recv).start;
        let next = self.peek();
        if next.kind == TokenKind::LParen && !next.after_newline {
            let args = self.parse_args()?;
            Ok(self.finish_node(start, Node::Send { recv, name, args }))
        } else {
            Ok(self.finish_node(start, Node::Get { recv, name }))
        }
    }

    fn parse_args(&mut self) -> Result<NodeList, ParseError> {
        self.expect(TokenKind::LParen)?;
        let (args, _) =
            self.parse_comma_list(TokenKind::RParen, |parser| parser.parse_expression())?;
        Ok(self.ast.list(&args))
    }

    fn parse_comma_list<T>(
        &mut self,
        close: TokenKind,
        mut item: impl FnMut(&mut Self) -> Result<T, ParseError>,
    ) -> Result<(Vec<T>, Token), ParseError> {
        let mut items = Vec::new();
        if self.peek().kind != close {
            loop {
                items.push(item(self)?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.peek().kind == close {
                    break;
                }
            }
        }
        let close = self.expect(close)?;
        Ok((items, close))
    }

    fn parse_primary(&mut self) -> Result<NodeId, ParseError> {
        let t = self.peek();
        match t.kind {
            TokenKind::Number => {
                self.next();
                self.number_node(t)
            }
            TokenKind::String => {
                self.next();
                let sym = Symbol(t.value.symbol().expect("string token carries a symbol"));
                Ok(self.ast.add(Node::String(sym), t.span))
            }
            TokenKind::True | TokenKind::False | TokenKind::Null | TokenKind::SelfKw => {
                self.next();
                self.name_primary(t)
            }
            TokenKind::SelfContext => {
                self.next();
                Ok(self.ast.add(Node::SelfContext, t.span))
            }
            TokenKind::Identifier => {
                self.next();
                self.name_primary(t)
            }
            TokenKind::LParen => {
                self.next();
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
            let t = self.peek();
            if t.kind == TokenKind::RBrace {
                break;
            }
            // `|params|` — or `||`, the empty marker (an `||` on its own
            // at an entry boundary cannot continue an expression)
            if t.kind == TokenKind::Pipe || t.kind == TokenKind::OrOr {
                if saw_stmt || params.is_some() {
                    self.push_error(ParseError::new(t.span, "params must precede the body"));
                }
                match self.parse_params() {
                    Ok((list, rest_param)) => {
                        params = Some(list);
                        rest = rest_param;
                    }
                    Err(error) => {
                        self.push_error(error);
                        self.recover_entry();
                    }
                }
                continue;
            }
            if self.eat(TokenKind::Comma) || self.eat(TokenKind::Semicolon) {
                continue;
            }
            let before = self.tokens_consumed;
            match self.parse_entry() {
                Ok(Entry::Slot(slot)) => {
                    if saw_stmt || params.is_some() {
                        self.push_error(ParseError::new(
                            self.ast.span(slot),
                            "slots must precede the body",
                        ));
                    }
                    slots.push(slot);
                }
                Ok(Entry::Stmt(stmt)) => {
                    saw_stmt = true;
                    stmts.push(stmt);
                    if let Err(error) = self.expect_semicolon() {
                        self.push_error(error);
                        self.recover_entry();
                    }
                }
                Err(error) => {
                    self.push_error(error);
                    self.recover_entry();
                }
            }
            if self.tokens_consumed == before {
                let next = self.peek();
                if next.kind != TokenKind::RBrace && next.kind != TokenKind::Eof {
                    self.next();
                }
            }
        }
        self.export_target = saved_export;
        let close = self.expect(TokenKind::RBrace)?;
        let body_span = ByteSpan::new(open.span.end, close.span.start);
        let body_list = self.ast.list(&stmts);
        let body = self.ast.add(Node::StmtList { stmts: body_list }, body_span);
        let slots = self.ast.list(&slots);
        Ok(self.finish_node(
            open.span.start,
            Node::Object(ObjectParts {
                slots,
                params,
                rest,
                body,
            }),
        ))
    }

    /// One entry inside braces: a slot (revealed by a trailing `:`) or a
    /// statement.
    fn parse_entry(&mut self) -> Result<Entry, ParseError> {
        let t = self.peek();
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
        if let Some(head) = self.slot_head()? {
            let value = self.parse_expression()?;
            return Ok(Entry::Slot(self.finish_node(
                head.start,
                Node::Slot {
                    kind: head.kind,
                    key: head.key,
                    value,
                },
            )));
        }
        if is_name_kind(t.kind) {
            let name_tok = self.next();
            // `name * expr` — the star is multiplication here; a parent slot
            // was already consumed by `slot_head`.
            if self.peek().kind == TokenKind::Star {
                let star = self.next();
                let lhs = self.name_primary(name_tok)?;
                let rhs = self.parse_binary(MUL_PREC + 1)?;
                let lhs = self.finish_node(
                    name_tok.span.start,
                    Node::Binary {
                        op: BinaryOp::Mul,
                        op_span: star.span,
                        lhs,
                        rhs,
                    },
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
        if self.eat(TokenKind::Colon) {
            let key = self.element_key(expr)?;
            let value = self.parse_expression()?;
            let start = self.ast.span(key).start;
            return Ok(Entry::Slot(self.finish_node(
                start,
                Node::Slot {
                    kind: SlotKind::Element,
                    key,
                    value,
                },
            )));
        }
        Ok(Entry::Stmt(self.expr_stmt(expr)?))
    }

    fn slot_head(&mut self) -> Result<Option<SlotHead>, ParseError> {
        let token = self.peek();
        if token.kind == TokenKind::QuotedName {
            let key_tok = self.next();
            if self.peek().kind != TokenKind::Colon {
                return Err(ParseError::new(
                    key_tok.span,
                    "a quoted name is only allowed as a slot key",
                ));
            }
            let symbol = Symbol(key_tok.value.symbol().expect("quoted name has a symbol"));
            let key = self.ast.add(Node::Ident(symbol), key_tok.span);
            self.expect(TokenKind::Colon)?;
            return Ok(Some(SlotHead {
                kind: SlotKind::Named,
                key,
                start: key_tok.span.start,
            }));
        }
        if !is_name_kind(token.kind) {
            return Ok(None);
        }
        let next = self.peek_nth(1);
        let (kind, accessor) = match next.kind {
            TokenKind::Colon => (SlotKind::Named, false),
            TokenKind::Star if self.peek_nth(2).kind == TokenKind::Colon => {
                (SlotKind::Parent, true)
            }
            TokenKind::GtGt => (SlotKind::Getter, true),
            TokenKind::LtLt => (SlotKind::Setter, true),
            _ => return Ok(None),
        };
        let key_tok = self.next();
        let key = self.named_key(key_tok)?;
        if accessor {
            self.next();
        }
        if matches!(kind, SlotKind::Named | SlotKind::Parent) {
            self.expect(TokenKind::Colon)?;
        }
        Ok(Some(SlotHead {
            kind,
            key,
            start: key_tok.span.start,
        }))
    }

    fn named_key(&mut self, token: Token) -> Result<NodeId, ParseError> {
        let symbol = self.token_name(token)?;
        Ok(self.ast.add(Node::Ident(symbol), token.span))
    }

    fn parse_rest_name(&mut self) -> Result<NodeId, ParseError> {
        self.expect(TokenKind::Period)?;
        self.expect(TokenKind::Period)?;
        self.expect(TokenKind::Period)?;
        let token = self.expect(TokenKind::Identifier)?;
        self.named_key(token)
    }

    /// `a, b, c: value` — several named slots sharing one initializer.
    fn multi_slot(&mut self) -> Result<Option<NodeId>, ParseError> {
        let first = self.peek_nth(0);
        if !is_name_kind(first.kind) {
            return Ok(None);
        }
        let mut count = 1;
        let mut saw_comma = false;
        loop {
            let separator = self.peek_nth(count);
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
            let next = self.peek_nth(count + 1);
            if !is_name_kind(next.kind) {
                return Ok(None);
            }
            count += 2;
        }
        let mut keys = Vec::with_capacity(count.div_ceil(2));
        for offset in 0..count {
            if offset % 2 == 0 {
                let token = self.next();
                let name = self.token_name(token)?;
                keys.push(self.ast.add(Node::Ident(name), token.span));
            } else {
                self.expect(TokenKind::Comma)?;
            }
        }
        self.expect(TokenKind::Colon)?;
        let value = self.parse_expression()?;
        let keys = self.ast.list(&keys);
        Ok(Some(self.finish_node(
            first.span.start,
            Node::SlotMulti {
                kind: SlotKind::Named,
                keys,
                value,
            },
        )))
    }

    fn expr_stmt(&mut self, expr: NodeId) -> Result<NodeId, ParseError> {
        let start = self.ast.span(expr).start;
        Ok(self.finish_node(start, Node::ExprStmt { expr }))
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
        if self.eat(TokenKind::OrOr) {
            return Ok((self.ast.list(&[]), None));
        }
        self.expect(TokenKind::Pipe)?;
        let mut params = Vec::new();
        let mut rest = None;
        if self.peek().kind != TokenKind::Pipe {
            loop {
                if self.peek().kind == TokenKind::Period {
                    rest = Some(self.parse_rest_name()?);
                    if self.peek().kind != TokenKind::Pipe {
                        return Err(ParseError::new(
                            self.peek().span,
                            "rest parameter must be last",
                        ));
                    }
                    break;
                }
                let token = self.expect(TokenKind::Identifier)?;
                params.push(self.named_key(token)?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(TokenKind::Pipe)?;
        Ok((self.ast.list(&params), rest))
    }

    fn parse_array(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LBracket)?;
        let (elements, _) =
            self.parse_comma_list(TokenKind::RBracket, |parser| parser.parse_expression())?;
        let elements = self.ast.list(&elements);
        Ok(self.finish_node(open.span.start, Node::Array { elements }))
    }

    // -- control flow --------------------------------------------------------

    fn parse_if(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::If)?;
        let cond = self.parse_expression()?;
        let then = self.parse_branch()?;
        let mut else_ = None;
        if self.peek().kind == TokenKind::Else {
            self.next();
            else_ = Some(if self.peek().kind == TokenKind::If {
                self.parse_if()?
            } else {
                self.parse_branch()?
            });
        }
        Ok(self.finish_node(kw.span.start, Node::If { cond, then, else_ }))
    }

    fn parse_while(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::While)?;
        let cond = self.parse_expression()?;
        let body = self.parse_branch()?;
        Ok(self.finish_node(kw.span.start, Node::While { cond, body }))
    }

    /// `for pattern in iterable { body }`
    fn parse_for(&mut self) -> Result<NodeId, ParseError> {
        let kw = self.expect(TokenKind::For)?;
        let first = self.parse_pattern()?;
        let (pattern, keyed) = if self.eat(TokenKind::Comma) {
            let start = self.ast.span(first).start;
            let mut elements = vec![first];
            loop {
                elements.push(self.parse_pattern()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            let elements = self.ast.list(&elements);
            (
                self.finish_node(
                    start,
                    Node::PatternArray {
                        elements,
                        rest: None,
                    },
                ),
                true,
            )
        } else {
            (first, false)
        };
        self.expect(TokenKind::In)?;
        let iterable = self.parse_expression()?;
        let body = self.parse_branch()?;
        Ok(self.finish_node(
            kw.span.start,
            Node::For {
                keyed,
                pattern,
                iterable,
                body,
            },
        ))
    }

    fn parse_prefixed_function(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek().span.start;
        let (params, rest) = self.parse_params()?;
        let saved_export = self.export_target;
        self.export_target = None;
        let body = self.parse_branch()?;
        self.export_target = saved_export;
        let slots = self.ast.list(&[]);
        Ok(self.finish_node(
            start,
            Node::Object(ObjectParts {
                slots,
                params: Some(params),
                rest,
                body,
            }),
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
        Ok(self.finish_node(
            kw.span.start,
            Node::Try {
                body,
                param,
                handler,
            },
        ))
    }

    /// `{ statements }` compiled inline: a plain statement list, not an
    /// object. The leading `{` has not been eaten.
    fn parse_branch(&mut self) -> Result<NodeId, ParseError> {
        let open = self.expect(TokenKind::LBrace)?;
        let saved = self.allow_mod;
        self.allow_mod = false;
        let stmts = self.parse_statement_list(TokenKind::RBrace);
        self.allow_mod = saved;
        self.expect(TokenKind::RBrace)?;
        let list = self.ast.list(&stmts);
        Ok(self.finish_node(open.span.start, Node::StmtList { stmts: list }))
    }
}
