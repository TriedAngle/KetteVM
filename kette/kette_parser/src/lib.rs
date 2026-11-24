mod scanner;

use std::sync::Arc;

use kette_syntax::SyntaxKind::{self, *};
use kette_syntax::name::NameId;
use kette_syntax::ops::{AssignOp, BinaryOp};
use kette_syntax::{Builder, NodeId, TextRange, Tree};
use scanner::{Lexeme, scan};

#[derive(Clone, Debug)]
pub struct ParseError {
    pub range: TextRange,
    pub message: std::string::String,
}

pub struct Parse {
    pub tree: Tree,
    pub errors: Vec<ParseError>,
}

#[derive(Clone, Copy)]
struct Sig {
    kind: SyntaxKind,
    range: TextRange,
    after_newline: bool,
    name: Option<NameId>,
}

struct Parser<'a> {
    source: &'a str,
    lexemes: Vec<Lexeme>,
    sig: Vec<usize>,
    pos: usize,
    emitted: usize,
    builder: Builder,
    last_end: u32,
    errors: Vec<ParseError>,
    in_module: bool,
    allow_mod: bool,
}

pub fn parse(source: &str) -> Parse {
    let (lexemes, names, lex_errors) = scan(source);
    let mut parser = Parser::new(source, lexemes);
    parser.builder.set_names(Arc::new(names));
    parser.errors.extend(lex_errors);
    parser.parse_script();
    let tree = parser.builder.finish();
    let mut errors = parser.errors;
    errors.sort_by_key(|error| error.range.start);
    errors.dedup_by(|a, b| a.range == b.range && a.message == b.message);
    errors.truncate(64);
    Parse { tree, errors }
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, lexemes: Vec<Lexeme>) -> Self {
        let sig = lexemes
            .iter()
            .enumerate()
            .filter_map(|(index, lexeme)| (!lexeme.trivia).then_some(index))
            .collect();
        Self {
            source,
            lexemes,
            sig,
            pos: 0,
            emitted: 0,
            builder: Builder::new(Arc::from(source)),
            last_end: 0,
            errors: Vec::new(),
            in_module: false,
            allow_mod: true,
        }
    }

    fn parse_script(&mut self) {
        self.builder.start_node(Script, 0);
        self.flush_trivia();
        loop {
            let token = self.peek();
            if token.kind == SyntaxKind::Eof {
                break;
            }
            if token.kind == Semicolon {
                self.bump();
                continue;
            }
            let depth = self.builder.depth();
            if self.parse_statement().is_err() {
                self.builder.close_to(depth);
                self.recover_statement();
                continue;
            }
            let next = self.peek();
            if next.kind == Semicolon {
                self.bump();
            } else if next.kind != SyntaxKind::Eof && !next.after_newline {
                self.record_error(next.range, "expected `;` or a newline");
                self.recover_statement();
            }
        }
        self.flush_trivia();
        self.builder.finish_node(self.source.len() as u32);
    }

    fn peek(&self) -> Sig {
        self.sig_at(0)
    }

    /// The next two lookahead tokens.
    fn peek2(&self) -> (Sig, Sig) {
        (self.sig_at(1), self.sig_at(2))
    }

    fn sig_at(&self, offset: usize) -> Sig {
        match self.sig.get(self.pos + offset).copied() {
            Some(index) => {
                let lexeme = self.lexemes[index];
                Sig {
                    kind: lexeme.kind,
                    range: lexeme.range,
                    after_newline: lexeme.after_newline,
                    name: lexeme.name,
                }
            }
            None => Sig {
                kind: SyntaxKind::Eof,
                range: TextRange::empty(self.source.len() as u32),
                after_newline: false,
                name: None,
            },
        }
    }

    fn bump(&mut self) -> Sig {
        let sig = self.peek();
        self.flush_trivia();
        if sig.kind != SyntaxKind::Eof {
            self.builder.token_named(sig.kind, sig.range, sig.name);
        }
        let index = self.sig[self.pos];
        self.emitted = index + 1;
        self.pos += 1;
        self.last_end = sig.range.end;
        sig
    }

    fn flush_trivia(&mut self) {
        let end = self
            .sig
            .get(self.pos)
            .copied()
            .unwrap_or(self.lexemes.len());
        while self.emitted < end {
            let lexeme = self.lexemes[self.emitted];
            if lexeme.trivia {
                self.builder
                    .token_named(lexeme.kind, lexeme.range, lexeme.name);
            }
            self.emitted += 1;
        }
    }

    fn start(&mut self, kind: SyntaxKind) -> NodeId {
        self.flush_trivia();
        let start = self.peek().range.start;
        self.builder.start_node(kind, start)
    }

    fn finish(&mut self) -> NodeId {
        self.builder.finish_node(self.last_end)
    }

    fn expect(&mut self, kind: SyntaxKind) -> Result<Sig, ()> {
        let token = self.peek();
        if token.kind == kind {
            Ok(self.bump())
        } else {
            self.flush_trivia();
            let at = token.range.start;
            self.builder.missing_token(kind, at);
            self.record_error(
                TextRange::empty(at),
                format!("expected {:?}, found {:?}", kind, token.kind),
            );
            Err(())
        }
    }

    fn expect_name(&mut self) -> Result<Sig, ()> {
        let token = self.peek();
        if token.kind.is_name() {
            Ok(self.bump())
        } else {
            self.flush_trivia();
            let at = token.range.start;
            self.builder.missing_token(Ident, at);
            self.record_error(
                TextRange::empty(at),
                format!("expected a name, found {:?}", token.kind),
            );
            Err(())
        }
    }

    fn record_error(&mut self, range: TextRange, message: impl Into<std::string::String>) {
        if self.errors.len() < 64 {
            self.errors.push(ParseError {
                range,
                message: message.into(),
            });
        }
    }

    fn fail<T>(
        &mut self,
        range: TextRange,
        message: impl Into<std::string::String>,
    ) -> Result<T, ()> {
        self.record_error(range, message);
        Err(())
    }

    fn recover_statement(&mut self) {
        self.start(ErrorNode);
        let start = self.pos;
        let mut depth = 0u32;
        loop {
            let token = self.peek();
            match token.kind {
                SyntaxKind::Eof => break,
                RBrace if depth == 0 => break,
                Semicolon if depth == 0 => {
                    self.bump();
                    break;
                }
                LBrace | LParen | LBracket => {
                    depth += 1;
                    self.bump();
                }
                RBrace | RParen | RBracket => {
                    depth = depth.saturating_sub(1);
                    self.bump();
                }
                _ if depth == 0 && token.after_newline => break,
                _ => {
                    self.bump();
                }
            }
        }
        if self.pos == start && self.peek().kind != SyntaxKind::Eof {
            self.bump();
        }
        self.finish();
    }

    fn recover_entry(&mut self) {
        self.start(ErrorNode);
        let start = self.pos;
        let mut depth = 0u32;
        loop {
            let token = self.peek();
            match token.kind {
                SyntaxKind::Eof => break,
                RBrace if depth == 0 => break,
                Comma | Semicolon if depth == 0 => {
                    self.bump();
                    break;
                }
                LBrace | LParen | LBracket => {
                    depth += 1;
                    self.bump();
                }
                RBrace | RParen | RBracket => {
                    depth = depth.saturating_sub(1);
                    self.bump();
                }
                _ if depth == 0 && token.after_newline => break,
                _ => {
                    self.bump();
                }
            }
        }
        if self.pos == start && self.peek().kind != SyntaxKind::Eof {
            self.bump();
        }
        self.finish();
    }

    fn is_contextual(&self, token: Sig, word: &str) -> bool {
        token.kind == Ident
            && &self.source[token.range.start as usize..token.range.end as usize] == word
    }

    fn at_contextual(&self, word: &str) -> bool {
        self.is_contextual(self.peek(), word)
    }

    fn parse_statement(&mut self) -> Result<(), ()> {
        let token = self.peek();
        if token.kind == Mod {
            if !self.allow_mod {
                return self.fail(token.range, "`mod` is not allowed here");
            }
            self.parse_mod()?;
            return Ok(());
        }
        if token.kind == Let || token.kind == LetStar {
            self.parse_let()?;
            return Ok(());
        }
        if self.at_contextual("use") && self.peek2().0.kind.is_name() {
            self.parse_use()?;
            return Ok(());
        }
        if self.in_module && self.at_contextual("export") {
            self.parse_export()?;
            return Ok(());
        }
        self.parse_expr_stmt()
    }

    fn parse_expr_stmt(&mut self) -> Result<(), ()> {
        self.start(ExprStmt);
        self.parse_expression()?;
        self.finish();
        Ok(())
    }

    fn parse_let(&mut self) -> Result<NodeId, ()> {
        self.start(LetStmt);
        if self.peek().kind == LetStar {
            self.bump();
        } else {
            self.expect(Let)?;
        }
        self.parse_pattern()?;
        self.expect(Assign)?;
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_mod(&mut self) -> Result<NodeId, ()> {
        self.start(ModDecl);
        self.expect(Mod)?;
        self.parse_path()?;

        if self.at_contextual("extend") {
            self.bump();
            self.expect_name()?;
        }

        let saved_module = self.in_module;
        let saved_allow = self.allow_mod;
        self.in_module = true;
        self.allow_mod = true;

        self.expect(LBrace)?;
        loop {
            let token = self.peek();
            if token.kind == RBrace || token.kind == SyntaxKind::Eof {
                break;
            }
            if token.kind == Semicolon {
                self.bump();
                continue;
            }
            let depth = self.builder.depth();
            if self.parse_statement().is_err() {
                self.builder.close_to(depth);
                self.recover_statement();
                continue;
            }
            let next = self.peek();
            if next.kind == Semicolon {
                self.bump();
            } else if next.kind != RBrace && !next.after_newline {
                self.record_error(next.range, "expected `;` or a newline");
                self.recover_statement();
            }
        }
        self.expect(RBrace)?;

        self.in_module = saved_module;
        self.allow_mod = saved_allow;
        Ok(self.finish())
    }

    fn parse_use(&mut self) -> Result<NodeId, ()> {
        self.start(UseDecl);
        self.bump();
        self.parse_path()?;
        if self.peek().kind == As {
            self.bump();
            self.expect_name()?;
        }
        if self.peek().kind == LBrace {
            self.parse_use_tree()?;
        }
        Ok(self.finish())
    }

    fn parse_use_tree(&mut self) -> Result<NodeId, ()> {
        self.start(UseTree);
        self.expect(LBrace)?;
        loop {
            if self.peek().kind == RBrace {
                break;
            }
            self.parse_path()?;
            if self.peek().kind == As {
                self.bump();
                self.expect_name()?;
            }
            if self.peek().kind == LBrace {
                self.parse_use_tree()?;
            }
            if self.peek().kind != Comma {
                break;
            }
            self.bump();
        }
        self.expect(RBrace)?;
        Ok(self.finish())
    }

    fn parse_export(&mut self) -> Result<NodeId, ()> {
        self.start(ExportDecl);
        self.bump();
        self.expect_name()?;
        self.expect(Assign)?;
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_path(&mut self) -> Result<NodeId, ()> {
        self.start(Path);
        self.expect_name()?;
        while self.peek().kind == Period {
            self.bump();
            self.expect_name()?;
        }
        Ok(self.finish())
    }

    fn parse_expression(&mut self) -> Result<NodeId, ()> {
        if self.peek().kind == Throw {
            return self.parse_throw();
        }
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<NodeId, ()> {
        self.flush_trivia();
        let checkpoint = self.builder.checkpoint();
        let lhs = self.parse_binary(1)?;
        let token = self.peek();
        if AssignOp::from_kind(token.kind).is_some() && !token.after_newline {
            self.builder.start_node_at(checkpoint, AssignExpr);
            self.bump();
            self.parse_assignment()?;
            return Ok(self.finish());
        }
        Ok(lhs)
    }

    fn parse_binary(&mut self, min_precedence: u8) -> Result<NodeId, ()> {
        self.flush_trivia();
        let checkpoint = self.builder.checkpoint();
        let mut lhs = self.parse_unary()?;
        loop {
            let token = self.peek();
            if token.after_newline {
                break;
            }
            let precedence = BinaryOp::from_kind(token.kind).map_or(0, BinaryOp::precedence);
            if precedence < min_precedence {
                break;
            }
            self.builder.start_node_at(checkpoint, Binary);
            self.bump();
            self.parse_binary(precedence + 1)?;
            lhs = self.finish();
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<NodeId, ()> {
        let token = self.peek();
        match token.kind {
            Minus | Bang => {
                self.start(Unary);
                self.bump();
                self.parse_unary()?;
                Ok(self.finish())
            }
            Return => {
                self.start(ReturnStmt);
                self.bump();
                if expression_can_start(self.peek(), false) {
                    self.parse_expression()?;
                }
                Ok(self.finish())
            }
            Break => {
                self.start(BreakStmt);
                self.bump();
                if expression_can_start(self.peek(), false) {
                    self.parse_expression()?;
                }
                Ok(self.finish())
            }
            Continue => {
                self.start(ContinueStmt);
                self.bump();
                Ok(self.finish())
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<NodeId, ()> {
        self.flush_trivia();
        let checkpoint = self.builder.checkpoint();
        let mut expr = self.parse_primary()?;
        loop {
            let token = self.peek();
            match token.kind {
                Period => {
                    let (field, args) = self.peek2();
                    if field.kind == Number {
                        self.builder.start_node_at(checkpoint, Index);
                        self.bump();
                        self.bump();
                        expr = self.finish();
                        continue;
                    }
                    if !field.kind.is_name() {
                        return self.fail(field.range, "expected a name after `.`");
                    }
                    let send = args.kind == LParen && !args.after_newline;
                    self.builder
                        .start_node_at(checkpoint, if send { Send } else { Get });
                    self.bump();
                    self.expect_name()?;
                    if send {
                        self.parse_args()?;
                    }
                    expr = self.finish();
                }
                LParen if !token.after_newline => {
                    self.builder.start_node_at(checkpoint, Call);
                    self.parse_args()?;
                    expr = self.finish();
                }
                LBracket if !token.after_newline => {
                    self.builder.start_node_at(checkpoint, Index);
                    self.bump();
                    self.parse_expression()?;
                    self.expect(RBracket)?;
                    expr = self.finish();
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<NodeId, ()> {
        let token = self.peek();
        match token.kind {
            Number | String | Ident | True | False | Null | SelfKw | SelfContext => {
                self.start(Atom);
                self.bump();
                Ok(self.finish())
            }
            Colon
                if (self.peek2().0.kind.is_name() || self.peek2().0.kind == String)
                    && token.range.end == self.peek2().0.range.start =>
            {
                self.start(AtomLiteral);
                self.bump();
                self.bump();
                Ok(self.finish())
            }
            LParen => {
                self.start(Paren);
                self.bump();
                self.parse_expression()?;
                self.expect(RParen)?;
                Ok(self.finish())
            }
            LBrace => self.parse_object(),
            LBracket => self.parse_array(),
            Pipe => self.parse_prefixed_function(),
            If => self.parse_if(),
            While => self.parse_while(),
            For => self.parse_for(),
            Try => self.parse_try(),
            Throw => self.parse_throw(),
            Match => self.parse_match(),
            _ => self.fail(
                token.range,
                format!("expected an expression, found {:?}", token.kind),
            ),
        }
    }

    fn parse_throw(&mut self) -> Result<NodeId, ()> {
        self.start(ThrowExpr);
        self.bump();
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_match(&mut self) -> Result<NodeId, ()> {
        self.flush_trivia();
        if self.peek2().0.kind == Pipe {
            return self.parse_match_closure();
        }
        self.start(MatchExpr);
        self.bump();
        self.parse_expression()?;
        self.parse_match_arms()?;
        Ok(self.finish())
    }

    fn parse_match_closure(&mut self) -> Result<NodeId, ()> {
        self.bump();
        self.start(Object);
        self.parse_params()?;
        self.start(ExprStmt);
        self.start(MatchExpr);
        self.parse_match_arms()?;
        self.finish();
        self.finish();
        Ok(self.finish())
    }

    fn parse_match_arms(&mut self) -> Result<(), ()> {
        self.expect(LBrace)?;
        loop {
            let token = self.peek();
            if token.kind == RBrace || token.kind == SyntaxKind::Eof {
                break;
            }
            if token.kind == Comma || token.kind == Semicolon {
                self.bump();
                continue;
            }
            self.start(MatchArm);
            self.parse_pattern()?;
            if self.peek().kind == If {
                self.bump();
                self.parse_expression()?;
            }
            self.expect(FatArrow)?;
            self.parse_expression()?;
            self.finish();
            let next = self.peek();
            if next.kind == RBrace {
                break;
            }
            if next.kind == Comma || next.kind == Semicolon {
                self.bump();
            } else if !next.after_newline {
                self.record_error(next.range, "expected a separator or `}`");
                self.recover_entry();
            }
        }
        self.expect(RBrace)?;
        Ok(())
    }

    fn parse_pattern(&mut self) -> Result<NodeId, ()> {
        let checkpoint = self.builder.checkpoint();
        let pattern = self.parse_pattern_atom()?;
        if self.peek().kind != Pipe {
            return Ok(pattern);
        }
        self.builder.start_node_at(checkpoint, PatternOr);
        while self.peek().kind == Pipe {
            self.bump();
            self.parse_pattern_atom()?;
        }
        Ok(self.finish())
    }

    fn parse_pattern_atom(&mut self) -> Result<NodeId, ()> {
        let checkpoint = self.builder.checkpoint();
        self.flush_trivia();
        let token = self.peek();
        let pattern = match token.kind {
            Ident if &self.source[token.range.start as usize..token.range.end as usize] == "_" => {
                self.start(PatternWildcard);
                self.bump();
                self.finish()
            }
            Ident
                if &self.source[token.range.start as usize..token.range.end as usize]
                    == "undefined" =>
            {
                self.start(PatternLiteral);
                self.bump();
                self.finish()
            }
            kind if kind.is_name() && self.peek2().0.kind == Period => {
                self.start(PatternValue);
                self.bump();
                while self.peek().kind == Period {
                    self.bump();
                    self.expect_name()?;
                }
                self.finish()
            }
            Ident => {
                if self.peek2().0.kind == Ident && !self.peek2().0.after_newline {
                    self.start(PatternType);
                    self.bump();
                    self.bump();
                    self.finish()
                } else {
                    self.start(PatternName);
                    self.bump();
                    self.finish()
                }
            }
            Minus if self.peek2().0.kind == Number => {
                self.start(PatternLiteral);
                self.bump();
                self.bump();
                self.finish()
            }
            Number | String | True | False | Null => {
                self.start(PatternLiteral);
                self.bump();
                self.finish()
            }
            LBrace => self.parse_object_pattern()?,
            LBracket => self.parse_array_pattern()?,
            Colon
                if (self.peek2().0.kind.is_name() || self.peek2().0.kind == String)
                    && token.range.end == self.peek2().0.range.start =>
            {
                self.start(PatternAtom);
                self.bump();
                self.bump();
                self.finish()
            }
            kind if kind.is_name() => {
                self.start(PatternName);
                self.bump();
                self.finish()
            }
            _ => {
                return self.fail(
                    token.range,
                    format!("expected a pattern, found {:?}", token.kind),
                );
            }
        };
        if self.peek().kind == As {
            self.builder.start_node_at(checkpoint, PatternAlias);
            self.bump();
            self.expect_name()?;
            return Ok(self.finish());
        }
        Ok(pattern)
    }

    fn parse_object_pattern(&mut self) -> Result<NodeId, ()> {
        self.start(PatternObject);
        self.expect(LBrace)?;
        loop {
            let token = self.peek();
            if token.kind == RBrace || token.kind == SyntaxKind::Eof {
                break;
            }
            if token.kind == Ellipsis {
                self.bump();
                if self.peek().kind != RBrace {
                    self.record_error(
                        token.range,
                        "object patterns use `...` only as an ignore marker",
                    );
                }
                break;
            }
            self.start(PatternField);
            if !(token.kind.is_name() || token.kind == Number || token.kind == String) {
                self.record_error(token.range, "expected a field key");
                self.recover_entry();
                self.finish();
                continue;
            }
            self.bump();
            if self.peek().kind == Colon {
                self.bump();
                self.parse_pattern()?;
            } else if self.peek().kind == As {
                self.bump();
                self.expect_name()?;
            }
            self.finish();
            let next = self.peek();
            if next.kind == RBrace {
                break;
            }
            if next.kind == Comma {
                self.bump();
            } else if !next.after_newline {
                self.record_error(next.range, "expected a separator or `}`");
                self.recover_entry();
            }
        }
        self.expect(RBrace)?;
        Ok(self.finish())
    }

    fn parse_array_pattern(&mut self) -> Result<NodeId, ()> {
        self.start(PatternArray);
        self.expect(LBracket)?;
        loop {
            if self.peek().kind == RBracket {
                break;
            }
            if self.peek().kind == Ellipsis {
                self.bump();
                self.expect_name()?;
                break;
            }
            self.parse_pattern()?;
            if self.peek().kind == Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(RBracket)?;
        Ok(self.finish())
    }

    fn parse_if(&mut self) -> Result<NodeId, ()> {
        self.start(IfExpr);
        self.expect(If)?;
        self.parse_expression()?;
        self.parse_control_body()?;
        if self.peek().kind == Else {
            self.bump();
            if self.peek().kind == If {
                self.parse_if()?;
            } else {
                self.parse_control_body()?;
            }
        }
        Ok(self.finish())
    }

    fn parse_while(&mut self) -> Result<NodeId, ()> {
        self.start(WhileExpr);
        self.expect(While)?;
        self.parse_expression()?;
        self.parse_control_body()?;
        Ok(self.finish())
    }

    fn parse_for(&mut self) -> Result<NodeId, ()> {
        self.start(ForExpr);
        self.expect(For)?;
        self.flush_trivia();
        self.parse_pattern()?;
        self.expect(In)?;
        self.parse_expression()?;
        self.parse_control_body()?;
        Ok(self.finish())
    }

    fn parse_try(&mut self) -> Result<NodeId, ()> {
        self.start(TryExpr);
        self.expect(Try)?;
        self.parse_control_body()?;
        self.expect(Catch)?;
        self.expect(LParen)?;
        self.expect_name()?;
        self.expect(RParen)?;
        self.parse_control_body()?;
        Ok(self.finish())
    }

    fn parse_object(&mut self) -> Result<NodeId, ()> {
        self.start(Object);
        self.parse_object_contents(false)?;
        Ok(self.finish())
    }

    /// A control body; a leading `|...|` starts a closure expression.
    fn parse_control_body(&mut self) -> Result<NodeId, ()> {
        self.start(Object);
        self.parse_object_contents(true)?;
        Ok(self.finish())
    }

    fn parse_prefixed_function(&mut self) -> Result<NodeId, ()> {
        self.start(Object);
        self.parse_params()?;
        self.parse_object_contents(true)?;
        Ok(self.finish())
    }

    fn parse_object_contents(&mut self, mut has_params: bool) -> Result<(), ()> {
        self.expect(LBrace)?;
        let mut saw_body = false;
        loop {
            let token = self.peek();
            if token.kind == RBrace || token.kind == SyntaxKind::Eof {
                break;
            }
            if token.kind == Semicolon || token.kind == Comma {
                self.bump();
                continue;
            }
            if token.kind == Pipe && !has_params && !saw_body {
                let depth = self.builder.depth();
                if self.parse_params().is_err() {
                    self.builder.close_to(depth);
                    self.recover_entry();
                }
                has_params = true;
                continue;
            }
            let depth = self.builder.depth();
            match self.parse_object_entry() {
                Err(()) => {
                    self.builder.close_to(depth);
                    self.recover_entry();
                    continue;
                }
                Ok(is_slot) => {
                    if !is_slot {
                        saw_body = true;
                    }
                }
            }
            let next = self.peek();
            if next.kind == RBrace {
                break;
            }
            if next.kind == Pipe {
                continue;
            }
            if next.kind == Semicolon || next.kind == Comma {
                self.bump();
            } else if !next.after_newline {
                self.record_error(next.range, "expected a separator or `}`");
                self.recover_entry();
            }
        }
        self.expect(RBrace)?;
        Ok(())
    }

    fn parse_object_entry(&mut self) -> Result<bool, ()> {
        let token = self.peek();
        if token.kind == Let || token.kind == LetStar {
            self.parse_let()?;
            return Ok(false);
        }

        if self.in_module && self.at_contextual("export") && self.peek2().0.kind.is_name() {
            self.parse_export()?;
            return Ok(false);
        }

        if token.kind == QuotedName {
            if self.peek2().0.kind != Colon {
                return self.fail(token.range, "a quoted name must be a slot key");
            }
            self.parse_named_slot(true)?;
            return Ok(true);
        }

        if token.kind == Number && self.peek2().0.kind == Colon {
            self.parse_number_slot()?;
            return Ok(true);
        }

        if token.kind.is_name() {
            let (next, after_next) = self.peek2();
            match next.kind {
                Colon => {
                    self.parse_named_slot(false)?;
                    return Ok(true);
                }
                GtGt | LtLt if after_next.kind == Colon => {
                    self.parse_accessor_slot()?;
                    return Ok(true);
                }
                Star if after_next.kind == Colon => {
                    self.parse_parent_slot()?;
                    return Ok(true);
                }
                Comma => {
                    self.parse_multi_slot()?;
                    return Ok(true);
                }
                _ => {}
            }
        }

        self.parse_expr_stmt()?;
        Ok(false)
    }

    fn parse_named_slot(&mut self, quoted: bool) -> Result<NodeId, ()> {
        self.start(Slot);
        if quoted {
            self.expect(QuotedName)?;
        } else {
            self.expect_name()?;
        }
        self.expect(Colon)?;
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_number_slot(&mut self) -> Result<NodeId, ()> {
        self.start(Slot);
        self.bump();
        self.expect(Colon)?;
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_parent_slot(&mut self) -> Result<NodeId, ()> {
        self.start(Slot);
        self.expect_name()?;
        self.expect(Star)?;
        self.expect(Colon)?;
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_accessor_slot(&mut self) -> Result<NodeId, ()> {
        self.start(Slot);
        self.expect_name()?;
        self.bump();
        self.expect(Colon)?;
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_multi_slot(&mut self) -> Result<NodeId, ()> {
        self.start(SlotMulti);
        loop {
            self.expect_name()?;
            if self.peek().kind != Comma {
                break;
            }
            self.bump();
        }
        self.expect(Colon)?;
        self.parse_expression()?;
        Ok(self.finish())
    }

    fn parse_params(&mut self) -> Result<NodeId, ()> {
        self.start(ParamList);
        self.expect(Pipe)?;
        if self.peek().kind != Pipe {
            loop {
                if self.peek().kind == Ellipsis {
                    self.bump();
                    self.expect_name()?;
                    break;
                }
                self.expect_name()?;
                if self.peek().kind != Comma {
                    break;
                }
                self.bump();
            }
        }
        self.expect(Pipe)?;
        Ok(self.finish())
    }

    fn parse_args(&mut self) -> Result<NodeId, ()> {
        self.start(ArgList);
        self.expect(LParen)?;
        loop {
            if self.peek().kind == RParen {
                break;
            }
            self.parse_expression()?;
            if self.peek().kind != Comma {
                break;
            }
            self.bump();
        }
        self.expect(RParen)?;
        Ok(self.finish())
    }

    fn parse_array(&mut self) -> Result<NodeId, ()> {
        self.start(Array);
        self.expect(LBracket)?;
        loop {
            if self.peek().kind == RBracket {
                break;
            }
            self.parse_expression()?;
            if self.peek().kind != Comma {
                break;
            }
            self.bump();
        }
        self.expect(RBracket)?;
        Ok(self.finish())
    }
}

fn expression_can_start(token: Sig, allow_newline: bool) -> bool {
    if token.after_newline && !allow_newline {
        return false;
    }
    !matches!(
        token.kind,
        SyntaxKind::Eof | RBrace | RParen | RBracket | Semicolon | Comma | Else | Catch
    )
}
