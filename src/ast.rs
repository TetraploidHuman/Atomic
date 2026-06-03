use std::fmt;
use crate::lexer::Span;

// ---- Types as written in source code ----

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// Named type: Int, String, MyType, List[Int]
    Named(String),
    /// Generic instantiation: List[Int], Option[String]
    Generic(Box<Type>, Vec<Type>),
    /// Function type: (Int, String) -> Bool
    Function(Vec<Type>, Box<Type>),
    /// Struct type: {x: Int, y: Int}
    Struct(Vec<(String, Type)>),
    /// Map type: Map<K, V>
    Map(Box<Type>, Box<Type>),
    /// Set type: Set<T>
    Set(Box<Type>),
    /// Task type: Task<T> (coroutine handle)
    Task(Box<Type>),
    /// Stream type: Stream<T> (coroutine channel)
    Stream(Box<Type>),
    /// LazyList type: LazyList<T> (lazy evaluation sequence)
    #[allow(dead_code)]
    LazyList(Box<Type>),
    /// CString type: null-terminated C string pointer
    #[allow(dead_code)]
    CString,
    /// Ptr<T>: typed pointer for FFI
    #[allow(dead_code)]
    Ptr(Box<Type>),
    /// FileHandle type: streaming file handle
    #[allow(dead_code)]
    FileHandle,
    /// Unit type: ()
    Unit,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Named(name) => write!(f, "{}", name),
            Type::Generic(base, args) => {
                write!(f, "{}[", base)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, "]")
            }
            Type::Function(params, ret) => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ") -> {}", ret)
            }
            Type::Struct(fields) => {
                write!(f, "{{")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", name, ty)?;
                }
                write!(f, "}}")
            }
            Type::Map(k, v) => write!(f, "Map<{}, {}>", k, v),
            Type::Set(t) => write!(f, "Set<{}>", t),
            Type::Task(t) => write!(f, "Task<{}>", t),
            Type::Stream(t) => write!(f, "Stream<{}>", t),
            Type::LazyList(t) => write!(f, "LazyList<{}>", t),
            Type::CString => write!(f, "CString"),
            Type::Ptr(t) => write!(f, "Ptr<{}>", t),
            Type::FileHandle => write!(f, "FileHandle"),
            Type::Unit => write!(f, "()"),
        }
    }
}

// ---- Literals ----

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Char(char),
    Unit,
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Int(v) => write!(f, "{}", v),
            Literal::Float(v) => write!(f, "{}", v),
            Literal::Bool(v) => write!(f, "{}", v),
            Literal::String(v) => write!(f, "\"{}\"", v),
            Literal::Char(c) => write!(f, "'{}'", c),
            Literal::Unit => write!(f, "()"),
        }
    }
}

// ---- Operators ----

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinaryOp {
    // Arithmetic
    Add, Sub, Mul, Div, Mod, Pow,
    // Comparison
    Eq, Neq, Lt, Gt, Lte, Gte,
    // Logical
    And, Or,
    // Bitwise
    BitAnd, BitOr, BitXor, Shl, Shr,
    // Range
    Range,
    RangeExclusive,
    // Containment / type test
    In,
    Is,
    // Assignment
    Assign,
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOp::Add => write!(f, "+"),
            BinaryOp::Sub => write!(f, "-"),
            BinaryOp::Mul => write!(f, "*"),
            BinaryOp::Div => write!(f, "/"),
            BinaryOp::Mod => write!(f, "%"),
            BinaryOp::Pow => write!(f, "**"),
            BinaryOp::Eq => write!(f, "=="),
            BinaryOp::Neq => write!(f, "!="),
            BinaryOp::Lt => write!(f, "<"),
            BinaryOp::Gt => write!(f, ">"),
            BinaryOp::Lte => write!(f, "<="),
            BinaryOp::Gte => write!(f, ">="),
            BinaryOp::And => write!(f, "&&"),
            BinaryOp::Or => write!(f, "||"),
            BinaryOp::BitAnd => write!(f, "&"),
            BinaryOp::BitOr => write!(f, "|"),
            BinaryOp::BitXor => write!(f, "^"),
            BinaryOp::Shl => write!(f, "<<"),
            BinaryOp::Shr => write!(f, ">>"),
            BinaryOp::Range => write!(f, ".."),
            BinaryOp::RangeExclusive => write!(f, "..<"),
            BinaryOp::In => write!(f, "in"),
            BinaryOp::Is => write!(f, "is"),
            BinaryOp::Assign => write!(f, "="),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOp::Neg => write!(f, "-"),
            UnaryOp::Not => write!(f, "!"),
            UnaryOp::BitNot => write!(f, "~"),
        }
    }
}

