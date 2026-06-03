// Submodule: pattern

use atomic::ast::*;
use inkwell::values::IntValue;
use inkwell::types::BasicTypeEnum;
use inkwell::IntPredicate;
use inkwell::FloatPredicate;
use std::collections::HashMap;

use super::{CodeGen, TypedValue, Scope, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn compile_when(&mut self, w: &When) -> Result<TypedValue<'ctx>, String> {
        match &w.kind {
            WhenKind::OneLine { condition, then_expr, else_expr } => {
                let c = self.compile_expr(condition)?;
                let c_bool = match c {
                    TypedValue::Bool(b) => b,
                    _ => return Err("when condition must be boolean".to_string()),
                };
                self.compile_when_branch_lazy(c_bool, then_expr, else_expr)
            }
            WhenKind::ValueMatch { value, arms } => {
                self.compile_value_match(value, arms)
            }
            WhenKind::ConditionChain { arms } => {
                self.compile_condition_chain(arms)
            }
        }
    }

    /// Compile a guard expression and return the boolean result.
    fn compile_guard(&mut self, guard: &Option<Box<Expr>>) -> Result<IntValue<'ctx>, String> {
        match guard {
            Some(expr) => {
                let val = self.compile_expr(expr)?;
                match val {
                    TypedValue::Bool(b) => Ok(b),
                    TypedValue::Int(i) => {
                        let zero = self.i64_ty().const_int(0, false);
                        Ok(self.builder.build_int_compare(IntPredicate::NE, i, zero, "guard_truthy")
                            .map_err(llvm_err)?)
                    }
                    _ => {
                        let b1 = self.bool_ty();
                        Ok(b1.const_int(1, false))
                    }
                }
            }
            None => Ok(self.bool_ty().const_int(1, false)),
        }
    }

    pub(super) fn compile_condition_chain(&mut self, arms: &[WhenArm]) -> Result<TypedValue<'ctx>, String> {
        if arms.is_empty() {
            return Ok(TypedValue::Unit);
        }

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile when outside function")?;

        let merge_block = self.context.append_basic_block(current_fn, "chain_merge");

        // Determine result type from first arm's body (for alloca)
        let first_body = self.compile_expr(&arms[0].body)?;
        let result_ty = first_body.get_type_for_alloca(self);

        // Allocate result at entry
        let entry = current_fn.get_first_basic_block().unwrap();
        let saved_pos = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(instr) => { let _ = self.builder.position_before(&instr); }
            None => self.builder.position_at_end(entry),
        }
        let result_alloca = self.builder.build_alloca(result_ty, "chain_result").map_err(llvm_err)?;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        let mut next_check = self.context.append_basic_block(current_fn, "chain_check0");
        let _ = self.builder.build_unconditional_branch(next_check);

        for (i, arm) in arms.iter().enumerate() {
            let is_last = i == arms.len() - 1;
            self.builder.position_at_end(next_check);

            let matches = self.compile_pattern_condition(&arm.pattern, None)?;
            // Check guard if present
            let matches = if arm.guard.is_some() {
                let mut saved_scope = Scope::new();
                std::mem::swap(&mut self.scope, &mut saved_scope);
                self.scope = Scope::with_parent(saved_scope);
                self.bind_pattern_vars(&arm.pattern, None, None)?;
                let guard_matches = self.compile_guard(&arm.guard)?;
                let combined = self.builder.build_and(matches, guard_matches, "guard_and").map_err(llvm_err)?;
                let mut parent = Scope::new();
                std::mem::swap(&mut self.scope, &mut parent);
                if let Some(p) = parent.parent { self.scope = *p; }
                combined
            } else {
                matches
            };
            let body_block = self.context.append_basic_block(current_fn, &format!("chain_body{}", i));

            if is_last {
                let _ = self.builder.build_unconditional_branch(body_block);
            } else {
                next_check = self.context.append_basic_block(current_fn, &format!("chain_check{}", i + 1));
                let _ = self.builder.build_conditional_branch(matches, body_block, next_check);
            }

            self.builder.position_at_end(body_block);
            // Create child scope for pattern bindings
            let mut saved_scope = Scope::new();
            std::mem::swap(&mut self.scope, &mut saved_scope);
            self.scope = Scope::with_parent(saved_scope);
            self.bind_pattern_vars(&arm.pattern, None, None)?;
            let body_val = self.compile_expr(&arm.body)?;
            self.store_value_to_alloca(&body_val, result_alloca)?;
            // Restore scope
            let mut parent = Scope::new();
            std::mem::swap(&mut self.scope, &mut parent);
            if let Some(p) = parent.parent { self.scope = *p; }
            let _ = self.builder.build_unconditional_branch(merge_block);
        }

        self.builder.position_at_end(merge_block);
        let loaded = self.builder.build_load(result_ty, result_alloca, "chain_ld").map_err(llvm_err)?;
        self.bv_to_typed(loaded)
    }

    pub(super) fn compile_value_match(&mut self, value: &Expr, arms: &[WhenArm]) -> Result<TypedValue<'ctx>, String> {
        if arms.is_empty() {
            return Ok(TypedValue::Unit);
        }

        // Check exhaustiveness for enum matching
        self.registry.check_when_exhaustive(arms)?;

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile when outside function")?;

        // Compile the matched value once
        let matched_val = self.compile_expr(value)?;
        // Infer the AST type of the matched value for resolving generic enum params
        let matched_type = self.infer_expr_type(value);

        // Infer result type: prefer enum type if any arm returns one, otherwise use Int
        let arm_types: Vec<Type> = arms.iter()
            .map(|a| self.infer_expr_type(&a.body))
            .collect();
        let result_type = arm_types.iter()
            .find(|t| matches!(t, Type::Named(n) if self.enum_types.contains_key(n) || n == "String"))
            .or_else(|| arm_types.first())
            .cloned()
            .unwrap_or_else(|| Type::Named("Int".into()));
        let result_ty = self.ast_type_to_basic_type(&result_type);

        // Allocate result at entry
        let entry = current_fn.get_first_basic_block().unwrap();
        let saved_pos = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(instr) => { let _ = self.builder.position_before(&instr); }
            None => self.builder.position_at_end(entry),
        }
        let result_alloca = self.builder.build_alloca(result_ty, "match_result").map_err(llvm_err)?;
        // Zero-initialize to prevent garbage reads when an arm stores fewer bytes
        // than the full result type (e.g., storing i64 into {i64, ptr} for Option)
        let zero = result_ty.const_zero();
        self.builder.build_store(result_alloca, zero).map_err(llvm_err)?;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        let merge_block = self.context.append_basic_block(current_fn, "match_merge");
        let mut next_check = self.context.append_basic_block(current_fn, "match_check0");
        let _ = self.builder.build_unconditional_branch(next_check);

        for (i, arm) in arms.iter().enumerate() {
            let is_last = i == arms.len() - 1;
            self.builder.position_at_end(next_check);

            let matches = self.compile_pattern_match(&arm.pattern, &matched_val)?;
            // Check guard if present
            let matches = if arm.guard.is_some() {
                let mut saved_scope = Scope::new();
                std::mem::swap(&mut self.scope, &mut saved_scope);
                self.scope = Scope::with_parent(saved_scope);
                self.bind_pattern_vars(&arm.pattern, Some(&matched_val), Some(&matched_type))?;
                let guard_matches = self.compile_guard(&arm.guard)?;
                let combined = self.builder.build_and(matches, guard_matches, "guard_and").map_err(llvm_err)?;
                let mut parent = Scope::new();
                std::mem::swap(&mut self.scope, &mut parent);
                if let Some(p) = parent.parent { self.scope = *p; }
                combined
            } else {
                matches
            };
            let body_block = self.context.append_basic_block(current_fn, &format!("match_body{}", i));

            if is_last {
                let _ = self.builder.build_unconditional_branch(body_block);
            } else {
                next_check = self.context.append_basic_block(current_fn, &format!("match_check{}", i + 1));
                let _ = self.builder.build_conditional_branch(matches, body_block, next_check);
            }

            self.builder.position_at_end(body_block);
            // Create child scope and bind pattern variables to the matched value
            let mut saved_scope = Scope::new();
            std::mem::swap(&mut self.scope, &mut saved_scope);
            self.scope = Scope::with_parent(saved_scope);
            self.bind_pattern_vars(&arm.pattern, Some(&matched_val), Some(&matched_type))?;
            // When arm body is a zero-param lambda (parser wraps { ... } blocks
            // after -> as lambdas), compile the inner body directly so pattern
            // bindings are visible in the current scope.
            let body_val = if let Expr::Lambda { params, body, .. } = arm.body.as_ref() {
                if params.is_empty() {
                    self.compile_expr(body)?
                } else {
                    self.compile_expr(&arm.body)?
                }
            } else {
                self.compile_expr(&arm.body)?
            };
            self.store_value_to_alloca(&body_val, result_alloca)?;
            let mut parent = Scope::new();
            std::mem::swap(&mut self.scope, &mut parent);
            if let Some(p) = parent.parent { self.scope = *p; }
            let _ = self.builder.build_unconditional_branch(merge_block);
        }

        self.builder.position_at_end(merge_block);
        let loaded = self.builder.build_load(result_ty, result_alloca, "match_ld").map_err(llvm_err)?;
        self.bv_to_typed(loaded)
    }

    /// Compile a pattern as a boolean condition (for ConditionChain).
    /// For ConditionChain, patterns act as conditions: Literal/Ident/Variable are truthy,
    /// Wildcard is always true.
    pub(super) fn compile_pattern_condition(&mut self, pattern: &Pattern, _matched_val: Option<&TypedValue<'ctx>>) -> Result<IntValue<'ctx>, String> {
        let b1 = self.bool_ty();
        match pattern {
            Pattern::Wildcard => Ok(b1.const_int(1, false)),
            Pattern::Literal(lit) => {
                // A literal is truthy — always true in condition context
                // Actually, for condition chain: `when { 0 -> "zero" }` — the literal IS the condition.
                // Treat any non-zero/non-null value as true
                match lit {
                    Literal::Bool(b) => Ok(b1.const_int(if *b { 1 } else { 0 }, false)),
                    Literal::Int(n) => Ok(b1.const_int(if *n != 0 { 1 } else { 0 }, false)),
                    Literal::Float(f) => Ok(b1.const_int(if *f != 0.0 { 1 } else { 0 }, false)),
                    Literal::Char(c) => Ok(b1.const_int(if *c != '\0' { 1 } else { 0 }, false)),
                    Literal::Unit => Ok(b1.const_int(0, false)),
                    _ => Ok(b1.const_int(1, false)),
                }
            }
            Pattern::Variable(_) => Ok(b1.const_int(1, false)), // Variable binding always matches
            Pattern::Range(_start, _end) => {
                // Range in condition context: treated as true (shouldn't normally appear here)
                Ok(b1.const_int(1, false))
            }
            Pattern::IsType(type_name) => Err(format!("'is {}' requires a matched value. Use 'when value {{ is {} -> ... }}' instead of a condition chain.", type_name, type_name)),
            Pattern::Or(patterns) => {
                let mut result = b1.const_int(0, false);
                for p in patterns {
                    let m = self.compile_pattern_condition(p, None)?;
                    result = self.builder.build_or(result, m, "or").map_err(llvm_err)?;
                }
                Ok(result)
            }
            Pattern::Constructor { .. } => Ok(b1.const_int(1, false)),
            Pattern::Expr(expr) => {
                let val = self.compile_expr(expr)?;
                match val {
                    TypedValue::Bool(b) => Ok(b),
                    TypedValue::Int(i) => {
                        let zero = self.i64_ty().const_int(0, false);
                        Ok(self.builder.build_int_compare(IntPredicate::NE, i, zero, "cond_expr")
                            .map_err(llvm_err)?)
                    }
                    _ => Ok(b1.const_int(1, false)),
                }
            }
        }
    }

    /// Compile a pattern match against a value (for ValueMatch).
    /// Returns an i1: true if the pattern matches, false otherwise.
    pub(super) fn compile_pattern_match(&mut self, pattern: &Pattern, val: &TypedValue<'ctx>) -> Result<IntValue<'ctx>, String> {
        let b1 = self.bool_ty();
        match pattern {
            Pattern::Wildcard => Ok(b1.const_int(1, false)),
            Pattern::Literal(lit) => {
                match lit {
                    Literal::Int(n) => {
                        if let TypedValue::Int(iv) = val {
                            let const_val = self.i64_ty().const_int(*n as u64, true);
                            Ok(self.builder.build_int_compare(IntPredicate::EQ, *iv, const_val, "match_int")
                                .map_err(llvm_err)?)
                        } else {
                            Ok(b1.const_int(0, false))
                        }
                    }
                    Literal::Bool(b) => {
                        if let TypedValue::Bool(bv) = val {
                            let const_val = b1.const_int(if *b { 1 } else { 0 }, false);
                            Ok(self.builder.build_int_compare(IntPredicate::EQ, *bv, const_val, "match_bool")
                                .map_err(llvm_err)?)
                        } else {
                            Ok(b1.const_int(0, false))
                        }
                    }
                    Literal::String(s) => {
                        if let TypedValue::Str(ptr) = val {
                            let str_val = self.load_string(*ptr)?;
                            // Build expected string constant
                            let str_bytes = s.as_bytes();
                            let arr_ty = self.context.i8_type().array_type(str_bytes.len() as u32);
                            self.str_pat_counter += 1;
                            let gname = format!(".str_pat_{}", self.str_pat_counter);
                            let global = self.module.add_global(arr_ty, None, &gname);
                            let arr = self.context.const_string(str_bytes, false);
                            global.set_initializer(&arr);
                            let pat_data = global.as_pointer_value();
                            let undef = self.string_type.get_undef();
                            let pat_len = self.i64_ty().const_int(str_bytes.len() as u64, false);
                            let s1 = self.builder.build_insert_value(undef, pat_len, 0, "pat_len").map_err(llvm_err)?;
                            let pat_str_agg = self.builder.build_insert_value(s1, pat_data, 1, "pat_str").map_err(llvm_err)?;
                            let pat_str = pat_str_agg.into_struct_value();
                            let cc = self.call_rt("atomic_string_eq", &[str_val.into(), pat_str.into()])?;
                            let eq_result = cc.try_as_basic_value().basic().unwrap().into_int_value();
                            Ok(eq_result)
                        } else {
                            Ok(b1.const_int(0, false))
                        }
                    }
                    Literal::Char(c) => {
                        if let TypedValue::Int(iv) = val {
                            let const_val = self.i64_ty().const_int(*c as u64, false);
                            Ok(self.builder.build_int_compare(IntPredicate::EQ, *iv, const_val, "match_char")
                                .map_err(llvm_err)?)
                        } else {
                            Ok(b1.const_int(0, false))
                        }
                    }
                    Literal::Float(f) => {
                        if let TypedValue::Float(fv) = val {
                            let const_val = self.f64_ty().const_float(*f);
                            Ok(self.builder.build_float_compare(FloatPredicate::OEQ, *fv, const_val, "match_float")
                                .map_err(llvm_err)?)
                        } else if let TypedValue::Int(iv) = val {
                            let fv = self.builder.build_signed_int_to_float(*iv, self.f64_ty(), "int2float").map_err(llvm_err)?;
                            let const_val = self.f64_ty().const_float(*f);
                            Ok(self.builder.build_float_compare(FloatPredicate::OEQ, fv, const_val, "match_float_from_int")
                                .map_err(llvm_err)?)
                        } else {
                            Ok(b1.const_int(0, false))
                        }
                    }
                    Literal::Unit => {
                        Ok(b1.const_int(0, false))
                    }
                }
            }
            Pattern::Variable(_) => Ok(b1.const_int(1, false)),
            Pattern::Constructor { name, args: _, .. } => {
                // Check if val is an enum with matching variant tag
                if let TypedValue::Enum(ptr, enum_st, ..) = val {
                    let bt: BasicTypeEnum = (*enum_st).into();
                    let loaded = self.builder.build_load(bt, *ptr, "enum_ld").map_err(llvm_err)?;
                    let enum_struct = loaded.into_struct_value();
                    let tag = self.builder.build_extract_value(enum_struct, 0, "tag")
                        .map_err(llvm_err)?.into_int_value();

                    if let Some((_, variant)) = self.registry.lookup_variant(name) {
                        let expected_tag = self.i64_ty().const_int(variant.tag as u64, false);
                        Ok(self.builder.build_int_compare(IntPredicate::EQ, tag, expected_tag, "tag_match")
                            .map_err(llvm_err)?)
                    } else {
                        Ok(b1.const_int(0, false))
                    }
                } else {
                    Ok(b1.const_int(0, false))
                }
            }
            Pattern::Range(start, end) => {
                if let TypedValue::Int(iv) = val {
                    let s = self.compile_expr(start)?;
                    let e = self.compile_expr(end)?;
                    let (sv, ev) = match (&s, &e) {
                        (TypedValue::Int(a), TypedValue::Int(b)) => (*a, *b),
                        _ => return Err("Range bounds must be integers".to_string()),
                    };
                    let ge = self.builder.build_int_compare(IntPredicate::SGE, *iv, sv, "range_lo")
                        .map_err(llvm_err)?;
                    let lt = self.builder.build_int_compare(IntPredicate::SLT, *iv, ev, "range_hi")
                        .map_err(llvm_err)?;
                    Ok(self.builder.build_and(ge, lt, "range_match").map_err(llvm_err)?)
                } else {
                    Ok(b1.const_int(0, false))
                }
            }
            Pattern::IsType(type_name) => {
                // Enum variant check: `is Some` on an Option enum value
                if let Some((_, variant)) = self.registry.lookup_variant(type_name) {
                    if let TypedValue::Enum(ptr, enum_st, ..) = val {
                        let bt: BasicTypeEnum = (*enum_st).into();
                        let loaded = self.builder.build_load(bt, *ptr, "is_enum_ld").map_err(llvm_err)?;
                        let enum_struct = loaded.into_struct_value();
                        let tag = self.builder.build_extract_value(enum_struct, 0, "is_tag")
                            .map_err(llvm_err)?.into_int_value();
                        let expected_tag = self.i64_ty().const_int(variant.tag as u64, false);
                        return Ok(self.builder.build_int_compare(IntPredicate::EQ, tag, expected_tag, "is_variant")
                            .map_err(llvm_err)?);
                    }
                    return Ok(b1.const_int(0, false));
                }
                // Compile-time type check against TypedValue variant
                let matches = match type_name.as_str() {
                    "Int" => matches!(val, TypedValue::Int(_)),
                    "Float" => matches!(val, TypedValue::Float(_)),
                    "Bool" => matches!(val, TypedValue::Bool(_)),
                    "String" => matches!(val, TypedValue::Str(_)),
                    "List" => matches!(val, TypedValue::List(_)),
                    _ => false,
                };
                Ok(b1.const_int(if matches { 1 } else { 0 }, false))
            }
            Pattern::Or(patterns) => {
                let mut result = b1.const_int(0, false);
                for p in patterns {
                    let m = self.compile_pattern_match(p, val)?;
                    result = self.builder.build_or(result, m, "or_match").map_err(llvm_err)?;
                }
                Ok(result)
            }
            Pattern::Expr(expr) => {
                // In value-match context, evaluate expression as a condition.
                // If the value matches (truthy), the expression acts as a guard.
                let val = self.compile_expr(expr)?;
                match val {
                    TypedValue::Bool(b) => Ok(b),
                    TypedValue::Int(i) => {
                        let zero = self.i64_ty().const_int(0, false);
                        Ok(self.builder.build_int_compare(IntPredicate::NE, i, zero, "expr_match")
                            .map_err(llvm_err)?)
                    }
                    _ => Ok(b1.const_int(1, false)),
                }
            }
        }
    }

    /// Bind pattern variables into the current scope.
    /// For ValueMatch: bind the matched value to the variable name.
    /// For ConditionChain: the variable binding is just the condition value itself.
    pub(super) fn bind_pattern_vars(&mut self, pattern: &Pattern, matched_val: Option<&TypedValue<'ctx>>, matched_type: Option<&Type>) -> Result<(), String> {
        match pattern {
            Pattern::Variable(name) => {
                if let Some(val) = matched_val {
                    let ty = val.get_type_for_alloca(self);
                    let alloca = self.builder.build_alloca(ty, name).map_err(llvm_err)?;
                    self.store_value_to_alloca(val, alloca)?;
                    self.scope.set(name.clone(), alloca, ty, val.val_kind());
                }
            }
            Pattern::Constructor { name: variant_name, args, named_fields } => {
                if let Some(TypedValue::Enum(ptr, enum_st, ..)) = matched_val {
                    let bt: BasicTypeEnum = (*enum_st).into();
                    let loaded = self.builder.build_load(bt, *ptr, "enum_ld").map_err(llvm_err)?;
                    let enum_struct = loaded.into_struct_value();
                    let data_ptr = self.builder.build_extract_value(enum_struct, 1, "data")
                        .map_err(llvm_err)?.into_pointer_value();

                    // Try to resolve variant params if we have the matched type
                    let resolved_params = self.resolve_variant_params(variant_name, matched_type, args.len() + named_fields.len());

                    if args.len() == 1 && named_fields.is_empty() && resolved_params.is_some() {
                        // Single positional param: use type info to create proper TypedValue
                        let param_types = resolved_params.as_ref().unwrap();
                        if let Some(param_ty) = param_types.first() {
                            if let Type::Named(name) = param_ty {
                                if self.named_structs.contains_key(name.as_str()) && args.len() == 1 {
                                    let st = self.named_structs[name.as_str()];
                                    let bt: BasicTypeEnum = st.into();
                                    let alloca = self.builder.build_alloca(bt, "pat_struct").map_err(llvm_err)?;
                                    // Load struct from heap (data_ptr points to the struct data)
                                    let loaded = self.builder.build_load(bt, data_ptr, "ps_ld").map_err(llvm_err)?;
                                    self.builder.build_store(alloca, loaded).map_err(llvm_err)?;
                                    let tv = TypedValue::Struct(alloca, st);
                                    self.bind_pattern_vars(&args[0], Some(&tv), Some(param_ty))?;
                                    return Ok(());
                                }
                            }
                        }
                    }

                    // Fallback: load i64 values from heap data (for simple types like Int, Bool)
                    let total_params = args.len() + named_fields.len();
                    if total_params > 0 {
                        // Bind positional sub-patterns
                        for (i, sub) in args.iter().enumerate() {
                            let data_i64 = self.builder.build_pointer_cast(data_ptr, self.ptr_ty(), "data_i64")
                                .map_err(llvm_err)?;
                            let idx = self.i64_ty().const_int(i as u64, false);
                            let field_ptr = unsafe { self.builder.build_gep(self.i64_ty(), data_i64, &[idx], "fld") }
                                .map_err(llvm_err)?;
                            let field_val = self.builder.build_load(self.i64_ty(), field_ptr, "fld_ld")
                                .map_err(llvm_err)?;
                            let tv = self.bv_to_typed(field_val);
                            if let Ok(tv) = tv {
                                let sub_ty = resolved_params.as_ref().and_then(|p| p.get(i));
                                self.bind_pattern_vars(sub, Some(&tv), sub_ty)?;
                            }
                        }
                        // Bind named fields similarly
                        for (ni, (_, sub)) in named_fields.iter().enumerate() {
                            let data_i64 = self.builder.build_pointer_cast(data_ptr, self.ptr_ty(), "data_i64")
                                .map_err(llvm_err)?;
                            let idx = self.i64_ty().const_int((args.len() + ni) as u64, false);
                            let field_ptr = unsafe { self.builder.build_gep(self.i64_ty(), data_i64, &[idx], "nfld") }
                                .map_err(llvm_err)?;
                            let field_val = self.builder.build_load(self.i64_ty(), field_ptr, "nfld_ld")
                                .map_err(llvm_err)?;
                            let tv = self.bv_to_typed(field_val);
                            if let Ok(tv) = tv {
                                let sub_ty = resolved_params.as_ref().and_then(|p| p.get(args.len() + ni));
                                self.bind_pattern_vars(sub, Some(&tv), sub_ty)?;
                            }
                        }
                    }
                } else {
                    // constructor not in registry (builtin stdlib enum, handled elsewhere)
                }
            }
            Pattern::Or(patterns) => {
                // For Or patterns, bind the first pattern's variables (simplified)
                if let Some(first) = patterns.first() {
                    self.bind_pattern_vars(first, matched_val, matched_type)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve variant parameter types using the matched expression's AST type.
    /// For example, if matched_type is Option<Date> and variant is Some(T),
    /// resolve T = Date and return [Date].
    pub(super) fn resolve_variant_params(&self, variant_name: &str, matched_type: Option<&Type>, expected_count: usize) -> Option<Vec<Type>> {
        let mt = matched_type?;
        // Get the enum info for this variant
        let (enum_info, variant_info) = self.registry.lookup_variant(variant_name)?;
        // Check if the matched type matches the enum
        let enum_name = &enum_info.name;
        // Extract the concrete type params from matched_type
        let concrete_params: Option<&Vec<Type>> = match mt {
            Type::Named(n) if n == enum_name => Some(&vec![]), // for non-generic enums or enum with no params
            Type::Generic(base, params) => {
                if let Type::Named(n) = base.as_ref() {
                    if n == enum_name { Some(params) } else { None }
                } else { None }
            }
            _ => None,
        };

        let concrete_params = concrete_params?;

        // Map type variable names to concrete types
        let mut type_map: HashMap<String, Type> = HashMap::new();
        for (i, tv) in enum_info.type_params.iter().enumerate() {
            if let Some(ct) = concrete_params.get(i) {
                type_map.insert(tv.clone(), ct.clone());
            }
        }

        // Now resolve each variant parameter
        let mut resolved = Vec::new();
        for param in &variant_info.params {
            let param_type = match param {
                EnumVariantParam::Positional(ty) => ty,
                EnumVariantParam::Named { ty, .. } => ty,
            };
            let concrete = self.resolve_type(param_type, &type_map);
            resolved.push(concrete);
        }

        if resolved.len() >= expected_count {
            Some(resolved)
        } else {
            None
        }
    }

    /// Resolve a type by substituting type variables with concrete types
    pub(super) fn resolve_type(&self, ty: &Type, type_map: &HashMap<String, Type>) -> Type {
        match ty {
            Type::Named(name) => {
                type_map.get(name).cloned().unwrap_or_else(|| ty.clone())
            }
            Type::Generic(base, params) => {
                let new_base = self.resolve_type(base, type_map);
                let new_params: Vec<Type> = params.iter()
                    .map(|p| self.resolve_type(p, type_map))
                    .collect();
                Type::Generic(Box::new(new_base), new_params)
            }
            _ => ty.clone(),
        }
    }

    pub(super) fn compile_when_branch_lazy(&mut self, c: IntValue<'ctx>, then_expr: &Expr, else_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile when outside function".to_string())?;

        let then_diverges = matches!(then_expr, Expr::Continue | Expr::Break);
        let else_diverges = matches!(else_expr, Expr::Continue | Expr::Break);

        // Result type hint from the non-divergent branch
        let result_hint = if !then_diverges { self.expr_type_hint(then_expr) }
                          else if !else_diverges { self.expr_type_hint(else_expr) }
                          else { "Int" };
        let result_ty: BasicTypeEnum = match result_hint {
            "String" => self.string_type.into(),
            _ => self.i64_ty().into(),
        };

        let then_block = self.context.append_basic_block(current_fn, "when_then");
        let else_block = self.context.append_basic_block(current_fn, "when_else");
        let merge_block = self.context.append_basic_block(current_fn, "when_merge");

        let _ = self.builder.build_conditional_branch(c, then_block, else_block);

        // Alloca at entry for the non-divergent branch(es) to store into
        let result_alloca = if !then_diverges || !else_diverges {
            let entry = current_fn.get_first_basic_block().unwrap();
            let saved_pos = self.builder.get_insert_block();
            match entry.get_first_instruction() {
                Some(instr) => { let _ = self.builder.position_before(&instr); }
                None => self.builder.position_at_end(entry),
            }
            let alloca = self.builder.build_alloca(result_ty, "when_result").map_err(llvm_err)?;
            if let Some(block) = saved_pos {
                self.builder.position_at_end(block);
            }
            Some(alloca)
        } else {
            None
        };

        // Then branch
        self.builder.position_at_end(then_block);
        if then_diverges {
            self.compile_expr(then_expr)?;
            // divergent: branch already built by compile_expr, nothing more
        } else {
            let tv = self.compile_expr(then_expr)?;
            self.store_value_to_alloca(&tv, result_alloca.unwrap())?;
            let _ = self.builder.build_unconditional_branch(merge_block);
        }

        // Else branch
        self.builder.position_at_end(else_block);
        if else_diverges {
            self.compile_expr(else_expr)?;
            // divergent: branch already built by compile_expr, nothing more
        } else {
            let ev = self.compile_expr(else_expr)?;
            self.store_value_to_alloca(&ev, result_alloca.unwrap())?;
            let _ = self.builder.build_unconditional_branch(merge_block);
        }

        // Merge: load result if at least one branch reaches here
        self.builder.position_at_end(merge_block);
        if let Some(alloca) = result_alloca {
            let loaded = self.builder.build_load(result_ty, alloca, "when_ld").map_err(llvm_err)?;
            self.bv_to_typed(loaded)
        } else {
            // Both branches diverged — this merge block is unreachable
            Ok(TypedValue::Unit)
        }
    }

}
