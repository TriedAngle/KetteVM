use crate::SyntaxKind::{self, *};
use crate::ops::{AssignOp, BinaryOp, UnaryOp};
pub use crate::{AstChildren, AstNode};

use crate::{Child, SyntaxElement, SyntaxNode, SyntaxToken, Tree};

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[repr(C)]
        #[derive(Clone, Copy, Debug)]
        pub struct $name<'a> {
            syntax: SyntaxNode<'a>,
        }

        impl<'a> AstNode<'a> for $name<'a> {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }

            fn cast(node: SyntaxNode<'a>) -> Option<Self> {
                Self::can_cast(node.kind()).then_some(Self { syntax: node })
            }

            fn syntax(&self) -> SyntaxNode<'a> {
                self.syntax
            }
        }
    };
}

macro_rules! ast_enum {
    ($name:ident { $($variant:ident($node:ident)),* $(,)? }) => {
        #[repr(C)]
        #[derive(Clone, Copy, Debug)]
        pub enum $name<'a> {
            $($variant($node<'a>)),*
        }

        impl<'a> AstNode<'a> for $name<'a> {
            fn can_cast(kind: SyntaxKind) -> bool {
                $($node::can_cast(kind))||*
            }

            fn cast(node: SyntaxNode<'a>) -> Option<Self> {
                $(
                    if $node::can_cast(node.kind()) {
                        return $node::cast(node).map($name::$variant);
                    }
                )*
                None
            }

            fn syntax(&self) -> SyntaxNode<'a> {
                match self {
                    $(Self::$variant(node) => node.syntax()),*
                }
            }
        }
    };
}

ast_node!(Script, Script);
ast_node!(ExprStmt, ExprStmt);
ast_node!(LetStmt, LetStmt);
ast_node!(ModDecl, ModDecl);
ast_node!(UseDecl, UseDecl);
ast_node!(UseTree, UseTree);
ast_node!(ExportDecl, ExportDecl);
ast_node!(Path, Path);
ast_node!(PatternName, PatternName);
ast_node!(PatternFields, PatternFields);
ast_node!(PatternArray, PatternArray);
ast_node!(PatternTuple, PatternTuple);
ast_node!(PatternAlias, PatternAlias);
ast_node!(Object, Object);
ast_node!(Slot, Slot);
ast_node!(SlotMulti, SlotMulti);
ast_node!(ParamList, ParamList);
ast_node!(ArgList, ArgList);
ast_node!(Array, Array);
ast_node!(GetExpr, Get);
ast_node!(IndexExpr, Index);
ast_node!(SendExpr, Send);
ast_node!(CallExpr, Call);
ast_node!(AssignExpr, AssignExpr);
ast_node!(UnaryExpr, Unary);
ast_node!(BinaryExpr, Binary);
ast_node!(ReturnStmt, ReturnStmt);
ast_node!(BreakStmt, BreakStmt);
ast_node!(ThrowExpr, ThrowExpr);
ast_node!(TryExpr, TryExpr);
ast_node!(IfExpr, IfExpr);
ast_node!(WhileExpr, WhileExpr);
ast_node!(ForExpr, ForExpr);
ast_node!(Paren, Paren);
ast_node!(Atom, Atom);

ast_enum!(Expr {
    Atom(Atom),
    Paren(Paren),
    Object(Object),
    Array(Array),
    If(IfExpr),
    While(WhileExpr),
    For(ForExpr),
    Try(TryExpr),
    Throw(ThrowExpr),
    Return(ReturnStmt),
    Break(BreakStmt),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    Assign(AssignExpr),
    Get(GetExpr),
    Index(IndexExpr),
    Send(SendExpr),
    Call(CallExpr),
});

ast_enum!(Stmt {
    Let(LetStmt),
    Expr(ExprStmt),
    Mod(ModDecl),
    Use(UseDecl),
    Export(ExportDecl),
});

ast_enum!(Pattern {
    Name(PatternName),
    Fields(PatternFields),
    Array(PatternArray),
    Tuple(PatternTuple),
    Alias(PatternAlias),
});

ast_enum!(SlotEntry {
    Slot(Slot),
    Multi(SlotMulti),
});

ast_enum!(ObjectEntry {
    Slot(SlotEntry),
    Params(ParamList),
    Let(LetStmt),
    Export(ExportDecl),
    Expr(ExprStmt),
});

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotKind {
    Named,
    Parent,
    Getter,
    Setter,
    Numeric,
    Quoted,
}