// ---- Patterns (for when arms) ----

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// Wildcard: _
    Wildcard,
    /// Literal pattern: 42, true, "hello"
    Literal(Literal),
    /// Variable binding: x (binds the matched value)
    Variable(String),
    /// Constructor pattern: Some(x), Circle(r), Node(left, right)
    Constructor {
        name: String,
        args: Vec<Pattern>,
        /// Named fields: Circle(r: Float)
        named_fields: Vec<(String, Pattern)>,
    },
    /// Range pattern: in 0..9
    Range(Box<Expr>, Box<Expr>),
    /// Type test: is Int, is String
    IsType(String),
    /// Or patterns: 'a', 'e', 'i', 'o', 'u'
    Or(Vec<Pattern>),
    /// Expression as condition (for when-condition chains): x < 0
    Expr(Box<Expr>),
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pattern::Wildcard => write!(f, "_"),
            Pattern::Literal(lit) => write!(f, "{}", lit),
            Pattern::Variable(name) => write!(f, "{}", name),
            Pattern::Constructor { name, args, named_fields } => {
                write!(f, "{}", name)?;
                if !args.is_empty() || !named_fields.is_empty() {
                    write!(f, "(")?;
                    let mut first = true;
                    for a in args {
                        if !first {
                            write!(f, ", ")?;
                        }
                        first = false;
                        write!(f, "{}", a)?;
                    }
                    for (n, p) in named_fields {
                        if !first {
                            write!(f, ", ")?;
                        }
                        first = false;
                        write!(f, "{}: {}", n, p)?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Pattern::Range(start, end) => write!(f, "in {}..{}", start, end),
            Pattern::IsType(name) => write!(f, "is {}", name),
            Pattern::Or(patterns) => {
                for (i, p) in patterns.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                Ok(())
            }
            Pattern::Expr(e) => write!(f, "{}", e),
        }
    }
}

