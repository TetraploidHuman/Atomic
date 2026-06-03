use crate::ast::*;
use crate::lexer::{Lexer, Span, Token, TokenKind};

/// Parse error with location info
#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Parse error at line {}, col {}: {}", self.line, self.col, self.message)
    }
}

/// Pratt parsing precedence levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Precedence {
    Lowest,
    Assignment,
    To,          // `to` infix for tuple creation
    LogicalOr,
    LogicalAnd,
    BitwiseOr,
    BitwiseXor,
    BitwiseAnd,
    Comparison,
    Shift,
    Range,
    Sum,
    Product,
    Power,    // ** (right-associative, higher than Product)
    Unary,
    Call,
}

impl Precedence {
    fn of_binary(op: &BinaryOp) -> Self {
        match op {
            BinaryOp::Assign => Precedence::Assignment,
            BinaryOp::Or => Precedence::LogicalOr,
            BinaryOp::And => Precedence::LogicalAnd,
            BinaryOp::BitOr => Precedence::BitwiseOr,
            BinaryOp::BitXor => Precedence::BitwiseXor,
            BinaryOp::BitAnd => Precedence::BitwiseAnd,
            BinaryOp::Eq | BinaryOp::Neq | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Lte | BinaryOp::Gte
            | BinaryOp::In | BinaryOp::Is => {
                Precedence::Comparison
            },
            BinaryOp::Shl | BinaryOp::Shr => Precedence::Shift,
            BinaryOp::Range | BinaryOp::RangeExclusive => Precedence::Range,
            BinaryOp::Add | BinaryOp::Sub => Precedence::Sum,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => Precedence::Product,
            BinaryOp::Pow => Precedence::Power,
        }
    }

    fn next(self) -> Self {
        match self {
            Precedence::Lowest => Precedence::Assignment,
            Precedence::Assignment => Precedence::To,
            Precedence::To => Precedence::LogicalOr,
            Precedence::LogicalOr => Precedence::LogicalAnd,
            Precedence::LogicalAnd => Precedence::BitwiseOr,
            Precedence::BitwiseOr => Precedence::BitwiseXor,
            Precedence::BitwiseXor => Precedence::BitwiseAnd,
            Precedence::BitwiseAnd => Precedence::Comparison,
            Precedence::Comparison => Precedence::Shift,
            Precedence::Shift => Precedence::Range,
            Precedence::Range => Precedence::Sum,
            Precedence::Sum => Precedence::Product,
            Precedence::Product => Precedence::Power,
            Precedence::Power => Precedence::Unary,
            Precedence::Unary => Precedence::Call,
            Precedence::Call => Precedence::Call,
        }
    }
}

/// Map token kind to binary operator
fn token_to_binary_op(kind: &TokenKind) -> Option<BinaryOp> {
    match kind {
        TokenKind::Plus => Some(BinaryOp::Add),
        TokenKind::Minus => Some(BinaryOp::Sub),
        TokenKind::Star => Some(BinaryOp::Mul),
        TokenKind::Slash => Some(BinaryOp::Div),
        TokenKind::Percent => Some(BinaryOp::Mod),
        TokenKind::EqEq => Some(BinaryOp::Eq),
        TokenKind::Neq => Some(BinaryOp::Neq),
        TokenKind::Lt => Some(BinaryOp::Lt),
        TokenKind::Gt => Some(BinaryOp::Gt),
        TokenKind::Lte => Some(BinaryOp::Lte),
        TokenKind::Gte => Some(BinaryOp::Gte),
        TokenKind::And => Some(BinaryOp::And),
        TokenKind::Or => Some(BinaryOp::Or),
        TokenKind::Ampersand => Some(BinaryOp::BitAnd),
        TokenKind::Pipe => Some(BinaryOp::BitOr),
        TokenKind::Caret => Some(BinaryOp::BitXor),
        TokenKind::Shl => Some(BinaryOp::Shl),
        TokenKind::Shr => Some(BinaryOp::Shr),
        TokenKind::StarStar => Some(BinaryOp::Pow),
        TokenKind::DotDot => Some(BinaryOp::Range),
        TokenKind::DotDotLt => Some(BinaryOp::RangeExclusive),
        TokenKind::Eq => Some(BinaryOp::Assign),
        // Compound assignment — mapped to the underlying op for desugaring
        TokenKind::PlusEq => Some(BinaryOp::Add),
        TokenKind::MinusEq => Some(BinaryOp::Sub),
        TokenKind::StarEq => Some(BinaryOp::Mul),
        TokenKind::SlashEq => Some(BinaryOp::Div),
        TokenKind::PercentEq => Some(BinaryOp::Mod),
        _ => None,
    }
}

/// Check if a token is a compound assignment operator
fn is_compound_assign(kind: &TokenKind) -> bool {
    matches!(kind, TokenKind::PlusEq | TokenKind::MinusEq | TokenKind::StarEq |
        TokenKind::SlashEq | TokenKind::PercentEq)
}

