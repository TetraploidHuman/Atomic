use crate::ast::*;
use crate::error::CompilerError;
use crate::lexer::Span;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct StructInfo {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    #[allow(dead_code)]
    pub field_index: HashMap<String, usize>,
}

#[derive(Clone, Debug)]
pub struct EnumInfo {
    pub name: String,
    #[allow(dead_code)]
    pub type_params: Vec<String>,
    pub variants: Vec<EnumVariantInfo>,
}

#[derive(Clone, Debug)]
pub struct EnumVariantInfo {
    pub name: String,
    pub tag: u8,
    pub params: Vec<EnumVariantParam>,
}

#[derive(Default)]
#[derive(Clone)]
pub struct TypeRegistry {
    pub structs: HashMap<String, StructInfo>,
    pub enums: HashMap<String, EnumInfo>,
    pub type_aliases: HashMap<String, Type>,
    pub variant_to_enum: HashMap<String, String>,
}

impl TypeRegistry {
    pub fn new() -> Self {
        TypeRegistry {
            structs: HashMap::new(),
            enums: HashMap::new(),
            type_aliases: HashMap::new(),
            variant_to_enum: HashMap::new(),
        }
    }

    pub fn register(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::TypeAlias { name, definition, .. } => {
                if let Type::Struct(fields) = definition {
                    let mut field_index = HashMap::new();
                    for (i, (fname, _)) in fields.iter().enumerate() {
                        field_index.insert(fname.clone(), i);
                    }
                    self.structs.insert(name.clone(), StructInfo {
                        name: name.clone(),
                        fields: fields.clone(),
                        field_index,
                    });
                }
                self.type_aliases.insert(name.clone(), definition.clone());
            }
            Stmt::Enum { name, type_params, variants, .. } => {
                let mut enum_variants = Vec::new();
                for (i, v) in variants.iter().enumerate() {
                    self.variant_to_enum.insert(v.name.clone(), name.clone());
                    enum_variants.push(EnumVariantInfo {
                        name: v.name.clone(),
                        tag: i as u8,
                        params: v.params.clone(),
                    });
                }
                self.enums.insert(name.clone(), EnumInfo {
                    name: name.clone(),
                    type_params: type_params.clone(),
                    variants: enum_variants,
                });
            }
            Stmt::ExternalType { name, .. } => {
                // Register as opaque struct (no fields)
                self.structs.insert(name.clone(), StructInfo {
                    name: name.clone(),
                    fields: vec![],
                    field_index: HashMap::new(),
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Find the struct type whose field names match exactly. Returns the struct info if unique.
    pub fn find_struct_by_fields(&self, field_names: &[String]) -> Option<&StructInfo> {
        let matches: Vec<&StructInfo> = self.structs.values().filter(|s| {
            if s.fields.len() != field_names.len() { return false; }
            field_names.iter().enumerate().all(|(i, name)| s.fields[i].0 == *name)
        }).collect();
        if matches.len() == 1 { Some(matches[0]) } else { None }
    }

    /// Look up an enum variant by name. Returns (enum_info, variant_info).
    pub fn lookup_variant(&self, variant_name: &str) -> Option<(&EnumInfo, &EnumVariantInfo)> {
        let enum_name = self.variant_to_enum.get(variant_name)?;
        let info = self.enums.get(enum_name)?;
        let variant = info.variants.iter().find(|v| v.name == variant_name)?;
        Some((info, variant))
    }

    pub fn get_struct(&self, name: &str) -> Option<&StructInfo> {
        self.structs.get(name)
    }

    #[allow(dead_code)]
    pub fn get_enum(&self, name: &str) -> Option<&EnumInfo> {
        self.enums.get(name)
    }

    /// Check that a set of when arms covers all variants of the enum they match on.
    /// Returns Ok(()) if exhaustive, Err(message) if any variant is missing.
    pub fn check_when_exhaustive(&self, arms: &[WhenArm]) -> Result<(), String> {
        let mut covered: HashSet<String> = HashSet::new();
        let mut enum_name: Option<String> = None;
        let mut has_wildcard = false;

        for arm in arms {
            self.collect_pattern_coverage(&arm.pattern, &mut covered, &mut enum_name, &mut has_wildcard);
        }

        if has_wildcard || enum_name.is_none() {
            return Ok(());
        }

        let info = self.enums.get(enum_name.as_ref().unwrap())
            .ok_or_else(|| format!("Unknown enum type: {}", enum_name.unwrap()))?;

        let mut missing: Vec<&str> = Vec::new();
        for v in &info.variants {
            if !covered.contains(&v.name) {
                missing.push(&v.name);
            }
        }

        if missing.is_empty() {
            Ok(())
        } else {
            let msg = missing.iter()
                .map(|n| format!("'{}'", n))
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "Non-exhaustive when: enum '{}' is missing variant(s): {}. Add them or add an else branch.",
                info.name, msg
            ))
        }
    }

    fn collect_pattern_coverage(
        &self,
        pattern: &Pattern,
        covered: &mut HashSet<String>,
        enum_name: &mut Option<String>,
        has_wildcard: &mut bool,
    ) {
        match pattern {
            Pattern::Wildcard | Pattern::Variable(_) => {
                *has_wildcard = true;
            }
            Pattern::Constructor { name, args, named_fields } => {
                if let Some(en) = self.variant_to_enum.get(name.as_str()) {
                    if enum_name.is_none() {
                        *enum_name = Some(en.clone());
                    }
                }
                covered.insert(name.clone());
                for sub in args {
                    self.collect_pattern_coverage(sub, covered, enum_name, has_wildcard);
                }
                for (_, sub) in named_fields {
                    self.collect_pattern_coverage(sub, covered, enum_name, has_wildcard);
                }
            }
            Pattern::Or(patterns) => {
                for p in patterns {
                    self.collect_pattern_coverage(p, covered, enum_name, has_wildcard);
                }
            }
            _ => {} // Literal, Range, IsType — not relevant for enum exhaustiveness
        }
    }
}

/// Type checker: walks the AST and verifies type consistency.
/// Reports all errors found (not just the first one).
pub struct TypeChecker {
    registry: TypeRegistry,
    /// Type environment mapping names to their types (functions, variables)
    type_env: HashMap<String, Type>,
    /// Current statement span for error reporting
    current_span: Span,
}

impl TypeChecker {
    pub fn new(registry: TypeRegistry) -> Self {
        TypeChecker { registry, type_env: HashMap::new(), current_span: Span::default() }
    }

    /// Build the type environment from top-level statements
    fn build_type_env(&mut self, program: &Program) {
        // First pass: detect overloaded function names
        let mut name_counts: HashMap<String, usize> = HashMap::new();
        for stmt in &program.stmts {
            if let Stmt::Fun { name, params, .. } = stmt {
                if params.iter().all(|p| p.ty.is_some()) {
                    *name_counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
        }
        let overloaded_names: std::collections::HashSet<String> = name_counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(name, _)| name)
            .collect();

        for stmt in &program.stmts {
            match stmt {
                Stmt::Fun { name, params, return_type, .. } => {
                    let param_tys: Vec<Type> = params.iter()
                        .map(|p| p.ty.clone().unwrap_or(Type::Named("Int".into())))
                        .collect();
                    let ret_ty = return_type.clone().unwrap_or(Type::Named("Int".into()));
                    let fn_type = Type::Function(param_tys, Box::new(ret_ty));

                    let all_typed = params.iter().all(|p| p.ty.is_some());
                    if all_typed && overloaded_names.contains(name.as_str()) {
                        // Use mangled name as key for overloaded functions
                        let mangled = Self::mangle_name(name, &params.iter()
                            .map(|p| p.ty.clone().unwrap_or(Type::Named("Int".into())))
                            .collect::<Vec<_>>());
                        self.type_env.insert(mangled, fn_type);
                    } else {
                        // Also store under original name for backward compat
                        self.type_env.insert(name.clone(), fn_type);
                    }
                }
                Stmt::Let { name, type_ann, value, .. } => {
                    let inferred = self.infer_expr_type(value);
                    let ty = type_ann.clone().unwrap_or(inferred);
                    self.type_env.insert(name.clone(), ty);
                }
                Stmt::Destructure { names, .. } => {
                    for name in names {
                        self.type_env.insert(name.clone(), Type::Named("Int".into()));
                    }
                }
                Stmt::Const { name, type_ann, value, .. } => {
                    let inferred = self.infer_expr_type(value);
                    let ty = type_ann.clone().unwrap_or(inferred);
                    self.type_env.insert(name.clone(), ty);
                }
                _ => {}
            }
        }
    }

    /// Mangle a function name (mirrors codegen version)
    fn mangle_name(name: &str, param_types: &[Type]) -> String {
        if param_types.is_empty() {
            return name.to_string();
        }
        let parts: Vec<String> = param_types.iter().map(|t| format!("{}", t)).collect();
        format!("{}_{}", name, parts.join("_"))
    }

    /// Run all checks on the program. Returns a list of errors.
    pub fn check(&mut self, program: &Program) -> Vec<CompilerError> {
        self.build_type_env(program);
        let mut errors = Vec::new();

        for stmt in &program.stmts {
            self.current_span = stmt.span();
            match stmt {
                Stmt::Fun { name, params, return_type, body, .. } => {
                    // Temporarily add function parameters to the type environment
                    let mut saved: Vec<(String, Option<Type>)> = Vec::new();
                    for p in params {
                        let param_ty = p.ty.clone().unwrap_or(Type::Named("Int".into()));
                        let old = self.type_env.insert(p.name.clone(), param_ty);
                        saved.push((p.name.clone(), old));
                    }

                    self.collect_expr_errors(body, &mut errors);
                    // Validate return type annotation if present
                    if let Some(declared_ret) = return_type {
                        let inferred = self.infer_expr_type(body);
                        // Skip check when inferred type is Int (fallback for unknown types)
                        if !matches!(&inferred, Type::Named(n) if n == "Int")
                            && !self.types_compatible(declared_ret, &inferred) {
                            errors.push(CompilerError::new(
                                format!("Function '{}' declares return type '{}' but body has type '{}'",
                                    name, declared_ret, inferred)
                            ).with_span(self.current_span));
                        }
                    }

                    // Restore parameter bindings
                    for (pname, old_val) in saved {
                        if let Some(ty) = old_val {
                            self.type_env.insert(pname, ty);
                        } else {
                            self.type_env.remove(&pname);
                        }
                    }
                }
                Stmt::Expr { expr, .. } => {
                    self.collect_expr_errors(expr, &mut errors);
                }
                Stmt::Let { name, type_ann, value, .. } => {
                    self.collect_expr_errors(value, &mut errors);
                    if let Some(ann) = type_ann {
                        let inferred = self.infer_expr_type(value);
                        if !self.types_compatible(ann, &inferred) {
                            errors.push(CompilerError::new(
                                format!("Variable '{}' declared as '{}' but initialized with '{}'",
                                    name, ann, inferred)
                            ).with_span(self.current_span));
                        }
                    }
                }
                Stmt::Destructure { value, .. } => {
                    self.collect_expr_errors(value, &mut errors);
                }
                Stmt::Const { name, type_ann, value, .. } => {
                    self.collect_expr_errors(value, &mut errors);
                    if let Some(ann) = type_ann {
                        let inferred = self.infer_expr_type(value);
                        if !self.types_compatible(ann, &inferred) {
                            errors.push(CompilerError::new(
                                format!("Constant '{}' declared as '{}' but initialized with '{}'",
                                    name, ann, inferred)
                            ).with_span(self.current_span));
                        }
                    }
                }
                _ => {}
            }
        }

        errors
    }

    /// Extract arms from a When expression, if it's a ValueMatch or ConditionChain
    fn when_arms<'a>(&self, w: &'a When) -> &'a [WhenArm] {
        match &w.kind {
            WhenKind::ValueMatch { arms, .. } => arms,
            WhenKind::ConditionChain { arms } => arms,
            _ => &[], // OneLine has no arms
        }
    }

    fn collect_expr_errors(&self, expr: &Expr, errors: &mut Vec<CompilerError>) {
        match expr {
            Expr::Binary(lhs, op, rhs) => {
                if let Err(e) = self.check_binary_op(lhs, *op, rhs) {
                    errors.push(e);
                }
                self.collect_expr_errors(lhs, errors);
                self.collect_expr_errors(rhs, errors);
            }
            Expr::When(w) => {
                let arms = self.when_arms(w);
                if !arms.is_empty() {
                    if let Err(e) = self.check_when_arms(arms) {
                        errors.push(e);
                    }
                    for arm in arms {
                        self.collect_expr_errors(&arm.body, errors);
                    }
                }
            }
            Expr::Call { func, args, trailing_lambda } => {
                if let Err(e) = self.check_call(func, args) {
                    errors.push(e);
                }
                self.collect_expr_errors(func, errors);
                for a in args {
                    self.collect_expr_errors(a, errors);
                }
                if let Some(lam) = trailing_lambda {
                    self.collect_expr_errors(lam, errors);
                }
            }
            Expr::Block(stmts) => {
                for s in stmts {
                    self.collect_stmt_errors(s, errors);
                }
            }
            Expr::For(for_expr) => {
                match &for_expr.kind {
                    ForKind::Iterate { body, .. } => self.collect_expr_errors(body, errors),
                    ForKind::IterateWithIndex { body, .. } => self.collect_expr_errors(body, errors),
                    ForKind::Condition { body, .. } => self.collect_expr_errors(body, errors),
                    ForKind::Infinite { body, .. } => self.collect_expr_errors(body, errors),
                    ForKind::NestedIterate { body, .. } => self.collect_expr_errors(body, errors),
                }
            }
            Expr::Lambda { body, .. } => {
                self.collect_expr_errors(body, errors);
            }
            Expr::FieldAccess(obj, _) => {
                self.collect_expr_errors(obj, errors);
            }
            Expr::Copy(inner) => {
                self.collect_expr_errors(inner, errors);
            }
            Expr::Unsafe(inner) => {
                self.collect_expr_errors(inner, errors);
            }
            Expr::Try(inner) => {
                self.collect_expr_errors(inner, errors);
            }
            Expr::Unary(_, operand) => {
                self.collect_expr_errors(operand, errors);
            }
            Expr::StructLiteral(fields) => {
                for (_, val) in fields {
                    self.collect_expr_errors(val, errors);
                }
            }
            Expr::Range(start, end) => {
                self.collect_expr_errors(start, errors);
                self.collect_expr_errors(end, errors);
            }
            Expr::Tuple(elements) => {
                for (_, e) in elements {
                    self.collect_expr_errors(e, errors);
                }
            }
            Expr::SafeFieldAccess(obj, _) => {
                self.collect_expr_errors(obj, errors);
            }
            Expr::SafeCall { receiver, args } => {
                self.collect_expr_errors(receiver, errors);
                for a in args {
                    self.collect_expr_errors(a, errors);
                }
            }
            Expr::Assign { value, .. } => {
                self.collect_expr_errors(value, errors);
            }
            _ => {} // Literal, Ident, Continue, Break, etc.
        }
    }

    fn collect_stmt_errors(&self, stmt: &Stmt, errors: &mut Vec<CompilerError>) {
        match stmt {
            Stmt::Expr { expr, .. } => self.collect_expr_errors(expr, errors),
            Stmt::Let { value, .. } | Stmt::Destructure { value, .. } | Stmt::Const { value, .. } => {
                self.collect_expr_errors(value, errors);
            }
            Stmt::Return { value: expr, .. } => {
                if let Some(e) = expr {
                    self.collect_expr_errors(e, errors);
                }
            }
            _ => {}
        }
    }

    fn check_binary_op(&self, lhs: &Expr, op: BinaryOp, rhs: &Expr) -> Result<(), CompilerError> {
        let lt = self.infer_expr_type(lhs);
        let rt = self.infer_expr_type(rhs);

        match op {
            BinaryOp::Add => {
                let ls = format!("{}", lt);
                let rs = format!("{}", rt);
                if ls == "String" || rs == "String" {
                    return Ok(());
                }
            }
            BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Pow => {
                let ls = format!("{}", lt);
                let rs = format!("{}", rt);
                if ls == "String" || rs == "String" || ls == "Bool" || rs == "Bool" {
                    return Err(CompilerError::new(format!(
                        "Arithmetic operation '{}' not supported for {}",
                        op, if ls == "Bool" || rs == "Bool" { "Bool" } else { "String" }
                    )).with_span(self.current_span));
                }
            }
            BinaryOp::Eq | BinaryOp::Neq => {
                return Ok(());
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Lte | BinaryOp::Gte => {
                let ls = format!("{}", lt);
                let rs = format!("{}", rt);
                // Allow Bool comparison (True > False), but disallow mixed Bool/other types
                if (ls == "Bool" || rs == "Bool") && ls != rs {
                    return Err(CompilerError::new(format!("Cannot compare '{}' with '{}'", ls, rs))
                        .with_span(self.current_span));
                }
            }
            BinaryOp::And | BinaryOp::Or => {
                if format!("{}", lt) != "Bool" || format!("{}", rt) != "Bool" {
                    return Err(CompilerError::new(format!(
                        "Logical operator '{}' requires Bool operands, got '{}' and '{}'", op, lt, rt
                    )).with_span(self.current_span));
                }
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr => {
                let ls = format!("{}", lt);
                let rs = format!("{}", rt);
                if ls != "Int" || rs != "Int" {
                    return Err(CompilerError::new(format!(
                        "Bitwise operator '{}' requires Int operands, got '{}' and '{}'", op, lt, rs
                    )).with_span(self.current_span));
                }
            }
            BinaryOp::Range | BinaryOp::RangeExclusive | BinaryOp::Assign
            | BinaryOp::In | BinaryOp::Is => {}
        }
        Ok(())
    }

    fn check_call(&self, func: &Expr, args: &[Expr]) -> Result<(), CompilerError> {
        if let Expr::Ident(name) = func {
            if let Some((_ei, vi)) = self.registry.lookup_variant(name) {
                let expected = vi.params.len();
                let actual = args.len();
                if expected != actual {
                    return Err(CompilerError::new(format!(
                        "Enum variant '{}' expects {} arguments, but got {}",
                        name, expected, actual
                    )).with_span(self.current_span));
                }
            }
        }
        Ok(())
    }

    fn check_when_arms(&self, arms: &[WhenArm]) -> Result<(), CompilerError> {
        if arms.is_empty() {
            return Ok(());
        }

        // Collect arm types, but be lenient with Int (fallback) when mixed with enums
        let types: Vec<Type> = arms.iter()
            .map(|a| self.infer_expr_type(&a.body))
            .collect();
        let first = &types[0];

        // If first type is Int, it might be a fallback — skip arm checking
        if matches!(first, Type::Named(ref n) if n == "Int") {
            return Ok(());
        }

        for (i, t) in types.iter().enumerate().skip(1) {
            // Skip Int fallback arms
            if matches!(t, Type::Named(ref n) if n == "Int") {
                continue;
            }
            if !self.types_compatible(first, t) {
                return Err(CompilerError::new(format!(
                    "When arm type mismatch: arm 1 is '{}' but arm {} is '{}'",
                    first, i + 1, t
                )).with_span(self.current_span));
            }
        }
        Ok(())
    }

    /// Infer the type of an expression (structural, not full HM inference)
    fn infer_expr_type(&self, expr: &Expr) -> Type {
        match expr {
            Expr::Literal(Literal::String(_)) | Expr::StringInterpolate(_) => Type::Named("String".into()),
            Expr::Literal(Literal::Int(_)) => Type::Named("Int".into()),
            Expr::Literal(Literal::Float(_)) => Type::Named("Float".into()),
            Expr::Literal(Literal::Bool(_)) => Type::Named("Bool".into()),
            Expr::Literal(Literal::Char(_)) => Type::Named("Char".into()),
            Expr::Literal(Literal::Unit) => Type::Unit,
            Expr::MapLiteral(_) => Type::Map(
                Box::new(Type::Named("String".into())),
                Box::new(Type::Named("Int".into())),
            ),
            Expr::SetLiteral(_) => Type::Set(Box::new(Type::Named("Int".into()))),
            Expr::Binary(lhs, op, rhs) => {
                let lt = self.infer_expr_type(lhs);
                let rt = self.infer_expr_type(rhs);
                if *op == BinaryOp::Add {
                    if matches!(&lt, Type::Named(ref n) if n == "String") || matches!(&rt, Type::Named(ref n) if n == "String") {
                        return Type::Named("String".into());
                    }
                }
                if *op == BinaryOp::And || *op == BinaryOp::Or || *op == BinaryOp::Eq ||
                   *op == BinaryOp::Neq || *op == BinaryOp::Lt || *op == BinaryOp::Gt ||
                   *op == BinaryOp::Lte || *op == BinaryOp::Gte ||
                   *op == BinaryOp::In || *op == BinaryOp::Is {
                    return Type::Named("Bool".into());
                }
                if *op == BinaryOp::BitAnd || *op == BinaryOp::BitOr || *op == BinaryOp::BitXor ||
                   *op == BinaryOp::Shl || *op == BinaryOp::Shr {
                    return Type::Named("Int".into());
                }
                if *op == BinaryOp::Pow {
                    // Return Float if either operand is Float
                    if matches!(&lt, Type::Named(ref n) if n == "Float") || matches!(&rt, Type::Named(ref n) if n == "Float") {
                        return Type::Named("Float".into());
                    }
                    return lt;
                }
                // Arithmetic: return Float if either operand is Float, else Int
                if matches!(&lt, Type::Named(ref n) if n == "Float") || matches!(&rt, Type::Named(ref n) if n == "Float") {
                    return Type::Named("Float".into());
                }
                Type::Named("Int".into())
            }
            Expr::Call { func, .. } => {
                if let Expr::Ident(name) = func.as_ref() {
                    match name.as_str() {
                        "print" | "println" | "send" | "close" | "cancel" => Type::Unit,
                        "toString" | "toUpper" | "toLower" => Type::Named("String".into()),
                        "receive" | "wait" => Type::Named("Int".into()),
                        "launch" => Type::Task(Box::new(Type::Named("Int".into()))),
                        "stream" => Type::Stream(Box::new(Type::Named("Int".into()))),
                        "is_done" | "is_cancelled" => Type::Named("Bool".into()),
                        "withTimeout" => Type::Named("Result".into()),
                        "coroutineScope" => Type::Named("List".into()),
                        // Callback-based list functions
                        "any" | "all" => Type::Named("Bool".into()),
                        "find" | "find_index" | "reduce" => Type::Named("Option".into()),
                        "fold_right" => Type::Named("Int".into()),
                        "take_while" | "drop_while" | "sorted_by" => Type::Named("List".into()),
                        _ => {
                            if self.registry.lookup_variant(name).is_some() {
                                let enum_name = self.registry.variant_to_enum.get(name)
                                    .cloned()
                                    .unwrap_or_default();
                                Type::Named(enum_name)
                            } else if let Some(Type::Function(_, ret)) = self.type_env.get(name) {
                                *ret.clone()
                            } else {
                                Type::Named("Int".into())
                            }
                        }
                    }
                } else if let Expr::FieldAccess(receiver, method) = func.as_ref() {
                    let recv_type = self.infer_expr_type(receiver);
                    match (recv_type, method.as_str()) {
                        // Map/Set UFCS methods
                        (Type::Map(_, _), "contains") | (Type::Set(_), "contains")
                            | (Type::Map(_, _), "is_empty") | (Type::Set(_), "is_empty") => Type::Named("Bool".into()),
                        (Type::Map(_, _), "insert") | (Type::Set(_), "insert") => Type::Unit,
                        (Type::Map(_, _), "remove") | (Type::Map(_, _), "get")
                            | (Type::Set(_), "remove") => Type::Named("Option".into()),
                        // Stream UFCS methods
                        (Type::Stream(_), "send") => Type::Unit,
                        (Type::Stream(_), "receive") => Type::Named("Int".into()),
                        (Type::Stream(_), "close") => Type::Unit,
                        // Task UFCS methods
                        (Type::Task(_), "cancel") => Type::Unit,
                        (Type::Task(_), "is_done") | (Type::Task(_), "is_cancelled") => Type::Named("Bool".into()),
                        (Type::Task(_), "wait") => Type::Named("Int".into()),
                        _ => Type::Named("Int".into()),
                    }
                } else {
                    Type::Named("Int".into())
                }
            }
            Expr::When(w) => {
                let arms = self.when_arms(w);
                arms.first()
                    .map(|a| self.infer_expr_type(&a.body))
                    .unwrap_or(Type::Unit)
            }
            Expr::Continue | Expr::Break => Type::Unit,
            Expr::For(_) => Type::Unit,
            Expr::FunctionRef(name) => {
                if let Some(ty) = self.type_env.get(name) {
                    ty.clone()
                } else {
                    Type::Function(vec![Type::Named("Int".into())], Box::new(Type::Named("Int".into())))
                }
            }
            Expr::Copy(inner) => self.infer_expr_type(inner),
            Expr::Try(inner) => {
                // expr? unwraps Option/Result — the result type is the inner success type
                // For now, use infer_expr_type which returns the enum type;
                // the codegen handles the actual unwrapping at runtime
                self.infer_expr_type(inner)
            }
            Expr::Unsafe(inner) => self.infer_expr_type(inner),
            Expr::Block(stmts) => {
                stmts.last().map(|s| match s {
                    Stmt::Expr { expr: e, .. } => self.infer_expr_type(e),
                    Stmt::Return { value: e, .. } => e.as_ref()
                        .map(|re| self.infer_expr_type(re))
                        .unwrap_or(Type::Unit),
                    _ => Type::Unit,
                }).unwrap_or(Type::Unit)
            }
            Expr::Ident(name) => {
                if self.registry.lookup_variant(name).is_some() {
                    let enum_name = self.registry.variant_to_enum.get(name)
                        .cloned().unwrap_or_default();
                    Type::Named(enum_name)
                } else if let Some(ty) = self.type_env.get(name) {
                    ty.clone()
                } else {
                    Type::Named("Int".into())
                }
            }
            Expr::Lambda { body, .. } => self.infer_expr_type(body),
            Expr::Index(obj, _) => {
                let obj_type = self.infer_expr_type(obj);
                match obj_type {
                    Type::Map(_, _) | Type::Set(_) => Type::Named("Option".into()),
                    Type::Named(ref n) if n == "String" => Type::Named("Int".into()),
                    _ => Type::Named("Int".into()),
                }
            }
            Expr::FieldAccess(_, _) => Type::Named("Int".into()),
            Expr::Unary(_, operand) => self.infer_expr_type(operand),
            Expr::StructLiteral(_) => Type::Unit,
            Expr::Range(_, _) => Type::Named("Range".into()),
            Expr::Tuple(elements) => {
                let types: Vec<Type> = elements.iter().map(|(_, e)| self.infer_expr_type(e)).collect();
                let joined = types.iter().map(|t| format!("{}", t)).collect::<Vec<_>>().join(", ");
                Type::Named(format!("({})", joined))
            }
            Expr::SafeFieldAccess(obj, _) => {
                let t = self.infer_expr_type(obj);
                Type::Generic(Box::new(Type::Named("Option".into())), vec![t])
            }
            Expr::SafeCall { receiver, .. } => {
                let ft = self.infer_expr_type(receiver);
                if let Type::Function(_, ret) = ft {
                    Type::Generic(Box::new(Type::Named("Option".into())), vec![*ret])
                } else {
                    Type::Named("Int".into())
                }
            }
            Expr::Assign { value, .. } => self.infer_expr_type(value),
        }
    }

    /// Check if two types are structurally compatible
    fn types_compatible(&self, declared: &Type, inferred: &Type) -> bool {
        match (declared, inferred) {
            (Type::Unit, _) | (_, Type::Unit) => true,
            (Type::Named(a), Type::Named(b)) => {
                if a == b { return true; }
                // Normalize type aliases: Str=String, Double=Float
                let norm_a = match a.as_str() {
                    "Str" => "String",
                    "Double" => "Float",
                    other => other,
                };
                let norm_b = match b.as_str() {
                    "Str" => "String",
                    "Double" => "Float",
                    other => other,
                };
                norm_a == norm_b
            }
            (Type::Struct(fa), Type::Struct(fb)) => {
                if fa.len() != fb.len() { return false; }
                fa.iter().zip(fb.iter()).all(|((na, ta), (nb, tb))| {
                    na == nb && self.types_compatible(ta, tb)
                })
            }
            (Type::Map(ka, va), Type::Map(kb, vb)) =>
                self.types_compatible(ka, kb) && self.types_compatible(va, vb),
            (Type::Set(ea), Type::Set(eb)) => self.types_compatible(ea, eb),
            (Type::Task(ta), Type::Task(tb)) => self.types_compatible(ta, tb),
            (Type::Stream(sa), Type::Stream(sb)) => self.types_compatible(sa, sb),
            (Type::Function(pa, ra), Type::Function(pb, rb)) => {
                if pa.len() != pb.len() { return false; }
                pa.iter().zip(pb.iter()).all(|(a, b)| self.types_compatible(a, b))
                    && self.types_compatible(ra, rb)
            }
            _ => true,
        }
    }
}