// ---- Expressions ----

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum Expr {
    /// Literal value: 42, true, "hello"
    Literal(Literal),
    /// Variable reference: x, myVar
    Ident(String),
    /// Binary operation: x + y
    Binary(Box<Expr>, BinaryOp, Box<Expr>),
    /// Unary operation: -x, !flag
    Unary(UnaryOp, Box<Expr>),
    /// Function call: f(a, b), f { lambda }
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        /// Lambda extracted outside parentheses: map([1,2,3]) { it * 2 }
        trailing_lambda: Option<Box<Expr>>,
    },
    /// Lambda: { x, y -> body }, { it * 2 }, { 42 }
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
        /// Whether using implicit `it` parameter
        implicit_it: bool,
    },
    /// when expression (see WhenKind)
    When(Box<When>),
    /// for expression (see ForKind)
    For(Box<For>),
    /// Block: { stmt1; stmt2; expr }
    Block(Vec<Stmt>),
    /// Struct literal: {x = 10, y = 20}
    StructLiteral(Vec<(String, Expr)>),
    /// Map literal: {"k": v, "k2": v2}, {:} for empty map
    MapLiteral(Vec<(Expr, Expr)>),
    /// Set literal: {1, 2, 3}, {} for empty set
    SetLiteral(Vec<Expr>),
    /// Field access: expr.field
    FieldAccess(Box<Expr>, String),
    /// Index access: expr[index]
    Index(Box<Expr>, Box<Expr>),
    /// Range: start..end
    Range(Box<Expr>, Box<Expr>),
    /// Tuple: (expr, expr, ...) or named: (name: expr, ...)
    Tuple(Vec<(Option<String>, Expr)>),
    /// Safe field access: expr?.field
    SafeFieldAccess(Box<Expr>, String),
    /// Safe call: expr?.method(args)
    SafeCall {
        receiver: Box<Expr>,
        args: Vec<Expr>,
    },
    /// Error propagation: expr? — unwraps Ok/Some, returns Err/None early
    Try(Box<Expr>),
    /// Assignment: x = value
    Assign {
        target: Box<Expr>,
        value: Box<Expr>,
        propagate: bool,
    },
    /// String interpolation segment (used internally)
    StringInterpolate(Vec<StringPart>),
    /// Continue expression (skip current iteration in a for expression)
    Continue,
    /// Break expression (exit a loop early)
    Break,
    /// Function reference: ::function_name, ::Type.method
    FunctionRef(String),
    /// Shallow copy: copy expr
    Copy(Box<Expr>),
    /// Unsafe block: unsafe { ... }
    Unsafe(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    Literal(String),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct When {
    pub kind: WhenKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WhenKind {
    /// when condition then true_expr else false_expr
    OneLine {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    /// when value { patterns -> expressions }
    ValueMatch {
        value: Box<Expr>,
        arms: Vec<WhenArm>,
    },
    /// when { conditions -> expressions }
    ConditionChain {
        arms: Vec<WhenArm>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhenArm {
    pub pattern: Pattern,
    pub guard: Option<Box<Expr>>, // extra condition
    pub body: Box<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct For {
    pub kind: ForKind,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum ForKind {
    /// for var in iterable { body } (collect = true for for-expression)
    Iterate {
        var: String,
        iterable: Box<Expr>,
        body: Box<Expr>,
        collect: bool,
    },
    /// for (var1, var2) in iterable.withIndex() { body }
    IterateWithIndex {
        vars: Vec<String>,
        iterable: Box<Expr>,
        body: Box<Expr>,
    },
    /// for condition { body }
    Condition {
        condition: Box<Expr>,
        body: Box<Expr>,
    },
    /// for { body } (infinite loop)
    Infinite {
        body: Box<Expr>,
    },
    /// Nested iterate: for x in xs, y in ys { body } (cartesian product)
    NestedIterate {
        bindings: Vec<(String, Expr)>,
        body: Box<Expr>,
        collect: bool,
    },
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Literal(lit) => write!(f, "{}", lit),
            Expr::Ident(name) => write!(f, "{}", name),
            Expr::Binary(lhs, op, rhs) => write!(f, "({} {} {})", lhs, op, rhs),
            Expr::Unary(op, expr) => write!(f, "{}{}", op, expr),
            Expr::Call { func, args, trailing_lambda } => {
                write!(f, "{}(", func)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, ")")?;
                if let Some(lam) = trailing_lambda {
                    write!(f, " {{ {} }}", lam)?;
                }
                Ok(())
            }
            Expr::Lambda { params, body, implicit_it } => {
                write!(f, "{{")?;
                if *implicit_it {
                    write!(f, " it -> {}", body)?;
                } else if params.is_empty() {
                    write!(f, " {}", body)?;
                } else {
                    for (i, p) in params.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", p)?;
                    }
                    write!(f, " -> {}", body)?;
                }
                write!(f, " }}")
            }
            Expr::When(w) => write!(f, "{}", w),
            Expr::For(fr) => write!(f, "{}", fr),
            Expr::Block(stmts) => {
                write!(f, "{{ ")?;
                for s in stmts {
                    write!(f, "{}; ", s)?;
                }
                write!(f, "}}")
            }
            Expr::StructLiteral(fields) => {
                write!(f, "{{")?;
                for (i, (name, val)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{} = {}", name, val)?;
                }
                write!(f, "}}")
            }
            Expr::MapLiteral(entries) => {
                write!(f, "{{")?;
                if entries.is_empty() {
                    write!(f, ":")?;
                } else {
                    for (i, (k, v)) in entries.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}: {}", k, v)?;
                    }
                }
                write!(f, "}}")
            }
            Expr::SetLiteral(elements) => {
                write!(f, "{{")?;
                for (i, e) in elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", e)?;
                }
                write!(f, "}}")
            }
            Expr::FieldAccess(expr, field) => write!(f, "{}.{}", expr, field),
            Expr::Index(expr, idx) => write!(f, "{}[{}]", expr, idx),
            Expr::Range(start, end) => write!(f, "{}..{}", start, end),
            Expr::Tuple(exprs) => {
                write!(f, "(")?;
                for (i, (name, e)) in exprs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if let Some(n) = name {
                        write!(f, "{}: {}", n, e)?;
                    } else {
                        write!(f, "{}", e)?;
                    }
                }
                write!(f, ")")
            }
            Expr::SafeFieldAccess(expr, field) => write!(f, "{}?.{}", expr, field),
            Expr::SafeCall { receiver, args } => {
                write!(f, "{}?.(", receiver)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, ")")
            }
            Expr::Try(inner) => write!(f, "{}?", inner),
            Expr::Assign { target, value, propagate: _ } => write!(f, "{} = {}", target, value),
            Expr::StringInterpolate(parts) => {
                write!(f, "\"")?;
                for p in parts {
                    match p {
                        StringPart::Literal(s) => write!(f, "{}", s)?,
                        StringPart::Expr(e) => write!(f, "${{{}}}", e)?,
                    }
                }
                write!(f, "\"")
            }
            Expr::Continue => write!(f, "continue"),
            Expr::Break => write!(f, "break"),
            Expr::FunctionRef(name) => write!(f, "::{}", name),
            Expr::Copy(expr) => write!(f, "copy {}", expr),
            Expr::Unsafe(expr) => write!(f, "unsafe {}", expr),
        }
    }
}