/// Get the underlying binary op for a compound assignment token
fn compound_to_binary(kind: &TokenKind) -> Option<BinaryOp> {
    match kind {
        TokenKind::PlusEq => Some(BinaryOp::Add),
        TokenKind::MinusEq => Some(BinaryOp::Sub),
        TokenKind::StarEq => Some(BinaryOp::Mul),
        TokenKind::SlashEq => Some(BinaryOp::Div),
        TokenKind::PercentEq => Some(BinaryOp::Mod),
        _ => None,
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

#[allow(dead_code)]
impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn current(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or_else(|| {
            // Last token should always be EOF
            self.tokens.last().unwrap()
        })
    }

    fn current_kind(&self) -> TokenKind {
        self.current().kind.clone()
    }

    fn advance(&mut self) {
        if self.current().kind != TokenKind::Eof {
            self.pos += 1;
        }
    }

    fn peek(&self) -> TokenKind {
        self.tokens.get(self.pos).map(|t| t.kind.clone()).unwrap_or(TokenKind::Eof)
    }

    fn peek2(&self) -> TokenKind {
        self.tokens.get(self.pos + 1).map(|t| t.kind.clone()).unwrap_or(TokenKind::Eof)
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token, ParseError> {
        let tok = self.current().clone();
        if std::mem::discriminant(&tok.kind) == std::mem::discriminant(&kind) {
            self.advance();
            Ok(tok)
        } else {
            Err(self.error(&format!("Expected {}, got {}", kind, tok.kind)))
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Result<(), ParseError> {
        let kind = self.current_kind();
        let matches = match &kind {
            TokenKind::Ident(s) => s == kw,
            TokenKind::Else => kw == "else",
            TokenKind::Fun => kw == "fun",
            _ => false,
        };
        if matches {
            self.advance();
            Ok(())
        } else {
            Err(self.error(&format!("Expected keyword '{}', got {}", kw, kind)))
        }
    }

    fn skip(&mut self, kind: TokenKind) -> bool {
        if std::mem::discriminant(&self.current_kind()) == std::mem::discriminant(&kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error(&self, msg: &str) -> ParseError {
        let tok = self.current();
        ParseError {
            message: msg.to_string(),
            line: tok.span.line,
            col: tok.span.col,
        }
    }

    fn error_at(&self, msg: &str, line: usize, col: usize) -> ParseError {
        ParseError { message: msg.to_string(), line, col }
    }

    fn current_span(&self) -> Span {
        self.current().span
    }

    // ---- Parse Program ----

    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut stmts = Vec::new();
        while self.current_kind() != TokenKind::Eof {
            stmts.push(self.parse_statement()?);
            // Optional semicolons between statements
            self.skip(TokenKind::Semicolon);
        }
        Ok(Program { stmts })
    }

    // ---- Statement Parsing ----

    fn parse_statement(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        match self.current_kind() {
            TokenKind::Val | TokenKind::Var | TokenKind::Lazy => self.parse_let(),
            TokenKind::Const => self.parse_const(),
            TokenKind::Fun => self.parse_fun_def(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Break => {
                let span = self.current_span();
                self.advance();
                Ok(Stmt::Break { span })
            }
            TokenKind::Continue => {
                let span = self.current_span();
                self.advance();
                Ok(Stmt::Continue { span })
            }
            TokenKind::Type => self.parse_type_alias(),
            TokenKind::Enum => self.parse_enum_def(),
            TokenKind::Module => self.parse_module(),
            TokenKind::Export => self.parse_export(),
            TokenKind::Import => self.parse_import(),
            TokenKind::Extension => self.parse_extension(),
            TokenKind::External => self.parse_external_fun(),
            _ => {
                let expr = self.parse_expr()?;
                self.skip(TokenKind::Semicolon);
                Ok(Stmt::Expr { expr, span: start_span })
            }
        }
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();

        // Check for lazy keyword
        let lazy_init = if self.current_kind() == TokenKind::Lazy {
            self.advance();
            true
        } else {
            false
        };

        let mutable = match &self.current_kind() {
            TokenKind::Var => true,
            TokenKind::Val => false,
            _ => return Err(self.error("Expected 'val' or 'var'")),
        };
        self.advance();

        // Check for destructuring pattern: val (x, y) = ... or val [a, b] = ...
        if self.current_kind() == TokenKind::LParen {
            self.advance(); // skip '('
            let mut names = Vec::new();
            loop {
                match &self.current_kind() {
                    TokenKind::Ident(s) => {
                        names.push(s.clone());
                        self.advance();
                    }
                    _ => return Err(self.error("Expected identifier in destructuring pattern")),
                }
                if self.skip(TokenKind::Comma) {
                    if self.current_kind() == TokenKind::RParen {
                        break;
                    }
                    continue;
                }
                break;
            }
            self.expect(TokenKind::RParen)?;

            // Optional error propagation
            let propagate = self.skip(TokenKind::Question);

            // Assignment
            self.expect(TokenKind::Eq)?;
            let value = self.parse_expr()?;

            return Ok(Stmt::Destructure {
                mutable, propagate, names, renames: vec![], rest: None,
                is_list: false, is_struct: false,
                value, span: start_span,
            });
        }

        // List destructuring: val [a, b, c] = list or val [head, ...tail] = list
        if self.current_kind() == TokenKind::LBracket {
            self.advance(); // skip '['
            let mut names = Vec::new();
            let mut rest = None;
            loop {
                match &self.current_kind() {
                    TokenKind::DotDotDot => {
                        self.advance(); // skip '...'
                        // Optional variable name after ...
                        if let TokenKind::Ident(s) = &self.current_kind() {
                            rest = Some(s.clone());
                            self.advance();
                        }
                        break;
                    }
                    TokenKind::Ident(s) => {
                        names.push(s.clone());
                        self.advance();
                    }
                    TokenKind::Comma => {
                        self.advance();
                        // After comma, check for ... or another ident
                        if self.current_kind() == TokenKind::DotDotDot {
                            self.advance();
                            if let TokenKind::Ident(s) = &self.current_kind() {
                                rest = Some(s.clone());
                                self.advance();
                            }
                            break;
                        }
                        if self.current_kind() == TokenKind::RBracket {
                            break; // trailing comma
                        }
                        continue;
                    }
                    _ => return Err(self.error("Expected identifier or '...' in list destructuring")),
                }
            }
            self.expect(TokenKind::RBracket)?;

            // Optional error propagation
            let propagate = self.skip(TokenKind::Question);

            // Assignment
            self.expect(TokenKind::Eq)?;
            let value = self.parse_expr()?;

            return Ok(Stmt::Destructure {
                mutable, propagate, names, renames: vec![], rest,
                is_list: true, is_struct: false,
                value, span: start_span,
            });
        }

        // Struct destructuring: val {x, y} = expr or val {x as px, y as py} = expr
        if self.current_kind() == TokenKind::LBrace {
            self.advance(); // skip '{'
            let mut names = Vec::new();
            let mut renames = Vec::new();
            loop {
                match &self.current_kind() {
                    TokenKind::Ident(s) => {
                        let field = s.clone();
                        self.advance();
                        // Check for rename: {x as px}
                        if self.current_kind() == TokenKind::As {
                            self.advance();
                            let local = match &self.current_kind() {
                                TokenKind::Ident(s) => s.clone(),
                                _ => return Err(self.error("Expected variable name after 'as'")),
                            };
                            self.advance();
                            names.push(field.clone());
                            renames.push((field, local));
                        } else {
                            names.push(field.clone());
                            renames.push((field.clone(), field));
                        }
                    }
                    TokenKind::Comma => {
                        self.advance();
                        if self.current_kind() == TokenKind::RBrace {
                            break; // trailing comma
                        }
                        continue;
                    }
                    _ => break,
                }
            }
            self.expect(TokenKind::RBrace)?;

            // Optional error propagation
            let propagate = self.skip(TokenKind::Question);

            // Assignment
            self.expect(TokenKind::Eq)?;
            let value = self.parse_expr()?;

            return Ok(Stmt::Destructure {
                mutable, propagate, names, renames,
                rest: None, is_list: false, is_struct: true,
                value, span: start_span,
            });
        }

        // Variable name
        let name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected variable name")),
        };
        self.advance();

        // Check for error propagation: val x? / var x?
        let propagate = self.skip(TokenKind::Question);

        // Optional type annotation
        let type_ann = if self.skip(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };

        // Assignment
        self.expect(TokenKind::Eq)?;
        let value = self.parse_expr()?;

        Ok(Stmt::Let { mutable, propagate, lazy_init, name, type_ann, value, span: start_span })
    }

    fn parse_const(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'const'

        let name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected constant name")),
        };
        self.advance();

        // Optional type annotation
        let type_ann = if self.skip(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };

        // Assignment
        self.expect(TokenKind::Eq)?;
        let value = self.parse_expr()?;

        Ok(Stmt::Const { name, type_ann, value, span: start_span })
    }

    fn parse_fun_def(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'fun'

        // Parse optional generic type parameters: fun <T, U> name(...)
        let mut type_params = Vec::new();
        if self.skip(TokenKind::Lt) {
            loop {
                let tp_name = match &self.current_kind() {
                    TokenKind::Ident(s) => s.clone(),
                    _ => return Err(self.error("Expected type parameter name")),
                };
                self.advance();
                type_params.push(tp_name);
                if !self.skip(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Gt)?;
        }

        let name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected function name")),
        };
        self.advance();

        // Parameters
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        while self.current_kind() != TokenKind::RParen {
            if !params.is_empty() {
                self.expect(TokenKind::Comma)?;
            }
            let param_name = match &self.current_kind() {
                TokenKind::Ident(s) => s.clone(),
                _ => return Err(self.error("Expected parameter name")),
            };
            self.advance();

            let ty = if self.skip(TokenKind::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            params.push(Param { name: param_name, ty });
        }
        self.expect(TokenKind::RParen)?;

        // Optional return type
        let return_type = if self.skip(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };

        // Body
        // When = is followed by {, treat it as a block body (same as without =)
        // so that function parameters remain in scope. Without this, { } in
        // expression position becomes a zero-param lambda which opens a new scope.
        let (body, is_single_expr) = if self.skip(TokenKind::Eq) {
            if self.current_kind() == TokenKind::LBrace {
                (self.parse_block_expr()?, false)
            } else {
                (self.parse_expr()?, true)
            }
        } else {
            (self.parse_block_expr()?, false)
        };

        Ok(Stmt::Fun { name, params, return_type, body, type_params, is_single_expr, span: start_span })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'return'

        // Check if there's an expression following
        if matches!(self.current_kind(),
            TokenKind::Semicolon | TokenKind::RBrace | TokenKind::Eof)
        {
            Ok(Stmt::Return { value: None, span: start_span })
        } else {
            let expr = self.parse_expr()?;
            Ok(Stmt::Return { value: Some(expr), span: start_span })
        }
    }

    // ---- Type Parsing ----

    fn parse_type(&mut self) -> Result<Type, ParseError> {
        let ty = self.parse_type_primary()?;

        // Function type arrow
        if self.skip(TokenKind::Arrow) {
            let params = match ty {
                Type::Unit => vec![],
                _ => vec![ty],
            };
            let ret = self.parse_type()?;
            return Ok(Type::Function(params, Box::new(ret)));
        }

        Ok(ty)
    }

    fn parse_type_primary(&mut self) -> Result<Type, ParseError> {
        match self.current_kind() {
            TokenKind::Ident(ref name) => {
                let name = name.clone();
                self.advance();

                // Check for generic instantiation: List[Int]
                if self.skip(TokenKind::LBracket) {
                    let mut args = Vec::new();
                    while self.current_kind() != TokenKind::RBracket {
                        if !args.is_empty() {
                            self.expect(TokenKind::Comma)?;
                        }
                        args.push(self.parse_type()?);
                    }
                    self.expect(TokenKind::RBracket)?;
                    Ok(Type::Generic(Box::new(Type::Named(name)), args))
                } else {
                    Ok(Type::Named(name))
                }
            }
            TokenKind::Task => {
                self.advance();
                if self.skip(TokenKind::LBracket) {
                    let inner = self.parse_type()?;
                    self.expect(TokenKind::RBracket)?;
                    Ok(Type::Task(Box::new(inner)))
                } else {
                    Ok(Type::Named("Task".into()))
                }
            }
            TokenKind::LParen => {
                self.advance();
                // Could be unit type () or tuple/function params
                if self.skip(TokenKind::RParen) {
                    return Ok(Type::Unit);
                }
                let mut params = Vec::new();
                while self.current_kind() != TokenKind::RParen {
                    if !params.is_empty() {
                        self.expect(TokenKind::Comma)?;
                    }
                    params.push(self.parse_type()?);
                }
                self.expect(TokenKind::RParen)?;

                if self.skip(TokenKind::Arrow) {
                    let ret = self.parse_type()?;
                    Ok(Type::Function(params, Box::new(ret)))
                } else {
                    // Just a parenthesized type (treat as first param)
                    Ok(params.into_iter().next().unwrap_or(Type::Unit))
                }
            }
            TokenKind::LBrace => {
                // Struct type: {x: Int, y: Int}
                self.advance();
                let mut fields = Vec::new();
                while self.current_kind() != TokenKind::RBrace {
                    if !fields.is_empty() {
                        self.expect(TokenKind::Comma)?;
                    }
                    let name = match &self.current_kind() {
                        TokenKind::Ident(s) => s.clone(),
                        _ => return Err(self.error("Expected field name")),
                    };
                    self.advance();
                    self.expect(TokenKind::Colon)?;
                    let ty = self.parse_type()?;
                    fields.push((name, ty));
                }
                self.expect(TokenKind::RBrace)?;
                Ok(Type::Struct(fields))
            }
            _ => Err(self.error("Expected type")),
        }
    }

    // ---- Expression Parsing (Pratt) ----

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_pratt(Precedence::Lowest)
    }

    fn parse_pratt(&mut self, min_prec: Precedence) -> Result<Expr, ParseError> {
        let mut left = self.parse_prefix()?;

        loop {
            // Postfix operators first — they bind tighter than any binary operator.
            // After each postfix, continue the outer loop so binary operators after
            // the postfix (e.g. r.x + 1) are correctly parsed.
            let postfix_applied = match self.current_kind() {
                TokenKind::LParen => {
                    left = self.parse_call_suffix(left)?;
                    true
                }
                TokenKind::Dot => {
                    self.advance();
                    let field = match &self.current_kind() {
                        TokenKind::Ident(s) => {
                            let name = s.clone();
                            self.advance();
                            name
                        }
                        TokenKind::IntLiteral(n) => {
                            let name = n.to_string();
                            self.advance();
                            name
                        }
                        TokenKind::When | TokenKind::For => {
                            let kw = self.current_kind().to_string();
                            self.advance();
                            kw
                        }
                        _ => return Err(self.error("Expected field name after '.'")),
                    };
                    left = Expr::FieldAccess(Box::new(left), field);
                    true
                }
                TokenKind::ColonColon => {
                    self.advance();
                    let method = match &self.current_kind() {
                        TokenKind::Ident(s) => {
                            let name = s.clone();
                            self.advance();
                            name
                        }
                        _ => return Err(self.error("Expected method name after '::'")),
                    };
                    let type_name = match &left {
                        Expr::Ident(name) => name.clone(),
                        _ => return Err(self.error("Expected type name before '::' (e.g., Int::toString)")),
                    };
                    left = Expr::FunctionRef(format!("{}.{}", type_name, method));
                    true
                }
                TokenKind::SafeDot => {
                    self.advance();
                    let field = match &self.current_kind() {
                        TokenKind::Ident(s) => s.clone(),
                        _ => return Err(self.error("Expected field name after '?.'")),
                    };
                    self.advance();
                    if self.current_kind() == TokenKind::LParen {
                        self.advance();
                        let mut args = Vec::new();
                        while self.current_kind() != TokenKind::RParen {
                            if !args.is_empty() {
                                self.expect(TokenKind::Comma)?;
                            }
                            args.push(self.parse_expr()?);
                        }
                        self.expect(TokenKind::RParen)?;
                        let method = Expr::FieldAccess(Box::new(left), field);
                        left = Expr::SafeCall {
                            receiver: Box::new(method),
                            args,
                        };
                    } else {
                        left = Expr::SafeFieldAccess(Box::new(left), field);
                    }
                    true
                }
                TokenKind::LBracket => {
                    self.advance();
                    let idx = self.parse_expr()?;
                    self.expect(TokenKind::RBracket)?;
                    left = Expr::Index(Box::new(left), Box::new(idx));
                    true
                }
                TokenKind::Question => {
                    self.advance();
                    if self.skip(TokenKind::Eq) {
                        let value = self.parse_expr()?;
                        left = Expr::Assign {
                            target: Box::new(left),
                            value: Box::new(value),
                            propagate: true,
                        };
                    } else {
                        left = Expr::Try(Box::new(left));
                    }
                    true
                }
                TokenKind::LBrace => {
                    let is_callable = matches!(&left, Expr::Ident(name)
                        if name == "launch" || name == "coroutineScope")
                        || matches!(&left, Expr::FieldAccess(_, _));
                    if is_callable {
                        let lambda = self.parse_lambda_or_struct()?;
                        if matches!(lambda, Expr::Lambda { .. }) {
                            left = Expr::Call {
                                func: Box::new(left),
                                args: vec![],
                                trailing_lambda: Some(Box::new(lambda)),
                            };
                            true
                        } else {
                            return Err(self.error("Expected lambda after call"));
                        }
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if postfix_applied {
                continue;
            }

            // Binary / compound / special operators
            let tok_kind = self.current_kind();
            if is_compound_assign(&tok_kind) {
                let base_op = compound_to_binary(&tok_kind).unwrap();
                self.advance();
                let right = self.parse_pratt(Precedence::Assignment.next())?;
                let lhs_clone = left.clone();
                left = Expr::Assign {
                    target: Box::new(left),
                    value: Box::new(Expr::Binary(Box::new(lhs_clone), base_op, Box::new(right))),
                    propagate: false,
                };
                continue;
            }

            if let Some(op) = token_to_binary_op(&tok_kind) {
                let prec = Precedence::of_binary(&op);
                if prec < min_prec {
                    break;
                }
                self.advance();
                let right = self.parse_pratt(prec.next())?;
                if op == BinaryOp::Assign {
                    left = Expr::Assign {
                        target: Box::new(left),
                        value: Box::new(right),
                        propagate: false,
                    };
                } else {
                    left = Expr::Binary(Box::new(left), op, Box::new(right));
                }
                continue;
            }

            if tok_kind == TokenKind::In || tok_kind == TokenKind::Is {
                let prec = Precedence::Comparison;
                if prec < min_prec {
                    break;
                }
                let op = if tok_kind == TokenKind::In { BinaryOp::In } else { BinaryOp::Is };
                self.advance();
                let right = self.parse_pratt(prec.next())?;
                left = Expr::Binary(Box::new(left), op, Box::new(right));
                continue;
            }

            if let TokenKind::Ident(ref s) = tok_kind {
                if s == "to" {
                    let prec = Precedence::To;
                    if prec < min_prec {
                        break;
                    }
                    self.advance();
                    let right = self.parse_pratt(prec.next())?;
                    let mut elements = if let Expr::Tuple(elems) = left {
                        elems
                    } else {
                        vec![(None, left)]
                    };
                    match right {
                        Expr::Tuple(elems) => elements.extend(elems),
                        _ => elements.push((None, right)),
                    }
                    left = Expr::Tuple(elements);
                    continue;
                }
            }

            break;
        }

        Ok(left)
    }

    fn parse_call_suffix(&mut self, func: Expr) -> Result<Expr, ParseError> {
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        while self.current_kind() != TokenKind::RParen {
            if !args.is_empty() {
                self.expect(TokenKind::Comma)?;
            }
            // Check for trailing lambda (syntax sugar)
            // If we see a { at the end, treat it as a lambda
            if self.current_kind() == TokenKind::LBrace && self.peek2() != TokenKind::Eq {
                // Could be a lambda { ... } or a struct literal {x = ...}
                // We distinguish by looking ahead: { ident -> ... } is lambda
                // { ident = ... } is struct literal
                // For simplicity: look ahead a few tokens
                // Let's just treat it as expression
                args.push(self.parse_expr()?);
            } else {
                args.push(self.parse_expr()?);
            }
        }
        self.expect(TokenKind::RParen)?;

        // Check for trailing lambda (outside parentheses).
        // Only consume { as trailing lambda if the content looks like a lambda
        // (params -> body or expression), not a statement block (var/val/for/when/...).
        let is_simple_target = matches!(&func, Expr::Ident(_))
            || matches!(&func, Expr::FieldAccess(_, _));
        if is_simple_target && self.current_kind() == TokenKind::LBrace && self.brace_is_lambda_like()
        {
            let lambda = self.parse_lambda_or_struct()?;
            if matches!(lambda, Expr::Lambda { .. }) {
                return Ok(Expr::Call {
                    func: Box::new(func),
                    args,
                    trailing_lambda: Some(Box::new(lambda)),
                });
            } else {
                return Err(self.error("Expected lambda after call"));
            }
        }

        Ok(Expr::Call { func: Box::new(func), args, trailing_lambda: None })
    }

    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        match self.current_kind() {
            TokenKind::IntLiteral(n) => {
                self.advance();
                Ok(Expr::int(n))
            }
            TokenKind::FloatLiteral(n) => {
                self.advance();
                Ok(Expr::float(n))
            }
            TokenKind::BoolLiteral(b) => {
                self.advance();
                Ok(Expr::bool(b))
            }
            TokenKind::CharLiteral(c) => {
                self.advance();
                Ok(Expr::Literal(Literal::Char(c)))
            }
            TokenKind::StringLiteral(ref s) => {
                let s = s.clone();
                self.advance();
                // Check for string interpolation: if string contains $ or ${
                if s.contains('$') {
                    self.parse_interpolated_string(&s)
                } else {
                    Ok(Expr::string(&s))
                }
            }
            TokenKind::Ident(ref name) => {
                let name = name.clone();
                self.advance();

                // Collection literals: List[...], Set[...], Map[...]
                if (name == "List" || name == "Set" || name == "Map")
                    && self.current_kind() == TokenKind::LBracket
                {
                    return self.parse_collection_literal(&name);
                }
                // Check for function call (identifier followed by paren)
                if self.current_kind() == TokenKind::LParen {
                    self.parse_call_suffix(Expr::Ident(name.clone()))
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            TokenKind::ColonColon => {
                self.advance(); // skip ::
                // Parse function reference path
                let mut path = String::new();
                match &self.current_kind() {
                    TokenKind::Ident(s) => {
                        path.push_str(s);
                        self.advance();
                    }
                    _ => return Err(self.error("Expected function name after ::").into()),
                }
                // Parse rest of path: ::method_name or .field_name
                while self.current_kind() == TokenKind::ColonColon || self.current_kind() == TokenKind::Dot {
                    if self.current_kind() == TokenKind::ColonColon {
                        path.push_str("::");
                    } else {
                        path.push('.');
                    }
                    self.advance();
                    match &self.current_kind() {
                        TokenKind::Ident(s) => {
                            path.push_str(s);
                            self.advance();
                        }
                        _ => return Err(self.error("Expected identifier in function reference path").into()),
                    }
                }
                Ok(Expr::FunctionRef(path))
            }
            TokenKind::Minus => {
                self.advance();
                let expr = self.parse_pratt(Precedence::Unary)?;
                Ok(Expr::unary(UnaryOp::Neg, expr))
            }
            TokenKind::Not => {
                self.advance();
                let expr = self.parse_pratt(Precedence::Unary)?;
                Ok(Expr::unary(UnaryOp::Not, expr))
            }
            TokenKind::Tilde => {
                self.advance();
                let expr = self.parse_pratt(Precedence::Unary)?;
                Ok(Expr::unary(UnaryOp::BitNot, expr))
            }
            TokenKind::Continue => {
                self.advance();
                Ok(Expr::Continue)
            }
            TokenKind::Break => {
                self.advance();
                Ok(Expr::Break)
            }
            TokenKind::When => self.parse_when(),
            TokenKind::For => self.parse_for(),
            TokenKind::Copy => {
                self.advance();
                let expr = self.parse_prefix()?;
                Ok(Expr::Copy(Box::new(expr)))
            }
            TokenKind::Unsafe => {
                self.advance();
                self.expect(TokenKind::LBrace)?;
                let body = self.parse_block_body()?;
                Ok(Expr::Unsafe(Box::new(body)))
            }
            TokenKind::LBrace => self.parse_lambda_or_struct(),
            TokenKind::LParen => self.parse_paren_or_tuple(),
            // [ alone is no longer a list literal — use List[...] instead
            TokenKind::LBracket => Err(self.error("Unexpected '[' — use List[...] for list literals, or variable[index] for indexing")),
            TokenKind::Underscore => {
                self.advance();
                // Wildcard pattern — typically used in patterns, return as Ident for now
                Ok(Expr::Ident("_".to_string()))
            }
            _ => Err(self.error(&format!("Unexpected token: {}", self.current_kind()))),
        }
    }

    fn parse_interpolated_string(&self, s: &str) -> Result<Expr, ParseError> {
        // Handle ${expr} interpolation only (per v6 spec)
        let mut parts = Vec::new();
        let mut current = String::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
                if !current.is_empty() {
                    parts.push(StringPart::Literal(current.clone()));
                    current.clear();
                }
                // ${expr}
                let mut expr_str = String::new();
                let mut depth = 1;
                i += 2;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '{' {
                        depth += 1;
                    } else if chars[i] == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    expr_str.push(chars[i]);
                    i += 1;
                }
                // Parse the embedded expression
                let mut sub_lexer = Lexer::new(&expr_str);
                let sub_tokens = sub_lexer.tokenize();
                let mut sub_parser = Parser::new(sub_tokens);
                let expr = sub_parser.parse_expr().unwrap_or_else(|_| Expr::string(&expr_str));
                parts.push(StringPart::Expr(Box::new(expr)));
            } else {
                current.push(chars[i]);
            }
            i += 1;
        }
        if !current.is_empty() {
            parts.push(StringPart::Literal(current));
        }
        Ok(Expr::StringInterpolate(parts))
    }

    fn parse_paren_or_tuple(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // skip '('

        if self.skip(TokenKind::RParen) {
            return Ok(Expr::Literal(Literal::Unit));
        }

        let first = self.parse_expr()?;

        // Check for named tuple: (name: value, ...)
        // If first expr is an Ident followed by ':', treat as named field

        // Check for named first element: (name: value, ...)
        let mut exprs: Vec<(Option<String>, Expr)> = Vec::new();

        // Check if first expr is named: ident followed by ':'
        let named_first = if let Expr::Ident(ref name) = first {
            if self.current_kind() == TokenKind::Colon {
                let field_name = name.clone();
                self.advance(); // skip ':'
                let value = self.parse_expr()?;
                Some((field_name, value))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((name, val)) = named_first {
            exprs.push((Some(name), val));
        } else {
            exprs.push((None, first));
        }

        if self.skip(TokenKind::RParen) {
            // Single expression in parens — but now unwrap from tuple wrapper
            if exprs.len() == 1 && exprs[0].0.is_none() {
                return Ok(exprs.remove(0).1);
            }
            return Ok(Expr::Tuple(exprs));
        }

        // Tuple
        self.expect(TokenKind::Comma)?;
        while self.current_kind() != TokenKind::RParen {
            // Check for named field: identifier : expression
            if let TokenKind::Ident(_) = &self.current_kind() {
                if self.peek2() == TokenKind::Colon {
                    let name = match &self.current_kind() {
                        TokenKind::Ident(s) => s.clone(),
                        _ => unreachable!(),
                    };
                    self.advance(); // skip name
                    self.advance(); // skip ':'
                    let value = self.parse_expr()?;
                    exprs.push((Some(name), value));
                } else {
                    exprs.push((None, self.parse_expr()?));
                }
            } else {
                exprs.push((None, self.parse_expr()?));
            }
            if self.current_kind() != TokenKind::RParen {
                self.expect(TokenKind::Comma)?;
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(Expr::Tuple(exprs))
    }

    /// Parse collection literal after `List`, `Set`, or `Map` keyword: List[...], Set[...], Map[...]
    fn parse_collection_literal(&mut self, kind: &str) -> Result<Expr, ParseError> {
        self.expect(TokenKind::LBracket)?; // consume '['

        match kind {
            "List" => {
                let mut items = Vec::new();
                while self.current_kind() != TokenKind::RBracket {
                    if !items.is_empty() {
                        self.expect(TokenKind::Comma)?;
                    }
                    if self.current_kind() == TokenKind::RBracket {
                        break; // trailing comma
                    }
                    items.push(self.parse_expr()?);
                }
                self.expect(TokenKind::RBracket)?;
                Ok(Expr::call(
                    Expr::Ident("__list".to_string()),
                    items,
                ))
            }
            "Set" => {
                let mut elements = Vec::new();
                while self.current_kind() != TokenKind::RBracket {
                    if !elements.is_empty() {
                        self.expect(TokenKind::Comma)?;
                    }
                    if self.current_kind() == TokenKind::RBracket {
                        break; // trailing comma
                    }
                    let elem = self.parse_expr()?;
                    elements.push(elem);
                }
                self.expect(TokenKind::RBracket)?;
                Ok(Expr::SetLiteral(elements))
            }
            "Map" => {
                let mut entries = Vec::new();
                while self.current_kind() != TokenKind::RBracket {
                    if !entries.is_empty() {
                        self.expect(TokenKind::Comma)?;
                    }
                    if self.current_kind() == TokenKind::RBracket {
                        break; // trailing comma
                    }
                    let key = self.parse_expr()?;
                    self.expect(TokenKind::Colon)?;
                    let value = self.parse_expr()?;
                    entries.push((key, value));
                }
                self.expect(TokenKind::RBracket)?;
                Ok(Expr::MapLiteral(entries))
            }
            _ => unreachable!(),
        }
    }

    /// Scan ahead from current position to see if there's an `->` before `}` (at depth 0).
    /// Used to distinguish lambda params from struct shorthand fields.
    /// Peek past `{` to check if content looks like a lambda/struct, not a block.
    /// Returns false if the brace starts with statement keywords (var, val, for, ...).
    fn brace_is_lambda_like(&self) -> bool {
        // Current token is LBrace; peek at the next token
        if self.pos + 1 >= self.tokens.len() {
            return false;
        }
        match &self.tokens[self.pos + 1].kind {
            TokenKind::Var | TokenKind::Val | TokenKind::For | TokenKind::When
            | TokenKind::Return | TokenKind::Const | TokenKind::Fun | TokenKind::Import
            | TokenKind::Export | TokenKind::Type | TokenKind::Enum | TokenKind::External
            | TokenKind::Module | TokenKind::RBrace => false,
            _ => true,
        }
    }

    fn scan_ahead_for_arrow(&self) -> bool {
        let saved = self.tokens.iter().skip(self.pos);
        let mut brace_depth = 0;
        for token in saved {
            match &token.kind {
                TokenKind::LBrace => brace_depth += 1,
                TokenKind::RBrace => {
                    if brace_depth == 0 {
                        return false; // found } before ->
                    }
                    brace_depth -= 1;
                }
                TokenKind::Arrow => {
                    if brace_depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn parse_lambda_or_struct(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // skip '{'

        // {} → empty block returning unit ()
        if self.skip(TokenKind::RBrace) {
            return Ok(Expr::Tuple(vec![])); // unit value
        }

        // {:} is an error — use Map[] instead
        if self.skip(TokenKind::Colon) {
            self.expect(TokenKind::RBrace)?;
            return Err(self.error("Use Map[] for empty map literal, not {:}"));
        }

        // To distinguish struct literal from lambda:
        // - {x = expr, ...} or {x: expr, ...} → struct (Ident + '=' or ':')
        // - {x, y} → struct if no '->' before '}' (shorthand fields)
        // - {x -> body} or {x, y -> body} → lambda (has '->')
        let is_struct = if matches!(self.current_kind(), TokenKind::Ident(_)) {
            match self.peek2() {
                TokenKind::Eq | TokenKind::Colon => true,
                TokenKind::Comma => {
                    !self.scan_ahead_for_arrow()
                }
                TokenKind::Arrow => false, // {x -> body} is lambda
                _ => false, // {expr} is lambda (block)
            }
        } else {
            false
        };

        if is_struct {
            return self.parse_struct_literal();
        }

        // Everything else in expression position with {} is a lambda
        // Check for explicit params: { x, y -> body } or { x -> body }
        let mut has_explicit_params = false;
        let mut implicit_it = false;

        // Look for identifiers before ->
        if let TokenKind::Ident(ref first_id) = self.current_kind() {
            let first_id = first_id.clone();
            // Peek ahead to see if we have -> after identifiers
            let mut peek_pos = self.pos;
            let mut found_arrow = false;
            loop {
                match self.tokens.get(peek_pos).map(|t| &t.kind) {
                    Some(TokenKind::Arrow) => {
                        found_arrow = true;
                        break;
                    }
                    Some(TokenKind::Comma) => {
                        peek_pos += 1;
                        match self.tokens.get(peek_pos).map(|t| &t.kind) {
                            Some(TokenKind::Ident(_)) => { peek_pos += 1; }
                            _ => break,
                        }
                    }
                    Some(TokenKind::Ident(_)) => { peek_pos += 1; }
                    _ => break,
                }
            }
            has_explicit_params = found_arrow;

            // If first ident is 'it' and not followed by ->, it's an implicit-it lambda
            if !has_explicit_params && first_id == "it" {
                implicit_it = true;
            }
        }

        if has_explicit_params {
            return self.parse_lambda_body(false);
        }

        if implicit_it {
            // { it ... } — body contains `it` reference
            let body = self.parse_expr()?;
            self.expect(TokenKind::RBrace)?;
            return Ok(Expr::it_lambda(body));
        }

        // { stmts } — no-param lambda with block body (handles both single expr and multi-stmt)
        let body = self.parse_block_body()?;
        Ok(Expr::Lambda { params: vec![], body: Box::new(body), implicit_it: false })
    }

    fn parse_struct_literal(&mut self) -> Result<Expr, ParseError> {
        let mut fields = Vec::new();
        while self.current_kind() != TokenKind::RBrace {
            if !fields.is_empty() {
                self.expect(TokenKind::Comma)?;
            }
            let name = match &self.current_kind() {
                TokenKind::Ident(s) => s.clone(),
                _ => return Err(self.error("Expected field name")),
            };
            self.advance();

            // Check for shorthand: {x, y} — field name used as variable
            if self.current_kind() == TokenKind::Eq {
                self.advance();
                let value = self.parse_expr()?;
                fields.push((name, value));
            } else {
                // Shorthand: {x} becomes {x: x}
                fields.push((name.clone(), Expr::Ident(name)));
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(Expr::StructLiteral(fields))
    }

    /// Check if the token after the current one is a colon (for Map detection)
    fn parse_lambda_body(&mut self, implicit_it: bool) -> Result<Expr, ParseError> {
        if implicit_it {
            // { expr } — single expression with implicit `it`
            let body = self.parse_expr()?;
            self.expect(TokenKind::RBrace)?;
            return Ok(Expr::it_lambda(body));
        }

        let mut params = Vec::new();

        // Parse parameters
        loop {
            match &self.current_kind() {
                TokenKind::Ident(name) => {
                    params.push(name.clone());
                    self.advance();
                    if self.current_kind() == TokenKind::Comma {
                        self.advance();
                        continue;
                    }
                    break;
                }
                _ => {
                    // No explicit params — treat as no-param lambda { expr }
                    let body = self.parse_expr()?;
                    self.expect(TokenKind::RBrace)?;
                    return Ok(Expr::Lambda {
                        params: vec![],
                        body: Box::new(body),
                        implicit_it: false,
                    });
                }
            }
        }

        // Expect ->
        self.expect(TokenKind::Arrow)?;

        let body = self.parse_expr()?;
        self.expect(TokenKind::RBrace)?;

        Ok(Expr::Lambda { params, body: Box::new(body), implicit_it: false })
    }

    fn parse_block_body(&mut self) -> Result<Expr, ParseError> {
        let mut stmts = Vec::new();

        loop {
            if self.current_kind() == TokenKind::RBrace {
                self.advance();
                break;
            }

            stmts.push(self.parse_statement()?);
            self.skip(TokenKind::Semicolon);

            // Check if this is the last expression (without semicolon)
            // If we hit a closing brace, we're done
            if self.current_kind() == TokenKind::RBrace {
                self.advance();
                break;
            }
        }

        Ok(Expr::Block(stmts))
    }

    fn parse_block_expr(&mut self) -> Result<Expr, ParseError> {
        self.expect(TokenKind::LBrace)?;
        self.parse_block_body()
    }

    fn parse_when(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // skip 'when'

        // Check for: when { cond -> body; ... }
        if self.current_kind() == TokenKind::LBrace {
            self.advance(); // skip '{'
            let mut arms = Vec::new();
            while self.current_kind() != TokenKind::RBrace {
                if !arms.is_empty() {
                    self.skip(TokenKind::Comma);
                    self.skip(TokenKind::Semicolon);
                }
                // Handle `else -> body` as wildcard (always matches)
                let pattern = if self.current_kind() == TokenKind::Else {
                    self.advance(); // skip 'else'
                    Pattern::Wildcard
                } else {
                    // Try parsing as expression first — condition chains use expressions
                    // like `x < 0 -> "negative"`. If it's a simple identifier or pattern,
                    // the expression will parse correctly too.
                    let expr = self.parse_expr()?;
                    Pattern::Expr(Box::new(expr))
                };
                let guard = if self.current_kind() == TokenKind::And {
                    self.advance(); // skip 'and'
                    Some(Box::new(self.parse_expr()?))
                } else {
                    None
                };
                self.expect(TokenKind::Arrow)?;
                let body = self.parse_expr()?;
                arms.push(WhenArm { pattern, guard, body: Box::new(body) });
                self.skip(TokenKind::Comma);
                self.skip(TokenKind::Semicolon);
            }
            self.expect(TokenKind::RBrace)?;
            return Ok(Expr::When(Box::new(When {
                kind: WhenKind::ConditionChain { arms },
            })));
        }

        // Parse the condition or value
        let first = self.parse_expr()?;

        // when first { ... }
        if self.current_kind() == TokenKind::LBrace {
            self.advance(); // skip '{'

            // Distinguish: binary { A else B } vs value-match { Pat -> expr, ... }
            // Try parsing first item as a pattern. If followed by ->, it's value-match.
            // Also handle optional `and guard` between pattern and ->.
            let saved_pos = self.pos;
            let is_value_match = self.parse_pattern().ok()
                .map_or(false, |_| {
                    // Skip optional and guard
                    if self.current_kind() == TokenKind::And {
                        self.advance();
                        let _ = self.parse_expr();
                    }
                    self.current_kind() == TokenKind::Arrow
                });
            self.pos = saved_pos;

            if is_value_match {
                // when value { Pat -> expr, ... }
                let mut arms = Vec::new();
                while self.current_kind() != TokenKind::RBrace {
                    if !arms.is_empty() {
                        self.skip(TokenKind::Comma);
                        self.skip(TokenKind::Semicolon);
                    }
                    let pattern = self.parse_pattern()?;
                    let guard = if self.current_kind() == TokenKind::And {
                        self.advance();
                        Some(Box::new(self.parse_expr()?))
                    } else {
                        None
                    };
                    self.expect(TokenKind::Arrow)?;
                    let body = self.parse_pratt(Precedence::Shift)?;
                    arms.push(WhenArm { pattern, guard, body: Box::new(body) });
                    self.skip(TokenKind::Comma);
                    self.skip(TokenKind::Semicolon);
                }
                self.expect(TokenKind::RBrace)?;
                return Ok(Expr::When(Box::new(When {
                    kind: WhenKind::ValueMatch {
                        value: Box::new(first),
                        arms,
                    },
                })));
            } else {
                // Binary conditional: when cond { true_expr else false_expr }
                // else clause is optional; if omitted, defaults to ()
                // Support struct-literal arms: when cond { {fields} else {fields} }
                // The outer { was consumed at line 1481; if arm starts with {, it's
                // a struct literal or block expression which needs its own {} pair.
                // The when-block's closing } must still appear after the last arm.
                let true_expr = self.parse_when_arm_expr()?;
                let false_expr = if self.current_kind() == TokenKind::Else {
                    self.advance();
                    self.parse_when_arm_expr()?
                } else {
                    Expr::Literal(Literal::Unit)
                };
                self.expect(TokenKind::RBrace)?;
                return Ok(Expr::When(Box::new(When {
                    kind: WhenKind::OneLine {
                        condition: Box::new(first),
                        then_expr: Box::new(true_expr),
                        else_expr: Box::new(false_expr),
                    },
                })));
            }
        }

        Err(self.error("Invalid when expression"))
    }

    /// Parse an expression that appears as a when arm (inside the when-block { }).
    /// After the outer { of the when block is consumed, the arm may itself be a
    /// struct literal or block starting with {, so we route to the right parser.
    fn parse_when_arm_expr(&mut self) -> Result<Expr, ParseError> {
        if self.current_kind() == TokenKind::LBrace {
            self.parse_lambda_or_struct()
        } else {
            self.parse_expr()
        }
    }

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let first = self.parse_single_pattern()?;
        // Or-patterns: 0 | 1 | 2
        if self.current_kind() == TokenKind::Pipe {
            let mut patterns = vec![first];
            while self.current_kind() == TokenKind::Pipe {
                self.advance(); // skip '|'
                patterns.push(self.parse_single_pattern()?);
            }
            Ok(Pattern::Or(patterns))
        } else {
            Ok(first)
        }
    }

    fn parse_single_pattern(&mut self) -> Result<Pattern, ParseError> {
        match self.current_kind() {
            TokenKind::Underscore => {
                self.advance();
                Ok(Pattern::Wildcard)
            }
            TokenKind::Else => {
                self.advance();
                Ok(Pattern::Wildcard)
            }
            TokenKind::IntLiteral(n) => {
                self.advance();
                Ok(Pattern::Literal(Literal::Int(n)))
            }
            TokenKind::BoolLiteral(b) => {
                self.advance();
                Ok(Pattern::Literal(Literal::Bool(b)))
            }
            TokenKind::StringLiteral(ref s) => {
                let s = s.clone();
                self.advance();
                Ok(Pattern::Literal(Literal::String(s)))
            }
            TokenKind::CharLiteral(c) => {
                self.advance();
                Ok(Pattern::Literal(Literal::Char(c)))
            }
            TokenKind::FloatLiteral(f) => {
                self.advance();
                Ok(Pattern::Literal(Literal::Float(f)))
            }
            TokenKind::Ident(ref name) => {
                let name = name.clone();
                self.advance();

                // Check if constructor with args: Some(x) or Circle(r: Float)
                if self.current_kind() == TokenKind::LParen {
                    self.advance();
                    let mut args = Vec::new();
                    let mut named_fields = Vec::new();

                    while self.current_kind() != TokenKind::RParen {
                        if !args.is_empty() || !named_fields.is_empty() {
                            self.expect(TokenKind::Comma)?;
                        }

                        // Check for named field: name: pattern
                        if let TokenKind::Ident(ref field_name) = self.current_kind() {
                            let field_name = field_name.clone();
                            if self.peek2() == TokenKind::Colon {
                                self.advance(); // field name
                                self.advance(); // ':'
                                let pat = self.parse_pattern()?;
                                named_fields.push((field_name, pat));
                                continue;
                            }
                        }
                        args.push(self.parse_pattern()?);
                    }
                    self.expect(TokenKind::RParen)?;
                    Ok(Pattern::Constructor { name, args, named_fields })
                } else if name.chars().next().map_or(false, |c| c.is_uppercase()) {
                    // Uppercase identifier without args -> nullary constructor (e.g. Red, None)
                    Ok(Pattern::Constructor { name, args: vec![], named_fields: vec![] })
                } else {
                    // Lowercase identifier -> variable pattern
                    Ok(Pattern::Variable(name))
                }
            }
            TokenKind::In => {
                self.advance();
                // Range pattern: in start..end
                let start = self.parse_expr()?;
                self.expect(TokenKind::DotDot)?;
                let end = self.parse_expr()?;
                Ok(Pattern::Range(Box::new(start), Box::new(end)))
            }
            TokenKind::Is => {
                self.advance();
                let type_name = match &self.current_kind() {
                    TokenKind::Ident(s) => s.clone(),
                    _ => return Err(self.error("Expected type name after 'is'")),
                };
                self.advance();
                Ok(Pattern::IsType(type_name))
            }
            _ => Err(self.error("Expected pattern")),
        }
    }

    fn parse_for(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // skip 'for'

        // Check for: for { body } (infinite loop)
        if self.current_kind() == TokenKind::LBrace {
            let body = self.parse_block_expr()?;
            return Ok(Expr::For(Box::new(For {
                kind: ForKind::Infinite { body: Box::new(body) },
            })));
        }

        // Check for shorthand: for List[...] / Set[...] / Map[...] { body } uses implicit "it"
        if let TokenKind::Ident(ref name) = self.current_kind() {
            if (name == "List" || name == "Set" || name == "Map")
                && self.peek2() == TokenKind::LBracket
            {
                let collection_kind = name.clone();
                self.advance(); // skip List/Set/Map
                let iterable = self.parse_collection_literal(&collection_kind)?;
                let body = self.parse_block_expr()?;
                return Ok(Expr::For(Box::new(For {
                    kind: ForKind::Iterate {
                        var: "it".to_string(),
                        iterable: Box::new(iterable),
                        body: Box::new(body),
                        collect: true,
                    },
                })));
            }
        }

        // Check for: for var in iterable ... (var is an identifier followed by 'in')
        if let TokenKind::Ident(ref var_name) = self.current_kind() {
            if self.peek2() == TokenKind::In {
                let var = var_name.clone();
                self.advance(); // skip var name
                self.advance(); // skip 'in'

                let first_iterable = self.parse_expr()?;
                let mut bindings = vec![(var.clone(), first_iterable)];

                // Parse additional bindings: for x in xs, y in ys, ...
                while self.skip(TokenKind::Comma) {
                    let v = match &self.current_kind() {
                        TokenKind::Ident(s) => s.clone(),
                        _ => return Err(self.error("Expected variable name after ',' in for loop")),
                    };
                    self.advance();
                    self.expect(TokenKind::In)?;
                    let iter = self.parse_expr()?;
                    bindings.push((v, iter));
                }

                // Multiple bindings → nested iterate (for expression, collects results)
                if bindings.len() > 1 {
                    let body = self.parse_block_expr()?;
                    return Ok(Expr::For(Box::new(For {
                        kind: ForKind::NestedIterate {
                            bindings,
                            body: Box::new(body),
                            collect: true,
                        },
                    })));
                }

                // Single binding
                let (var_single, iterable_single) = bindings.into_iter().next().unwrap();

                // for var in iterable { body } → for expression (collects results)
                let body = self.parse_block_expr()?;
                return Ok(Expr::For(Box::new(For {
                    kind: ForKind::Iterate {
                        var: var_single,
                        iterable: Box::new(iterable_single),
                        body: Box::new(body),
                        collect: true,
                    },
                })));
            }
        }

        // Parse the first expression (for condition loops)
        let first = self.parse_expr()?;

        // for condition { body }
        if self.current_kind() == TokenKind::LBrace {
            let body = self.parse_block_expr()?;
            return Ok(Expr::For(Box::new(For {
                kind: ForKind::Condition {
                    condition: Box::new(first),
                    body: Box::new(body),
                },
            })));
        }

        Err(self.error("Invalid for expression"))
    }

    // ---- Module / Import / Export / Type / Enum ----

    fn parse_type_alias(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'type'

        let name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected type name")),
        };
        self.advance();

        // Optional type parameters: type Foo[A, B] = ...
        let type_params = if self.skip(TokenKind::LBracket) {
            let mut params = Vec::new();
            while self.current_kind() != TokenKind::RBracket {
                if !params.is_empty() {
                    self.expect(TokenKind::Comma)?;
                }
                match &self.current_kind() {
                    TokenKind::Ident(s) => params.push(s.clone()),
                    _ => return Err(self.error("Expected type parameter name")),
                }
                self.advance();
            }
            self.expect(TokenKind::RBracket)?;
            params
        } else {
            vec![]
        };

        self.expect(TokenKind::Eq)?;
        let definition = self.parse_type()?;

        Ok(Stmt::TypeAlias { name, type_params, definition, span: start_span })
    }

    fn parse_enum_def(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'enum'

        let name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected enum name")),
        };
        self.advance();

        let type_params = if self.skip(TokenKind::LBracket) {
            let mut params = Vec::new();
            while self.current_kind() != TokenKind::RBracket {
                if !params.is_empty() {
                    self.expect(TokenKind::Comma)?;
                }
                match &self.current_kind() {
                    TokenKind::Ident(s) => params.push(s.clone()),
                    _ => return Err(self.error("Expected type parameter name")),
                }
                self.advance();
            }
            self.expect(TokenKind::RBracket)?;
            params
        } else {
            vec![]
        };

        self.expect(TokenKind::LBrace)?;
        let mut variants = Vec::new();

        while self.current_kind() != TokenKind::RBrace {
            if !variants.is_empty() {
                self.skip(TokenKind::Comma);
            }

            let variant_name = match &self.current_kind() {
                TokenKind::Ident(s) => s.clone(),
                _ => return Err(self.error("Expected variant name")),
            };
            self.advance();

            let params = if self.skip(TokenKind::LParen) {
                let mut variant_params = Vec::new();
                while self.current_kind() != TokenKind::RParen {
                    if !variant_params.is_empty() {
                        self.expect(TokenKind::Comma)?;
                    }
                    // Check for named param: name: Type
                    if let TokenKind::Ident(ref pname) = self.current_kind() {
                        let pname = pname.clone();
                        if self.peek2() == TokenKind::Colon {
                            self.advance(); // param name
                            self.advance(); // ':'
                            let ty = self.parse_type()?;
                            variant_params.push(EnumVariantParam::Named { name: pname, ty });
                            continue;
                        }
                    }
                    let ty = self.parse_type()?;
                    variant_params.push(EnumVariantParam::Positional(ty));
                }
                self.expect(TokenKind::RParen)?;
                variant_params
            } else {
                vec![]
            };

            variants.push(EnumVariant { name: variant_name, params });
        }
        self.expect(TokenKind::RBrace)?;

        Ok(Stmt::Enum { name, type_params, variants, span: start_span })
    }

    fn parse_module(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'module'

        let name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected module name")),
        };
        self.advance();

        self.expect(TokenKind::LBrace)?;
        let mut exports = Vec::new();
        let mut body = Vec::new();

        while self.current_kind() != TokenKind::RBrace {
            if self.skip(TokenKind::Export) {
                if self.skip(TokenKind::LBrace) {
                    // export { fun f1 ... fun f2 ... } block
                    while self.current_kind() != TokenKind::RBrace {
                        let stmt = self.parse_statement()?;
                        match &stmt {
                            Stmt::Fun { name, .. } => {
                                exports.push(ExportItem::Function(name.clone()));
                            }
                            Stmt::Const { name, .. } => {
                                exports.push(ExportItem::Constant(name.clone()));
                            }
                            _ => {}
                        }
                        body.push(stmt);
                        self.skip(TokenKind::Semicolon);
                    }
                    self.expect(TokenKind::RBrace)?;
                } else {
                    // Parse the exported statement properly
                    let stmt = self.parse_statement()?;
                    match &stmt {
                        Stmt::Fun { name, .. } => {
                            exports.push(ExportItem::Function(name.clone()));
                        }
                        Stmt::Const { name, .. } => {
                            exports.push(ExportItem::Constant(name.clone()));
                        }
                        Stmt::TypeAlias { name, .. } => {
                            exports.push(ExportItem::Type(name.clone()));
                        }
                        _ => {}
                    }
                    body.push(stmt);
                }
            } else {
                body.push(self.parse_statement()?);
            }
            self.skip(TokenKind::Semicolon);
        }
        self.expect(TokenKind::RBrace)?;

        Ok(Stmt::Module { name, exports, body, span: start_span })
    }

    fn parse_export(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'export'
        let stmt = self.parse_statement()?;
        Ok(Stmt::Export { stmt: Box::new(stmt), span: start_span })
    }

    fn parse_import(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'import'

        let module = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected module name")),
        };
        self.advance();

        // import math.{add, PI}
        let items = if self.skip(TokenKind::Dot) {
            self.expect(TokenKind::LBrace)?;
            let mut its = Vec::new();
            while self.current_kind() != TokenKind::RBrace {
                if !its.is_empty() {
                    self.expect(TokenKind::Comma)?;
                }
                match &self.current_kind() {
                    TokenKind::Ident(s) => its.push(s.clone()),
                    _ => return Err(self.error("Expected import item name")),
                }
                self.advance();
            }
            self.expect(TokenKind::RBrace)?;
            Some(its)
        } else {
            None
        };

        // import math as m
        let alias = if self.skip(TokenKind::As) {
            match &self.current_kind() {
                TokenKind::Ident(s) => Some(s.clone()),
                _ => return Err(self.error("Expected alias name")),
            }
        } else {
            None
        };

        Ok(Stmt::Import { module, items, alias, span: start_span })
    }

    fn parse_extension(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'extension'

        let type_name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected type name after 'extension'")),
        };
        self.advance();

        self.expect(TokenKind::LBrace)?;
        let mut methods = Vec::new();
        while self.current_kind() != TokenKind::RBrace {
            if self.current_kind() == TokenKind::Eof {
                return Err(self.error("Unterminated extension block"));
            }
            let stmt = self.parse_statement()?;
            methods.push(stmt);
            self.skip(TokenKind::Semicolon);
        }
        self.expect(TokenKind::RBrace)?;

        Ok(Stmt::Extension { type_name, methods, span: start_span })
    }

    fn parse_external_fun(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.current_span();
        self.advance(); // skip 'external'

        // external type Name
        if self.skip(TokenKind::Type) {
            let name = match &self.current_kind() {
                TokenKind::Ident(s) => s.clone(),
                _ => return Err(self.error("Expected type name after 'external type'")),
            };
            self.advance();
            return Ok(Stmt::ExternalType { name, span: start_span });
        }

        if !self.skip(TokenKind::Fun) {
            return Err(self.error("Expected 'fun' or 'type' after 'external'"));
        }

        let name = match &self.current_kind() {
            TokenKind::Ident(s) => s.clone(),
            _ => return Err(self.error("Expected function name after 'external fun'")),
        };
        self.advance();

        // Parameters
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        while self.current_kind() != TokenKind::RParen {
            if !params.is_empty() {
                self.expect(TokenKind::Comma)?;
            }
            let param_name = match &self.current_kind() {
                TokenKind::Ident(s) => s.clone(),
                _ => return Err(self.error("Expected parameter name")),
            };
            self.advance();

            let ty = if self.skip(TokenKind::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            params.push(Param { name: param_name, ty });
        }
        self.expect(TokenKind::RParen)?;

        // Optional return type
        let return_type = if self.skip(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };

        Ok(Stmt::External { name, params, return_type, span: start_span })
    }

    fn skip_to_next_stmt(&mut self) {
        // Skip tokens until we hit a meaningful statement boundary
        while self.current_kind() != TokenKind::Eof
            && self.current_kind() != TokenKind::RBrace
            && self.current_kind() != TokenKind::Semicolon
        {
            self.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(source: &str) -> Result<Program, ParseError> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        parser.parse_program()
    }

    fn parse_expr(source: &str) -> Result<Expr, ParseError> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        parser.parse_expr()
    }

    #[test]
    fn test_let_val() {
        let prog = parse("val x = 10").unwrap();
        assert_eq!(prog.stmts.len(), 1);
        match &prog.stmts[0] {
            Stmt::Let { mutable, name, .. } => {
                assert!(!mutable);
                assert_eq!(name, "x");
            }
            _ => panic!("Expected Let"),
        }
    }

    #[test]
    fn test_let_var() {
        let prog = parse("var y = 20").unwrap();
        match &prog.stmts[0] {
            Stmt::Let { mutable, name, .. } => {
                assert!(*mutable);
                assert_eq!(name, "y");
            }
            _ => panic!("Expected Let"),
        }
    }

    #[test]
    fn test_fun_def() {
        let prog = parse("fun add(x, y) { x + y }").unwrap();
        match &prog.stmts[0] {
            Stmt::Fun { name, params, .. } => {
                assert_eq!(name, "add");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].name, "x");
                assert_eq!(params[1].name, "y");
            }
            _ => panic!("Expected Fun"),
        }
    }

    #[test]
    fn test_fun_single_expr() {
        let prog = parse("fun add(x: Int, y: Int): Int = x + y").unwrap();
        match &prog.stmts[0] {
            Stmt::Fun { name, params, return_type, is_single_expr, .. } => {
                assert_eq!(name, "add");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].name, "x");
                assert!(return_type.is_some());
                assert!(*is_single_expr);
            }
            _ => panic!("Expected Fun"),
        }
    }

    #[test]
    fn test_binary_expr() {
        let expr = parse_expr("1 + 2 * 3").unwrap();
        // Should be: 1 + (2 * 3)
        match expr {
            Expr::Binary(lhs, op, rhs) => {
                assert_eq!(op, BinaryOp::Add);
                match *lhs {
                    Expr::Literal(Literal::Int(1)) => {}
                    _ => panic!("Expected 1"),
                }
                match *rhs {
                    Expr::Binary(_, BinaryOp::Mul, _) => {}
                    _ => panic!("Expected multiplication"),
                }
            }
            _ => panic!("Expected binary"),
        }
    }

    #[test]
    fn test_when_one_line() {
        let expr = parse_expr("when a > b { a else b }").unwrap();
        match expr {
            Expr::When(w) => match &w.kind {
                WhenKind::OneLine { .. } => {}
                _ => panic!("Expected one-line when"),
            },
            _ => panic!("Expected when"),
        }
    }

    #[test]
    fn test_when_value_match() {
        let prog = parse("when x { 0 -> \"zero\"; 1 -> \"one\"; else -> \"many\" }").unwrap();
        match &prog.stmts[0] {
            Stmt::Expr { expr: Expr::When(w), .. } => match &w.kind {
                WhenKind::ValueMatch { arms, .. } => {
                    assert_eq!(arms.len(), 3);
                }
                _ => panic!("Expected value match"),
            },
            _ => panic!("Expected when expr"),
        }
    }

    #[test]
    fn test_lambda() {
        let expr = parse_expr("{ it * 2 }").unwrap();
        match expr {
            Expr::Lambda { implicit_it, .. } => {
                assert!(implicit_it);
            }
            _ => panic!("Expected lambda"),
        }
    }

    #[test]
    fn test_for_iterate() {
        let prog = parse("for item in List[1,2,3] { println(item) }").unwrap();
        match &prog.stmts[0] {
            Stmt::Expr { expr: Expr::For(f), .. } => match &f.kind {
                ForKind::Iterate { var, .. } => {
                    assert_eq!(var, "item");
                }
                _ => panic!("Expected iterate"),
            },
            _ => panic!("Expected for"),
        }
    }

    #[test]
    fn test_for_expression() {
        let expr = parse_expr("for x in List[1,2,3,4,5] { x * x }").unwrap();
        match expr {
            Expr::For(f) => match &f.kind {
                ForKind::Iterate { var, .. } => {
                    assert_eq!(var, "x");
                }
                _ => panic!("Expected iterate"),
            },
            _ => panic!("Expected for"),
        }
    }

    #[test]
    fn test_enum_def() {
        let prog = parse("enum Option[T] { Some(T), None }").unwrap();
        match &prog.stmts[0] {
            Stmt::Enum { name, type_params, variants, .. } => {
                assert_eq!(name, "Option");
                assert_eq!(type_params, &vec!["T"]);
                assert_eq!(variants.len(), 2);
                assert_eq!(variants[0].name, "Some");
                assert_eq!(variants[1].name, "None");
            }
            _ => panic!("Expected Enum"),
        }
    }

    #[test]
    fn test_type_alias() {
        let prog = parse("type Point = {x: Int, y: Int}").unwrap();
        match &prog.stmts[0] {
            Stmt::TypeAlias { name, type_params, .. } => {
                assert_eq!(name, "Point");
                assert!(type_params.is_empty());
            }
            _ => panic!("Expected TypeAlias"),
        }
    }

    #[test]
    fn test_struct_literal() {
        let expr = parse_expr("{x = 10, y = 20}").unwrap();
        match expr {
            Expr::StructLiteral(fields) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "x");
                assert_eq!(fields[1].0, "y");
            }
            _ => panic!("Expected struct literal"),
        }
    }

    #[test]
    fn test_field_access() {
        let expr = parse_expr("p.x").unwrap();
        match expr {
            Expr::FieldAccess(obj, field) => {
                match *obj {
                    Expr::Ident(name) => assert_eq!(name, "p"),
                    _ => panic!("Expected identifier"),
                }
                assert_eq!(field, "x");
            }
            _ => panic!("Expected field access"),
        }
    }

    #[test]
    fn test_let_with_error_propagation() {
        let prog = parse("val x? = parse_int(\"123\")").unwrap();
        match &prog.stmts[0] {
            Stmt::Let { propagate, name, .. } => {
                assert!(*propagate);
                assert_eq!(name, "x");
            }
            _ => panic!("Expected Let with propagation"),
        }
    }
}
