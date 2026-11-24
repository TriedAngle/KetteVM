use crate::ParseError;
use kette_syntax::SyntaxKind::{self, *};
use kette_syntax::TextRange;
use kette_syntax::name::{NameId, NameTable};

#[derive(Clone, Copy)]
pub struct Lexeme {
    pub kind: SyntaxKind,
    pub range: TextRange,
    pub trivia: bool,
    pub after_newline: bool,
    pub name: Option<NameId>,
}

pub fn scan(source: &str) -> (Vec<Lexeme>, NameTable, Vec<ParseError>) {
    let mut scanner = Scanner {
        source,
        pos: 0,
        after_newline: false,
        lexemes: Vec::new(),
        names: NameTable::new(),
        pending_name: None,
        errors: Vec::new(),
    };
    scanner.run();
    (scanner.lexemes, scanner.names, scanner.errors)
}

struct Scanner<'a> {
    source: &'a str,
    pos: usize,
    after_newline: bool,
    lexemes: Vec<Lexeme>,
    names: NameTable,
    pending_name: Option<NameId>,
    errors: Vec<ParseError>,
}

impl Scanner<'_> {
    fn run(&mut self) {
        loop {
            self.skip_trivia();
            let after_newline = std::mem::take(&mut self.after_newline);
            let start = self.pos;
            let Some(c) = self.peek() else {
                self.push(Eof, start, false, after_newline, None);
                break;
            };
            self.pending_name = None;
            let kind = self.scan_token(c, start);
            let name = self.pending_name.take();
            self.push(kind, start, false, after_newline, name);
        }
    }

    fn scan_token(&mut self, c: char, start: usize) -> SyntaxKind {
        match c {
            '"' | '\'' => self.scan_string(c),
            '`' => self.scan_quoted_name(),
            '0'..='9' => {
                self.scan_number();
                Number
            }
            c if is_ident_start(c) => self.scan_identifier(start),
            _ => self.scan_punctuation(),
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            let start = self.pos;
            let Some(c) = self.peek() else {
                return;
            };
            match c {
                '\t' | '\u{000b}' | '\u{000c}' | ' ' => {
                    while matches!(self.peek(), Some('\t' | '\u{000b}' | '\u{000c}' | ' ')) {
                        self.bump();
                    }
                    self.push(Whitespace, start, true, false, None);
                }
                '\n' | '\r' => {
                    while matches!(self.peek(), Some('\n' | '\r')) {
                        self.consume_newline();
                    }
                    self.after_newline = true;
                    self.push(Newline, start, true, false, None);
                }
                '/' => {
                    let save = self.pos;
                    self.bump();
                    match self.peek() {
                        Some('/') => {
                            while let Some(c) = self.peek() {
                                if is_newline(c) {
                                    break;
                                }
                                self.bump();
                            }
                            self.push(LineComment, start, true, false, None);
                        }
                        Some('*') => {
                            self.bump();
                            let mut depth = 1u32;
                            while depth > 0 {
                                match self.peek() {
                                    None => break,
                                    Some(c) if is_newline(c) => {
                                        self.consume_newline();
                                        self.after_newline = true;
                                    }
                                    Some('*') => {
                                        self.bump();
                                        if self.peek() == Some('/') {
                                            self.bump();
                                            depth -= 1;
                                        }
                                    }
                                    Some('/') => {
                                        self.bump();
                                        if self.peek() == Some('*') {
                                            self.bump();
                                            depth += 1;
                                        }
                                    }
                                    Some(_) => {
                                        self.bump();
                                    }
                                }
                            }
                            let range = self.range(start);
                            if depth != 0 {
                                self.error(range, "unterminated block comment");
                            }
                            self.push(BlockComment, start, true, false, None);
                        }
                        _ => {
                            self.pos = save;
                            return;
                        }
                    }
                }
                c if c >= '\u{80}' && (c == '\u{feff}' || c.is_whitespace()) => {
                    self.bump();
                    self.push(Whitespace, start, true, false, None);
                }
                _ => return,
            }
        }
    }

    fn scan_string(&mut self, quote: char) -> SyntaxKind {
        let start = self.pos;
        self.bump();
        loop {
            match self.peek() {
                None => {
                    self.error(self.range(start), "unterminated string literal");
                    return Error;
                }
                Some(c) if is_newline(c) => {
                    self.error(self.range(start), "unterminated string literal");
                    return Error;
                }
                Some(c) if c == quote => {
                    self.bump();
                    return String;
                }
                Some('\\') => {
                    self.bump();
                    if let Some(c) = self.peek() {
                        if c == '\r' {
                            self.bump();
                            if self.peek() == Some('\n') {
                                self.bump();
                            }
                        } else {
                            self.bump();
                        }
                    }
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn scan_quoted_name(&mut self) -> SyntaxKind {
        let start = self.pos;
        self.bump();
        let content_start = self.pos;
        loop {
            match self.peek() {
                None | Some('\n') | Some('\r') => {
                    self.error(self.range(start), "unterminated quoted name");
                    return Error;
                }
                Some('`') => {
                    let empty = content_start == self.pos;
                    self.bump();
                    if empty {
                        self.error(self.range(start), "empty quoted name");
                        return Error;
                    }
                    return QuotedName;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn scan_number(&mut self) {
        while matches!(self.peek(), Some('0'..='9')) {
            self.bump();
        }
        if self.peek() == Some('.') && matches!(self.peek_second(), Some('0'..='9')) {
            self.bump();
            while matches!(self.peek(), Some('0'..='9')) {
                self.bump();
            }
        }
    }

    fn scan_identifier(&mut self, start: usize) -> SyntaxKind {
        while matches!(self.peek(), Some(c) if is_ident_continue(c)) {
            self.bump();
        }
        if self.peek() == Some('?') {
            self.bump();
        }
        let mut kind = keyword(&self.source[start..self.pos]).unwrap_or(Ident);
        if kind == Let && self.peek() == Some('*') {
            self.bump();
            kind = LetStar;
        }
        if matches!(kind, And | Or) && self.peek() == Some('=') {
            self.bump();
            kind = if kind == And { AndEq } else { OrEq };
        }
        if kind.is_name() {
            self.pending_name = Some(self.names.intern(&self.source[start..self.pos]));
        }
        kind
    }

    fn scan_punctuation(&mut self) -> SyntaxKind {
        let start = self.pos;
        let Some(c) = self.bump() else {
            return Error;
        };
        match c {
            '(' => LParen,
            ')' => RParen,
            '{' => LBrace,
            '}' => RBrace,
            '[' => LBracket,
            ']' => RBracket,
            ',' => Comma,
            ':' => Colon,
            ';' => Semicolon,
            '.' => {
                if self.peek() == Some('.') && self.peek_second() == Some('.') {
                    self.bump();
                    self.bump();
                    Ellipsis
                } else {
                    Period
                }
            }
            '*' => {
                if self.peek() == Some('=') {
                    self.bump();
                    StarEq
                } else {
                    Star
                }
            }
            '+' => {
                if self.peek() == Some('=') {
                    self.bump();
                    PlusEq
                } else {
                    Plus
                }
            }
            '-' => {
                if self.peek() == Some('=') {
                    self.bump();
                    MinusEq
                } else {
                    Minus
                }
            }
            '/' => {
                if self.peek() == Some('=') {
                    self.bump();
                    SlashEq
                } else {
                    Slash
                }
            }
            '%' => {
                if self.peek() == Some('=') {
                    self.bump();
                    PercentEq
                } else {
                    Percent
                }
            }
            '=' => {
                if self.peek() == Some('=') {
                    self.bump();
                    EqEq
                } else if self.peek() == Some('>') {
                    self.bump();
                    FatArrow
                } else {
                    Assign
                }
            }
            '!' => {
                if self.peek() == Some('=') {
                    self.bump();
                    NotEq
                } else {
                    Bang
                }
            }
            '<' => match self.peek() {
                Some('=') => {
                    self.bump();
                    LtEq
                }
                Some('<') => {
                    self.bump();
                    if self.peek() == Some('=') {
                        self.bump();
                        LtLtEq
                    } else {
                        LtLt
                    }
                }
                _ => Lt,
            },
            '>' => match self.peek() {
                Some('=') => {
                    self.bump();
                    GtEq
                }
                Some('>') => {
                    self.bump();
                    if self.peek() == Some('=') {
                        self.bump();
                        GtGtEq
                    } else {
                        GtGt
                    }
                }
                _ => Gt,
            },
            '|' => {
                if self.peek() == Some('=') {
                    self.bump();
                    self.error(
                        self.range(start),
                        "bitwise compound assignment is not supported",
                    );
                    Error
                } else {
                    Pipe
                }
            }
            '&' => {
                if self.peek() == Some('=') {
                    self.bump();
                }
                self.error(self.range(start), "bitwise operators are not supported");
                Error
            }
            '^' => {
                if self.peek() == Some('=') {
                    self.bump();
                }
                self.error(self.range(start), "bitwise operators are not supported");
                Error
            }
            _ => {
                self.error(self.range(start), "unexpected character");
                Error
            }
        }
    }

    fn consume_newline(&mut self) {
        if self.peek() == Some('\r') {
            self.bump();
            if self.peek() == Some('\n') {
                self.bump();
            }
        } else {
            self.bump();
        }
    }

    fn push(
        &mut self,
        kind: SyntaxKind,
        start: usize,
        trivia: bool,
        after_newline: bool,
        name: Option<NameId>,
    ) {
        self.lexemes.push(Lexeme {
            kind,
            range: self.range(start),
            trivia,
            after_newline,
            name,
        });
    }

    fn error(&mut self, range: TextRange, message: impl Into<std::string::String>) {
        self.errors.push(ParseError {
            range,
            message: message.into(),
        });
    }

    fn range(&self, start: usize) -> TextRange {
        TextRange::new(start as u32, self.pos as u32)
    }

    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        let mut chars = self.source[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
}

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r')
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_alphanumeric()
}

fn keyword(text: &str) -> Option<SyntaxKind> {
    Some(match text {
        "let" => Let,
        "mod" => Mod,
        "if" => If,
        "else" => Else,
        "while" => While,
        "for" => For,
        "in" => In,
        "and" => And,
        "or" => Or,
        "self" => SelfKw,
        "null" => Null,
        "true" => True,
        "false" => False,
        "return" => Return,
        "break" => Break,
        "as" => As,
        "selfContext" => SelfContext,
        "try" => Try,
        "catch" => Catch,
        "throw" => Throw,
        "match" => Match,
        _ => return None,
    })
}