impl fmt::Display for When {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WhenKind::OneLine { condition, then_expr, else_expr } => {
                write!(f, "when {} {{ {} else {} }}", condition, then_expr, else_expr)
            }
            WhenKind::ValueMatch { value, arms } => {
                write!(f, "when {} {{\n", value)?;
                for arm in arms {
                    write!(f, "    {} -> {}\n", arm.pattern, arm.body)?;
                }
                write!(f, "}}")
            }
            WhenKind::ConditionChain { arms } => {
                write!(f, "when {{\n")?;
                for arm in arms {
                    write!(f, "    {} -> {}\n", arm.pattern, arm.body)?;
                }
                write!(f, "}}")
            }
        }
    }
}

impl fmt::Display for For {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ForKind::Iterate { var, iterable, body, .. } => {
                write!(f, "for {} in {} {{ {} }}", var, iterable, body)
            }
            ForKind::IterateWithIndex { vars, iterable, body } => {
                let vs = vars.join(", ");
                write!(f, "for ({}) in {} {{ {} }}", vs, iterable, body)
            }
            ForKind::Condition { condition, body } => {
                write!(f, "for {} {{ {} }}", condition, body)
            }
            ForKind::Infinite { body } => {
                write!(f, "for {{ {} }}", body)
            }
            ForKind::NestedIterate { bindings, body, .. } => {
                write!(f, "for ")?;
                for (i, (var, iter)) in bindings.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{} in {}", var, iter)?;
                }
                write!(f, " {{ {} }}", body)
            }
        }
    }
}

