#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SyntaxKind {
    Tombstone = 0,
    Error = 1,
    Missing = 2,

    Eof = 3,
    Whitespace = 4,
    Newline = 5,
    LineComment = 6,
    BlockComment = 7,
    Ident = 8,
    Number = 9,
    String = 10,
    QuotedName = 11,

    LParen = 12,
    RParen = 13,
    LBrace = 14,
    RBrace = 15,
    LBracket = 16,
    RBracket = 17,
    Comma = 18,
    Period = 19,
    Colon = 20,
    Semicolon = 21,
    Star = 22,
    Assign = 23,
    Pipe = 24,

    And = 25,
    Or = 26,
    EqEq = 27,
    NotEq = 28,
    Lt = 29,
    Gt = 30,
    LtEq = 31,
    GtEq = 32,
    Plus = 33,
    Minus = 34,
    Slash = 35,
    Percent = 36,
    Bang = 37,
    LtLt = 38,
    GtGt = 39,

    Let = 40,
    Mod = 41,
    If = 42,
    Else = 43,
    While = 44,
    For = 45,
    In = 46,
    SelfKw = 47,
    Null = 48,
    True = 49,
    False = 50,
    Return = 51,
    Break = 52,
    As = 53,
    SelfContext = 54,
    Try = 55,
    Catch = 56,
    Throw = 57,
    Ellipsis = 59,
    PlusEq = 60,
    MinusEq = 61,
    StarEq = 62,
    SlashEq = 63,
    PercentEq = 64,
    AndEq = 68,
    OrEq = 69,
    LtLtEq = 70,
    GtGtEq = 71,

    Script = 100,
    Body = 101,
    ExprStmt = 102,
    LetStmt = 103,
    Object = 104,
    Slot = 105,
    SlotMulti = 106,
    ParamList = 107,
    ArgList = 108,
    Array = 109,
    PatternName = 110,
    PatternFields = 111,
    PatternArray = 112,
    PatternTuple = 113,
    Get = 114,
    Index = 115,
    Send = 116,
    Call = 117,
    AssignExpr = 118,
    Unary = 119,
    Binary = 120,
    ReturnStmt = 121,
    BreakStmt = 122,
    ThrowExpr = 123,
    TryExpr = 124,
    IfExpr = 125,
    WhileExpr = 126,
    ForExpr = 127,
    ModDecl = 128,
    UseDecl = 129,
    UseTree = 130,
    ExportDecl = 131,
    Paren = 132,
    Atom = 133,
    Path = 134,
    ErrorNode = 135,
    PatternAlias = 136,
}

impl SyntaxKind {
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::Newline | Self::LineComment | Self::BlockComment
        )
    }

    /// Tokens that can stand in for a name, including keywords used as slot
    /// keys or parameters.
    pub fn is_name(self) -> bool {
        matches!(
            self,
            Self::Ident
                | Self::Let
                | Self::Mod
                | Self::If
                | Self::Else
                | Self::While
                | Self::For
                | Self::In
                | Self::SelfKw
                | Self::Null
                | Self::True
                | Self::False
                | Self::Return
                | Self::Break
                | Self::As
                | Self::SelfContext
                | Self::Try
                | Self::Catch
                | Self::Throw
        )
    }
}
