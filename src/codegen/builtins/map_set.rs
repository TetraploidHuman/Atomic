use inkwell::values::BasicValue;
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, llvm_err};
use atomic::ast::Expr;

impl<'ctx> CodeGen<'ctx> {
    /// Callback-based map functions: map_filter, map_map_values, map_fold
    pub(super) fn builtin_callback_map(&mut self, name: &str, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        match name {
            "map_filter" => self.builtin_map_filter(args, trailing),
            "map_map_values" => self.builtin_map_map_values(args, trailing),
            "map_fold" => self.builtin_map_fold(args, trailing),
            _ => Err(format!("Unknown callback map builtin: {}", name)),
        }
    }

    /// map_filter(map, predicate) or map_filter(predicate, map) or map_filter(map) { k, v -> ... }
    /// Predicate takes (key_tag, val_tag) -> Bool (fat {i64,ptr} with tag=1 true, 0 false)
    pub(super) fn builtin_map_filter(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, map_ptr) = if let Some(lam) = trailing {
            if args.len() != 1 {
                return Err("map_filter with trailing lambda expects 1 argument (map)".to_string());
            }
            let mv = self.compile_expr(&args[0])?;
            let fv = self.compile_expr(lam)?;
            (fv, mv)
        } else if args.len() == 2 {
            // Could be map_filter(map, fn) or map_filter(fn, map) - check types
            let a0 = self.compile_expr(&args[0])?;
            let a1 = self.compile_expr(&args[1])?;
            if matches!(a0, TypedValue::Map(_)) {
                (a1, a0)
            } else {
                (a0, a1)
            }
        } else {
            return Err("map_filter expects 2 arguments (map, predicate)".to_string());
        };

        let fn_ptr = match fn_ptr {
            TypedValue::Fn(p, _) => p,
            _ => return Err("map_filter: predicate must be a function".to_string()),
        };
        let map_ptr = match map_ptr {
            TypedValue::Map(p) => p,
            _ => return Err("map_filter: first argument must be a map".to_string()),
        };

        let map_struct = self.load_list(map_ptr)?;
        let input_len = self.list_len_val(map_struct)?;
        let data_ptr = self.list_data_ptr(map_struct)?;

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile map_filter outside function")?;

        let i64 = self.i64_ty();
        let ptr = self.ptr_ty();
        let str_ty = self.string_type;