// ---- Statements ----

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// val/var binding: val x = 10
    Let {
        mutable: bool,
        propagate: bool, // val? / var? for error propagation
        lazy_init: bool, // lazy val / lazy var
        name: String,
        type_ann: Option<Type>,
        value: Expr,
        span: Span,
    },
    /// Destructuring binding: val (x, y) = expr / val [a, b] = expr / val [head, ...tail] = expr / val {x, y} = expr
    Destructure {
        mutable: bool,
        propagate: bool,
        names: Vec<String>,
        /// Renames for struct destructuring: val {x as px, y as py} = point
        renames: Vec<(String, String)>, // (field_name, local_name)
        rest: Option<String>, // for list rest pattern: val [head, ...tail] = list
        is_list: bool, // true for list destructuring [a, b], false for tuple (x, y)
        is_struct: bool, // true for struct destructuring {x, y}
        value: Expr,
        span: Span,
    },
    /// Function definition: fun name(params): Type = body
    Fun {
        name: String,
        params: Vec<Param>,
        return_type: Option<Type>,
        body: Expr,
        /// Generic type parameters: fun <T, U> name(...)
        type_params: Vec<String>,
        /// For single-expression functions (no block)
        is_single_expr: bool,
        span: Span,
    },
    /// Expression statement
    Expr { expr: Expr, span: Span },
    /// Return statement: return expr
    Return { value: Option<Expr>, span: Span },
    /// Break statement
    Break { span: Span },
    /// Continue statement
    Continue { span: Span },
    /// Type alias: type Point = {x: Int, y: Int}
    TypeAlias {
        name: String,
        type_params: Vec<String>,
        definition: Type,
        span: Span,
    },
    /// Enum definition: enum Option[T] { Some(T), None }
    Enum {
        name: String,
        type_params: Vec<String>,
        variants: Vec<EnumVariant>,
        span: Span,
    },
    /// Module declaration: module math { ... }
    Module {
        name: String,
        exports: Vec<ExportItem>,
        body: Vec<Stmt>,
        span: Span,
    },
    /// Export statement: export fun ...
    Export { stmt: Box<Stmt>, span: Span },
    /// Import statement: import math
    Import {
        module: String,
        items: Option<Vec<String>>, // None = import all, Some = specific items
        alias: Option<String>,
        span: Span,
    },
    /// Const declaration: const PI = 3.14159
    Const {
        name: String,
        type_ann: Option<Type>,
        value: Expr,
        span: Span,
    },
    /// Extension block: extension TypeName { fun method(self, ...) ... }
    Extension {
        type_name: String,
        methods: Vec<Stmt>,
        span: Span,
    },
    /// External function declaration: external fun name(params): Type
    External {
        name: String,
        params: Vec<Param>,
        return_type: Option<Type>,
        span: Span,
    },
    /// External type declaration: external type Name
    ExternalType {
        name: String,
        span: Span,
    },
}

