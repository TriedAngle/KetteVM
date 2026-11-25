use crate::SyntaxKind::{self, *};
use std::fmt;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Or,
    And,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    BitOr,
    BitXor,
    BitAnd,
    ShiftLeft,
    ShiftRight,
    Concat,
    Pipe,
    RangeExclusive,
    RangeInclusive,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
}

impl BinaryOp {
    pub fn from_kind(kind: SyntaxKind) -> Option<Self> {
        Some(match kind {
            Or => Self::Or,
            And => Self::And,
            EqEq => Self::EqEq,
            NotEq => Self::NotEq,
            Lt => Self::Lt,
            Gt => Self::Gt,
            LtEq => Self::LtEq,
            GtEq => Self::GtEq,
            PipePipePipe => Self::BitOr,
            CaretCaretCaret => Self::BitXor,
            AmpAmpAmp => Self::BitAnd,
            LtLtLt => Self::ShiftLeft,
            GtGtGt => Self::ShiftRight,
            LtGt => Self::Concat,
            PipeGt => Self::Pipe,
            DotDot => Self::RangeExclusive,
            DotDotEq => Self::RangeInclusive,
            Plus => Self::Plus,
            Minus => Self::Minus,
            Star => Self::Star,
            Slash => Self::Slash,
            Percent => Self::Percent,
            _ => return None,
        })
    }

    pub fn kind(self) -> SyntaxKind {
        match self {
            Self::Or => Or,
            Self::And => And,
            Self::EqEq => EqEq,
            Self::NotEq => NotEq,
            Self::Lt => Lt,
            Self::Gt => Gt,
            Self::LtEq => LtEq,
            Self::GtEq => GtEq,
            Self::BitOr => PipePipePipe,
            Self::BitXor => CaretCaretCaret,
            Self::BitAnd => AmpAmpAmp,
            Self::ShiftLeft => LtLtLt,
            Self::ShiftRight => GtGtGt,
            Self::Concat => LtGt,
            Self::Pipe => PipeGt,
            Self::RangeExclusive => DotDot,
            Self::RangeInclusive => DotDotEq,
            Self::Plus => Plus,
            Self::Minus => Minus,
            Self::Star => Star,
            Self::Slash => Slash,
            Self::Percent => Percent,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
            Self::EqEq => "==",
            Self::NotEq => "!=",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::LtEq => "<=",
            Self::GtEq => ">=",
            Self::BitOr => "|||",
            Self::BitXor => "^^^",
            Self::BitAnd => "&&&",
            Self::ShiftLeft => "<<<",
            Self::ShiftRight => ">>>",
            Self::Concat => "<>",
            Self::Pipe => "|>",
            Self::RangeExclusive => "..",
            Self::RangeInclusive => "..=",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Percent => "%",
        }
    }

    /// Larger binds tighter. `0` means "not a binary operator".
    pub fn precedence(self) -> u8 {
        match self {
            Self::Or => 1,
            Self::And => 2,
            Self::EqEq | Self::NotEq => 3,
            Self::Lt | Self::Gt | Self::LtEq | Self::GtEq => 4,
            Self::BitOr => 5,
            Self::BitXor => 6,
            Self::BitAnd => 7,
            Self::ShiftLeft | Self::ShiftRight | Self::Concat => 8,
            Self::Pipe | Self::RangeExclusive | Self::RangeInclusive => 1,
            Self::Plus | Self::Minus => 9,
            Self::Star | Self::Slash | Self::Percent => 10,
        }
    }

    /// Whether the operator is lowered to a method send.
    pub fn is_dispatch(self) -> bool {
        !matches!(
            self,
            Self::And | Self::Or | Self::Pipe | Self::RangeExclusive | Self::RangeInclusive
        )
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Not,
    Bnot,
}

impl UnaryOp {
    pub fn from_kind(kind: SyntaxKind) -> Option<Self> {
        Some(match kind {
            Minus => Self::Neg,
            Bang => Self::Not,
            TildeTildeTilde => Self::Bnot,
            _ => return None,
        })
    }

    pub fn kind(self) -> SyntaxKind {
        match self {
            Self::Neg => Minus,
            Self::Not => Bang,
            Self::Bnot => TildeTildeTilde,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::Neg => "-",
            Self::Not => "!",
            Self::Bnot => "~~~",
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
}

impl AssignOp {
    pub fn from_kind(kind: SyntaxKind) -> Option<Self> {
        Some(match kind {
            Assign => Self::Assign,
            PlusEq => Self::Add,
            MinusEq => Self::Sub,
            StarEq => Self::Mul,
            SlashEq => Self::Div,
            PercentEq => Self::Rem,
            AndEq => Self::And,
            OrEq => Self::Or,
            LtLtLtEq => Self::Shl,
            GtGtGtEq => Self::Shr,
            AmpAmpAmpEq => Self::BitAnd,
            PipePipePipeEq => Self::BitOr,
            CaretCaretCaretEq => Self::BitXor,
            _ => return None,
        })
    }

    pub fn kind(self) -> SyntaxKind {
        match self {
            Self::Assign => Assign,
            Self::Add => PlusEq,
            Self::Sub => MinusEq,
            Self::Mul => StarEq,
            Self::Div => SlashEq,
            Self::Rem => PercentEq,
            Self::And => AndEq,
            Self::Or => OrEq,
            Self::Shl => LtLtLtEq,
            Self::Shr => GtGtGtEq,
            Self::BitAnd => AmpAmpAmpEq,
            Self::BitOr => PipePipePipeEq,
            Self::BitXor => CaretCaretCaretEq,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::Assign => "=",
            Self::Add => "+=",
            Self::Sub => "-=",
            Self::Mul => "*=",
            Self::Div => "/=",
            Self::Rem => "%=",
            Self::And => "and=",
            Self::Or => "or=",
            Self::Shl => "<<<=",
            Self::Shr => ">>>=",
            Self::BitAnd => "&&&=",
            Self::BitOr => "|||=",
            Self::BitXor => "^^^=",
        }
    }

    pub fn is_compound(self) -> bool {
        self != Self::Assign
    }
}

impl fmt::Display for AssignOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text())
    }
}
