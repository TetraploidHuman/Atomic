use inkwell::values::{IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, InnerType, llvm_err};
use atomic::ast::Expr;

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn builtin_map(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        // map(fn, list) or map(list) { lambda }
        let (fn_ptr, list_val) = if let Some(lam) = trailing {
            // map(list) { lambda }
            if args.len() != 1 {
                return Err("map with trailing lambda expects 1 argument (list)".to_string());
            }
            let lv = self.compile_expr(&args[0])?;
            let fv = self.compile_expr(lam)?;
            (fv, lv)
        } else if args.len() == 2 {
            let fv = self.compile_expr(&args[0])?;
            let lv = self.compile_expr(&args[1])?;
            (fv, lv)
        } else {
            return Err("map expects 2 arguments (fn, list)".to_string());
        };

        let fn_ptr = match fn_ptr {
            TypedValue::Fn(p, _) => p,
            _ => return Err("map: first argument must be a function".to_string()),
        };
        let list_ptr = match list_val {
            TypedValue::List(p) => p,
            _ => return Err("map: second argument must be a list".to_string()),
        };

        // Build the result list
        let list_struct = self.load_list(list_ptr)?;
        let input_len = self.list_len_val(list_struct)?;

        // Create new list with same capacity
        let new_list_cc = self.call_rt("atomic_list_create", &[input_len.into()])?;
        let new_list_bv = new_list_cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "map_result").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, new_list_bv).map_err(llvm_err)?;

        // Build loop
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile map outside function")?;

        let i64 = self.i64_ty();
        let i_alloca = self.builder.build_alloca(i64, "map_i").map_err(llvm_err)?;
        let zero = i64.const_int(0, false);
        self.builder.build_store(i_alloca, zero).map_err(llvm_err)?;

        let loop_header = self.context.append_basic_block(current_fn, "map_header");
        let loop_body = self.context.append_basic_block(current_fn, "map_body");
        let loop_exit = self.context.append_basic_block(current_fn, "map_exit");

        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_header);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_val, input_len, "map_cond")
            .map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        self.builder.position_at_end(loop_body);

        // Get element from input list (fat {i64,ptr} struct)
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), i_val.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        // Extract tag (first field) to pass to lambda (lambdas still take i64)
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "elem_tag")
            .map_err(llvm_err)?;

        // Call the lambda with the element tag (returns fat {i64,ptr})
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "map_call")
            .map_err(llvm_err)?;
        let mapped_bv = call_result.try_as_basic_value().basic().ok_or("map call failed")?;

        // Push lambda result (fat {i64,ptr}) to result list
        let result_list = self.load_list(result_alloca)?;
        let push_cc = self.call_rt("atomic_list_push", &[result_list.into(), mapped_bv.into()])?;
        let pushed = push_cc.try_as_basic_value().basic().ok_or("list_push failed")?;
        self.builder.build_store(result_alloca, pushed).map_err(llvm_err)?;

        // Increment counter
        let one = i64.const_int(1, false);
        let next = self.builder.build_int_add(i_val, one, "i_next").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, next).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_exit);
        Ok(TypedValue::List(result_alloca))
    }

    pub(super) fn builtin_filter(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_val) = if let Some(lam) = trailing {
            if args.len() != 1 {
                return Err("filter with trailing lambda expects 1 argument (list)".to_string());
            }
            let lv = self.compile_expr(&args[0])?;
            let fv = self.compile_expr(lam)?;
            (fv, lv)
        } else if args.len() == 2 {
            let fv = self.compile_expr(&args[0])?;
            let lv = self.compile_expr(&args[1])?;
            (fv, lv)
        } else {
            return Err("filter expects 2 arguments (fn, list)".to_string());
        };

        let fn_ptr = match fn_ptr {
            TypedValue::Fn(p, _) => p,
            _ => return Err("filter: first argument must be a function".to_string()),
        };
        let list_ptr = match list_val {
            TypedValue::List(p) => p,
            _ => return Err("filter: second argument must be a list".to_string()),
        };

        let list_struct = self.load_list(list_ptr)?;
        let input_len = self.list_len_val(list_struct)?;

        let new_list_cc = self.call_rt("atomic_list_create", &[input_len.into()])?;
        let new_list_bv = new_list_cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "filter_result").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, new_list_bv).map_err(llvm_err)?;

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile filter outside function")?;

        let i64 = self.i64_ty();
        let i_alloca = self.builder.build_alloca(i64, "filter_i").map_err(llvm_err)?;
        let zero = i64.const_int(0, false);
        self.builder.build_store(i_alloca, zero).map_err(llvm_err)?;

        let loop_header = self.context.append_basic_block(current_fn, "filter_header");
        let loop_body = self.context.append_basic_block(current_fn, "filter_body");
        let loop_push = self.context.append_basic_block(current_fn, "filter_push");
        let loop_inc = self.context.append_basic_block(current_fn, "filter_inc");
        let loop_exit = self.context.append_basic_block(current_fn, "filter_exit");

        let _ = self.builder.build_unconditional_branch(loop_header);

        // Header: check i < len
        self.builder.position_at_end(loop_header);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_val, input_len, "filter_cond")
            .map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        // Body: get element, call predicate
        self.builder.position_at_end(loop_body);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), i_val.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        // Extract tag to pass to predicate (lambdas still take i64)
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "elem_tag")
            .map_err(llvm_err)?;

        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "filter_call")
            .map_err(llvm_err)?;
        let pred_bv = call_result.try_as_basic_value().basic().ok_or("filter call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "filter_val")
                .map_err(llvm_err)?.into_int_value()
        } else {
            pred_bv.into_int_value()
        };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, zero, "is_true")
            .map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_true, loop_push, loop_inc);

        // Push: add original fat struct element to result list
        self.builder.position_at_end(loop_push);
        let result_list = self.load_list(result_alloca)?;
        let push_cc = self.call_rt("atomic_list_push", &[result_list.into(), elem_val.into()])?;
        let pushed = push_cc.try_as_basic_value().basic().ok_or("list_push failed")?;
        self.builder.build_store(result_alloca, pushed).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_inc);

        // Increment: i++ then back to header
        self.builder.position_at_end(loop_inc);
        let i_next = self.builder.build_load(i64, i_alloca, "i_next").map_err(llvm_err)?.into_int_value();
        let one = i64.const_int(1, false);
        let next = self.builder.build_int_add(i_next, one, "i_inc").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, next).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_exit);
        Ok(TypedValue::List(result_alloca))
    }

    pub(super) fn builtin_fold(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        // fold(fn, init, list) or fold(init, list) { lambda }
        let (fn_ptr, init_val, list_val) = if let Some(lam) = trailing {
            if args.len() != 2 {
                return Err("fold with trailing lambda expects 2 arguments (init, list)".to_string());
            }
            let iv = self.compile_expr(&args[0])?;
            let lv = self.compile_expr(&args[1])?;
            let fv = self.compile_expr(lam)?;
            (fv, iv, lv)
        } else if args.len() == 3 {
            let fv = self.compile_expr(&args[0])?;
            let iv = self.compile_expr(&args[1])?;
            let lv = self.compile_expr(&args[2])?;
            (fv, iv, lv)
        } else {
            return Err("fold expects 3 arguments (fn, init, list)".to_string());
        };

        let fn_ptr = match fn_ptr {
            TypedValue::Fn(p, _) => p,
            _ => return Err("fold: first argument must be a function".to_string()),
        };
        let list_ptr = match list_val {
            TypedValue::List(p) => p,
            _ => return Err("fold: third argument must be a list".to_string()),
        };
        let init_i64 = match init_val {
            TypedValue::Int(v) => v,
            _ => return Err("fold: init must be an integer".to_string()),
        };

        let list_struct = self.load_list(list_ptr)?;
        let input_len = self.list_len_val(list_struct)?;

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile fold outside function")?;

        let i64 = self.i64_ty();
        let acc_alloca = self.builder.build_alloca(i64, "fold_acc").map_err(llvm_err)?;
        self.builder.build_store(acc_alloca, init_i64).map_err(llvm_err)?;

        let i_alloca = self.builder.build_alloca(i64, "fold_i").map_err(llvm_err)?;
        let zero = i64.const_int(0, false);
        self.builder.build_store(i_alloca, zero).map_err(llvm_err)?;

        let loop_header = self.context.append_basic_block(current_fn, "fold_header");
        let loop_body = self.context.append_basic_block(current_fn, "fold_body");
        let loop_exit = self.context.append_basic_block(current_fn, "fold_exit");

        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_header);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_val, input_len, "fold_cond")
            .map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        self.builder.position_at_end(loop_body);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), i_val.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        // Extract tag to pass to fold lambda
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "elem_tag")
            .map_err(llvm_err)?;
        let acc = self.builder.build_load(i64, acc_alloca, "acc").map_err(llvm_err)?;

        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into(), i64.into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, fn_ptr, &[acc.into(), elem_tag.into()], "fold_call")
            .map_err(llvm_err)?;
        let new_acc_bv = call_result.try_as_basic_value().basic().ok_or("fold call failed")?;
        let new_acc = if new_acc_bv.is_struct_value() {
            self.builder.build_extract_value(new_acc_bv.into_struct_value(), 0, "fold_val")
                .map_err(llvm_err)?.into_int_value()
        } else {
            new_acc_bv.into_int_value()
        };
        self.builder.build_store(acc_alloca, new_acc).map_err(llvm_err)?;

        let one = i64.const_int(1, false);
        let next = self.builder.build_int_add(i_val, one, "i_next").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, next).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_header);

        self.builder.position_at_end(loop_exit);
        let final_acc = self.builder.build_load(i64, acc_alloca, "final_acc").map_err(llvm_err)?;
        Ok(TypedValue::Int(final_acc.into_int_value()))
    }

    /// flat_map(fn, list) = flatten(map(fn, list))
    pub(super) fn builtin_flat_map_list(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let mapped = self.builtin_map(args, trailing)?;
        match mapped {
            TypedValue::List(lp) => {
                let lv = self.load_list(lp)?;
                let cc = self.call_rt("atomic_list_flatten", &[lv.into()])?;
                let result = cc.try_as_basic_value().basic().ok_or("flatten failed")?;
                let alloca = self.builder.build_alloca(self.list_type, "flat_map").map_err(llvm_err)?;
                self.builder.build_store(alloca, result).map_err(llvm_err)?;
                Ok(TypedValue::List(alloca))
            }
            _ => Err("flat_map: map result must be a list".to_string()),
        }
    }

    pub(super) fn builtin_callback_list(&mut self, name: &str, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        match name {
            "any" => self.builtin_any(args, trailing),
            "all" => self.builtin_all(args, trailing),
            "find" => self.builtin_find(args, trailing),
            "find_index" => self.builtin_find_index(args, trailing),
            "reduce" => self.builtin_reduce(args, trailing),
            "fold_right" => self.builtin_fold_right(args, trailing),
            "take_while" => self.builtin_take_while(args, trailing),
            "drop_while" => self.builtin_drop_while(args, trailing),
            "sorted_by" => self.builtin_sorted_by(args, trailing),
            "partition" => self.builtin_partition(args, trailing),
            "count" => self.builtin_count(args, trailing),
            _ => Err(format!("Unknown callback list builtin: {}", name)),
        }
    }

    /// any(list, fn) or any(list) { lambda } -> Bool
    pub(super) fn builtin_any(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "any")?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let result_a = self.builder.build_alloca(self.bool_ty(), "any_res").map_err(llvm_err)?;
        self.builder.build_store(result_a, self.bool_ty().const_zero()).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "any_hdr");
        let bdy = self.context.append_basic_block(current_fn, "any_bdy");
        let ext = self.context.append_basic_block(current_fn, "any_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "any_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        // Accumulate: result = result OR is_true
        let cur = self.builder.build_load(self.bool_ty(), result_a, "cur").map_err(llvm_err)?.into_int_value();
        let new_res = self.builder.build_or(cur, is_true, "new_res").map_err(llvm_err)?;
        self.builder.build_store(result_a, new_res).map_err(llvm_err)?;
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        let res = self.builder.build_load(self.bool_ty(), result_a, "res").map_err(llvm_err)?;
        Ok(TypedValue::Bool(res.into_int_value()))
    }

    /// all(list, fn) or all(list) { lambda } -> Bool
    pub(super) fn builtin_all(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "all")?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let result_a = self.builder.build_alloca(self.bool_ty(), "all_res").map_err(llvm_err)?;
        self.builder.build_store(result_a, self.bool_ty().const_int(1, false)).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "all_hdr");
        let bdy = self.context.append_basic_block(current_fn, "all_bdy");
        let ext = self.context.append_basic_block(current_fn, "all_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "all_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        // Accumulate: result = result AND is_true
        let cur = self.builder.build_load(self.bool_ty(), result_a, "cur").map_err(llvm_err)?.into_int_value();
        let new_res = self.builder.build_and(cur, is_true, "new_res").map_err(llvm_err)?;
        self.builder.build_store(result_a, new_res).map_err(llvm_err)?;
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        let res = self.builder.build_load(self.bool_ty(), result_a, "res").map_err(llvm_err)?;
        Ok(TypedValue::Bool(res.into_int_value()))
    }

    /// find(list, fn) or find(list) { lambda } -> Option<T>
    pub(super) fn builtin_find(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "find")?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        // Allocate fat struct slot for found element
        let found_a = self.builder.build_alloca(self.string_type, "found").map_err(llvm_err)?;
        let found_flag_a = self.builder.build_alloca(self.bool_ty(), "found_f").map_err(llvm_err)?;
        self.builder.build_store(found_flag_a, self.bool_ty().const_zero()).map_err(llvm_err)?;
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "find_hdr");
        let bdy = self.context.append_basic_block(current_fn, "find_bdy");
        let found_bb = self.context.append_basic_block(current_fn, "find_found");
        let ext = self.context.append_basic_block(current_fn, "find_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "find_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_true, found_bb, hdr);
        self.builder.position_at_end(found_bb);
        self.builder.build_store(found_a, elem_val).map_err(llvm_err)?;
        self.builder.build_store(found_flag_a, self.bool_ty().const_int(1, false)).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(ext);
        self.builder.position_at_end(ext);
        // Build Option enum: Some(found) or None
        self.build_option_from_fat_struct(found_a, found_flag_a, InnerType::Int)
    }

    /// find_index(list, fn) or find_index(list) { lambda } -> Option<Int>
    pub(super) fn builtin_find_index(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "find_index")?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let result_a = self.builder.build_alloca(i64, "fi_idx").map_err(llvm_err)?;
        self.builder.build_store(result_a, i64.const_int((-1i64) as u64, true)).map_err(llvm_err)?;
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "fi_hdr");
        let bdy = self.context.append_basic_block(current_fn, "fi_bdy");
        let ext = self.context.append_basic_block(current_fn, "fi_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "fi_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        self.builder.build_store(result_a, iv).map_err(llvm_err)?;
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let fi_hdr2 = self.context.append_basic_block(current_fn, "fi_chk");
        let _ = self.builder.build_conditional_branch(is_true, ext, fi_hdr2);
        self.builder.position_at_end(fi_hdr2);
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        let found_idx = self.builder.build_load(i64, result_a, "found_idx").map_err(llvm_err)?.into_int_value();
        let is_found = self.builder.build_int_compare(IntPredicate::SGE, found_idx, i64.const_int(0, false), "is_found").map_err(llvm_err)?;
        // Build Option<Int>: Some(idx) or None
        self.build_option_int(found_idx, is_found)
    }

    /// reduce(list, fn) or reduce(list) { lambda } -> Option<T>
    pub(super) fn builtin_reduce(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "reduce")?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let zero = i64.const_int(0, false);
        let one = i64.const_int(1, false);
        let is_empty = self.builder.build_int_compare(IntPredicate::EQ, input_len, zero, "is_empty").map_err(llvm_err)?;
        // Accumulator: fat {i64,ptr}
        let acc_a = self.builder.build_alloca(self.string_type, "reduce_acc").map_err(llvm_err)?;
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, one).map_err(llvm_err)?;
        // Init: load first element into acc
        let init_bb = self.context.append_basic_block(current_fn, "reduce_init");
        let loop_hdr = self.context.append_basic_block(current_fn, "reduce_hdr");
        let loop_bdy = self.context.append_basic_block(current_fn, "reduce_bdy");
        let loop_ext = self.context.append_basic_block(current_fn, "reduce_ext");
        let empty_bb = self.context.append_basic_block(current_fn, "reduce_empty");
        let merge_bb = self.context.append_basic_block(current_fn, "reduce_merge");
        let _ = self.builder.build_conditional_branch(is_empty, empty_bb, init_bb);
        // Init: load first element
        self.builder.position_at_end(init_bb);
        let input_list0 = self.load_list(list_ptr)?;
        let first = self.call_rt("atomic_list_get", &[input_list0.into(), zero.into()])?;
        let first_val = first.try_as_basic_value().basic().ok_or("list_get failed")?;
        self.builder.build_store(acc_a, first_val).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_hdr);
        // Loop
        self.builder.position_at_end(loop_hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_bdy, loop_ext);
        self.builder.position_at_end(loop_bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let acc_fat = self.builder.build_load(self.string_type, acc_a, "acc").map_err(llvm_err)?;
        let acc_tag = self.builder.build_extract_value(acc_fat.into_struct_value(), 0, "acc_tag").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into(), i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[acc_tag.into(), elem_tag.into()], "reduce_call").map_err(llvm_err)?;
        let new_acc = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        self.builder.build_store(acc_a, new_acc).map_err(llvm_err)?;
        let ni = self.builder.build_int_add(iv, one, "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_hdr);
        self.builder.position_at_end(loop_ext);
        let final_acc = self.builder.build_load(self.string_type, acc_a, "final_acc").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // Empty: build None
        self.builder.position_at_end(empty_bb);
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // Merge: build Option from fat struct or None
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(self.string_type, "reduce_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&final_acc, loop_ext), (&self.string_type.get_undef(), empty_bb)]);
        let phi_val = phi.as_basic_value();
        let found_flag_a = self.builder.build_alloca(self.bool_ty(), "red_found").map_err(llvm_err)?;
        let phi_flag = self.builder.build_phi(self.bool_ty(), "red_flag").map_err(llvm_err)?;
        phi_flag.add_incoming(&[(&self.bool_ty().const_int(1, false), loop_ext), (&self.bool_ty().const_zero(), empty_bb)]);
        self.builder.build_store(found_flag_a, phi_flag.as_basic_value()).map_err(llvm_err)?;
        let acc_alloca = self.builder.build_alloca(self.string_type, "red_acc_s").map_err(llvm_err)?;
        self.builder.build_store(acc_alloca, phi_val).map_err(llvm_err)?;
        self.build_option_from_fat_struct(acc_alloca, found_flag_a, InnerType::Int)
    }

    /// fold_right(list, init, fn) or fold_right(list, init) { lambda } -> T
    pub(super) fn builtin_fold_right(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr, init_val) = self.extract_fold_right_args(args, trailing)?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let zero = i64.const_int(0, false);
        let one = i64.const_int(1, false);
        let acc_a = self.builder.build_alloca(i64, "fr_acc").map_err(llvm_err)?;
        self.builder.build_store(acc_a, init_val).map_err(llvm_err)?;
        // Iterate backwards: i = len-1 down to 0
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        let start_i = self.builder.build_int_sub(input_len, one, "start_i").map_err(llvm_err)?;
        self.builder.build_store(i_a, start_i).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "fr_hdr");
        let bdy = self.context.append_basic_block(current_fn, "fr_bdy");
        let ext = self.context.append_basic_block(current_fn, "fr_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SGE, iv, zero, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let acc = self.builder.build_load(i64, acc_a, "acc").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into(), i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into(), acc.into()], "fr_call").map_err(llvm_err)?;
        let new_acc_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let new_acc = if new_acc_bv.is_struct_value() {
            self.builder.build_extract_value(new_acc_bv.into_struct_value(), 0, "fr_val").map_err(llvm_err)?.into_int_value()
        } else { new_acc_bv.into_int_value() };
        self.builder.build_store(acc_a, new_acc).map_err(llvm_err)?;
        let ni = self.builder.build_int_sub(iv, one, "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        let final_acc = self.builder.build_load(i64, acc_a, "final_acc").map_err(llvm_err)?.into_int_value();
        Ok(TypedValue::Int(final_acc))
    }

    /// take_while(list, fn) or take_while(list) { lambda } -> List<T>
    pub(super) fn builtin_take_while(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "take_while")?;
        let list_struct = self.load_list(list_ptr)?;
        let input_len = self.list_len_val(list_struct)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        // Create result list
        let cc = self.call_rt("atomic_list_create", &[input_len.into()])?;
        let res_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let res_a = self.builder.build_alloca(self.list_type, "tw_res").map_err(llvm_err)?;
        self.builder.build_store(res_a, res_bv).map_err(llvm_err)?;
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "tw_hdr");
        let bdy = self.context.append_basic_block(current_fn, "tw_bdy");
        let ext = self.context.append_basic_block(current_fn, "tw_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "tw_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        let push_bb = self.context.append_basic_block(current_fn, "tw_push");
        let _ = self.builder.build_conditional_branch(is_true, push_bb, ext);
        self.builder.position_at_end(push_bb);
        let rl = self.builder.build_load(self.list_type, res_a, "rl").map_err(llvm_err)?.into_struct_value();
        let rp = self.call_rt("atomic_list_push", &[rl.into(), elem_val.into()])?;
        self.builder.build_store(res_a, rp.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        Ok(TypedValue::List(res_a))
    }

    /// drop_while(list, fn) or drop_while(list) { lambda } -> List<T>
    pub(super) fn builtin_drop_while(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "drop_while")?;
        let list_struct = self.load_list(list_ptr)?;
        let input_len = self.list_len_val(list_struct)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let cc = self.call_rt("atomic_list_create", &[input_len.into()])?;
        let res_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let res_a = self.builder.build_alloca(self.list_type, "dw_res").map_err(llvm_err)?;
        self.builder.build_store(res_a, res_bv).map_err(llvm_err)?;
        let dropping_a = self.builder.build_alloca(self.bool_ty(), "dropping").map_err(llvm_err)?;
        self.builder.build_store(dropping_a, self.bool_ty().const_int(1, false)).map_err(llvm_err)?;
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "dw_hdr");
        let bdy = self.context.append_basic_block(current_fn, "dw_bdy");
        let ext = self.context.append_basic_block(current_fn, "dw_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let dropping = self.builder.build_load(self.bool_ty(), dropping_a, "dropping").map_err(llvm_err)?.into_int_value();
        let is_dropping = self.builder.build_int_compare(IntPredicate::NE, dropping, self.bool_ty().const_zero(), "is_dropping").map_err(llvm_err)?;
        // Only call predicate if still dropping
        let call_bb = self.context.append_basic_block(current_fn, "dw_call");
        let push_bb = self.context.append_basic_block(current_fn, "dw_push");
        let inc_bb = self.context.append_basic_block(current_fn, "dw_inc");
        let _ = self.builder.build_conditional_branch(is_dropping, call_bb, push_bb);
        // Call predicate
        self.builder.position_at_end(call_bb);
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "dw_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        // If true, still dropping, skip element (go to inc). If false, stop dropping, push element.
        let _ = self.builder.build_conditional_branch(is_true, inc_bb, push_bb);
        // Push element
        self.builder.position_at_end(push_bb);
        self.builder.build_store(dropping_a, self.bool_ty().const_zero()).map_err(llvm_err)?;
        let rl = self.builder.build_load(self.list_type, res_a, "rl").map_err(llvm_err)?.into_struct_value();
        let rp = self.call_rt("atomic_list_push", &[rl.into(), elem_val.into()])?;
        self.builder.build_store(res_a, rp.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(inc_bb);
        // Increment
        self.builder.position_at_end(inc_bb);
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        Ok(TypedValue::List(res_a))
    }

    /// sorted_by(list, fn) or sorted_by(list) { lambda } -> List<T>
    /// Uses insertion sort since we can't easily do merge sort with callbacks in LLVM IR
    pub(super) fn builtin_sorted_by(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "sorted_by")?;
        let list_struct = self.load_list(list_ptr)?;
        let input_len = self.list_len_val(list_struct)?;
        let _input_data = self.list_data_ptr(list_struct)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let zero = i64.const_int(0, false);
        let one = i64.const_int(1, false);
        // Create result list (copy of input)
        let cc = self.call_rt("atomic_list_create", &[input_len.into()])?;
        let res_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let res_a = self.builder.build_alloca(self.list_type, "sb_res").map_err(llvm_err)?;
        self.builder.build_store(res_a, res_bv).map_err(llvm_err)?;
        // Copy all elements to result
        let i_copy_a = self.builder.build_alloca(i64, "i_copy").map_err(llvm_err)?;
        self.builder.build_store(i_copy_a, zero).map_err(llvm_err)?;
        let copy_hdr = self.context.append_basic_block(current_fn, "sb_copy_hdr");
        let copy_bdy = self.context.append_basic_block(current_fn, "sb_copy_bdy");
        let copy_ext = self.context.append_basic_block(current_fn, "sb_copy_ext");
        let _ = self.builder.build_unconditional_branch(copy_hdr);
        self.builder.position_at_end(copy_hdr);
        let ic = self.builder.build_load(i64, i_copy_a, "ic").map_err(llvm_err)?.into_int_value();
        let cc_cond = self.builder.build_int_compare(IntPredicate::SLT, ic, input_len, "c_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cc_cond, copy_bdy, copy_ext);
        self.builder.position_at_end(copy_bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), ic.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let rl = self.builder.build_load(self.list_type, res_a, "rl").map_err(llvm_err)?.into_struct_value();
        let rp = self.call_rt("atomic_list_push", &[rl.into(), elem_val.into()])?;
        self.builder.build_store(res_a, rp.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let nic = self.builder.build_int_add(ic, one, "nic").map_err(llvm_err)?;
        self.builder.build_store(i_copy_a, nic).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(copy_hdr);
        self.builder.position_at_end(copy_ext);
        // Insertion sort: for i=1..len, for j=i..>0, compare res[j-1] > res[j], swap if so
        let i_a = self.builder.build_alloca(i64, "sb_i").map_err(llvm_err)?;
        self.builder.build_store(i_a, one).map_err(llvm_err)?;
        let outer_hdr = self.context.append_basic_block(current_fn, "sb_outer_hdr");
        let outer_bdy = self.context.append_basic_block(current_fn, "sb_outer_bdy");
        let outer_ext = self.context.append_basic_block(current_fn, "sb_outer_ext");
        let _ = self.builder.build_unconditional_branch(outer_hdr);
        self.builder.position_at_end(outer_hdr);
        let iv_o = self.builder.build_load(i64, i_a, "iv_o").map_err(llvm_err)?.into_int_value();
        let o_cond = self.builder.build_int_compare(IntPredicate::SLT, iv_o, input_len, "o_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(o_cond, outer_bdy, outer_ext);
        self.builder.position_at_end(outer_bdy);
        let j_a = self.builder.build_alloca(i64, "sb_j").map_err(llvm_err)?;
        self.builder.build_store(j_a, iv_o).map_err(llvm_err)?;
        let inner_hdr = self.context.append_basic_block(current_fn, "sb_inner_hdr");
        let inner_bdy = self.context.append_basic_block(current_fn, "sb_inner_bdy");
        let inner_ext = self.context.append_basic_block(current_fn, "sb_inner_ext");
        let _ = self.builder.build_unconditional_branch(inner_hdr);
        self.builder.position_at_end(inner_hdr);
        let jv = self.builder.build_load(i64, j_a, "jv").map_err(llvm_err)?.into_int_value();
        let j_cond = self.builder.build_int_compare(IntPredicate::SGT, jv, zero, "j_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(j_cond, inner_bdy, inner_ext);
        self.builder.position_at_end(inner_bdy);
        let jm1 = self.builder.build_int_sub(jv, one, "jm1").map_err(llvm_err)?;
        let res_list_jm1 = self.load_list(res_a)?;
        let elem_jm1 = self.call_rt("atomic_list_get", &[res_list_jm1.into(), jm1.into()])?;
        let ev_jm1 = elem_jm1.try_as_basic_value().basic().ok_or("list_get failed")?;
        let tag_jm1 = self.builder.build_extract_value(ev_jm1.into_struct_value(), 0, "t_jm1").map_err(llvm_err)?.into_int_value();
        let res_list_j = self.load_list(res_a)?;
        let elem_j = self.call_rt("atomic_list_get", &[res_list_j.into(), jv.into()])?;
        let ev_j = elem_j.try_as_basic_value().basic().ok_or("list_get failed")?;
        let tag_j = self.builder.build_extract_value(ev_j.into_struct_value(), 0, "t_j").map_err(llvm_err)?.into_int_value();
        // Call comparator: fn(a, b) -> Bool, returns true if a > b (need swap)
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into(), i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[tag_jm1.into(), tag_j.into()], "sb_cmp").map_err(llvm_err)?;
        let cmp_bv = call_r.try_as_basic_value().basic().ok_or("cmp failed")?;
        let cmp = if cmp_bv.is_struct_value() {
            self.builder.build_extract_value(cmp_bv.into_struct_value(), 0, "cmp").map_err(llvm_err)?.into_int_value()
        } else { cmp_bv.into_int_value() };
        let should_swap = self.builder.build_int_compare(IntPredicate::NE, cmp, zero, "should_swap").map_err(llvm_err)?;
        let swap_bb = self.context.append_basic_block(current_fn, "sb_swap");
        let no_swap_bb = self.context.append_basic_block(current_fn, "sb_noswap");
        let _ = self.builder.build_conditional_branch(should_swap, swap_bb, no_swap_bb);
        // Swap: use atomic_list_set
        self.builder.position_at_end(swap_bb);
        let rl_sw = self.load_list(res_a)?;
        let _set1 = self.call_rt("atomic_list_set", &[rl_sw.into(), jm1.into(), ev_j.into()])?;
        let rl2_sw = self.load_list(res_a)?;
        let set2 = self.call_rt("atomic_list_set", &[rl2_sw.into(), jv.into(), ev_jm1.into()])?;
        let set_bv = set2.try_as_basic_value().basic().ok_or("list_set failed")?;
        self.builder.build_store(res_a, set_bv).map_err(llvm_err)?;
        self.builder.build_store(j_a, jm1).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(inner_hdr);
        self.builder.position_at_end(no_swap_bb);
        let _ = self.builder.build_unconditional_branch(inner_ext);
        self.builder.position_at_end(inner_ext);
        let ni_o = self.builder.build_int_add(iv_o, one, "ni_o").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni_o).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(outer_hdr);
        self.builder.position_at_end(outer_ext);
        Ok(TypedValue::List(res_a))
    }

    /// partition(list, fn) or partition(list) { lambda } -> (List<T>, List<T>)
    pub(super) fn builtin_partition(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "partition")?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        // Create two result lists
        let cap = i64.const_int(4, false);
        let left_cc = self.call_rt("atomic_list_create", &[cap.into()])?;
        let left_bv = left_cc.try_as_basic_value().basic().ok_or("list_create left")?;
        let left_a = self.builder.build_alloca(self.list_type, "part_left").map_err(llvm_err)?;
        self.builder.build_store(left_a, left_bv).map_err(llvm_err)?;
        let right_cc = self.call_rt("atomic_list_create", &[cap.into()])?;
        let right_bv = right_cc.try_as_basic_value().basic().ok_or("list_create right")?;
        let right_a = self.builder.build_alloca(self.list_type, "part_right").map_err(llvm_err)?;
        self.builder.build_store(right_a, right_bv).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "part_hdr");
        let bdy = self.context.append_basic_block(current_fn, "part_bdy");
        let ext = self.context.append_basic_block(current_fn, "part_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "part_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        let left_bb = self.context.append_basic_block(current_fn, "part_left");
        let right_bb = self.context.append_basic_block(current_fn, "part_right");
        let part_merge = self.context.append_basic_block(current_fn, "part_merge2");
        let _ = self.builder.build_conditional_branch(is_true, left_bb, right_bb);
        // Push to left
        self.builder.position_at_end(left_bb);
        let ll = self.load_list(left_a)?;
        let lp = self.call_rt("atomic_list_push", &[ll.into(), elem_val.into()])?;
        let lp_bv = lp.try_as_basic_value().basic().ok_or("push left")?;
        self.builder.build_store(left_a, lp_bv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(part_merge);
        // Push to right
        self.builder.position_at_end(right_bb);
        let rl = self.load_list(right_a)?;
        let rp = self.call_rt("atomic_list_push", &[rl.into(), elem_val.into()])?;
        let rp_bv = rp.try_as_basic_value().basic().ok_or("push right")?;
        self.builder.build_store(right_a, rp_bv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(part_merge);
        self.builder.position_at_end(part_merge);
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        // Build tuple struct: {list_type, list_type}
        let lv = self.builder.build_load(self.list_type, left_a, "lv").map_err(llvm_err)?;
        let rv = self.builder.build_load(self.list_type, right_a, "rv").map_err(llvm_err)?;
        let tuple_ty = self.context.struct_type(&[self.list_type.into(), self.list_type.into()], false);
        let undef = tuple_ty.get_undef();
        let t1 = self.builder.build_insert_value(undef, lv, 0, "t_l").map_err(llvm_err)?;
        let t2 = self.builder.build_insert_value(t1, rv, 1, "t_r").map_err(llvm_err)?;
        let alloca = self.builder.build_alloca(tuple_ty, "part_tuple").map_err(llvm_err)?;
        self.builder.build_store(alloca, t2).map_err(llvm_err)?;
        Ok(TypedValue::Struct(alloca, tuple_ty))
    }

    /// count(list, fn) or count(list) { lambda } -> Int
    pub(super) fn builtin_count(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let (fn_ptr, list_ptr) = self.extract_callback_args(args, trailing, 1, "count")?;
        let input_len = self.list_len_val(self.load_list(list_ptr)?)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let i64 = self.i64_ty();
        let i_a = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let cnt_a = self.builder.build_alloca(i64, "cnt").map_err(llvm_err)?;
        self.builder.build_store(cnt_a, i64.const_int(0, false)).map_err(llvm_err)?;
        let hdr = self.context.append_basic_block(current_fn, "cnt_hdr");
        let bdy = self.context.append_basic_block(current_fn, "cnt_bdy");
        let ext = self.context.append_basic_block(current_fn, "cnt_ext");
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(hdr);
        let iv = self.builder.build_load(i64, i_a, "iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, input_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, bdy, ext);
        self.builder.position_at_end(bdy);
        let input_list = self.load_list(list_ptr)?;
        let elem = self.call_rt("atomic_list_get", &[input_list.into(), iv.into()])?;
        let elem_val = elem.try_as_basic_value().basic().ok_or("list_get failed")?;
        let elem_tag = self.builder.build_extract_value(elem_val.into_struct_value(), 0, "et").map_err(llvm_err)?.into_int_value();
        let fat_ret_ty = self.string_type;
        let fn_type = fat_ret_ty.fn_type(&[i64.into()], false);
        let call_r = self.builder.build_indirect_call(fn_type, fn_ptr, &[elem_tag.into()], "cnt_call").map_err(llvm_err)?;
        let pred_bv = call_r.try_as_basic_value().basic().ok_or("call failed")?;
        let pred = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let is_true = self.builder.build_int_compare(IntPredicate::NE, pred, i64.const_int(0, false), "is_true").map_err(llvm_err)?;
        let one_or_zero = self.builder.build_int_z_extend(is_true, i64, "one_or_zero").map_err(llvm_err)?;
        let cur = self.builder.build_load(i64, cnt_a, "cur").map_err(llvm_err)?.into_int_value();
        let inc = self.builder.build_int_add(cur, one_or_zero, "inc").map_err(llvm_err)?;
        self.builder.build_store(cnt_a, inc).map_err(llvm_err)?;
        let ni = self.builder.build_int_add(iv, i64.const_int(1, false), "ni").map_err(llvm_err)?;
        self.builder.build_store(i_a, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(hdr);
        self.builder.position_at_end(ext);
        let result = self.builder.build_load(i64, cnt_a, "result").map_err(llvm_err)?;
        Ok(TypedValue::Int(result.into_int_value()))
    }

    /// Helper: extract (fn_ptr, list_ptr) from args for callback-based list functions
    pub(super) fn extract_callback_args(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>, expected_args: usize, name: &str) -> Result<(PointerValue<'ctx>, PointerValue<'ctx>), String> {
        let (fn_expr, list_expr) = if let Some(lam) = trailing {
            if args.len() != expected_args {
                return Err(format!("{} with trailing lambda expects {} argument(s) (list)", name, expected_args));
            }
            let lv = self.compile_expr(&args[0])?;
            let fv = self.compile_expr(lam)?;
            (fv, lv)
        } else if args.len() == expected_args + 1 {
            let fv = self.compile_expr(&args[0])?;
            let lv = self.compile_expr(&args[expected_args])?;
            (fv, lv)
        } else {
            return Err(format!("{} expects {} argument(s) (fn, list)", name, expected_args + 1));
        };
        let fn_ptr = match fn_expr {
            TypedValue::Fn(p, _) => p,
            _ => return Err(format!("{}: first argument must be a function", name)),
        };
        let list_ptr = match list_expr {
            TypedValue::List(p) => p,
            _ => return Err(format!("{}: last argument must be a list", name)),
        };
        Ok((fn_ptr, list_ptr))
    }

    /// Helper: extract (fn_ptr, list_ptr, init_i64) for fold_right
    pub(super) fn extract_fold_right_args(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<(PointerValue<'ctx>, PointerValue<'ctx>, IntValue<'ctx>), String> {
        let (fn_expr, list_expr, init_expr) = if let Some(lam) = trailing {
            if args.len() != 2 {
                return Err("fold_right with trailing lambda expects 2 arguments (init, list)".to_string());
            }
            let iv = self.compile_expr(&args[0])?;
            let lv = self.compile_expr(&args[1])?;
            let fv = self.compile_expr(lam)?;
            (fv, lv, iv)
        } else if args.len() == 3 {
            let fv = self.compile_expr(&args[0])?;
            let iv = self.compile_expr(&args[1])?;
            let lv = self.compile_expr(&args[2])?;
            (fv, lv, iv)
        } else {
            return Err("fold_right expects 3 arguments (fn, init, list)".to_string());
        };
        let fn_ptr = match fn_expr {
            TypedValue::Fn(p, _) => p,
            _ => return Err("fold_right: first argument must be a function".to_string()),
        };
        let list_ptr = match list_expr {
            TypedValue::List(p) => p,
            _ => return Err("fold_right: last argument must be a list".to_string()),
        };
        let init_val = match init_expr {
            TypedValue::Int(v) => v,
            _ => return Err("fold_right: init must be an integer".to_string()),
        };
        Ok((fn_ptr, list_ptr, init_val))
    }
}