impl Stmt {
    /// Get the span of this statement
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. } => *span,
            Stmt::Destructure { span, .. } => *span,
            Stmt::Fun { span, .. } => *span,
            Stmt::Expr { span, .. } => *span,
            Stmt::Return { span, .. } => *span,
            Stmt::Break { span } => *span,
            Stmt::Continue { span } => *span,
            Stmt::TypeAlias { span, .. } => *span,
            Stmt::Enum { span, .. } => *span,
            Stmt::Module { span, .. } => *span,
            Stmt::Export { span, .. } => *span,
            Stmt::Import { span, .. } => *span,
            Stmt::Const { span, .. } => *span,
            Stmt::Extension { span, .. } => *span,
            Stmt::External { span, .. } => *span,
            Stmt::ExternalType { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub params: Vec<EnumVariantParam>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnumVariantParam {
    /// Positional parameter: Some(T)
    Positional(Type),
    /// Named parameter: Node(left: Tree[T], right: Tree[T])
    Named { name: String, ty: Type },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportItem {
    Function(String),
    Constant(String),
    Type(String),
}

impl fmt::Display for Param {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.ty {
            Some(ty) => write!(f, "{}: {}", self.name, ty),
            None => write!(f, "{}", self.name),
        }
    }
}

impl fmt::Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stmt::Let { mutable, propagate, lazy_init, name, type_ann, value, .. } => {
                let lazy_kw = if *lazy_init { "lazy " } else { "" };
                let kw = if *mutable { "var" } else { "val" };
                let propagation = if *propagate { "?" } else { "" };
                write!(f, "{}{}{} {}", lazy_kw, kw, propagation, name)?;
                if let Some(ty) = type_ann {
                    write!(f, ": {}", ty)?;
                }
                write!(f, " = {}", value)
            }
            Stmt::Destructure { mutable, propagate, names, renames, rest, is_list, is_struct, value, .. } => {
                let kw = if *mutable { "var" } else { "val" };
                let propagation = if *propagate { "?" } else { "" };
                if *is_struct {
                    write!(f, "{}{} {{", kw, propagation)?;
                } else if *is_list {
                    write!(f, "{}{} [", kw, propagation)?;
                } else {
                    write!(f, "{}{} (", kw, propagation)?;
                }
                for (i, n) in names.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    // Check for rename
                    if let Some((_, local)) = renames.iter().find(|(fld, _)| fld == n) {
                        write!(f, "{}", n)?;
                        if n != local { write!(f, " as {}", local)?; }
                    } else {
                        write!(f, "{}", n)?;
                    }
                }
                if let Some(r) = rest {
                    if !names.is_empty() { write!(f, ", ")?; }
                    write!(f, "...{}", r)?;
                }
                let close = if *is_struct { "}" } else if *is_list { "]" } else { ")" };
                write!(f, "{} = {}", close, value)
            }
            Stmt::Fun { name, params, return_type, body, type_params, .. } => {
                if !type_params.is_empty() {
                    write!(f, "fun <")?;
                    for (i, tp) in type_params.iter().enumerate() {
                        if i > 0 { write!(f, ", ")?; }
                        write!(f, "{}", tp)?;
                    }
                    write!(f, "> ")?;
                } else {
                    write!(f, "fun ")?;
                }
                write!(f, "{}(", name)?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ")")?;
                if let Some(ty) = return_type {
                    write!(f, ": {}", ty)?;
                }
                write!(f, " = {}", body)
            }
            Stmt::Expr { expr, .. } => write!(f, "{}", expr),
            Stmt::Return { value: Some(expr), .. } => write!(f, "return {}", expr),
            Stmt::Return { value: None, .. } => write!(f, "return"),
            Stmt::Break { .. } => write!(f, "break"),
            Stmt::Continue { .. } => write!(f, "continue"),
            Stmt::TypeAlias { name, type_params, definition, .. } => {
                if type_params.is_empty() {
                    write!(f, "type {} = {}", name, definition)
                } else {
                    write!(f, "type {}[{}] = {}", name, type_params.join(", "), definition)
                }
            }
            Stmt::Enum { name, type_params, variants, .. } => {
                if type_params.is_empty() {
                    write!(f, "enum {} {{\n", name)?;
                } else {
                    write!(f, "enum {}[{}] {{\n", name, type_params.join(", "))?;
                }
                for v in variants {
                    if v.params.is_empty() {
                        write!(f, "    {}\n", v.name)?;
                    } else {
                        write!(f, "    {}(", v.name)?;
                        for (i, p) in v.params.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            match p {
                                EnumVariantParam::Positional(ty) => write!(f, "{}", ty)?,
                                EnumVariantParam::Named { name, ty } => write!(f, "{}: {}", name, ty)?,
                            }
                        }
                        write!(f, ")\n")?;
                    }
                }
                write!(f, "}}")
            }
            Stmt::Module { name, exports, body, .. } => {
                write!(f, "module {} {{\n", name)?;
                for e in exports {
                    match e {
                        ExportItem::Function(n) => write!(f, "    export fun {}\n", n)?,
                        ExportItem::Constant(n) => write!(f, "    export const {}\n", n)?,
                        ExportItem::Type(n) => write!(f, "    export type {}\n", n)?,
                    }
                }
                for s in body {
                    write!(f, "    {}\n", s)?;
                }
                write!(f, "}}")
            }
            Stmt::Export { stmt, .. } => write!(f, "export {}", stmt),
            Stmt::Import { module, items, alias, .. } => {
                write!(f, "import {}", module)?;
                if let Some(its) = items {
                    write!(f, ".{{{}}}", its.join(", "))?;
                }
                if let Some(alias) = alias {
                    write!(f, " as {}", alias)?;
                }
                Ok(())
            }
            Stmt::Const { name, type_ann, value, .. } => {
                write!(f, "const {}", name)?;
                if let Some(ty) = type_ann {
                    write!(f, ": {}", ty)?;
                }
                write!(f, " = {}", value)
            }
            Stmt::Extension { type_name, methods, .. } => {
                write!(f, "extension {} {{\n", type_name)?;
                for m in methods {
                    write!(f, "    {}\n", m)?;
                }
                write!(f, "}}")
            }
            Stmt::External { name, params, return_type, .. } => {
                write!(f, "external fun {}(", name)?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", p.name)?;
                    if let Some(ref ty) = p.ty {
                        write!(f, ": {}", ty)?;
                    }
                }
                write!(f, ")")?;
                if let Some(rt) = return_type {
                    write!(f, ": {}", rt)?;
                }
                write!(f, ";")
            }
            Stmt::ExternalType { name, .. } => {
                write!(f, "external type {};", name)
            }
        }
    }
}