impl SlotKind {
    /// Canonical marker between the key and the value.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Named | Self::Numeric | Self::Quoted => ":",
            Self::Parent => "*:",
            Self::Getter => ">>:",
            Self::Setter => "<<:",
        }
    }
}

impl Tree {
    pub fn script(&self) -> Script<'_> {
        Script::cast(self.root()).expect("syntax root is Script")
    }
}

impl<'a> Script<'a> {
    pub fn statements(self) -> AstChildren<'a, Stmt<'a>> {
        self.syntax.children_as()
    }
}

impl<'a> ModDecl<'a> {
    pub fn path(self) -> Option<Path<'a>> {
        self.syntax.child_as()
    }

    /// The `extend` target after `mod name extend`.
    pub fn extend(self) -> Option<SyntaxToken<'a>> {
        let mut tokens = Tokens::new(self.syntax);
        while let Some(token) = tokens.next_token() {
            if token.text() == "extend" {
                return tokens.next_token().filter(|token| token.kind().is_name());
            }
        }
        None
    }

    pub fn statements(self) -> AstChildren<'a, Stmt<'a>> {
        self.syntax.children_as()
    }
}

impl<'a> UseDecl<'a> {
    pub fn path(self) -> Option<Path<'a>> {
        self.syntax.child_as()
    }

    pub fn alias(self) -> Option<SyntaxToken<'a>> {
        let mut tokens = Tokens::new(self.syntax);
        while let Some(token) = tokens.next_token() {
            if token.kind() == As {
                return tokens.next_token().filter(|token| token.kind().is_name());
            }
        }
        None
    }

    pub fn tree(self) -> Option<UseTree<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> UseTree<'a> {
    pub fn items(self) -> UseItems<'a> {
        UseItems {
            elements: Elements::new(self.syntax),
            done: false,
        }
    }

    pub fn paths(self) -> AstChildren<'a, Path<'a>> {
        self.syntax.children_as()
    }

    pub fn trees(self) -> AstChildren<'a, UseTree<'a>> {
        self.syntax.children_as()
    }
}

impl<'a> ExportDecl<'a> {
    pub fn name(self) -> Option<SyntaxToken<'a>> {
        self.syntax
            .tokens()
            .filter(|token| token.kind().is_name())
            .nth(1)
    }

    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> Path<'a> {
    pub fn segments(self) -> impl Iterator<Item = SyntaxToken<'a>> + 'a {
        self.syntax.tokens().filter(|token| token.kind().is_name())
    }

    pub fn last_segment(self) -> Option<SyntaxToken<'a>> {
        let mut last = None;
        for token in self.segments() {
            last = Some(token);
        }
        last
    }
}

impl<'a> PatternName<'a> {
    pub fn name(self) -> Option<SyntaxToken<'a>> {
        Tokens::new(self.syntax).next_token()
    }
}

impl<'a> PatternFields<'a> {
    pub fn fields(self) -> Fields<'a> {
        Fields {
            tokens: Tokens::new(self.syntax),
        }
    }
}

impl<'a> PatternAlias<'a> {
    pub fn pattern(self) -> Option<Pattern<'a>> {
        self.syntax.child_as()
    }

    pub fn alias(self) -> Option<SyntaxToken<'a>> {
        let mut tokens = Tokens::new(self.syntax);
        while let Some(token) = tokens.next_token() {
            if token.kind() == As {
                return tokens.next_token().filter(|token| token.kind().is_name());
            }
        }
        None
    }
}

impl<'a> PatternArray<'a> {
    pub fn elements(self) -> AstChildren<'a, Pattern<'a>> {
        self.syntax.children_as()
    }

    /// The name after `...`.
    pub fn rest(self) -> Option<SyntaxToken<'a>> {
        let mut tokens = Tokens::new(self.syntax);
        while let Some(token) = tokens.next_token() {
            if token.kind() == Ellipsis {
                return tokens.next_token().filter(|token| token.kind().is_name());
            }
        }
        None
    }
}

impl<'a> PatternTuple<'a> {
    pub fn elements(self) -> AstChildren<'a, Pattern<'a>> {
        self.syntax.children_as()
    }
}