        // Create new empty map (use input_len as capacity)
        let cc = self.call_rt("atomic_list_create", &[input_len.into()])?;
        let new_map_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "mf_result").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, new_map_bv).map_err(llvm_err)?;

        let i_alloca = self.builder.build_alloca(i64, "mf_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i64.const_int(0, false)).map_err(llvm_err)?;

        let loop_header = self.context.append_basic_block(current_fn, "mf_hdr");
        let loop_body = self.context.append_basic_block(current_fn, "mf_bdy");
        let loop_insert = self.context.append_basic_block(current_fn, "mf_ins");
        let loop_next = self.context.append_basic_block(current_fn, "mf_nxt");
        let loop_exit = self.context.append_basic_block(current_fn, "mf_ext");

        let _ = self.builder.build_unconditional_branch(loop_header);

        // Header: check i < len
        self.builder.position_at_end(loop_header);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_val, input_len, "mf_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        // Body: read entry, call predicate
        self.builder.position_at_end(loop_body);
        let off = self.builder.build_int_mul(i_val, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let di64 = self.builder.build_pointer_cast(data_ptr, ptr, "di64").map_err(llvm_err)?;

        let kt_ptr = unsafe { self.builder.build_gep(i64, di64, &[off], "kt_ptr").map_err(llvm_err) }?;
        let kt = self.builder.build_load(i64, kt_ptr, "kt").map_err(llvm_err)?.into_int_value();
        let off1 = self.builder.build_int_add(off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let kp_ptr = unsafe { self.builder.build_gep(i64, di64, &[off1], "kp_ptr").map_err(llvm_err) }?;
        let kp = self.builder.build_load(i64, kp_ptr, "kp").map_err(llvm_err)?.into_int_value();
        let off2 = self.builder.build_int_add(off, i64.const_int(2, false), "off2").map_err(llvm_err)?;
        let vt_ptr = unsafe { self.builder.build_gep(i64, di64, &[off2], "vt_ptr").map_err(llvm_err) }?;
        let vt = self.builder.build_load(i64, vt_ptr, "vt").map_err(llvm_err)?.into_int_value();
        let off3 = self.builder.build_int_add(off, i64.const_int(3, false), "off3").map_err(llvm_err)?;
        let vp_ptr = unsafe { self.builder.build_gep(i64, di64, &[off3], "vp_ptr").map_err(llvm_err) }?;
        let vp = self.builder.build_load(i64, vp_ptr, "vp").map_err(llvm_err)?.into_int_value();

        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into(), i64.into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, fn_ptr, &[kt.into(), vt.into()], "mf_call").map_err(llvm_err)?;
        let pred_bv = call_result.try_as_basic_value().basic().ok_or("mf call failed")?;
        let pred_tag = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let keep = self.builder.build_int_compare(IntPredicate::NE, pred_tag, i64.const_int(0, false), "keep").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(keep, loop_insert, loop_next);

        // Insert: add entry to result map, then go to next
        self.builder.position_at_end(loop_insert);
        let key_undef = str_ty.get_undef();
        let key1 = self.builder.build_insert_value(key_undef, kt, 0, "key1").map_err(llvm_err)?;
        let kp_val = self.builder.build_int_to_ptr(kp, ptr, "kp_val").map_err(llvm_err)?;
        let key_fat = self.builder.build_insert_value(key1, kp_val, 1, "key_fat").map_err(llvm_err)?;
        let val_undef = str_ty.get_undef();
        let val1 = self.builder.build_insert_value(val_undef, vt, 0, "val1").map_err(llvm_err)?;
        let vp_val = self.builder.build_int_to_ptr(vp, ptr, "vp_val").map_err(llvm_err)?;
        let val_fat = self.builder.build_insert_value(val1, vp_val, 1, "val_fat").map_err(llvm_err)?;

        let cur_map = self.builder.build_load(self.list_type, result_alloca, "cur_map").map_err(llvm_err)?.into_struct_value();
        let ins_cc = self.call_rt("atomic_map_insert", &[cur_map.into(), key_fat.as_basic_value_enum().into(), val_fat.as_basic_value_enum().into()])?;
        let new_map = ins_cc.try_as_basic_value().basic().ok_or("map_insert failed")?;
        self.builder.build_store(result_alloca, new_map).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_next);

        // Next: increment i, go back to header
        self.builder.position_at_end(loop_next);
        let ni = self.builder.build_int_add(i_val, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_header);

        // Exit
        self.builder.position_at_end(loop_exit);
        Ok(TypedValue::Map(result_alloca))
    }

    /// map_map_values(map, transform) or map_map_values(transform, map) or map_map_values(map) { v -> ... }
    /// Transform takes val_tag -> new_val (fat {i64, ptr})
    pub(super) fn builtin_map_map_values(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, map_ptr) = if let Some(lam) = trailing {
            if args.len() != 1 {
                return Err("map_map_values with trailing lambda expects 1 argument (map)".to_string());
            }
            let mv = self.compile_expr(&args[0])?;
            let fv = self.compile_expr(lam)?;
            (fv, mv)
        } else if args.len() == 2 {
            let a0 = self.compile_expr(&args[0])?;
            let a1 = self.compile_expr(&args[1])?;
            if matches!(a0, TypedValue::Map(_)) {
                (a1, a0)
            } else {
                (a0, a1)
            }
        } else {
            return Err("map_map_values expects 2 arguments (map, transform)".to_string());
        };

        let fn_ptr = match fn_ptr {
            TypedValue::Fn(p, _) => p,
            _ => return Err("map_map_values: transform must be a function".to_string()),
        };
        let map_ptr = match map_ptr {
            TypedValue::Map(p) => p,
            _ => return Err("map_map_values: first argument must be a map".to_string()),
        };

        let map_struct = self.load_list(map_ptr)?;
        let input_len = self.list_len_val(map_struct)?;
        let data_ptr = self.list_data_ptr(map_struct)?;

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile map_map_values outside function")?;

        let i64 = self.i64_ty();
        let ptr = self.ptr_ty();
        let str_ty = self.string_type;

        // Create new empty map for transformed values
        let cap = self.builder.build_int_add(input_len, i64.const_int(4, false), "mmv_cap").map_err(llvm_err)?;
        let cc = self.call_rt("atomic_list_create", &[cap.into()])?;
        let new_map_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "mmv_result").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, new_map_bv).map_err(llvm_err)?;

        let i_alloca = self.builder.build_alloca(i64, "mmv_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i64.const_int(0, false)).map_err(llvm_err)?;

        let loop_header = self.context.append_basic_block(current_fn, "mmv_hdr");
        let loop_body = self.context.append_basic_block(current_fn, "mmv_bdy");
        let loop_exit = self.context.append_basic_block(current_fn, "mmv_ext");

        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_header);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_val, input_len, "mmv_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        self.builder.position_at_end(loop_body);
        let off = self.builder.build_int_mul(i_val, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let di64 = self.builder.build_pointer_cast(data_ptr, ptr, "di64").map_err(llvm_err)?;

        // Read key tag, key ptr
        let kt_ptr = unsafe { self.builder.build_gep(i64, di64, &[off], "kt_ptr").map_err(llvm_err) }?;
        let kt = self.builder.build_load(i64, kt_ptr, "kt").map_err(llvm_err)?.into_int_value();
        let off1 = self.builder.build_int_add(off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let kp_ptr = unsafe { self.builder.build_gep(i64, di64, &[off1], "kp_ptr").map_err(llvm_err) }?;
        let kp = self.builder.build_load(i64, kp_ptr, "kp").map_err(llvm_err)?.into_int_value();

        // Read val tag, val ptr
        let off2 = self.builder.build_int_add(off, i64.const_int(2, false), "off2").map_err(llvm_err)?;
        let vt_ptr = unsafe { self.builder.build_gep(i64, di64, &[off2], "vt_ptr").map_err(llvm_err) }?;
        let vt = self.builder.build_load(i64, vt_ptr, "vt").map_err(llvm_err)?.into_int_value();

        // Call transform(val_tag) -> fat {i64, ptr} (new value)
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, fn_ptr, &[vt.into()], "mmv_call").map_err(llvm_err)?;
        let new_val_bv = call_result.try_as_basic_value().basic().ok_or("mmv call failed")?;
        let new_val = new_val_bv.into_struct_value();
        let new_vt = self.builder.build_extract_value(new_val, 0, "new_vt").map_err(llvm_err)?.into_int_value();
        let new_vp = self.builder.build_extract_value(new_val, 1, "new_vp").map_err(llvm_err)?.into_pointer_value();

        // Build key fat {i64, ptr}
        let key_undef = str_ty.get_undef();
        let key1 = self.builder.build_insert_value(key_undef, kt, 0, "key1").map_err(llvm_err)?;
        let kp_val = self.builder.build_int_to_ptr(kp, ptr, "kp_val").map_err(llvm_err)?;
        let key_fat = self.builder.build_insert_value(key1, kp_val, 1, "key_fat").map_err(llvm_err)?;

        // Build new val fat {i64, ptr}
        let val_undef = str_ty.get_undef();
        let val1 = self.builder.build_insert_value(val_undef, new_vt, 0, "val1").map_err(llvm_err)?;
        let val_fat = self.builder.build_insert_value(val1, new_vp, 1, "val_fat").map_err(llvm_err)?;

        let cur_map = self.builder.build_load(self.list_type, result_alloca, "cur_map").map_err(llvm_err)?.into_struct_value();
        let ins_cc = self.call_rt("atomic_map_insert", &[cur_map.into(), key_fat.as_basic_value_enum().into(), val_fat.as_basic_value_enum().into()])?;
        let new_map = ins_cc.try_as_basic_value().basic().ok_or("map_insert failed")?;
        self.builder.build_store(result_alloca, new_map).map_err(llvm_err)?;

        let ni = self.builder.build_int_add(i_val, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_exit);
        Ok(TypedValue::Map(result_alloca))
    }

    /// map_fold(map, init, folder) or map_fold(init, folder, map) or map_fold(init, map) { acc, k, v -> ... }
    /// Folder takes (acc_tag, key_tag, val_tag) -> new_acc (fat {i64, ptr})
    pub(super) fn builtin_map_fold(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, init_val, map_ptr) = if let Some(lam) = trailing {
            if args.len() != 2 {
                return Err("map_fold with trailing lambda expects 2 arguments (map, init)".to_string());
            }
            let a0 = self.compile_expr(&args[0])?;
            let a1 = self.compile_expr(&args[1])?;
            let fv = self.compile_expr(lam)?;
            if matches!(a0, TypedValue::Map(_)) {
                (fv, a1, a0)
            } else {
                (fv, a0, a1)
            }
        } else if args.len() == 3 {
            // Could be map_fold(fn, init, map) or map_fold(init, fn, map) or map_fold(init, map, fn)
            // Try to determine by checking which arg is a map
            let a0 = self.compile_expr(&args[0])?;
            let a1 = self.compile_expr(&args[1])?;
            let a2 = self.compile_expr(&args[2])?;
            if matches!(a2, TypedValue::Map(_)) {
                // Last is map, first two are fn+init or init+fn
                if matches!(a1, TypedValue::Fn(_, _)) {
                    (a1, a0, a2) // fn, init, map
                } else {
                    (a0, a1, a2) // fn(assume a0), init(a1), map(a2)
                }
            } else if matches!(a1, TypedValue::Map(_)) {
                (a0, a2, a1) // fn(a0), init(a2), map(a1)
            } else if matches!(a0, TypedValue::Map(_)) {
                (a1, a2, a0) // fn(a1), init(a2), map(a0)
            } else {
                return Err("map_fold: one argument must be a map".to_string());
            }
        } else {
            return Err("map_fold expects 3 arguments (map, init, folder)".to_string());
        };

        let fn_ptr = match fn_ptr {
            TypedValue::Fn(p, _) => p,
            _ => return Err("map_fold: folder must be a function".to_string()),
        };
        let map_ptr = match map_ptr {
            TypedValue::Map(p) => p,
            _ => return Err("map_fold: map argument must be a map".to_string()),
        };
        let init_i64 = match init_val {
            TypedValue::Int(v) => v,
            _ => return Err("map_fold: init must be an integer".to_string()),
        };

        let map_struct = self.load_list(map_ptr)?;
        let input_len = self.list_len_val(map_struct)?;
        let data_ptr = self.list_data_ptr(map_struct)?;

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile map_fold outside function")?;

        let i64 = self.i64_ty();
        let ptr = self.ptr_ty();

        let acc_alloca = self.builder.build_alloca(i64, "mfld_acc").map_err(llvm_err)?;
        self.builder.build_store(acc_alloca, init_i64).map_err(llvm_err)?;

        let i_alloca = self.builder.build_alloca(i64, "mfld_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i64.const_int(0, false)).map_err(llvm_err)?;

        let loop_header = self.context.append_basic_block(current_fn, "mfld_hdr");
        let loop_body = self.context.append_basic_block(current_fn, "mfld_bdy");
        let loop_exit = self.context.append_basic_block(current_fn, "mfld_ext");

        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_header);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_val, input_len, "mfld_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        self.builder.position_at_end(loop_body);
        let off = self.builder.build_int_mul(i_val, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let di64 = self.builder.build_pointer_cast(data_ptr, ptr, "di64").map_err(llvm_err)?;

        let kt_ptr = unsafe { self.builder.build_gep(i64, di64, &[off], "kt_ptr").map_err(llvm_err) }?;
        let kt = self.builder.build_load(i64, kt_ptr, "kt").map_err(llvm_err)?.into_int_value();

        let off2 = self.builder.build_int_add(off, i64.const_int(2, false), "off2").map_err(llvm_err)?;
        let vt_ptr = unsafe { self.builder.build_gep(i64, di64, &[off2], "vt_ptr").map_err(llvm_err) }?;
        let vt = self.builder.build_load(i64, vt_ptr, "vt").map_err(llvm_err)?.into_int_value();

        // Call folder(acc_tag, key_tag, val_tag) -> fat {i64, ptr} (new acc)
        let acc = self.builder.build_load(i64, acc_alloca, "acc").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into(), i64.into(), i64.into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, fn_ptr, &[acc.into(), kt.into(), vt.into()], "mfld_call").map_err(llvm_err)?;
        let new_acc_bv = call_result.try_as_basic_value().basic().ok_or("mfld call failed")?;
        let new_acc = if new_acc_bv.is_struct_value() {
            self.builder.build_extract_value(new_acc_bv.into_struct_value(), 0, "mfld_val").map_err(llvm_err)?.into_int_value()
        } else {
            new_acc_bv.into_int_value()
        };
        self.builder.build_store(acc_alloca, new_acc).map_err(llvm_err)?;

        let ni = self.builder.build_int_add(i_val, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_exit);
        let final_acc = self.builder.build_load(i64, acc_alloca, "final_acc").map_err(llvm_err)?;
        Ok(TypedValue::Int(final_acc.into_int_value()))
    }
}
