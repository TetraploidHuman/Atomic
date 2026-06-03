// Submodule: for_loop

use atomic::ast::*;
use inkwell::values::{IntValue, PointerValue};
use inkwell::types::BasicTypeEnum;
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, ValKind, Scope, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    /// Return a type hint string for an expression AST (without compiling it).
    pub(super) fn expr_type_hint(&self, expr: &Expr) -> &'static str {
        match expr {
            Expr::Literal(Literal::String(_)) => "String",
            Expr::Literal(Literal::Int(_)) => "Int",
            Expr::Literal(Literal::Float(_)) => "Float",
            Expr::Literal(Literal::Bool(_)) => "Bool",
            Expr::Call { func, .. } => {
                if let Expr::Ident(name) = func.as_ref() {
                    if name == "print" || name == "println" { return "Unit"; }
                    if self.registry.lookup_variant(name).is_some() { return "Enum"; }
                    // Look up known functions that return String
                    if name == "substring" || name == "unwrap_or" || name == "read_line"
                        || name == "jsonEscape" || name == "handleChat" || name == "chatOnce"
                        || name == "storeMessages" || name == "extractContent"
                        || name == "httpRequest" || name == "str"
                    {
                        return "String";
                    }
                }
                "Int"
            }
            Expr::Ident(name) => {
                if self.registry.lookup_variant(name).is_some() { return "Enum"; }
                // Look up in scope for type
                if let Some(sv) = self.scope.get(name) {
                    match sv.kind {
                        ValKind::Str => return "String",
                        ValKind::Int => return "Int",
                        ValKind::Float => return "Float",
                        ValKind::Bool => return "Bool",
                        ValKind::Struct => return "Struct",
                        ValKind::Enum => return "Enum",
                        ValKind::List => return "List",
                        ValKind::Map => return "Map",
                        ValKind::Set => return "Set",
                        _ => {}
                    }
                }
                "Int"
            }
            Expr::Binary(lhs, op, _) => {
                if *op == BinaryOp::Add {
                    // String concat returns String; look at LHS
                    let lh = self.expr_type_hint(lhs);
                    if lh == "String" { return "String"; }
                }
                "Int"
            }
            Expr::StructLiteral(_) => "Struct",
            _ => "Int",
        }
    }

    pub(super) fn store_value_to_alloca(&mut self, v: &TypedValue<'ctx>, alloca: PointerValue<'ctx>) -> Result<(), String> {
        match v {
            TypedValue::Str(ptr) => {
                let str_val = self.load_string(*ptr)?;
                self.builder.build_store(alloca, str_val).map_err(llvm_err)?;
            }
            TypedValue::List(ptr) => {
                let list_val = self.load_list(*ptr)?;
                self.builder.build_store(alloca, list_val).map_err(llvm_err)?;
            }
            TypedValue::Map(ptr) => {
                let map_val = self.load_list(*ptr)?;
                self.builder.build_store(alloca, map_val).map_err(llvm_err)?;
            }
            TypedValue::Set(ptr) => {
                let set_val = self.load_list(*ptr)?;
                self.builder.build_store(alloca, set_val).map_err(llvm_err)?;
            }
            TypedValue::Task(ptr) => {
                self.builder.build_store(alloca, *ptr).map_err(llvm_err)?;
            }
            TypedValue::Stream(ptr) => {
                self.builder.build_store(alloca, *ptr).map_err(llvm_err)?;
            }
            TypedValue::LazyList(ptr) => {
                let ll_val = self.builder.build_load(self.lazylist_type, *ptr, "ll_ld").map_err(llvm_err)?;
                self.builder.build_store(alloca, ll_val).map_err(llvm_err)?;
            }
            TypedValue::CString(p) | TypedValue::Ptr(p) | TypedValue::FileHandle(p) => {
                self.builder.build_store(alloca, *p).map_err(llvm_err)?;
            }
            TypedValue::Struct(ptr, ty) => {
                let bt: BasicTypeEnum = (*ty).into();
                let loaded = self.builder.build_load(bt, *ptr, "struct_ld").map_err(llvm_err)?;
                self.builder.build_store(alloca, loaded).map_err(llvm_err)?;
            }
            TypedValue::Enum(ptr, ty, ..) => {
                let bt: BasicTypeEnum = (*ty).into();
                let loaded = self.builder.build_load(bt, *ptr, "enum_ld").map_err(llvm_err)?;
                self.builder.build_store(alloca, loaded).map_err(llvm_err)?;
            }
            _ => {
                if let Some(bv) = v.to_bv() {
                    self.builder.build_store(alloca, bv).map_err(llvm_err)?;
                }
            }
        }
        Ok(())
    }

    /// Store a TypedValue to an alloca, coercing types when the alloca type differs.
    pub(super) fn store_typed_value(&mut self, v: &TypedValue<'ctx>, alloca: PointerValue<'ctx>, target_ty: BasicTypeEnum<'ctx>) -> Result<(), String> {
        match (v, target_ty) {
            // Int -> Float coercion
            (TypedValue::Int(iv), BasicTypeEnum::FloatType(_)) => {
                let fv = self.builder.build_signed_int_to_float(*iv, self.f64_ty(), "int2float")
                    .map_err(llvm_err)?;
                self.builder.build_store(alloca, fv).map_err(llvm_err)?;
            }
            // Float -> Int coercion
            (TypedValue::Float(fv), BasicTypeEnum::IntType(_)) => {
                let iv = self.builder.build_float_to_signed_int(*fv, self.i64_ty(), "float2int")
                    .map_err(llvm_err)?;
                self.builder.build_store(alloca, iv).map_err(llvm_err)?;
            }
            _ => self.store_value_to_alloca(v, alloca)?,
        }
        Ok(())
    }

    pub(super) fn compile_for(&mut self, f: &For) -> Result<TypedValue<'ctx>, String> {
        match &f.kind {
            ForKind::Iterate { var, iterable, body, collect, .. } => {
                self.compile_for_iterate(var, iterable, body, *collect)
            }
            ForKind::Condition { condition, body, .. } => {
                self.compile_for_condition(condition, body)
            }
            ForKind::Infinite { body, .. } => {
                self.compile_for_infinite(body)
            }
            ForKind::NestedIterate { bindings, body, collect } => {
                self.compile_for_nested_iterate(bindings, body, *collect)
            }
            _ => Ok(TypedValue::Unit),
        }
    }

    pub(super) fn compile_for_condition(&mut self, condition: &Expr, body: &Expr) -> Result<TypedValue<'ctx>, String> {
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile for outside function")?;

        let header = self.context.append_basic_block(current_fn, "for_cond_hdr");
        let body_block = self.context.append_basic_block(current_fn, "for_cond_body");
        let exit = self.context.append_basic_block(current_fn, "for_cond_exit");

        let saved_continue = self.continue_target;
        let saved_break = self.break_target;
        self.continue_target = Some(header);
        self.break_target = Some(exit);

        let _ = self.builder.build_unconditional_branch(header);
        self.builder.position_at_end(header);
        let cv = self.compile_expr(condition)?;
        let cond_val = match cv {
            TypedValue::Bool(b) => b,
            TypedValue::Int(v) => self.builder.build_int_compare(
                inkwell::IntPredicate::NE, v, self.i64_ty().const_int(0, false), "cond"
            ).map_err(llvm_err)?,
            _ => return Err("for condition must evaluate to Bool or Int".to_string()),
        };
        let _ = self.builder.build_conditional_branch(cond_val, body_block, exit);

        self.builder.position_at_end(body_block);
        self.compile_expr(body)?;
        let _ = self.builder.build_unconditional_branch(header);

        self.builder.position_at_end(exit);
        self.continue_target = saved_continue;
        self.break_target = saved_break;

        Ok(TypedValue::Unit)
    }

    pub(super) fn compile_for_infinite(&mut self, body: &Expr) -> Result<TypedValue<'ctx>, String> {
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile for outside function")?;

        let body_block = self.context.append_basic_block(current_fn, "for_inf_body");
        let exit = self.context.append_basic_block(current_fn, "for_inf_exit");

        let saved_continue = self.continue_target;
        let saved_break = self.break_target;
        self.continue_target = Some(body_block);
        self.break_target = Some(exit);

        let _ = self.builder.build_unconditional_branch(body_block);
        self.builder.position_at_end(body_block);
        self.compile_expr(body)?;
        let _ = self.builder.build_unconditional_branch(body_block);

        self.builder.position_at_end(exit);
        self.continue_target = saved_continue;
        self.break_target = saved_break;

        Ok(TypedValue::Unit)
    }

    pub(super) fn compile_for_iterate(&mut self, variable: &str, iterator: &Expr, body: &Box<Expr>, collect: bool) -> Result<TypedValue<'ctx>, String> {
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile for outside function".to_string())?;

        let i64 = self.i64_ty();

        // Determine iteration kind: range or list
        let (start_val, end_val, input_list_ptr) = match iterator {
            Expr::Binary(lhs, BinaryOp::Range, rhs) | Expr::Binary(lhs, BinaryOp::RangeExclusive, rhs) => {
                let start_v = self.compile_expr(lhs)?;
                let end_v = self.compile_expr(rhs)?;
                let (s, e) = match (start_v, end_v) {
                    (TypedValue::Int(s), TypedValue::Int(e)) => (s, e),
                    _ => return Err("Range bounds must be integers".to_string()),
                };
                (s, e, None)
            }
            _ => {
                // Try as a list expression (shorthand for [list] { body })
                let list_val = self.compile_expr(iterator)?;
                let list_ptr = match &list_val {
                    TypedValue::List(p) | TypedValue::Set(p) | TypedValue::Map(p) => *p,
                    TypedValue::Stream(p) => {
                        self.builder.build_struct_gep(self.stream_type, *p, 1, "for_sl").map_err(llvm_err)?
                    }
                    TypedValue::LazyList(_) => {
                        // Convert LazyList to List for iteration
                        let converted = self.convert_lazylist_to_list(&list_val)?;
                        let alloca = self.builder.build_alloca(self.list_type, "ll_to_list").map_err(llvm_err)?;
                        self.builder.build_store(alloca, converted).map_err(llvm_err)?;
                        alloca
                    }
                    _ => return Err("Only range iterators (1..10), lists, sets, maps, streams and lazy lists are supported for for expressions".to_string()),
                };
                let loaded = self.load_list(list_ptr)?;
                let len = self.list_len_val(loaded)?;
                let zero = i64.const_int(0, false);
                (zero, len, Some(list_ptr))
            }
        };

        // Create result list if collecting
        let result_list = if collect {
            let len = self.builder.build_int_sub(end_val, start_val, "est_len").map_err(llvm_err)?;
            let list_cc = self.call_rt("atomic_list_create", &[len.into()])?;
            let list_bv = list_cc.try_as_basic_value().basic().ok_or("list_create failed")?;
            let result_alloca = self.builder.build_alloca(self.list_type, "collect_result").map_err(llvm_err)?;
            self.builder.build_store(result_alloca, list_bv).map_err(llvm_err)?;
            Some(result_alloca)
        } else {
            None
        };

        // Track write position in result list (separate from loop counter,
        // needed when continue skips some elements)
        let collect_pos = if result_list.is_some() {
            let pos = self.builder.build_alloca(i64, "collect_pos").map_err(llvm_err)?;
            self.builder.build_store(pos, i64.const_int(0, false)).map_err(llvm_err)?;
            Some(pos)
        } else {
            None
        };

        // Allocate loop counter (index)
        let idx_alloca = self.builder.build_alloca(i64, "for_idx").map_err(llvm_err)?;
        self.builder.build_store(idx_alloca, start_val).map_err(llvm_err)?;

        // For list iteration, allocate separate element value storage
        let val_alloca = if input_list_ptr.is_some() {
            Some(self.builder.build_alloca(i64, "for_val").map_err(llvm_err)?)
        } else {
            None
        };

        // Create blocks
        let loop_header = self.context.append_basic_block(current_fn, "for_header");
        let loop_body = self.context.append_basic_block(current_fn, "for_body");
        let loop_next = self.context.append_basic_block(current_fn, "for_next"); // continue target + increment
        let loop_exit = self.context.append_basic_block(current_fn, "for_exit");

        // Set continue target so `continue` inside the body branches here
        let saved_continue_target = self.continue_target;
        let saved_break_target = self.break_target;
        self.continue_target = Some(loop_next);
        self.break_target = Some(loop_exit);

        // Branch to header
        let _ = self.builder.build_unconditional_branch(loop_header);

        // Loop header: check condition
        self.builder.position_at_end(loop_header);
        let current = self.builder.build_load(i64, idx_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, current, end_val, "for_cond")
            .map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        // Loop body
        self.builder.position_at_end(loop_body);

        // For list iteration: load the element at current index into val_alloca
        if let (Some(va), Some(list_ptr)) = (val_alloca, input_list_ptr) {
            let loaded = self.load_list(list_ptr)?;
            let data_ptr = self.list_data_ptr(loaded)?;
            let fat_elem_ptr = unsafe { self.builder.build_gep(self.string_type, data_ptr, &[current], "fat_elem").map_err(llvm_err) }?;
            let fat_elem = self.builder.build_load(self.string_type, fat_elem_ptr, "fat_val").map_err(llvm_err)?;
            let tag = self.builder.build_extract_value(fat_elem.into_struct_value(), 0, "elem_tag").map_err(llvm_err)?.into_int_value();
            self.builder.build_store(va, tag).map_err(llvm_err)?;
        }

        // Add loop variable to scope
        let mut saved_scope = Scope::new();
        std::mem::swap(&mut self.scope, &mut saved_scope);
        self.scope = Scope::with_parent(saved_scope);
        if let Some(va) = val_alloca {
            self.scope.set(variable.to_string(), va, i64.into(), ValKind::Int);
        } else {
            self.scope.set(variable.to_string(), idx_alloca, i64.into(), ValKind::Int);
        };

        // Compile body
        let body_val = self.compile_expr(body)?;

        // Collect result if needed
        if let (Some(list_ptr), Some(pos)) = (result_list, collect_pos) {
            let list_loaded = self.load_list(list_ptr)?;
            let elem_fat = self.to_fat_struct(&body_val)?;
            let data_ptr = self.list_data_ptr(list_loaded)?;
            let pos_val = self.builder.build_load(i64, pos, "pos_val").map_err(llvm_err)?.into_int_value();
            let fat_elem_ptr = unsafe { self.builder.build_gep(self.string_type, data_ptr, &[pos_val], "collect_elem").map_err(llvm_err) }?;
            self.builder.build_store(fat_elem_ptr, elem_fat).map_err(llvm_err)?;
            let next_pos = self.builder.build_int_add(pos_val, i64.const_int(1, false), "pos_next").map_err(llvm_err)?;
            self.builder.build_store(pos, next_pos).map_err(llvm_err)?;
        }

        // Branch to loop_next (increment)
        self.builder.build_unconditional_branch(loop_next).map_err(llvm_err)?;

        // loop_next: restore scope, increment, loop back (also the continue target)
        self.builder.position_at_end(loop_next);

        // Restore scope
        let mut parent = Scope::new();
        std::mem::swap(&mut self.scope, &mut parent);
        if let Some(p) = parent.parent { self.scope = *p; }

        // Increment counter
        let next_val = self.builder.build_load(i64, idx_alloca, "i_next").map_err(llvm_err)?.into_int_value();
        let one = i64.const_int(1, false);
        let inc = self.builder.build_int_add(next_val, one, "i_inc").map_err(llvm_err)?;
        self.builder.build_store(idx_alloca, inc).map_err(llvm_err)?;

        // Jump back to header
        let _ = self.builder.build_unconditional_branch(loop_header);

        // Continue at exit
        self.builder.position_at_end(loop_exit);

        // Restore continue target
        self.continue_target = saved_continue_target;
        self.break_target = saved_break_target;

        if let Some(list_ptr) = result_list {
            Ok(TypedValue::List(list_ptr))
        } else {
            Ok(TypedValue::Unit)
        }
    }

    pub(super) fn compile_for_nested_iterate(
        &mut self, bindings: &[(String, Expr)], body: &Expr, collect: bool,
    ) -> Result<TypedValue<'ctx>, String> {
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile nested for outside function")?;

        let i64 = self.i64_ty();
        let saved_continue_target = self.continue_target;
        let saved_break_target = self.break_target;

        // Pre-allocate all loop counters and bounds: (idx_alloca, start_val, end_val)
        let mut loops: Vec<(PointerValue, IntValue, IntValue)> = Vec::new();
        for (i, (_var, iterable)) in bindings.iter().enumerate() {
            let (start, end) = match iterable {
                Expr::Binary(lhs, BinaryOp::Range, rhs) | Expr::Binary(lhs, BinaryOp::RangeExclusive, rhs) => {
                    let s = self.compile_expr(lhs)?;
                    let e = self.compile_expr(rhs)?;
                    match (s, e) {
                        (TypedValue::Int(s), TypedValue::Int(e)) => (s, e),
                        _ => return Err("Range bounds must be integers".to_string()),
                    }
                }
                _ => {
                    let list_val = self.compile_expr(iterable)?;
                    let list_ptr = match &list_val {
                        TypedValue::List(p) | TypedValue::Set(p) | TypedValue::Map(p) => *p,
                        TypedValue::Stream(p) => {
                            self.builder.build_struct_gep(self.stream_type, *p, 1, "nested_for_sl").map_err(llvm_err)?
                        }
                        TypedValue::LazyList(_) => {
                            let converted = self.convert_lazylist_to_list(&list_val)?;
                            let alloca = self.builder.build_alloca(self.list_type, "nested_ll_to_list").map_err(llvm_err)?;
                            self.builder.build_store(alloca, converted).map_err(llvm_err)?;
                            alloca
                        }
                        _ => return Err("Only ranges, lists, sets, maps, streams and lazy lists are supported in nested for".to_string()),
                    };
                    let loaded = self.load_list(list_ptr)?;
                    let len = self.list_len_val(loaded)?;
                    (i64.const_int(0, false), len)
                }
            };
            let idx = self.builder.build_alloca(i64, &format!("nested_idx_{}", i)).map_err(llvm_err)?;
            self.builder.build_store(idx, start).map_err(llvm_err)?;
            loops.push((idx, start, end));
        }

        // Create result list if collecting
        let result_list = if collect {
            let cap = i64.const_int(16, false);
            let list_cc = self.call_rt("atomic_list_create", &[cap.into()])?;
            let list_bv = list_cc.try_as_basic_value().basic().ok_or("list_create failed")?;
            let ra = self.builder.build_alloca(self.list_type, "nested_result").map_err(llvm_err)?;
            self.builder.build_store(ra, list_bv).map_err(llvm_err)?;
            Some(ra)
        } else {
            None
        };

        // Build nested loop structure: header_0 → body_0 → ... → header_n → body_n → innermost
        let outer_header = self.context.append_basic_block(current_fn, "nested_h0");
        let innermost_body = self.context.append_basic_block(current_fn, "nested_inner");
        let outer_next = self.context.append_basic_block(current_fn, "nested_next");
        let outer_exit = self.context.append_basic_block(current_fn, "nested_exit");

        self.continue_target = Some(outer_next);
        self.break_target = Some(outer_exit);

        // Branch to first header
        let _ = self.builder.build_unconditional_branch(outer_header);

        // ---- Outer loop (level 0) ----
        self.builder.position_at_end(outer_header);
        let (outer_idx, _outer_start, outer_end) = loops[0];
        let outer_val = self.builder.build_load(i64, outer_idx, "o_val").map_err(llvm_err)?.into_int_value();
        let outer_cond = self.builder.build_int_compare(IntPredicate::SLT, outer_val, outer_end, "o_cond").map_err(llvm_err)?;

        // ---- Inner loop (level 1) ----
        let inner_header = self.context.append_basic_block(current_fn, "nested_h1");
        let inner_next = self.context.append_basic_block(current_fn, "nested_inext");
        let _ = self.builder.build_conditional_branch(outer_cond, inner_header, outer_exit);

        self.builder.position_at_end(inner_header);
        let (inner_idx, _inner_start, inner_end) = loops[1];
        let inner_val = self.builder.build_load(i64, inner_idx, "i_val").map_err(llvm_err)?.into_int_value();
        let inner_cond = self.builder.build_int_compare(IntPredicate::SLT, inner_val, inner_end, "i_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(inner_cond, innermost_body, outer_next);

        // ---- Innermost body ----
        self.builder.position_at_end(innermost_body);

        // Set up scope with all binding variables
        let mut saved_scope = Scope::new();
        std::mem::swap(&mut self.scope, &mut saved_scope);
        self.scope = Scope::with_parent(saved_scope);
        for (i, (var, _)) in bindings.iter().enumerate() {
            let (idx, _, _) = loops[i];
            self.scope.set(var.clone(), idx, i64.into(), ValKind::Int);
        }

        // Compile body
        let body_val = self.compile_expr(body)?;

        // Collect result
        if let Some(list_ptr) = result_list {
            let list_loaded = self.load_list(list_ptr)?;
            let elem_fat = self.to_fat_struct(&body_val)?;
            let push_cc = self.call_rt("atomic_list_push", &[list_loaded.into(), elem_fat.into()])?;
            let pushed = push_cc.try_as_basic_value().basic().ok_or("list_push failed")?;
            self.builder.build_store(list_ptr, pushed).map_err(llvm_err)?;
        }

        // Restore scope
        let mut parent = Scope::new();
        std::mem::swap(&mut self.scope, &mut parent);
        if let Some(p) = parent.parent { self.scope = *p; }

        // Branch to inner_next
        let _ = self.builder.build_unconditional_branch(inner_next);

        // ---- Inner next (increment inner counter) ----
        self.builder.position_at_end(inner_next);
        let inner_load = self.builder.build_load(i64, inner_idx, "i_load").map_err(llvm_err)?.into_int_value();
        let inner_inc = self.builder.build_int_add(inner_load, i64.const_int(1, false), "i_inc").map_err(llvm_err)?;
        self.builder.build_store(inner_idx, inner_inc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(inner_header);

        // ---- Outer next (increment outer counter, reset inner) ----
        self.builder.position_at_end(outer_next);
        let outer_load = self.builder.build_load(i64, outer_idx, "o_load").map_err(llvm_err)?.into_int_value();
        let outer_inc = self.builder.build_int_add(outer_load, i64.const_int(1, false), "o_inc").map_err(llvm_err)?;
        self.builder.build_store(outer_idx, outer_inc).map_err(llvm_err)?;
        // Reset inner counter to start value
        let (inner_idx2_reset, inner_start_reset, _) = loops[1];
        self.builder.build_store(inner_idx2_reset, inner_start_reset).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(outer_header);

        // ---- Exit ----
        self.builder.position_at_end(outer_exit);

        self.continue_target = saved_continue_target;
        self.break_target = saved_break_target;

        if let Some(list_ptr) = result_list {
            Ok(TypedValue::List(list_ptr))
        } else {
            Ok(TypedValue::Unit)
        }
    }

}