impl<'a> Object<'a> {
    pub fn params(self) -> Option<ParamList<'a>> {
        self.syntax.child_as()
    }

    pub fn entries(self) -> AstChildren<'a, ObjectEntry<'a>> {
        self.syntax.children_as()
    }

    pub fn slots(self) -> AstChildren<'a, SlotEntry<'a>> {
        self.syntax.children_as()
    }

    pub fn statements(self) -> AstChildren<'a, Stmt<'a>> {
        self.syntax.children_as()
    }

    pub fn has_slots(self) -> bool {
        self.slots().next().is_some()
    }

    pub fn is_callable(self) -> bool {
        self.params().is_some() || self.statements().next().is_some()
    }

    /// The final expression in the body, if any.
    pub fn tail(self) -> Option<Expr<'a>> {
        self.statements()
            .filter_map(|statement| match statement {
                Stmt::Expr(statement) => statement.expr(),
                _ => None,
            })
            .next_back()
    }
}

impl<'a> Slot<'a> {
    pub fn key(self) -> Option<SyntaxToken<'a>> {
        let token = Tokens::new(self.syntax).next_token()?;
        (token.kind().is_name() || matches!(token.kind(), Number | QuotedName)).then_some(token)
    }

    pub fn kind(self) -> Option<SlotKind> {
        let mut tokens = Tokens::new(self.syntax);
        let key = tokens.next_token()?;
        let marker = tokens.next_token()?;
        Some(match marker.kind() {
            GtGt => SlotKind::Getter,
            LtLt => SlotKind::Setter,
            Star => SlotKind::Parent,
            Colon => match key.kind() {
                Number => SlotKind::Numeric,
                QuotedName => SlotKind::Quoted,
                _ if key.kind().is_name() => SlotKind::Named,
                _ => return None,
            },
            _ => return None,
        })
    }

    /// Property name without the backticks of a quoted key.
    pub fn key_text(self) -> Option<&'a str> {
        let token = self.key()?;
        Some(match token.kind() {
            QuotedName => token
                .text()
                .strip_prefix('`')
                .and_then(|text| text.strip_suffix('`'))
                .unwrap_or(token.text()),
            _ => token.text(),
        })
    }

    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> SlotMulti<'a> {
    pub fn keys(self) -> impl Iterator<Item = SyntaxToken<'a>> + 'a {
        self.syntax.tokens().filter(|token| token.kind().is_name())
    }

    pub fn key_texts(self) -> impl Iterator<Item = &'a str> + 'a {
        self.keys().map(|token| token.text())
    }

    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> SlotEntry<'a> {
    pub fn single(self) -> Option<Slot<'a>> {
        match self {
            Self::Slot(slot) => Some(slot),
            Self::Multi(_) => None,
        }
    }

    pub fn multi(self) -> Option<SlotMulti<'a>> {
        match self {
            Self::Multi(slot) => Some(slot),
            Self::Slot(_) => None,
        }
    }

    pub fn value(self) -> Option<Expr<'a>> {
        match self {
            Self::Slot(slot) => slot.value(),
            Self::Multi(slot) => slot.value(),
        }
    }
}

impl<'a> ParamList<'a> {
    pub fn params(self) -> Params<'a> {
        Params {
            tokens: Tokens::new(self.syntax),
            done: false,
            started: false,
        }
    }

    /// The name after `...`.
    pub fn rest(self) -> Option<SyntaxToken<'a>> {
        let mut tokens = Tokens::new(self.syntax);
        while let Some(token) = tokens.next_token() {
            if token.kind() == Ellipsis {
                return tokens.next_token().filter(|token| token.kind().is_name());
            }
        }
        None
    }
}

impl<'a> ArgList<'a> {
    pub fn args(self) -> AstChildren<'a, Expr<'a>> {
        self.syntax.children_as()
    }
}

impl<'a> Array<'a> {
    pub fn elements(self) -> AstChildren<'a, Expr<'a>> {
        self.syntax.children_as()
    }
}

impl<'a> GetExpr<'a> {
    pub fn receiver(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }

    pub fn name(self) -> Option<SyntaxToken<'a>> {
        self.syntax.tokens().find(|token| token.kind().is_name())
    }
}

impl<'a> IndexExpr<'a> {
    pub fn receiver(self) -> Option<Expr<'a>> {
        self.syntax.children_as().next()
    }

    /// The expression inside `[...]`; `None` for the `.number` form.
    pub fn index(self) -> Option<Expr<'a>> {
        self.syntax.children_as().nth(1)
    }

    pub fn number(self) -> Option<SyntaxToken<'a>> {
        self.syntax.tokens().find(|token| token.kind() == Number)
    }
}

