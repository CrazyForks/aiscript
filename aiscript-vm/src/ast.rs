use crate::{scanner::Token, string::InternedString};
use std::fmt;

#[derive(Debug, Clone)]
pub enum Expr<'gc> {
    Binary {
        left: Box<Expr<'gc>>,
        operator: Token<'gc>,
        right: Box<Expr<'gc>>,
        line: u32,
    },
    Grouping {
        expression: Box<Expr<'gc>>,
        line: u32,
    },
    Literal {
        value: LiteralValue<'gc>,
        line: u32,
    },
    Unary {
        operator: Token<'gc>,
        right: Box<Expr<'gc>>,
        line: u32,
    },
    Variable {
        name: Token<'gc>,
        line: u32,
    },
    Assign {
        name: Token<'gc>,
        value: Box<Expr<'gc>>,
        line: u32,
    },
    And {
        left: Box<Expr<'gc>>,
        right: Box<Expr<'gc>>,
        line: u32,
    },
    Or {
        left: Box<Expr<'gc>>,
        right: Box<Expr<'gc>>,
        line: u32,
    },
    Call {
        callee: Box<Expr<'gc>>,
        // paren: Token<'gc>,
        arguments: Vec<Expr<'gc>>,
        line: u32,
    },
    Invoke {
        object: Box<Expr<'gc>>,
        method: Token<'gc>,
        arguments: Vec<Expr<'gc>>,
        line: u32,
    },
    Get {
        object: Box<Expr<'gc>>,
        name: Token<'gc>,
        line: u32,
    },
    Set {
        object: Box<Expr<'gc>>,
        name: Token<'gc>,
        value: Box<Expr<'gc>>,
        line: u32,
    },
    This {
        // keyword: Token<'gc>,
        line: u32,
    },
    Super {
        // keyword: Token<'gc>,
        method: Token<'gc>,
        arguments: Vec<Expr<'gc>>,
        line: u32,
    },
    Prompt {
        expression: Box<Expr<'gc>>,
        line: u32,
    },
}

impl<'gc> Expr<'gc> {
    pub fn line(&self) -> u32 {
        match self {
            Self::Binary { line, .. }
            | Self::Grouping { line, .. }
            | Self::Literal { line, .. }
            | Self::Unary { line, .. }
            | Self::Variable { line, .. }
            | Self::Assign { line, .. }
            | Self::And { line, .. }
            | Self::Or { line, .. }
            | Self::Call { line, .. }
            | Self::Invoke { line, .. }
            | Self::Get { line, .. }
            | Self::Set { line, .. }
            | Self::This { line, .. }
            | Self::Super { line, .. }
            | Self::Prompt { line, .. } => *line,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Stmt<'gc> {
    Expression {
        expression: Expr<'gc>,
        line: u32,
    },
    Print {
        expression: Expr<'gc>,
        line: u32,
    },
    Let {
        name: Token<'gc>,
        initializer: Option<Expr<'gc>>,
        line: u32,
    },
    Block {
        statements: Vec<Stmt<'gc>>,
        line: u32,
    },
    If {
        condition: Expr<'gc>,
        then_branch: Box<Stmt<'gc>>,
        else_branch: Option<Box<Stmt<'gc>>>,
        line: u32,
    },
    While {
        condition: Expr<'gc>,
        body: Box<Stmt<'gc>>,
        line: u32,
    },
    Function {
        name: Token<'gc>,
        params: Vec<Token<'gc>>,
        body: Vec<Stmt<'gc>>,
        is_ai: bool,
        line: u32,
    },
    Return {
        // keyword: Token<'gc>,
        value: Option<Expr<'gc>>,
        line: u32,
    },
    Class {
        name: Token<'gc>,
        superclass: Option<Expr<'gc>>,
        methods: Vec<Stmt<'gc>>,
        line: u32,
    },
}

impl<'gc> Stmt<'gc> {
    pub fn line(&self) -> u32 {
        match self {
            Self::Expression { line, .. }
            | Self::Print { line, .. }
            | Self::Let { line, .. }
            | Self::Block { line, .. }
            | Self::If { line, .. }
            | Self::While { line, .. }
            | Self::Function { line, .. }
            | Self::Return { line, .. }
            | Self::Class { line, .. } => *line,
        }
    }
}

#[derive(Debug, Clone)]
pub enum LiteralValue<'gc> {
    Number(f64),
    String(InternedString<'gc>),
    Boolean(bool),
    Nil,
}

#[derive(Debug, Clone)]
pub struct Program<'gc> {
    pub statements: Vec<Stmt<'gc>>,
}

impl<'gc> Program<'gc> {
    pub fn new() -> Self {
        Self {
            statements: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub line: u32,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[line {}] Error: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

impl ParseError {
    pub fn new(message: impl Into<String>, line: u32) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }
}
