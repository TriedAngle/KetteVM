pub mod ast;
pub mod parser;
pub mod resolver;
pub mod scanner;
pub mod token;

use std::collections::HashMap;

/// A half-open `[start, end)` range of byte offsets into the source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByteSpan {
    pub start: u32,
    pub end: u32,
}

impl ByteSpan {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

/// Interned identifier / string handle into a [`SymbolTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Symbol(pub u32);

/// A frontend parse failure, in a form any parser can produce.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: ByteSpan,
    pub message: String,
}

impl ParseError {
    pub fn new(span: ByteSpan, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {}..{}",
            self.message, self.span.start, self.span.end
        )
    }
}

impl std::error::Error for ParseError {}

/// A 1-based source position. `column` counts Unicode scalar values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineColumn {
    pub line: u32,
    pub column: u32,
}

/// A parse failure with human-facing source coordinates.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub span: ByteSpan,
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{} (bytes {}..{}): {}",
            self.line, self.column, self.span.start, self.span.end, self.message
        )
    }
}

/// Line starts for byte-offset to line/column conversion.
#[derive(Clone, Debug)]
pub struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    pub fn from_line_starts(line_starts: Vec<u32>) -> Self {
        debug_assert!(line_starts.first() == Some(&0));
        Self { line_starts }
    }

    pub fn line_column(&self, source: &str, offset: u32) -> LineColumn {
        let max = source.len().min(u32::MAX as usize) as u32;
        let mut offset = offset.min(max) as usize;
        while offset > 0 && !source.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self
            .line_starts
            .partition_point(|&start| start as usize <= offset)
            - 1;
        let line_start = self.line_starts[line] as usize;
        let column = source[line_start..offset].chars().count() as u32 + 1;
        LineColumn {
            line: line as u32 + 1,
            column,
        }
    }
}

impl ParseError {
    pub fn diagnostic(&self, index: &LineIndex, source: &str) -> Diagnostic {
        let position = index.line_column(source, self.span.start);
        Diagnostic {
            span: self.span,
            line: position.line,
            column: position.column,
            message: self.message.clone(),
        }
    }
}

/// Parse-local byte-slice interner; heap internalization at materialization.
#[derive(Default)]
pub struct SymbolTable {
    map: HashMap<Vec<u8>, Symbol>,
    strings: Vec<Vec<u8>>,
}

impl SymbolTable {
    pub fn intern(&mut self, s: &[u8]) -> Symbol {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }
        let sym = Symbol(self.strings.len() as u32);
        let owned = s.to_vec();
        self.strings.push(owned.clone());
        self.map.insert(owned, sym);
        sym
    }

    pub fn get(&self, sym: Symbol) -> &[u8] {
        &self.strings[sym.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// Pull-based stream of decoded code points; the only encoding-specific
/// layer. UTF-16 streams may yield lone surrogates (0xD800..=0xDFFF).
pub trait CharStream {
    /// &mut self: buffered/chunked streams may need to fetch data to answer
    fn peek(&mut self) -> Option<u32>;
    fn advance(&mut self);
    /// byte offset for UTF-8, code units for UTF-16
    fn pos(&self) -> u32;
    /// backwards-only, over already-consumed data
    fn seek(&mut self, pos: u32);
    /// zero-copy access to a consumed range; `None` if the bytes are not
    /// contiguous in memory (chunked streams), callers then fall back to
    /// copying via seek/read
    fn slice(&self, _start: u32, _end: u32) -> Option<&[u8]> {
        None
    }
}

pub struct Utf8SliceStream<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Utf8SliceStream<'a> {
    pub fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }
}

impl CharStream for Utf8SliceStream<'_> {
    fn peek(&mut self) -> Option<u32> {
        self.src[self.pos..].chars().next().map(|c| c as u32)
    }

    fn advance(&mut self) {
        if let Some(c) = self.src[self.pos..].chars().next() {
            self.pos += c.len_utf8();
        }
    }

    fn pos(&self) -> u32 {
        self.pos as u32
    }

    fn seek(&mut self, pos: u32) {
        let pos = pos as usize;
        assert!(pos <= self.pos, "streams only seek backwards");
        assert!(self.src.is_char_boundary(pos), "seek to non-boundary {pos}");
        self.pos = pos;
    }

    fn slice(&self, start: u32, end: u32) -> Option<&[u8]> {
        Some(&self.src.as_bytes()[start as usize..end as usize])
    }
}

pub use ast::{Ast, BinaryOp, Node, NodeId, NodeList, ObjectParts, SlotKind, UnaryOp};
pub use parser::{ParseOutput, Parser};
pub use resolver::{Declaration, Resolution, Resolved, ScopeId, ScopeInfo, ScopeKind, resolve};
pub use scanner::Scanner;
pub use token::{SyntaxToken, Token, TokenInfo, TokenKind, TokenValue, Trivia, TriviaKind};