impl<'a> SendExpr<'a> {
    pub fn receiver(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }

    pub fn name(self) -> Option<SyntaxToken<'a>> {
        self.syntax.tokens().find(|token| token.kind().is_name())
    }

    pub fn args(self) -> Option<ArgList<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> CallExpr<'a> {
    pub fn receiver(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }

    pub fn args(self) -> Option<ArgList<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> AssignExpr<'a> {
    pub fn place(self) -> Option<Expr<'a>> {
        self.syntax.children_as().next()
    }

    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.children_as().nth(1)
    }

    pub fn op(self) -> Option<AssignOp> {
        self.syntax
            .tokens()
            .find_map(|token| AssignOp::from_kind(token.kind()))
    }
}

impl<'a> UnaryExpr<'a> {
    pub fn op(self) -> Option<UnaryOp> {
        Tokens::new(self.syntax)
            .next_token()
            .and_then(|token| UnaryOp::from_kind(token.kind()))
    }

    pub fn operand(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> BinaryExpr<'a> {
    pub fn lhs(self) -> Option<Expr<'a>> {
        self.syntax.children_as().next()
    }

    pub fn rhs(self) -> Option<Expr<'a>> {
        self.syntax.children_as().nth(1)
    }

    pub fn op(self) -> Option<BinaryOp> {
        self.syntax
            .tokens()
            .find_map(|token| BinaryOp::from_kind(token.kind()))
    }
}

impl<'a> ReturnStmt<'a> {
    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> BreakStmt<'a> {
    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> ThrowExpr<'a> {
    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> TryExpr<'a> {
    pub fn body(self) -> Option<Object<'a>> {
        self.syntax.child_nodes().next().and_then(Object::cast)
    }

    pub fn catch_name(self) -> Option<SyntaxToken<'a>> {
        let mut tokens = Tokens::new(self.syntax);
        while let Some(token) = tokens.next_token() {
            if token.kind() != Catch {
                continue;
            }
            while let Some(token) = tokens.next_token() {
                if token.kind().is_name() {
                    return Some(token);
                }
            }
        }
        None
    }

    pub fn catch(self) -> Option<Object<'a>> {
        self.syntax.child_nodes().nth(1).and_then(Object::cast)
    }
}

impl<'a> IfExpr<'a> {
    pub fn condition(self) -> Option<Expr<'a>> {
        self.syntax.child_nodes().next().and_then(Expr::cast)
    }

    pub fn then_branch(self) -> Option<Object<'a>> {
        self.syntax.child_nodes().nth(1).and_then(Object::cast)
    }

    /// An `Object` or a chained `IfExpr`.
    pub fn else_branch(self) -> Option<Expr<'a>> {
        self.syntax.child_nodes().nth(2).and_then(Expr::cast)
    }
}

impl<'a> WhileExpr<'a> {
    pub fn condition(self) -> Option<Expr<'a>> {
        self.syntax.child_nodes().next().and_then(Expr::cast)
    }

    pub fn body(self) -> Option<Object<'a>> {
        self.syntax.child_nodes().nth(1).and_then(Object::cast)
    }
}

impl<'a> ForExpr<'a> {
    pub fn pattern(self) -> Option<Pattern<'a>> {
        self.syntax.child_nodes().next().and_then(Pattern::cast)
    }

    pub fn iterable(self) -> Option<Expr<'a>> {
        self.syntax.child_nodes().nth(1).and_then(Expr::cast)
    }

    pub fn body(self) -> Option<Object<'a>> {
        self.syntax.child_nodes().nth(2).and_then(Object::cast)
    }
}

impl<'a> Paren<'a> {
    pub fn expr(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> Atom<'a> {
    pub fn token(self) -> Option<SyntaxToken<'a>> {
        Tokens::new(self.syntax).next_token()
    }
}

impl<'a> ExprStmt<'a> {
    pub fn expr(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }
}

impl<'a> LetStmt<'a> {
    pub fn pattern(self) -> Option<Pattern<'a>> {
        self.syntax.child_as()
    }

    pub fn value(self) -> Option<Expr<'a>> {
        self.syntax.child_as()
    }

    pub fn is_recursive(self) -> bool {
        self.syntax.child_token_of_kind(LetStar).is_some()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Field<'a> {
    pub name: SyntaxToken<'a>,
    pub alias: Option<SyntaxToken<'a>>,
}