// ---- Top-level program ----

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for stmt in &self.stmts {
            writeln!(f, "{}", stmt)?;
        }
        Ok(())
    }
}

// ---- Useful constructors ----

#[allow(dead_code)]
impl Expr {
    pub fn int(n: i64) -> Self {
        Expr::Literal(Literal::Int(n))
    }

    pub fn float(n: f64) -> Self {
        Expr::Literal(Literal::Float(n))
    }

    pub fn bool(b: bool) -> Self {
        Expr::Literal(Literal::Bool(b))
    }

    pub fn string(s: &str) -> Self {
        Expr::Literal(Literal::String(s.to_string()))
    }

    pub fn ident(name: &str) -> Self {
        Expr::Ident(name.to_string())
    }

    pub fn call(func: Expr, args: Vec<Expr>) -> Self {
        Expr::Call { func: Box::new(func), args, trailing_lambda: None }
    }

    pub fn call_with_lambda(func: Expr, args: Vec<Expr>, lambda: Expr) -> Self {
        Expr::Call { func: Box::new(func), args, trailing_lambda: Some(Box::new(lambda)) }
    }

    pub fn lambda(params: Vec<&str>, body: Expr) -> Self {
        Expr::Lambda {
            params: params.into_iter().map(|s| s.to_string()).collect(),
            body: Box::new(body),
            implicit_it: false,
        }
    }

    pub fn it_lambda(body: Expr) -> Self {
        Expr::Lambda { params: vec!["it".to_string()], body: Box::new(body), implicit_it: true }
    }

    pub fn binary(lhs: Expr, op: BinaryOp, rhs: Expr) -> Self {
        Expr::Binary(Box::new(lhs), op, Box::new(rhs))
    }

    pub fn unary(op: UnaryOp, expr: Expr) -> Self {
        Expr::Unary(op, Box::new(expr))
    }
}

#[allow(dead_code)]
impl Stmt {
    pub fn val(name: &str, value: Expr) -> Self {
        Stmt::Let { mutable: false, propagate: false, lazy_init: false, name: name.to_string(), type_ann: None, value, span: Span::default() }
    }

    pub fn var(name: &str, value: Expr) -> Self {
        Stmt::Let { mutable: true, propagate: false, lazy_init: false, name: name.to_string(), type_ann: None, value, span: Span::default() }
    }

    pub fn fun(name: &str, params: Vec<Param>, return_type: Option<Type>, body: Expr) -> Self {
        Stmt::Fun { name: name.to_string(), params, return_type, body, type_params: vec![], is_single_expr: false, span: Span::default() }
    }

    pub fn const_(name: &str, type_ann: Option<Type>, value: Expr) -> Self {
        Stmt::Const { name: name.to_string(), type_ann, value, span: Span::default() }
    }
}
