use inkwell::values::BasicValue;
use inkwell::types::BasicTypeEnum;
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, InnerType, llvm_err};
use atomic::ast::Expr;

impl<'ctx> CodeGen<'ctx> {
    // ---- Option/Result convenience methods ----

    /// Check if an enum has a specific tag value (used by is_some/is_none/is_ok/is_err)
    pub(super) fn builtin_enum_is_tag(&mut self, expr: &Expr, expected_tag: u64) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(expr)?;
        let (enum_ptr, enum_ty) = match val {
            TypedValue::Enum(p, t, ..) => (p, t),
            _ => return Err("is_some/is_none/is_ok/is_err: argument must be an enum (Option or Result)".to_string()),
        };
        let i64 = self.i64_ty();
        let enum_bt: BasicTypeEnum = enum_ty.into();
        let loaded = self.builder.build_load(enum_bt, enum_ptr, "chk_enum").map_err(llvm_err)?;
        let tag = self.builder.build_extract_value(loaded.into_struct_value(), 0, "chk_tag")
            .map_err(llvm_err)?.into_int_value();
        let is_match = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(expected_tag, false), "is_match")
            .map_err(llvm_err)?;
        Ok(TypedValue::Bool(is_match))
    }

    /// unwrap_or(enum, default) - extract value from Some/Ok, or return default
    pub(super) fn builtin_unwrap_or(&mut self, enum_expr: &Expr, default_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(enum_expr)?;
        let (enum_ptr, enum_ty, inner_type) = match val {
            TypedValue::Enum(p, t, it, _) => (p, t, it),
            _ => return Err("unwrap_or: first argument must be an enum (Option or Result)".to_string()),
        };
        let i64 = self.i64_ty();
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile unwrap_or outside function")?;

        let enum_bt: BasicTypeEnum = enum_ty.into();
        let loaded = self.builder.build_load(enum_bt, enum_ptr, "uwo_enum").map_err(llvm_err)?;
        let enum_sv = loaded.into_struct_value();
        let tag = self.builder.build_extract_value(enum_sv, 0, "uwo_tag").map_err(llvm_err)?.into_int_value();
        let is_some = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "uwo_is_some")
            .map_err(llvm_err)?;

        let merge_block = self.context.append_basic_block(current_fn, "uwo_merge");
        let some_block = self.context.append_basic_block(current_fn, "uwo_some");
        let none_block = self.context.append_basic_block(current_fn, "uwo_none");

        let _ = self.builder.build_conditional_branch(is_some, some_block, none_block);

        // Some/Ok branch: extract value based on inner type
        self.builder.position_at_end(some_block);
        let data_ptr = self.builder.build_extract_value(enum_sv, 1, "uwo_data").map_err(llvm_err)?.into_pointer_value();
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, self.ptr_ty(), "uwo_inner").map_err(llvm_err)?;

        match inner_type {
            InnerType::Int => {
                let inner_val = self.builder.build_load(i64, inner_ptr, "uwo_v").map_err(llvm_err)?.into_int_value();
                let _ = self.builder.build_unconditional_branch(merge_block);
                // None/Err branch: compute default
                self.builder.position_at_end(none_block);
                let default_val = self.compile_expr(default_expr)?;
                let default_bv = default_val.to_bv().unwrap_or_else(|| i64.const_int(0, false).as_basic_value_enum());
                let _ = self.builder.build_unconditional_branch(merge_block);
                // Merge
                self.builder.position_at_end(merge_block);
                let phi = self.builder.build_phi(i64, "uwo_phi").map_err(llvm_err)?;
                phi.add_incoming(&[(&inner_val, some_block), (&default_bv.into_int_value(), none_block)]);
                Ok(TypedValue::Int(phi.as_basic_value().into_int_value()))
            }
            InnerType::Float => {
                let f64_ty = self.context.f64_type();
                let inner_val = self.builder.build_load(f64_ty, inner_ptr, "uwo_fv").map_err(llvm_err)?.into_float_value();
                let _ = self.builder.build_unconditional_branch(merge_block);
                self.builder.position_at_end(none_block);
                let default_val = self.compile_expr(default_expr)?;
                let default_fv = match default_val {
                    TypedValue::Float(f) => f,
                    TypedValue::Int(i) => self.builder.build_signed_int_to_float(i, f64_ty, "int_to_f").map_err(llvm_err)?,
                    _ => return Err("unwrap_or: default must be numeric for Option<Float>".to_string()),
                };
                let _ = self.builder.build_unconditional_branch(merge_block);
                self.builder.position_at_end(merge_block);
                let phi = self.builder.build_phi(f64_ty, "uwo_fphi").map_err(llvm_err)?;
                phi.add_incoming(&[(&inner_val, some_block), (&default_fv, none_block)]);
                Ok(TypedValue::Float(phi.as_basic_value().into_float_value()))
            }
            InnerType::Str => {
                let str_val = self.builder.build_load(self.string_type, inner_ptr, "uwo_str").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_block);
                self.builder.position_at_end(none_block);
                let default_val = self.compile_expr(default_expr)?;
                let default_ptr = match default_val {
                    TypedValue::Str(p) => p,
                    _ => return Err("unwrap_or: default must be a string for Option<String>".to_string()),
                };
                let dv = self.builder.build_load(self.string_type, default_ptr, "uwo_dv").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_block);
                self.builder.position_at_end(merge_block);
                let phi = self.builder.build_phi(self.string_type, "uwo_sphi").map_err(llvm_err)?;
                phi.add_incoming(&[(&str_val, some_block), (&dv, none_block)]);
                let result_alloca = self.builder.build_alloca(self.string_type, "uwo_str_res").map_err(llvm_err)?;
                self.builder.build_store(result_alloca, phi.as_basic_value()).map_err(llvm_err)?;
                Ok(TypedValue::Str(result_alloca))
            }
        }
    }

    /// unwrap(enum) - extract value from Some/Ok, return 0 on None/Err (debug builds can panic)
    pub(super) fn builtin_unwrap(&mut self, enum_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(enum_expr)?;
        let (enum_ptr, enum_ty) = match val {
            TypedValue::Enum(p, t, ..) => (p, t),
            _ => return Err("unwrap: argument must be an enum (Option or Result)".to_string()),
        };
        let i64 = self.i64_ty();
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile unwrap outside function")?;

        let enum_bt: BasicTypeEnum = enum_ty.into();
        let loaded = self.builder.build_load(enum_bt, enum_ptr, "uw_enum").map_err(llvm_err)?;
        let enum_sv = loaded.into_struct_value();
        let tag = self.builder.build_extract_value(enum_sv, 0, "uw_tag").map_err(llvm_err)?.into_int_value();
        let is_some = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "uw_is_some")
            .map_err(llvm_err)?;

        let merge_block = self.context.append_basic_block(current_fn, "uw_merge");
        let some_block = self.context.append_basic_block(current_fn, "uw_some");
        let none_block = self.context.append_basic_block(current_fn, "uw_none");

        let _ = self.builder.build_conditional_branch(is_some, some_block, none_block);

        // Some/Ok branch: extract value
        self.builder.position_at_end(some_block);
        let data_ptr = self.builder.build_extract_value(enum_sv, 1, "uw_data").map_err(llvm_err)?.into_pointer_value();
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, self.ptr_ty(), "uw_inner").map_err(llvm_err)?;
        let inner_val = self.builder.build_load(i64, inner_ptr, "uw_v").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // None/Err branch: return 0 (safe default, avoid complex panic machinery)
        self.builder.position_at_end(none_block);
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge
        self.builder.position_at_end(merge_block);
        let phi = self.builder.build_phi(i64, "uw_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&inner_val, some_block), (&i64.const_int(0, false), none_block)]);
        Ok(TypedValue::Int(phi.as_basic_value().into_int_value()))
    }

    /// or_else(enum, handler_or_default) - for Result: extract value or call handler with error
    /// For Option: extract value or return default
    pub(super) fn builtin_or_else(&mut self, enum_expr: &Expr, handler_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(enum_expr)?;
        let (enum_ptr, enum_ty) = match val {
            TypedValue::Enum(p, t, ..) => (p, t),
            _ => return Err("or_else: first argument must be an enum (Option or Result)".to_string()),
        };
        let i64 = self.i64_ty();
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile or_else outside function")?;

        let enum_bt: BasicTypeEnum = enum_ty.into();
        let loaded = self.builder.build_load(enum_bt, enum_ptr, "oe_enum").map_err(llvm_err)?;
        let enum_sv = loaded.into_struct_value();
        let tag = self.builder.build_extract_value(enum_sv, 0, "oe_tag").map_err(llvm_err)?.into_int_value();
        let is_some = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "oe_is_some")
            .map_err(llvm_err)?;

        let merge_block = self.context.append_basic_block(current_fn, "oe_merge");
        let some_block = self.context.append_basic_block(current_fn, "oe_some");
        let none_block = self.context.append_basic_block(current_fn, "oe_none");

        let _ = self.builder.build_conditional_branch(is_some, some_block, none_block);

        // Some/Ok branch: extract and return the value
        self.builder.position_at_end(some_block);
        let data_ptr = self.builder.build_extract_value(enum_sv, 1, "oe_data").map_err(llvm_err)?.into_pointer_value();
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, self.ptr_ty(), "oe_inner").map_err(llvm_err)?;
        let inner_val = self.builder.build_load(i64, inner_ptr, "oe_v").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // None/Err branch: evaluate handler/default
        self.builder.position_at_end(none_block);
        let handler_val = self.compile_expr(handler_expr)?;
        let handler_bv = handler_val.to_bv().unwrap_or_else(|| i64.const_int(0, false).as_basic_value_enum());
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge
        self.builder.position_at_end(merge_block);
        let phi = self.builder.build_phi(i64, "oe_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&inner_val, some_block), (&handler_bv.into_int_value(), none_block)]);
        Ok(TypedValue::Int(phi.as_basic_value().into_int_value()))
    }

    /// ok(option, err_val) - convert Option<T> to Result<T, E>
    /// Some(v) → Ok(v), None → Err(err_val)
    pub(super) fn builtin_ok(&mut self, opt_expr: &Expr, err_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(opt_expr)?;
        let (opt_ptr, opt_ty) = match val {
            TypedValue::Enum(p, t, ..) => (p, t),
            _ => return Err("ok: first argument must be an Option enum".to_string()),
        };
        let i64 = self.i64_ty();
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile ok outside function")?;

        // Look up the Result enum type
        let result_ty = *self.enum_types.get("Result")
            .unwrap();

        let opt_bt: BasicTypeEnum = opt_ty.into();
        let loaded = self.builder.build_load(opt_bt, opt_ptr, "ok_opt").map_err(llvm_err)?;
        let opt_sv = loaded.into_struct_value();
        let tag = self.builder.build_extract_value(opt_sv, 0, "ok_tag").map_err(llvm_err)?.into_int_value();
        let is_some = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "ok_is_some")
            .map_err(llvm_err)?;

        let merge_block = self.context.append_basic_block(current_fn, "ok_merge");
        let some_block = self.context.append_basic_block(current_fn, "ok_some");
        let none_block = self.context.append_basic_block(current_fn, "ok_none");

        // Allocate result on entry
        let result_bt: BasicTypeEnum = result_ty.into();
        let entry = current_fn.get_first_basic_block().unwrap();
        let saved_pos = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(instr) => { let _ = self.builder.position_before(&instr); }
            None => self.builder.position_at_end(entry),
        }
        let result_alloca = self.builder.build_alloca(result_bt, "ok_result").map_err(llvm_err)?;
        let zero = result_bt.const_zero();
        self.builder.build_store(result_alloca, zero).map_err(llvm_err)?;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        let _ = self.builder.build_conditional_branch(is_some, some_block, none_block);

        // Some branch: extract value, create Ok(result)
        self.builder.position_at_end(some_block);
        let data_ptr = self.builder.build_extract_value(opt_sv, 1, "ok_data").map_err(llvm_err)?.into_pointer_value();
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, self.ptr_ty(), "ok_inner").map_err(llvm_err)?;
        let inner_val = self.builder.build_load(i64, inner_ptr, "ok_v").map_err(llvm_err)?;

        // Allocate heap memory and store the inner value
        let buf = self.malloc_rc(i64.const_int(8, false))?;
        let buf_i64 = self.builder.build_pointer_cast(buf, self.ptr_ty(), "ok_buf_p").map_err(llvm_err)?;
        self.builder.build_store(buf_i64, inner_val).map_err(llvm_err)?;
        self.rc_inc(buf)?;

        let undef = result_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, i64.const_int(0, false), 0, "ok_tag").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, buf, 1, "ok_data").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, r2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // None branch: create Err(err_val)
        self.builder.position_at_end(none_block);
        let err_val = self.compile_expr(err_expr)?;
        // Store err_val in heap
        let err_buf = self.malloc_rc(i64.const_int(8, false))?;
        let err_bv = err_val.to_bv().unwrap_or_else(|| i64.const_int(0, false).as_basic_value_enum());
        let err_ptr = self.builder.build_pointer_cast(err_buf, self.ptr_ty(), "ok_err_p").map_err(llvm_err)?;
        self.builder.build_store(err_ptr, err_bv).map_err(llvm_err)?;
        self.rc_inc(err_buf)?;

        let undef2 = result_ty.get_undef();
        let e1 = self.builder.build_insert_value(undef2, i64.const_int(1, false), 0, "ok_err_tag").map_err(llvm_err)?;
        let e2 = self.builder.build_insert_value(e1, err_buf, 1, "ok_err_data").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, e2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge
        self.builder.position_at_end(merge_block);
        Ok(TypedValue::Enum(result_alloca, result_ty, InnerType::Int, true))
    }
}