pub struct Fields<'a> {
    tokens: Tokens<'a>,
}

impl<'a> Iterator for Fields<'a> {
    type Item = Field<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let token = self.tokens.next_token()?;
            match token.kind() {
                LParen | Comma => continue,
                RParen => return None,
                _ if token.kind().is_name() => {
                    let alias = if self.tokens.peek_kind() == Some(As) {
                        self.tokens.next_token();
                        self.tokens.next_token()
                    } else {
                        None
                    };
                    return Some(Field { name: token, alias });
                }
                _ => return None,
            }
        }
    }
}

/// Parameter names; stops at the closing `|` and before `...name`.
pub struct Params<'a> {
    tokens: Tokens<'a>,
    done: bool,
    started: bool,
}

impl<'a> Iterator for Params<'a> {
    type Item = SyntaxToken<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            let token = self.tokens.next_token()?;
            match token.kind() {
                Pipe if !self.started => {
                    self.started = true;
                }
                Pipe | Ellipsis => {
                    self.done = true;
                    return None;
                }
                _ if token.kind().is_name() => return Some(token),
                _ => {}
            }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UseItem<'a> {
    pub path: Path<'a>,
    pub alias: Option<SyntaxToken<'a>>,
    pub tree: Option<UseTree<'a>>,
}

pub struct UseItems<'a> {
    elements: Elements<'a>,
    done: bool,
}

impl<'a> Iterator for UseItems<'a> {
    type Item = UseItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            match self.elements.next_element()? {
                SyntaxElement::Token(token) => match token.kind() {
                    LBrace | Comma => continue,
                    RBrace => {
                        self.done = true;
                        return None;
                    }
                    _ => {
                        self.done = true;
                        return None;
                    }
                },
                SyntaxElement::Node(node) => {
                    let path = match Path::cast(node) {
                        Some(path) => path,
                        None => {
                            self.done = true;
                            return None;
                        }
                    };
                    let alias = if self.elements.peek_kind() == Some(As) {
                        self.elements.next_element();
                        self.elements
                            .next_element()
                            .and_then(SyntaxElement::as_token)
                            .filter(|token| token.kind().is_name())
                    } else {
                        None
                    };
                    let tree = match self.elements.peek_element() {
                        Some(SyntaxElement::Node(node)) if UseTree::can_cast(node.kind()) => {
                            self.elements.next_element();
                            UseTree::cast(node)
                        }
                        _ => None,
                    };
                    return Some(UseItem { path, alias, tree });
                }
            }
        }
    }
}

#[derive(Clone)]
struct Elements<'a> {
    tree: &'a Tree,
    iter: std::slice::Iter<'a, Child>,
}

impl<'a> Elements<'a> {
    fn new(node: SyntaxNode<'a>) -> Self {
        Self {
            tree: node.tree(),
            iter: node.tree().children(node.id()).iter(),
        }
    }

    fn next_element(&mut self) -> Option<SyntaxElement<'a>> {
        for child in self.iter.by_ref() {
            let element = match *child {
                Child::Node(id) => SyntaxElement::Node(SyntaxNode::new(self.tree, id)),
                Child::Token(id) => SyntaxElement::Token(SyntaxToken::new(self.tree, id)),
            };
            if let SyntaxElement::Token(token) = element
                && token.kind().is_trivia()
            {
                continue;
            }
            return Some(element);
        }
        None
    }

    fn peek_element(&self) -> Option<SyntaxElement<'a>> {
        self.clone().next_element()
    }

    fn peek_kind(&self) -> Option<SyntaxKind> {
        self.peek_element().map(SyntaxElement::kind)
    }
}

#[derive(Clone)]
struct Tokens<'a> {
    tree: &'a Tree,
    iter: std::slice::Iter<'a, Child>,
}

impl<'a> Tokens<'a> {
    fn new(node: SyntaxNode<'a>) -> Self {
        Self {
            tree: node.tree(),
            iter: node.tree().children(node.id()).iter(),
        }
    }

    fn next_token(&mut self) -> Option<SyntaxToken<'a>> {
        for child in self.iter.by_ref() {
            if let Child::Token(id) = *child {
                let token = SyntaxToken::new(self.tree, id);
                if !token.kind().is_trivia() {
                    return Some(token);
                }
            }
        }
        None
    }

    fn peek_kind(&self) -> Option<SyntaxKind> {
        self.clone().next_token().map(|token| token.kind())
    }
}
