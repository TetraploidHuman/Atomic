use inkwell::types::BasicTypeEnum;
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, InnerType, llvm_err};
use atomic::ast::Expr;

impl<'ctx> CodeGen<'ctx> {
    /// lazy_list(seed) - create a lazy list with a seed value
    /// lazy_list(seed) { fn } - create a lazy list with seed and step function
    pub(super) fn builtin_lazy_list(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        if args.is_empty() {
            return Err("lazy_list expects at least 1 argument (seed)".to_string());
        }
        let seed = self.compile_expr(&args[0])?;
        let seed_i64 = match &seed {
            TypedValue::Int(v) => *v,
            _ => return Err("lazy_list: seed must be an Int".to_string()),
        };

        // Compile step function if provided
        let (step_fn_ptr, state, take_count) = if let Some(lam) = trailing {
            let step_fn_val = self.compile_lambda_for_lazy(lam)?;
            // -1 means "infinite" — only bounded by explicit take()
            (step_fn_val, seed_i64, self.i64_ty().const_int(-1_i64 as u64, true))
        } else {
            // No step function: only the seed element
            (self.ptr_ty().const_null(), self.i64_ty().const_int(0, false), self.i64_ty().const_int(0, false))
        };

        // Build LazyList struct: {head_val: i64, step_fn: i8*, state: i64, take_count: i64, map_fn: i8*}
        let ll_alloca = self.builder.build_alloca(self.lazylist_type, "ll").map_err(llvm_err)?;
        let undef = self.lazylist_type.get_undef();
        let v0 = self.builder.build_insert_value(undef, seed_i64, 0, "ll_head").map_err(llvm_err)?;
        let v1 = self.builder.build_insert_value(v0, step_fn_ptr, 1, "ll_fn").map_err(llvm_err)?;
        let v2 = self.builder.build_insert_value(v1, state, 2, "ll_state").map_err(llvm_err)?;
        let v3 = self.builder.build_insert_value(v2, take_count, 3, "ll_tc").map_err(llvm_err)?;
        let v4 = self.builder.build_insert_value(v3, self.ptr_ty().const_null(), 4, "ll_map").map_err(llvm_err)?;
        let v5 = self.builder.build_insert_value(v4, self.ptr_ty().const_null(), 5, "ll_filt").map_err(llvm_err)?;
        self.builder.build_store(ll_alloca, v5).map_err(llvm_err)?;
        Ok(TypedValue::LazyList(ll_alloca))
    }

    /// Compile a lambda for use as a lazy list step function.
    /// Returns a function pointer that can be called with (i64 state) -> next_i64.
    fn compile_lambda_for_lazy(&mut self, lam: &Expr) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        match lam {
            Expr::Lambda { params, body, .. } => {
                if params.is_empty() {
                    return Err("lazy_list step function expects 1 parameter".to_string());
                }
                let fn_val = self.compile_lambda(params, body)?;
                match fn_val {
                    TypedValue::Fn(ptr, _) => Ok(ptr),
                    _ => Err("lazy_list: step function compilation failed".to_string()),
                }
            }
            _ => Err("lazy_list: expected lambda body".to_string()),
        }
    }

    // ---- LazyList operations ----

    /// If the value is a LazyList, convert it to a List and return the list alloca pointer.
    /// If it's already a List, return the pointer directly.
    fn ensure_list_ptr(&self, val: &TypedValue<'ctx>, prefix: &str) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        match val {
            TypedValue::LazyList(_) => {
                let list_sv = self.convert_lazylist_to_list(val)?;
                let alloca = self.builder.build_alloca(self.list_type, &format!("{}_list", prefix)).map_err(llvm_err)?;
                self.builder.build_store(alloca, list_sv).map_err(llvm_err)?;
                Ok(alloca)
            }
            TypedValue::List(p) => Ok(*p),
            _ => Err(format!("{}: argument must be a List or LazyList", prefix)),
        }
    }

    /// Convert a LazyList to a List struct value (i.e., the loaded StructValue of the list).
    /// This forces evaluation: iterates the step function take_count times.
    pub(crate) fn convert_lazylist_to_list(&self, ll_val: &TypedValue<'ctx>) -> Result<inkwell::values::StructValue<'ctx>, String> {
        let ll_ptr = match ll_val {
            TypedValue::LazyList(p) => *p,
            _ => return Err("convert_lazylist_to_list: expected LazyList".to_string()),
        };
        let ll_sv = self.builder.build_load(self.lazylist_type, ll_ptr, "ll_conv").map_err(llvm_err)?;
        let ll_struct = ll_sv.into_struct_value();
        let head_val = self.builder.build_extract_value(ll_struct, 0, "ll_head").map_err(llvm_err)?.into_int_value();
        let step_fn = self.builder.build_extract_value(ll_struct, 1, "ll_fn").map_err(llvm_err)?.into_pointer_value();
        let state_val = self.builder.build_extract_value(ll_struct, 2, "ll_state").map_err(llvm_err)?.into_int_value();
        let take_count_val = self.builder.build_extract_value(ll_struct, 3, "ll_tc").map_err(llvm_err)?.into_int_value();
        let map_fn = self.builder.build_extract_value(ll_struct, 4, "ll_map").map_err(llvm_err)?.into_pointer_value();
        let filter_fn = self.builder.build_extract_value(ll_struct, 5, "ll_filt").map_err(llvm_err)?.into_pointer_value();

        let zero = self.i64_ty().const_int(0, false);
        let one = self.i64_ty().const_int(1, false);
        let neg_one = self.i64_ty().const_int((-1_i64) as u64, true);

        let has_step = self.builder.build_int_compare(IntPredicate::NE, step_fn, self.ptr_ty().const_null(), "has_step").map_err(llvm_err)?;
        let state_nz = self.builder.build_int_compare(IntPredicate::NE, state_val, zero, "state_nz").map_err(llvm_err)?;
        // list-backed: no step fn but state holds a valid data pointer (from to_lazy_list)
        let not_has_step = self.builder.build_not(has_step, "not_has_step").map_err(llvm_err)?;
        let is_list_backed = self.builder.build_and(
            not_has_step,
            state_nz, "is_list_backed").map_err(llvm_err)?;

        let tc_gt_zero = self.builder.build_int_compare(IntPredicate::SGT, take_count_val, zero, "tc_gt0").map_err(llvm_err)?;
        let tc_is_neg1 = self.builder.build_int_compare(IntPredicate::EQ, take_count_val, neg_one, "tc_neg1").map_err(llvm_err)?;
        let tc_or_inf = self.builder.build_or(tc_gt_zero, tc_is_neg1, "tc_or_inf").map_err(llvm_err)?;
        let should_generate = self.builder.build_and(has_step, tc_or_inf, "should_gen").map_err(llvm_err)?;

        // Compute final_count:
        //   list-backed: use take_count (at least 1 for the already-pushed head)
        //   has_step:    use max(1, take_count)
        //   head-only:   1
        let total_elems = self.builder.build_select(tc_is_neg1, one, take_count_val, "total_raw").map_err(llvm_err)?.into_int_value();
        let step_count = self.builder.build_select(has_step, total_elems, one, "step_count").map_err(llvm_err)?.into_int_value();
        // For list-backed, take_count is already the right count (>=0); ensure at least 1 for head
        let lb_count = self.builder.build_select(tc_gt_zero, take_count_val, one, "lb_count").map_err(llvm_err)?.into_int_value();
        let final_count = self.builder.build_select(is_list_backed, lb_count, step_count, "final_count").map_err(llvm_err)?.into_int_value();

        // Create result list
        let cc = self.call_rt("atomic_list_create", &[final_count.into()])?;
        let list_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "ll_result").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, list_bv).map_err(llvm_err)?;

        let has_map = self.builder.build_int_compare(IntPredicate::NE, map_fn, self.ptr_ty().const_null(), "has_map").map_err(llvm_err)?;
        let has_filter = self.builder.build_int_compare(IntPredicate::NE, filter_fn, self.ptr_ty().const_null(), "has_filt").map_err(llvm_err)?;
        let map_fn_type = self.string_type.fn_type(&[self.i64_ty().into()], false);
        let filt_fn_type = self.string_type.fn_type(&[self.i64_ty().into()], false);
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;

        // ---- Head push with optional map and filter ----
        // Flow: map_head_bb / no_map_head_bb → head_check_bb
        //       head_check_bb: phi → if has_filter → call_filt_head_bb else → head_push_bb
        //       call_filt_head_bb: call filter → if pass → head_push_bb else → head_skip_bb
        //       head_push_bb: push, i=1 → after_head_bb
        //       head_skip_bb: i=0 → after_head_bb
        //       after_head_bb: check need_more → loop_hdr or loop_exit
        let map_head_bb = self.context.append_basic_block(current_fn, "map_head");
        let no_map_head_bb = self.context.append_basic_block(current_fn, "no_map_head");
        let head_check_bb = self.context.append_basic_block(current_fn, "head_check");
        let call_filt_head_bb = self.context.append_basic_block(current_fn, "call_filt_head");
        let head_push_bb = self.context.append_basic_block(current_fn, "head_push");
        let head_skip_bb = self.context.append_basic_block(current_fn, "head_skip");
        let after_head_bb = self.context.append_basic_block(current_fn, "after_head");
        let _ = self.builder.build_conditional_branch(has_map, map_head_bb, no_map_head_bb);

        // Map head
        self.builder.position_at_end(map_head_bb);
        let mapped_head = self.builder.build_indirect_call(map_fn_type, map_fn, &[head_val.into()], "mh_call").map_err(llvm_err)?;
        let mapped_head_bv = mapped_head.try_as_basic_value().basic().ok_or("map head call failed")?;
        let mapped_head_val = if mapped_head_bv.is_struct_value() {
            self.builder.build_extract_value(mapped_head_bv.into_struct_value(), 0, "mh_val").map_err(llvm_err)?.into_int_value()
        } else { mapped_head_bv.into_int_value() };
        let _ = self.builder.build_unconditional_branch(head_check_bb);

        // No map head
        self.builder.position_at_end(no_map_head_bb);
        let _ = self.builder.build_unconditional_branch(head_check_bb);

        // ---- head_check_bb: phi for head, then branch on has_filter ----
        self.builder.position_at_end(head_check_bb);
        let head_phi = self.builder.build_phi(self.i64_ty(), "head_phi").map_err(llvm_err)?;
        head_phi.add_incoming(&[(&mapped_head_val, map_head_bb), (&head_val, no_map_head_bb)]);
        let candidate_head = head_phi.as_basic_value().into_int_value();
        let _ = self.builder.build_conditional_branch(has_filter, call_filt_head_bb, head_push_bb);

        // ---- call_filt_head_bb: call filter on head ----
        self.builder.position_at_end(call_filt_head_bb);
        let filt_head_call = self.builder.build_indirect_call(filt_fn_type, filter_fn, &[candidate_head.into()], "fh_call").map_err(llvm_err)?;
        let filt_head_bv = filt_head_call.try_as_basic_value().basic().ok_or("filt head call failed")?;
        let filt_head_tag = if filt_head_bv.is_struct_value() {
            self.builder.build_extract_value(filt_head_bv.into_struct_value(), 0, "fh_val").map_err(llvm_err)?.into_int_value()
        } else { filt_head_bv.into_int_value() };
        let head_passes = self.builder.build_int_compare(IntPredicate::NE, filt_head_tag, zero, "head_passes").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(head_passes, head_push_bb, head_skip_bb);

        // ---- head_push_bb: push head, i=1 ----
        self.builder.position_at_end(head_push_bb);
        let head_fat = self.make_int_fat(candidate_head)?;
        let cur_list_h = self.load_list(result_alloca)?;
        let cc_h = self.call_rt("atomic_list_push", &[cur_list_h.into(), head_fat.into()])?;
        let new_list_h = cc_h.try_as_basic_value().basic().ok_or("push head failed")?;
        self.builder.build_store(result_alloca, new_list_h).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(after_head_bb);

        // ---- head_skip_bb: head filtered out, i=0 ----
        self.builder.position_at_end(head_skip_bb);
        let _ = self.builder.build_unconditional_branch(after_head_bb);

        // ---- after_head_bb: init i counter and state, check need_more ----
        self.builder.position_at_end(after_head_bb);
        let i_init_phi = self.builder.build_phi(self.i64_ty(), "i_init").map_err(llvm_err)?;
        i_init_phi.add_incoming(&[(&one, head_push_bb), (&zero, head_skip_bb)]);
        let i_alloca = self.builder.build_alloca(self.i64_ty(), "ll_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i_init_phi.as_basic_value().into_int_value()).map_err(llvm_err)?;
        let state_phi_alloca = self.builder.build_alloca(self.i64_ty(), "ll_state_phi").map_err(llvm_err)?;
        self.builder.build_store(state_phi_alloca, state_val).map_err(llvm_err)?;

        let need_more = self.builder.build_or(should_generate, is_list_backed, "need_more").map_err(llvm_err)?;

        let loop_hdr = self.context.append_basic_block(current_fn, "ll_gen_hdr");
        let loop_body = self.context.append_basic_block(current_fn, "ll_gen_body");
        let loop_exit = self.context.append_basic_block(current_fn, "ll_gen_exit");
        let _ = self.builder.build_conditional_branch(need_more, loop_hdr, loop_exit);

        // ---- loop_hdr: check i < final_count ----
        self.builder.position_at_end(loop_hdr);
        let i_loaded = self.builder.build_load(self.i64_ty(), i_alloca, "ll_i_val").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_loaded, final_count, "ll_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        // ---- loop_body: generate next element ----
        self.builder.position_at_end(loop_body);

        let data_ptr = self.builder.build_int_to_ptr(state_val, self.ptr_ty(), "data_ptr").map_err(llvm_err)?;
        let step_block = self.context.append_basic_block(current_fn, "ll_step_blk");
        let lb_block = self.context.append_basic_block(current_fn, "ll_lb_blk");
        let merge_block = self.context.append_basic_block(current_fn, "ll_merge_blk");
        let _ = self.builder.build_conditional_branch(is_list_backed, lb_block, step_block);

        // Step-function path
        self.builder.position_at_end(step_block);
        let current_state = self.builder.build_load(self.i64_ty(), state_phi_alloca, "ll_cur_state").map_err(llvm_err)?.into_int_value();
        let fn_type = self.fat_return_type.fn_type(&[self.i64_ty().into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, step_fn, &[current_state.into()], "ll_step_call").map_err(llvm_err)?;
        let step_fat = call_result.try_as_basic_value().basic().ok_or("step call returned void")?;
        let step_fat_sv = step_fat.into_struct_value();
        let step_elem = self.builder.build_extract_value(step_fat_sv, 0, "ll_next").map_err(llvm_err)?.into_int_value();
        self.builder.build_store(state_phi_alloca, step_elem).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // List-backed path
        self.builder.position_at_end(lb_block);
        let elem_gep = unsafe { self.builder.build_gep(self.fat_return_type, data_ptr, &[i_loaded], "lb_gep").map_err(llvm_err) }?;
        let elem_fat = self.builder.build_load(self.fat_return_type, elem_gep, "lb_fat").map_err(llvm_err)?.into_struct_value();
        let lb_elem = self.builder.build_extract_value(elem_fat, 0, "lb_elem").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge element
        self.builder.position_at_end(merge_block);
        let phi = self.builder.build_phi(self.i64_ty(), "elem_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&step_elem, step_block), (&lb_elem, lb_block)]);
        let elem_val = phi.as_basic_value().into_int_value();

        // Apply map_fn if present
        let map_elem_bb = self.context.append_basic_block(current_fn, "map_elem");
        let no_map_elem_bb = self.context.append_basic_block(current_fn, "no_map_elem");
        let filt_elem_check_bb = self.context.append_basic_block(current_fn, "filt_elem_check");
        let _ = self.builder.build_conditional_branch(has_map, map_elem_bb, no_map_elem_bb);

        self.builder.position_at_end(map_elem_bb);
        let mapped_elem_call = self.builder.build_indirect_call(map_fn_type, map_fn, &[elem_val.into()], "me_call").map_err(llvm_err)?;
        let mapped_elem_bv = mapped_elem_call.try_as_basic_value().basic().ok_or("map elem call failed")?;
        let mapped_elem_val = if mapped_elem_bv.is_struct_value() {
            self.builder.build_extract_value(mapped_elem_bv.into_struct_value(), 0, "me_val").map_err(llvm_err)?.into_int_value()
        } else { mapped_elem_bv.into_int_value() };
        let _ = self.builder.build_unconditional_branch(filt_elem_check_bb);

        self.builder.position_at_end(no_map_elem_bb);
        let _ = self.builder.build_unconditional_branch(filt_elem_check_bb);

        // ---- filt_elem_check_bb: phi for mapped/unmapped elem, branch on has_filter ----
        self.builder.position_at_end(filt_elem_check_bb);
        let elem_phi_filt = self.builder.build_phi(self.i64_ty(), "elem_phi_filt").map_err(llvm_err)?;
        elem_phi_filt.add_incoming(&[(&mapped_elem_val, map_elem_bb), (&elem_val, no_map_elem_bb)]);
        let candidate_elem = elem_phi_filt.as_basic_value().into_int_value();

        let call_filt_elem_bb = self.context.append_basic_block(current_fn, "call_filt_elem");
        let elem_pass_bb = self.context.append_basic_block(current_fn, "elem_pass");
        let elem_fail_bb = self.context.append_basic_block(current_fn, "elem_fail");
        let _ = self.builder.build_conditional_branch(has_filter, call_filt_elem_bb, elem_pass_bb);

        // ---- call_filt_elem_bb: call filter on element ----
        self.builder.position_at_end(call_filt_elem_bb);
        let filt_elem_call = self.builder.build_indirect_call(filt_fn_type, filter_fn, &[candidate_elem.into()], "fe_call").map_err(llvm_err)?;
        let filt_elem_bv = filt_elem_call.try_as_basic_value().basic().ok_or("filt elem call failed")?;
        let filt_elem_tag = if filt_elem_bv.is_struct_value() {
            self.builder.build_extract_value(filt_elem_bv.into_struct_value(), 0, "fe_val").map_err(llvm_err)?.into_int_value()
        } else { filt_elem_bv.into_int_value() };
        let elem_passes = self.builder.build_int_compare(IntPredicate::NE, filt_elem_tag, zero, "elem_passes").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(elem_passes, elem_pass_bb, elem_fail_bb);

        // ---- elem_pass_bb: push element, increment i ----
        self.builder.position_at_end(elem_pass_bb);
        let elem_fat = self.make_int_fat(candidate_elem)?;
        let cur_list2 = self.load_list(result_alloca)?;
        let cc2 = self.call_rt("atomic_list_push", &[cur_list2.into(), elem_fat.into()])?;
        let new_list2 = cc2.try_as_basic_value().basic().ok_or("push2 failed")?;
        self.builder.build_store(result_alloca, new_list2).map_err(llvm_err)?;
        let new_i = self.builder.build_int_add(i_loaded, one, "ll_i_inc").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, new_i).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_hdr);

        // ---- elem_fail_bb: skip this element, try next ----
        self.builder.position_at_end(elem_fail_bb);
        let _ = self.builder.build_unconditional_branch(loop_body);

        // ---- loop_exit ----
        self.builder.position_at_end(loop_exit);
        let final_list = self.load_list(result_alloca)?;
        Ok(final_list)
    }

    /// Create a fat struct {i64, i8*} from an i64 value (using string_type to match list_push expectations)
    pub(crate) fn make_int_fat(&self, val: inkwell::values::IntValue<'ctx>) -> Result<inkwell::values::StructValue<'ctx>, String> {
        let undef = self.string_type.get_undef();
        let null_ptr = self.ptr_ty().const_null();
        let aggregate = self.builder.build_insert_value(undef, val, 0, "fat_v").map_err(llvm_err)?;
        let aggregate2 = self.builder.build_insert_value(aggregate, null_ptr, 1, "fat_p").map_err(llvm_err)?;
        Ok(aggregate2.into_struct_value())
    }

    /// range.contains(value): check if value is within the range [start, end) or [start, end]
    pub(super) fn builtin_range_contains(&mut self, range_expr: &Expr, val_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let range_val = self.compile_expr(range_expr)?;
        let val_val = self.compile_expr(val_expr)?;
        let (ptr, st) = match range_val {
            TypedValue::Struct(p, s) => (p, s),
            _ => return Err("range.contains requires a range value".to_string()),
        };
        let val_int = match val_val {
            TypedValue::Int(v) => v,
            _ => return Err("range.contains requires an integer argument".to_string()),
        };
        let bt: BasicTypeEnum = st.into();
        let loaded = self.builder.build_load(bt, ptr, "range_ld").map_err(llvm_err)?.into_struct_value();
        let start = self.builder.build_extract_value(loaded, 0, "r_start").map_err(llvm_err)?.into_int_value();
        let end = self.builder.build_extract_value(loaded, 1, "r_end").map_err(llvm_err)?.into_int_value();
        let _inclusive = self.builder.build_extract_value(loaded, 2, "r_inc").map_err(llvm_err)?.into_int_value();
        let ge_start = self.builder.build_int_compare(IntPredicate::SGE, val_int, start, "ge_s").map_err(llvm_err)?;
        // If inclusive, use SLE; otherwise SLT
        let end_cmp = self.builder.build_int_compare(
            IntPredicate::SLE, val_int, end, "le_e"
        ).map_err(llvm_err)?;
        let result = self.builder.build_and(ge_start, end_cmp, "in_range").map_err(llvm_err)?;
        Ok(TypedValue::Bool(result))
    }

    /// range.toList(): expand the range into a List<Int> of all values
    pub(super) fn builtin_range_to_list(&mut self, range_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let range_val = self.compile_expr(range_expr)?;
        let (ptr, st) = match range_val {
            TypedValue::Struct(p, s) => (p, s),
            _ => return Err("range.toList requires a range value".to_string()),
        };
        let bt: BasicTypeEnum = st.into();
        let loaded = self.builder.build_load(bt, ptr, "range_ld").map_err(llvm_err)?.into_struct_value();
        let start_val = self.builder.build_extract_value(loaded, 0, "r_start").map_err(llvm_err)?.into_int_value();
        let end_val = self.builder.build_extract_value(loaded, 1, "r_end").map_err(llvm_err)?.into_int_value();
        let inclusive = self.builder.build_extract_value(loaded, 2, "r_inc").map_err(llvm_err)?.into_int_value();

        // end_bound = end + inclusive (for inclusive range, iterate up to and including end)
        let end_bound = self.builder.build_int_add(end_val, inclusive, "end_bound").map_err(llvm_err)?;

        // Create list and store in alloca
        let cap_val = self.i64_ty().const_int(16, false);
        let list_cc = self.call_rt("atomic_list_create", &[cap_val.into()])?;
        let list_bv = list_cc.try_as_basic_value().basic().ok_or("range_toList create fail")?;
        let list_alloca = self.builder.build_alloca(self.list_type, "rtl_list").map_err(llvm_err)?;
        self.builder.build_store(list_alloca, list_bv).map_err(llvm_err)?;

        // Loop to populate list
        let current_fn = self.builder.get_insert_block().unwrap().get_parent().ok_or("block has no parent function")?;
        let entry_block = self.builder.get_insert_block().unwrap();
        let loop_block = self.context.append_basic_block(current_fn, "rtl_loop");
        let body_block = self.context.append_basic_block(current_fn, "rtl_body");
        let done_block = self.context.append_basic_block(current_fn, "rtl_done");
        self.builder.build_unconditional_branch(loop_block).map_err(llvm_err)?;

        // Loop header: check if i < end_bound
        self.builder.position_at_end(loop_block);
        let i_phi = self.builder.build_phi(self.i64_ty(), "rtl_i").map_err(llvm_err)?;
        let list_phi = self.builder.build_phi(self.list_type, "rtl_lphi").map_err(llvm_err)?;
        i_phi.add_incoming(&[(&start_val, entry_block)]);
        list_phi.add_incoming(&[(&list_bv, entry_block)]);
        let done_cond = self.builder.build_int_compare(
            IntPredicate::SGE, i_phi.as_basic_value().into_int_value(), end_bound, "rtl_done_cond"
        ).map_err(llvm_err)?;
        self.builder.build_conditional_branch(done_cond, done_block, body_block).map_err(llvm_err)?;

        // Loop body: push current value
        self.builder.position_at_end(body_block);
        let val_i = i_phi.as_basic_value().into_int_value();
        let fat = self.make_int_fat(val_i)?;
        let cur_list = list_phi.as_basic_value();
        let pushed = self.call_rt("atomic_list_push", &[cur_list.into(), fat.into()])?;
        let new_list = pushed.try_as_basic_value().basic().ok_or("rtl push fail")?;
        let next_i = self.builder.build_int_add(val_i, self.i64_ty().const_int(1, false), "rtl_next").map_err(llvm_err)?;
        let body_end_block = self.builder.get_insert_block().unwrap();
        i_phi.add_incoming(&[(&next_i, body_end_block)]);
        list_phi.add_incoming(&[(&new_list, body_end_block)]);
        self.builder.build_unconditional_branch(loop_block).map_err(llvm_err)?;

        self.builder.position_at_end(done_block);
        let final_list = list_phi.as_basic_value();
        self.builder.build_store(list_alloca, final_list).map_err(llvm_err)?;
        Ok(TypedValue::List(list_alloca))
    }

    /// to_list(lazy_or_set) - convert a LazyList or Set to a List
    pub(super) fn builtin_to_list(&mut self, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(expr)?;
        match val {
            TypedValue::LazyList(_) => {
                let list_sv = self.convert_lazylist_to_list(&val)?;
                let new_alloca = self.builder.build_alloca(self.list_type, "to_list").map_err(llvm_err)?;
                self.builder.build_store(new_alloca, list_sv).map_err(llvm_err)?;
                Ok(TypedValue::List(new_alloca))
            }
            TypedValue::Set(ptr) => {
                let list_val = self.load_list(ptr)?;
                let new_alloca = self.builder.build_alloca(self.list_type, "to_list_s").map_err(llvm_err)?;
                self.builder.build_store(new_alloca, list_val).map_err(llvm_err)?;
                Ok(TypedValue::List(new_alloca))
            }
            TypedValue::List(_) => Ok(val),
            _ => Err("to_list: argument must be a LazyList or Set".to_string()),
        }
    }

    /// to_lazy_list(list) - convert a List to a LazyList
    pub(super) fn builtin_to_lazy_list(&mut self, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(expr)?;
        match val {
            TypedValue::List(ptr) => {
                // Load list, extract first element as head
                let list_sv = self.load_list(ptr)?;
                let data = self.builder.build_extract_value(list_sv, 0, "toll_data").map_err(llvm_err)?.into_pointer_value();
                let len = self.builder.build_extract_value(list_sv, 1, "toll_len").map_err(llvm_err)?.into_int_value();
                // Load first element (fat struct) from data[0]
                let first_fat_ptr = unsafe { self.builder.build_gep(self.fat_return_type, data, &[self.i64_ty().const_int(0, false)], "toll_gep").map_err(llvm_err) }?;
                let first_fat = self.builder.build_load(self.fat_return_type, first_fat_ptr, "toll_fat").map_err(llvm_err)?;
                let head_val = self.builder.build_extract_value(first_fat.into_struct_value(), 0, "toll_head").map_err(llvm_err)?.into_int_value();

                // Store data pointer as i64 in state field so round-trip to_list can recover all elements
                let data_as_i64 = self.builder.build_ptr_to_int(data, self.i64_ty(), "data_i64").map_err(llvm_err)?;

                // Create LazyList with head, no step fn, state = data_ptr, take_count = len
                let ll_alloca = self.builder.build_alloca(self.lazylist_type, "to_ll").map_err(llvm_err)?;
                let undef = self.lazylist_type.get_undef();
                let v0 = self.builder.build_insert_value(undef, head_val, 0, "ll_h").map_err(llvm_err)?;
                let v1 = self.builder.build_insert_value(v0, self.ptr_ty().const_null(), 1, "ll_fn").map_err(llvm_err)?;
                let v2 = self.builder.build_insert_value(v1, data_as_i64, 2, "ll_s").map_err(llvm_err)?;
                let v3 = self.builder.build_insert_value(v2, len, 3, "ll_tc").map_err(llvm_err)?;
                let v4 = self.builder.build_insert_value(v3, self.ptr_ty().const_null(), 4, "ll_map").map_err(llvm_err)?;
                let v5 = self.builder.build_insert_value(v4, self.ptr_ty().const_null(), 5, "ll_filt").map_err(llvm_err)?;
                self.builder.build_store(ll_alloca, v5).map_err(llvm_err)?;
                Ok(TypedValue::LazyList(ll_alloca))
            }
            TypedValue::LazyList(_) => Ok(val),
            _ => Err("to_lazy_list: argument must be a List".to_string()),
        }
    }

    /// lazy_take(n, lazy_list) - limit lazy list to first n elements (lazy: just updates take_count)
    pub(super) fn builtin_lazy_take(&mut self, n_expr: &Expr, lazy_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let n_val = self.compile_expr(n_expr)?;
        let n = match n_val {
            TypedValue::Int(v) => v,
            _ => return Err("lazy_take: first argument must be an Int".to_string()),
        };
        let lazy_val = self.compile_expr(lazy_expr)?;
        let lazy_ptr = match &lazy_val {
            TypedValue::LazyList(p) => *p,
            _ => return Err("lazy_take: second argument must be a LazyList".to_string()),
        };
        // Load the LazyList struct, copy it with updated take_count
        let ll_sv = self.builder.build_load(self.lazylist_type, lazy_ptr, "lt_ll").map_err(llvm_err)?.into_struct_value();
        let head_val = self.builder.build_extract_value(ll_sv, 0, "lt_head").map_err(llvm_err)?;
        let step_fn = self.builder.build_extract_value(ll_sv, 1, "lt_fn").map_err(llvm_err)?;
        let state_val = self.builder.build_extract_value(ll_sv, 2, "lt_st").map_err(llvm_err)?;
        let map_fn = self.builder.build_extract_value(ll_sv, 4, "lt_map").map_err(llvm_err)?;
        let filter_fn = self.builder.build_extract_value(ll_sv, 5, "lt_filt").map_err(llvm_err)?;

        let result_alloca = self.builder.build_alloca(self.lazylist_type, "lt_result").map_err(llvm_err)?;
        let undef = self.lazylist_type.get_undef();
        let v0 = self.builder.build_insert_value(undef, head_val, 0, "lt_h").map_err(llvm_err)?;
        let v1 = self.builder.build_insert_value(v0, step_fn, 1, "lt_f").map_err(llvm_err)?;
        let v2 = self.builder.build_insert_value(v1, state_val, 2, "lt_s").map_err(llvm_err)?;
        let v3 = self.builder.build_insert_value(v2, n, 3, "lt_n").map_err(llvm_err)?;
        let v4 = self.builder.build_insert_value(v3, map_fn, 4, "lt_map").map_err(llvm_err)?;
        let v5 = self.builder.build_insert_value(v4, filter_fn, 5, "lt_filt").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, v5).map_err(llvm_err)?;
        Ok(TypedValue::LazyList(result_alloca))
    }

    /// lazy_drop(n, lazy_list) - drop first n elements (truly lazy: advances state without materializing list)
    pub(super) fn builtin_lazy_drop(&mut self, n_expr: &Expr, lazy_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let n_val = self.compile_expr(n_expr)?;
        let n = match n_val {
            TypedValue::Int(v) => v,
            _ => return Err("lazy_drop: first argument must be an Int".to_string()),
        };
        let lazy_val = self.compile_expr(lazy_expr)?;
        let lazy_ptr = match &lazy_val {
            TypedValue::LazyList(p) => *p,
            _ => return Err("lazy_drop: second argument must be a LazyList".to_string()),
        };

        let ll_sv = self.builder.build_load(self.lazylist_type, lazy_ptr, "ld_ll").map_err(llvm_err)?.into_struct_value();
        let head_val = self.builder.build_extract_value(ll_sv, 0, "ld_head").map_err(llvm_err)?.into_int_value();
        let step_fn = self.builder.build_extract_value(ll_sv, 1, "ld_fn").map_err(llvm_err)?.into_pointer_value();
        let state_val = self.builder.build_extract_value(ll_sv, 2, "ld_st").map_err(llvm_err)?.into_int_value();
        let take_count_val = self.builder.build_extract_value(ll_sv, 3, "ld_tc").map_err(llvm_err)?.into_int_value();
        let map_fn = self.builder.build_extract_value(ll_sv, 4, "ld_map").map_err(llvm_err)?.into_pointer_value();
        let filter_fn = self.builder.build_extract_value(ll_sv, 5, "ld_filt").map_err(llvm_err)?.into_pointer_value();

        let zero = self.i64_ty().const_int(0, false);
        let one = self.i64_ty().const_int(1, false);
        let neg_one = self.i64_ty().const_int((-1_i64) as u64, true);

        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;

        // Determine if list-backed (no step fn, state holds data ptr)
        let has_step = self.builder.build_int_compare(IntPredicate::NE, step_fn, self.ptr_ty().const_null(), "ld_has_step").map_err(llvm_err)?;
        let state_nz = self.builder.build_int_compare(IntPredicate::NE, state_val, zero, "ld_state_nz").map_err(llvm_err)?;
        let not_has_step = self.builder.build_not(has_step, "ld_not_step").map_err(llvm_err)?;
        let is_list_backed = self.builder.build_and(not_has_step, state_nz, "ld_is_lb").map_err(llvm_err)?;

        // Check if n >= take_count (result is empty)
        let tc_is_inf = self.builder.build_int_compare(IntPredicate::EQ, take_count_val, neg_one, "ld_tc_inf").map_err(llvm_err)?;
        let n_ge_tc = self.builder.build_int_compare(IntPredicate::SGE, n, take_count_val, "ld_n_ge_tc").map_err(llvm_err)?;
        let not_inf = self.builder.build_not(tc_is_inf, "ld_not_inf").map_err(llvm_err)?;
        let becomes_empty = self.builder.build_and(not_inf, n_ge_tc, "ld_empty").map_err(llvm_err)?;

        // Branch: empty? → fast path; otherwise → drop path
        let empty_block = self.context.append_basic_block(current_fn, "ld_empty");
        let drop_block = self.context.append_basic_block(current_fn, "ld_drop");
        let merge_block = self.context.append_basic_block(current_fn, "ld_merge");
        let _ = self.builder.build_conditional_branch(becomes_empty, empty_block, drop_block);

        // Empty result: head=0, no step fn, state=0, tc=0, keep map/filter (won't matter)
        self.builder.position_at_end(empty_block);
        let e_result = self.builder.build_alloca(self.lazylist_type, "ld_e_result").map_err(llvm_err)?;
        let e_undef = self.lazylist_type.get_undef();
        let e0 = self.builder.build_insert_value(e_undef, zero, 0, "e_h").map_err(llvm_err)?;
        let e1 = self.builder.build_insert_value(e0, self.ptr_ty().const_null(), 1, "e_fn").map_err(llvm_err)?;
        let e2 = self.builder.build_insert_value(e1, zero, 2, "e_st").map_err(llvm_err)?;
        let e3 = self.builder.build_insert_value(e2, zero, 3, "e_tc").map_err(llvm_err)?;
        let e4 = self.builder.build_insert_value(e3, self.ptr_ty().const_null(), 4, "e_map").map_err(llvm_err)?;
        let e5 = self.builder.build_insert_value(e4, self.ptr_ty().const_null(), 5, "e_filt").map_err(llvm_err)?;
        self.builder.build_store(e_result, e5).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Drop path: advance head/state by n
        self.builder.position_at_end(drop_block);

        // Branch on list-backed vs step-function
        let lb_drop_block = self.context.append_basic_block(current_fn, "ld_lb_drop");
        let step_drop_block = self.context.append_basic_block(current_fn, "ld_step_drop");
        let drop_merge_block = self.context.append_basic_block(current_fn, "ld_drop_merge");
        let _ = self.builder.build_conditional_branch(is_list_backed, lb_drop_block, step_drop_block);

        // List-backed drop: advance data ptr by n elements, load new head
        self.builder.position_at_end(lb_drop_block);
        let data_ptr = self.builder.build_int_to_ptr(state_val, self.ptr_ty(), "ld_dp").map_err(llvm_err)?;
        let new_data_gep = unsafe { self.builder.build_gep(self.fat_return_type, data_ptr, &[n], "ld_ndp").map_err(llvm_err) }?;
        let new_data_i64 = self.builder.build_ptr_to_int(new_data_gep, self.i64_ty(), "ld_ndp_i64").map_err(llvm_err)?;
        let new_head_fat = self.builder.build_load(self.fat_return_type, new_data_gep, "ld_nh_fat").map_err(llvm_err)?.into_struct_value();
        let new_head = self.builder.build_extract_value(new_head_fat, 0, "ld_nh").map_err(llvm_err)?.into_int_value();
        let new_tc_lb = self.builder.build_int_sub(take_count_val, n, "ld_new_tc_lb").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(drop_merge_block);

        // Step-function drop: call step_fn n times to advance
        self.builder.position_at_end(step_drop_block);
        let i_alloca = self.builder.build_alloca(self.i64_ty(), "ld_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, zero).map_err(llvm_err)?;
        let cur_state_alloca = self.builder.build_alloca(self.i64_ty(), "ld_cs").map_err(llvm_err)?;
        self.builder.build_store(cur_state_alloca, state_val).map_err(llvm_err)?;
        let cur_head_alloca = self.builder.build_alloca(self.i64_ty(), "ld_ch").map_err(llvm_err)?;
        self.builder.build_store(cur_head_alloca, head_val).map_err(llvm_err)?;

        let step_loop_hdr = self.context.append_basic_block(current_fn, "ld_step_hdr");
        let step_loop_body = self.context.append_basic_block(current_fn, "ld_step_body");
        let step_done = self.context.append_basic_block(current_fn, "ld_step_done");
        let _ = self.builder.build_unconditional_branch(step_loop_hdr);

        self.builder.position_at_end(step_loop_hdr);
        let i_val = self.builder.build_load(self.i64_ty(), i_alloca, "ld_i_val").map_err(llvm_err)?.into_int_value();
        let i_lt_n = self.builder.build_int_compare(IntPredicate::SLT, i_val, n, "ld_i_lt_n").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(i_lt_n, step_loop_body, step_done);

        self.builder.position_at_end(step_loop_body);
        let cs = self.builder.build_load(self.i64_ty(), cur_state_alloca, "ld_cs_val").map_err(llvm_err)?.into_int_value();
        let fn_type = self.fat_return_type.fn_type(&[self.i64_ty().into()], false);
        let call_result = self.builder.build_indirect_call(fn_type, step_fn, &[cs.into()], "ld_step_call").map_err(llvm_err)?;
        let step_fat = call_result.try_as_basic_value().basic().ok_or("step call returned void")?;
        let step_fat_sv = step_fat.into_struct_value();
        let next_val = self.builder.build_extract_value(step_fat_sv, 0, "ld_next").map_err(llvm_err)?.into_int_value();
        self.builder.build_store(cur_state_alloca, next_val).map_err(llvm_err)?;
        self.builder.build_store(cur_head_alloca, next_val).map_err(llvm_err)?;
        let new_i = self.builder.build_int_add(i_val, one, "ld_ni").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, new_i).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(step_loop_hdr);

        self.builder.position_at_end(step_done);
        let new_head_step = self.builder.build_load(self.i64_ty(), cur_head_alloca, "ld_nh_step").map_err(llvm_err)?.into_int_value();
        let new_state = self.builder.build_load(self.i64_ty(), cur_state_alloca, "ld_ns").map_err(llvm_err)?.into_int_value();
        let new_tc_step = self.builder.build_select(tc_is_inf, take_count_val, self.builder.build_int_sub(take_count_val, n, "ld_tc_sub").map_err(llvm_err)?, "ld_new_tc_step").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_unconditional_branch(drop_merge_block);

        // Drop merge: phi for new head, new state, new take_count
        self.builder.position_at_end(drop_merge_block);
        let d_nh_phi = self.builder.build_phi(self.i64_ty(), "ld_nh_phi").map_err(llvm_err)?;
        d_nh_phi.add_incoming(&[(&new_head, lb_drop_block), (&new_head_step, step_done)]);
        let d_ns_phi = self.builder.build_phi(self.i64_ty(), "ld_ns_phi").map_err(llvm_err)?;
        d_ns_phi.add_incoming(&[(&new_data_i64, lb_drop_block), (&new_state, step_done)]);
        let d_ntc_phi = self.builder.build_phi(self.i64_ty(), "ld_ntc_phi").map_err(llvm_err)?;
        d_ntc_phi.add_incoming(&[(&new_tc_lb, lb_drop_block), (&new_tc_step, step_done)]);

        let d_result = self.builder.build_alloca(self.lazylist_type, "ld_d_result").map_err(llvm_err)?;
        let d_undef = self.lazylist_type.get_undef();
        let d0 = self.builder.build_insert_value(d_undef, d_nh_phi.as_basic_value().into_int_value(), 0, "d_h").map_err(llvm_err)?;
        let d1 = self.builder.build_insert_value(d0, step_fn, 1, "d_fn").map_err(llvm_err)?;
        let d2 = self.builder.build_insert_value(d1, d_ns_phi.as_basic_value().into_int_value(), 2, "d_st").map_err(llvm_err)?;
        let d3 = self.builder.build_insert_value(d2, d_ntc_phi.as_basic_value().into_int_value(), 3, "d_tc").map_err(llvm_err)?;
        let d4 = self.builder.build_insert_value(d3, map_fn, 4, "d_map").map_err(llvm_err)?;
        let d5 = self.builder.build_insert_value(d4, filter_fn, 5, "d_filt").map_err(llvm_err)?;
        self.builder.build_store(d_result, d5).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Final merge: phi for the result LazyList pointer
        self.builder.position_at_end(merge_block);
        let m_phi = self.builder.build_phi(self.ptr_ty(), "ld_m_phi").map_err(llvm_err)?;
        m_phi.add_incoming(&[(&e_result, empty_block), (&d_result, drop_merge_block)]);
        let result_ptr = m_phi.as_basic_value().into_pointer_value();
        Ok(TypedValue::LazyList(result_ptr))
    }

    /// lazy_map(fn, lazy_list) - truly lazy: creates a wrapper step function composing map with the original step fn.
    /// Falls back to eager evaluation for list-backed lazy lists (from to_lazy_list).
    pub(super) fn builtin_lazy_map(&mut self, fn_expr: &Expr, lazy_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let fn_val = self.compile_expr(fn_expr)?;
        let (map_fn_ptr, _fn_type) = match fn_val {
            TypedValue::Fn(p, ft) => (p, ft),
            _ => return Err("lazy_map: first argument must be a function".to_string()),
        };
        let lazy_val = self.compile_expr(lazy_expr)?;
        match &lazy_val {
            TypedValue::LazyList(ll_ptr) => {
                self.lazy_map_impl(map_fn_ptr, *ll_ptr)
            }
            TypedValue::List(_) => {
                // Convert to lazy list first, then map lazily
                let ll_val = self.builtin_to_lazy_list(lazy_expr)?;
                match ll_val {
                    TypedValue::LazyList(ll_ptr) => self.lazy_map_impl(map_fn_ptr, ll_ptr),
                    _ => Err("lazy_map: to_lazy_list did not return LazyList".to_string()),
                }
            }
            _ => Err("lazy_map: second argument must be a LazyList or List".to_string()),
        }
    }

    /// lazy_map_impl: store map_fn in the LazyList for deferred application during to_list()
    fn lazy_map_impl(&mut self, map_fn_ptr: inkwell::values::PointerValue<'ctx>, ll_ptr: inkwell::values::PointerValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        let ll_sv = self.builder.build_load(self.lazylist_type, ll_ptr, "lm_ll").map_err(llvm_err)?.into_struct_value();
        let head_val = self.builder.build_extract_value(ll_sv, 0, "lm_head").map_err(llvm_err)?;
        let step_fn = self.builder.build_extract_value(ll_sv, 1, "lm_sf").map_err(llvm_err)?;
        let state_val = self.builder.build_extract_value(ll_sv, 2, "lm_st").map_err(llvm_err)?;
        let take_count = self.builder.build_extract_value(ll_sv, 3, "lm_tc").map_err(llvm_err)?;
        let old_map_fn = self.builder.build_extract_value(ll_sv, 4, "lm_old_map").map_err(llvm_err)?;
        let filter_fn = self.builder.build_extract_value(ll_sv, 5, "lm_filt").map_err(llvm_err)?;

        // Compose with existing map_fn if present
        let has_old_map = self.builder.build_int_compare(IntPredicate::NE, old_map_fn.into_pointer_value(), self.ptr_ty().const_null(), "has_old_map").map_err(llvm_err)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
        let compose_block = self.context.append_basic_block(current_fn, "lm_compose");
        let no_compose_block = self.context.append_basic_block(current_fn, "lm_no_compose");
        let merge_block = self.context.append_basic_block(current_fn, "lm_merge");

        let _ = self.builder.build_conditional_branch(has_old_map, compose_block, no_compose_block);

        // Compose: new_fn(x) = map_fn_ptr(old_map_fn(x))
        self.builder.position_at_end(compose_block);
        let wrapper_name = format!("lm_compose_{}", self.wrapper_counter);
        self.wrapper_counter += 1;
        let fat_ty = self.string_type;
        let wrapper_fn = self.module.add_function(&wrapper_name, fat_ty.fn_type(&[self.i64_ty().into()], false), None);
        let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
        let saved_block = self.builder.get_insert_block();

        let cap_ty = self.context.struct_type(&[self.ptr_ty().into(), self.ptr_ty().into()], false);
        let cap_global = self.module.add_global(cap_ty, None, &format!("{}_cap", wrapper_name));
        cap_global.set_initializer(&cap_ty.const_zero());
        let cap_ptr = cap_global.as_pointer_value();
        let c_gep0 = self.builder.build_struct_gep(cap_ty, cap_ptr, 0, "cg0").map_err(llvm_err)?;
        self.builder.build_store(c_gep0, old_map_fn).map_err(llvm_err)?;
        let c_gep1 = self.builder.build_struct_gep(cap_ty, cap_ptr, 1, "cg1").map_err(llvm_err)?;
        self.builder.build_store(c_gep1, map_fn_ptr).map_err(llvm_err)?;

        self.builder.position_at_end(wrapper_entry);
        let w_state = wrapper_fn.get_first_param().unwrap().into_int_value();
        let cap_load = self.builder.build_load(cap_ty, cap_ptr, "cap_load").map_err(llvm_err)?.into_struct_value();
        let w_old_fn = self.builder.build_extract_value(cap_load, 0, "w_old").map_err(llvm_err)?.into_pointer_value();
        let w_new_fn = self.builder.build_extract_value(cap_load, 1, "w_new").map_err(llvm_err)?.into_pointer_value();
        let map_fn_type = fat_ty.fn_type(&[self.i64_ty().into()], false);
        let old_call = self.builder.build_indirect_call(map_fn_type, w_old_fn, &[w_state.into()], "w_old_call").map_err(llvm_err)?;
        let old_result = old_call.try_as_basic_value().basic().ok_or("old call failed")?;
        let old_val = if old_result.is_struct_value() {
            self.builder.build_extract_value(old_result.into_struct_value(), 0, "w_old_val").map_err(llvm_err)?.into_int_value()
        } else { old_result.into_int_value() };
        let new_call = self.builder.build_indirect_call(map_fn_type, w_new_fn, &[old_val.into()], "w_new_call").map_err(llvm_err)?;
        let new_result = new_call.try_as_basic_value().basic().ok_or("new call failed")?;
        self.builder.build_return(Some(&new_result)).map_err(llvm_err)?;

        self.builder.position_at_end(saved_block.ok_or("no saved block")?);
        let composed_fn = wrapper_fn.as_global_value().as_pointer_value();
        let _ = self.builder.build_unconditional_branch(merge_block);

        self.builder.position_at_end(no_compose_block);
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge: pick the right map_fn
        self.builder.position_at_end(merge_block);
        let phi_map = self.builder.build_phi(self.ptr_ty(), "lm_phi_map").map_err(llvm_err)?;
        phi_map.add_incoming(&[(&composed_fn, compose_block), (&map_fn_ptr, no_compose_block)]);

        // Build result LazyList with updated map_fn, head unchanged (deferred mapping in to_list)
        let result_alloca = self.builder.build_alloca(self.lazylist_type, "lm_result").map_err(llvm_err)?;
        let undef = self.lazylist_type.get_undef();
        let v0 = self.builder.build_insert_value(undef, head_val, 0, "lm_h").map_err(llvm_err)?;
        let v1 = self.builder.build_insert_value(v0, step_fn, 1, "lm_f").map_err(llvm_err)?;
        let v2 = self.builder.build_insert_value(v1, state_val, 2, "lm_s").map_err(llvm_err)?;
        let v3 = self.builder.build_insert_value(v2, take_count, 3, "lm_t").map_err(llvm_err)?;
        let v4 = self.builder.build_insert_value(v3, phi_map.as_basic_value().into_pointer_value(), 4, "lm_map").map_err(llvm_err)?;
        let v5 = self.builder.build_insert_value(v4, filter_fn, 5, "lm_filt").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, v5).map_err(llvm_err)?;
        Ok(TypedValue::LazyList(result_alloca))
    }

    /// lazy_filter(fn, lazy_list) - truly lazy: stores filter_fn in LazyList for deferred application during to_list()
    pub(super) fn builtin_lazy_filter(&mut self, fn_expr: &Expr, lazy_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let fn_val = self.compile_expr(fn_expr)?;
        let (filter_fn_ptr, _) = match fn_val {
            TypedValue::Fn(p, _) => (p, fn_val),
            _ => return Err("lazy_filter: first argument must be a function".to_string()),
        };
        let lazy_val = self.compile_expr(lazy_expr)?;
        match &lazy_val {
            TypedValue::LazyList(ll_ptr) => {
                self.lazy_filter_impl(filter_fn_ptr, *ll_ptr)
            }
            TypedValue::List(_) => {
                let ll_val = self.builtin_to_lazy_list(lazy_expr)?;
                match ll_val {
                    TypedValue::LazyList(ll_ptr) => self.lazy_filter_impl(filter_fn_ptr, ll_ptr),
                    _ => Err("lazy_filter: to_lazy_list did not return LazyList".to_string()),
                }
            }
            _ => Err("lazy_filter: second argument must be a LazyList or List".to_string()),
        }
    }

    /// lazy_filter_impl: store filter_fn in the LazyList for deferred application during to_list()
    fn lazy_filter_impl(&mut self, filter_fn_ptr: inkwell::values::PointerValue<'ctx>, ll_ptr: inkwell::values::PointerValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        let ll_sv = self.builder.build_load(self.lazylist_type, ll_ptr, "lf_ll").map_err(llvm_err)?.into_struct_value();
        let head_val = self.builder.build_extract_value(ll_sv, 0, "lf_head").map_err(llvm_err)?;
        let step_fn = self.builder.build_extract_value(ll_sv, 1, "lf_sf").map_err(llvm_err)?;
        let state_val = self.builder.build_extract_value(ll_sv, 2, "lf_st").map_err(llvm_err)?;
        let take_count = self.builder.build_extract_value(ll_sv, 3, "lf_tc").map_err(llvm_err)?;
        let map_fn = self.builder.build_extract_value(ll_sv, 4, "lf_map").map_err(llvm_err)?;
        let old_filter_fn = self.builder.build_extract_value(ll_sv, 5, "lf_old_filt").map_err(llvm_err)?;

        // Compose filters if there's already a filter_fn
        let has_old_filter = self.builder.build_int_compare(IntPredicate::NE, old_filter_fn.into_pointer_value(), self.ptr_ty().const_null(), "has_old_filt").map_err(llvm_err)?;
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
        let compose_block = self.context.append_basic_block(current_fn, "lf_compose");
        let no_compose_block = self.context.append_basic_block(current_fn, "lf_no_compose");
        let merge_block = self.context.append_basic_block(current_fn, "lf_merge");

        let _ = self.builder.build_conditional_branch(has_old_filter, compose_block, no_compose_block);

        // Compose: new_filter(x) = old_filter(x) && new_filter(x)
        self.builder.position_at_end(compose_block);
        let wrapper_name = format!("lf_compose_{}", self.wrapper_counter);
        self.wrapper_counter += 1;
        let fat_ty = self.string_type;
        let wrapper_fn = self.module.add_function(&wrapper_name, fat_ty.fn_type(&[self.i64_ty().into()], false), None);
        let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
        let saved_block = self.builder.get_insert_block();

        // Capture both filter functions via global
        let cap_ty = self.context.struct_type(&[self.ptr_ty().into(), self.ptr_ty().into()], false);
        let cap_global = self.module.add_global(cap_ty, None, &format!("{}_cap", wrapper_name));
        cap_global.set_initializer(&cap_ty.const_zero());
        let cap_ptr = cap_global.as_pointer_value();
        let c_gep0 = self.builder.build_struct_gep(cap_ty, cap_ptr, 0, "cg0").map_err(llvm_err)?;
        self.builder.build_store(c_gep0, old_filter_fn).map_err(llvm_err)?;
        let c_gep1 = self.builder.build_struct_gep(cap_ty, cap_ptr, 1, "cg1").map_err(llvm_err)?;
        self.builder.build_store(c_gep1, filter_fn_ptr).map_err(llvm_err)?;

        self.builder.position_at_end(wrapper_entry);
        let w_state = wrapper_fn.get_first_param().unwrap().into_int_value();
        let cap_load = self.builder.build_load(cap_ty, cap_ptr, "cap_load").map_err(llvm_err)?.into_struct_value();
        let w_old_fn = self.builder.build_extract_value(cap_load, 0, "w_old").map_err(llvm_err)?.into_pointer_value();
        let w_new_fn = self.builder.build_extract_value(cap_load, 1, "w_new").map_err(llvm_err)?.into_pointer_value();
        // Call old_filter(state)
        let filt_fn_type = fat_ty.fn_type(&[self.i64_ty().into()], false);
        let old_call = self.builder.build_indirect_call(filt_fn_type, w_old_fn, &[w_state.into()], "w_old_call").map_err(llvm_err)?;
        let old_result = old_call.try_as_basic_value().basic().ok_or("old filt call failed")?;
        let old_val = if old_result.is_struct_value() {
            self.builder.build_extract_value(old_result.into_struct_value(), 0, "w_old_val").map_err(llvm_err)?.into_int_value()
        } else { old_result.into_int_value() };
        let old_true = self.builder.build_int_compare(IntPredicate::NE, old_val, self.i64_ty().const_int(0, false), "old_true").map_err(llvm_err)?;

        let then_block = self.context.append_basic_block(wrapper_fn, "then_call");
        let else_block = self.context.append_basic_block(wrapper_fn, "else_zero");
        let w_merge = self.context.append_basic_block(wrapper_fn, "w_merge");
        let _ = self.builder.build_conditional_branch(old_true, then_block, else_block);

        self.builder.position_at_end(then_block);
        let new_call = self.builder.build_indirect_call(filt_fn_type, w_new_fn, &[w_state.into()], "w_new_call").map_err(llvm_err)?;
        let new_result = new_call.try_as_basic_value().basic().ok_or("new filt call failed")?;
        let new_val = if new_result.is_struct_value() {
            self.builder.build_extract_value(new_result.into_struct_value(), 0, "w_new_val").map_err(llvm_err)?.into_int_value()
        } else { new_result.into_int_value() };
        let new_true = self.builder.build_int_compare(IntPredicate::NE, new_val, self.i64_ty().const_int(0, false), "new_true").map_err(llvm_err)?;
        let new_i64 = self.builder.build_int_z_extend(new_true, self.i64_ty(), "new_i64").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(w_merge);

        self.builder.position_at_end(else_block);
        let _ = self.builder.build_unconditional_branch(w_merge);

        self.builder.position_at_end(w_merge);
        let phi = self.builder.build_phi(self.i64_ty(), "filt_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&new_i64, then_block), (&self.i64_ty().const_int(0, false), else_block)]);
        // Return as fat struct {i64, i8*}
        let undef_ret = fat_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef_ret, phi.as_basic_value().into_int_value(), 0, "fr_v").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, self.ptr_ty().const_null(), 1, "fr_p").map_err(llvm_err)?;
        self.builder.build_return(Some(&r2)).map_err(llvm_err)?;

        self.builder.position_at_end(saved_block.ok_or("no saved block")?);
        let composed_fn = wrapper_fn.as_global_value().as_pointer_value();
        let _ = self.builder.build_unconditional_branch(merge_block);

        // No composition needed
        self.builder.position_at_end(no_compose_block);
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge: pick the right filter_fn
        self.builder.position_at_end(merge_block);
        let phi_filt = self.builder.build_phi(self.ptr_ty(), "lf_phi_filt").map_err(llvm_err)?;
        phi_filt.add_incoming(&[(&composed_fn, compose_block), (&filter_fn_ptr, no_compose_block)]);

        let result_alloca = self.builder.build_alloca(self.lazylist_type, "lf_result").map_err(llvm_err)?;
        let undef = self.lazylist_type.get_undef();
        let v0 = self.builder.build_insert_value(undef, head_val, 0, "lf_h").map_err(llvm_err)?;
        let v1 = self.builder.build_insert_value(v0, step_fn, 1, "lf_f").map_err(llvm_err)?;
        let v2 = self.builder.build_insert_value(v1, state_val, 2, "lf_s").map_err(llvm_err)?;
        let v3 = self.builder.build_insert_value(v2, take_count, 3, "lf_t").map_err(llvm_err)?;
        let v4 = self.builder.build_insert_value(v3, map_fn, 4, "lf_map").map_err(llvm_err)?;
        let v5 = self.builder.build_insert_value(v4, phi_filt.as_basic_value().into_pointer_value(), 5, "lf_filt").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, v5).map_err(llvm_err)?;
        Ok(TypedValue::LazyList(result_alloca))
    }

    /// lazy_take_while(fn, lazy_list) - take elements while predicate is true
    pub(super) fn builtin_lazy_take_while(&mut self, fn_expr: &Expr, lazy_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let fn_val = self.compile_expr(fn_expr)?;
        let (fn_ptr, _) = match fn_val {
            TypedValue::Fn(p, _) => (p, fn_val),
            _ => return Err("lazy_take_while: first argument must be a function".to_string()),
        };
        let lazy_val = self.compile_expr(lazy_expr)?;
        let lazy_ptr = self.ensure_list_ptr(&lazy_val, "ltw")?;
        let list = self.load_list(lazy_ptr)?;
        let len = self.builder.build_extract_value(list, 1, "len").map_err(llvm_err)?.into_int_value();
        let data = self.builder.build_extract_value(list, 0, "data").map_err(llvm_err)?.into_pointer_value();

        let cc = self.call_rt("atomic_list_create", &[len.into()])?;
        let new_list = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "ltw_result").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, new_list).map_err(llvm_err)?;

        let i64 = self.i64_ty();
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
        let i_alloca = self.builder.build_alloca(i64, "ltw_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i64.const_int(0, false)).map_err(llvm_err)?;

        let loop_hdr = self.context.append_basic_block(current_fn, "ltw_hdr");
        let loop_bdy = self.context.append_basic_block(current_fn, "ltw_bdy");
        let loop_ins = self.context.append_basic_block(current_fn, "ltw_ins");
        let loop_ext = self.context.append_basic_block(current_fn, "ltw_ext");

        let _ = self.builder.build_unconditional_branch(loop_hdr);

        self.builder.position_at_end(loop_hdr);
        let i = self.builder.build_load(i64, i_alloca, "ltw_iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i, len, "ltw_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_bdy, loop_ext);

        self.builder.position_at_end(loop_bdy);
        let src_ptr = unsafe { self.builder.build_gep(self.string_type, data, &[i], "ltw_sp").map_err(llvm_err) }?;
        let elem = self.builder.build_load(self.string_type, src_ptr, "ltw_el").map_err(llvm_err)?.into_struct_value();
        let tag = self.builder.build_extract_value(elem, 0, "ltw_tag").map_err(llvm_err)?.into_int_value();

        let fat_ty = self.string_type;
        let lam_fn_type = fat_ty.fn_type(&[i64.into()], false);
        let cc = self.builder.build_indirect_call(lam_fn_type, fn_ptr, &[tag.into()], "ltw_call").map_err(llvm_err)?;
        let pred_bv = cc.try_as_basic_value().basic().ok_or("ltw call failed")?;
        let pred_tag = if pred_bv.is_struct_value() {
            self.builder.build_extract_value(pred_bv.into_struct_value(), 0, "pred").map_err(llvm_err)?.into_int_value()
        } else { pred_bv.into_int_value() };
        let keep = self.builder.build_int_compare(IntPredicate::NE, pred_tag, i64.const_int(0, false), "keep").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(keep, loop_ins, loop_ext);

        self.builder.position_at_end(loop_ins);
        let cur = self.load_list(result_alloca)?;
        let pcc = self.call_rt("atomic_list_push", &[cur.into(), elem.into()])?;
        let nl = pcc.try_as_basic_value().basic().ok_or("list_push failed")?;
        self.builder.build_store(result_alloca, nl).map_err(llvm_err)?;
        let ni = self.builder.build_int_add(i, i64.const_int(1, false), "ltw_ni").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_hdr);

        self.builder.position_at_end(loop_ext);
        Ok(TypedValue::List(result_alloca))
    }

    /// lazy_head(lazy_list) - return first element as Some, or None if empty
    pub(super) fn builtin_lazy_head(&mut self, lazy_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let lazy_val = self.compile_expr(lazy_expr)?;
        // If LazyList, extract head directly. If List, use first element.
        let (head_val, is_empty) = match &lazy_val {
            TypedValue::LazyList(ptr) => {
                let ll_sv = self.builder.build_load(self.lazylist_type, *ptr, "head_ll").map_err(llvm_err)?.into_struct_value();
                let h = self.builder.build_extract_value(ll_sv, 0, "head_h").map_err(llvm_err)?;
                // A LazyList always has a head, so is_empty = false (i1)
                (h, self.bool_ty().const_int(0, false))
            }
            TypedValue::List(ptr) => {
                let list = self.load_list(*ptr)?;
                let len = self.builder.build_extract_value(list, 1, "len").map_err(llvm_err)?.into_int_value();
                let data = self.builder.build_extract_value(list, 0, "data").map_err(llvm_err)?.into_pointer_value();
                let zero = self.i64_ty().const_int(0, false);
                let is_empty_cond = self.builder.build_int_compare(IntPredicate::EQ, len, zero, "is_empty").map_err(llvm_err)?;
                // Load first element's fat struct
                let first_ptr = unsafe { self.builder.build_gep(self.fat_return_type, data, &[zero], "head_gep").map_err(llvm_err) }?;
                let first_fat = self.builder.build_load(self.fat_return_type, first_ptr, "head_fat").map_err(llvm_err)?.into_struct_value();
                let h = self.builder.build_extract_value(first_fat, 0, "head_h").map_err(llvm_err)?;
                (h, is_empty_cond)
            }
            _ => return Err("lazy_head: argument must be a LazyList or List".to_string()),
        };

        let i64 = self.i64_ty();

        // Get the Option enum type
        let option_ty = *self.enum_types.get("Option").unwrap();
        let option_bt: BasicTypeEnum = option_ty.into();

        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;

        let result_alloca = self.builder.build_alloca(option_bt, "lh_result").map_err(llvm_err)?;

        let merge_block = self.context.append_basic_block(current_fn, "lh_merge");
        let some_block = self.context.append_basic_block(current_fn, "lh_some");
        let none_block = self.context.append_basic_block(current_fn, "lh_none");

        let _ = self.builder.build_conditional_branch(is_empty, none_block, some_block);

        // Some branch: head_val contains the i64 value
        self.builder.position_at_end(some_block);
        // Extract i64 value from head_val (which is either IntValue or BasicValueEnum)
        let head_i64 = head_val.into_int_value();

        // Store head on heap and create Some(head)
        let buf = self.malloc_rc(i64.const_int(8, false))?;
        let buf_ptr = self.builder.build_pointer_cast(buf, self.ptr_ty(), "lh_bp").map_err(llvm_err)?;
        self.builder.build_store(buf_ptr, head_i64).map_err(llvm_err)?;
        self.rc_inc(buf)?;

        let undef = option_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, i64.const_int(0, false), 0, "lh_ok_tag").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, buf, 1, "lh_ok_data").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, r2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // None branch
        self.builder.position_at_end(none_block);
        let undef2 = option_ty.get_undef();
        let n1 = self.builder.build_insert_value(undef2, i64.const_int(1, false), 0, "lh_none_tag").map_err(llvm_err)?;
        let n2 = self.builder.build_insert_value(n1, self.ptr_ty().const_zero(), 1, "lh_none_data").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, n2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        self.builder.position_at_end(merge_block);
        Ok(TypedValue::Enum(result_alloca, option_ty, InnerType::Int, true))
    }

    /// lazy_zip(lazy1, lazy2) - zip two lazy lists eagerly, return as List
    pub(super) fn builtin_lazy_zip(&mut self, lazy1_expr: &Expr, lazy2_expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let v1 = self.compile_expr(lazy1_expr)?;
        let v2 = self.compile_expr(lazy2_expr)?;
        let p1 = self.ensure_list_ptr(&v1, "lz1")?;
        let p2 = self.ensure_list_ptr(&v2, "lz2")?;
        let l1 = self.load_list(p1)?;
        let l2 = self.load_list(p2)?;
        let len1 = self.builder.build_extract_value(l1, 1, "lz_len1").map_err(llvm_err)?.into_int_value();
        let len2 = self.builder.build_extract_value(l2, 1, "lz_len2").map_err(llvm_err)?.into_int_value();
        let d1 = self.builder.build_extract_value(l1, 0, "lz_d1").map_err(llvm_err)?.into_pointer_value();
        let d2 = self.builder.build_extract_value(l2, 0, "lz_d2").map_err(llvm_err)?.into_pointer_value();

        let i64 = self.i64_ty();
        let is_len1_lt_len2 = self.builder.build_int_compare(IntPredicate::SLT, len1, len2, "is_len1_lt_len2").map_err(llvm_err)?;
        let min_len = self.builder.build_select(is_len1_lt_len2, len1, len2, "lz_min").map_err(llvm_err)?.into_int_value();

        let cc = self.call_rt("atomic_list_create", &[min_len.into()])?;
        let new_list = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "lz_result").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, new_list).map_err(llvm_err)?;

        // Zip elements as tuple-like: store (tag1, tag2) as two sequential entries
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
        let i_alloca = self.builder.build_alloca(i64, "lz_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i64.const_int(0, false)).map_err(llvm_err)?;

        let loop_hdr = self.context.append_basic_block(current_fn, "lz_hdr");
        let loop_bdy = self.context.append_basic_block(current_fn, "lz_bdy");
        let loop_ext = self.context.append_basic_block(current_fn, "lz_ext");

        let _ = self.builder.build_unconditional_branch(loop_hdr);

        self.builder.position_at_end(loop_hdr);
        let i = self.builder.build_load(i64, i_alloca, "lz_iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i, min_len, "lz_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_bdy, loop_ext);

        self.builder.position_at_end(loop_bdy);
        let sp1 = unsafe { self.builder.build_gep(self.string_type, d1, &[i], "lz_sp1").map_err(llvm_err) }?;
        let e1 = self.builder.build_load(self.string_type, sp1, "lz_e1").map_err(llvm_err)?;
        let sp2 = unsafe { self.builder.build_gep(self.string_type, d2, &[i], "lz_sp2").map_err(llvm_err) }?;
        let e2 = self.builder.build_load(self.string_type, sp2, "lz_e2").map_err(llvm_err)?;

        // Push both as separate elements (pair is two sequential entries)
        let cur = self.load_list(result_alloca)?;
        let cc = self.call_rt("atomic_list_push", &[cur.into(), e1.into()])?;
        let nl = cc.try_as_basic_value().basic().ok_or("list_push e1 failed")?;
        self.builder.build_store(result_alloca, nl).map_err(llvm_err)?;
        let cur2 = self.load_list(result_alloca)?;
        let cc2 = self.call_rt("atomic_list_push", &[cur2.into(), e2.into()])?;
        let nl2 = cc2.try_as_basic_value().basic().ok_or("list_push e2 failed")?;
        self.builder.build_store(result_alloca, nl2).map_err(llvm_err)?;

        let ni = self.builder.build_int_add(i, i64.const_int(1, false), "lz_ni").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_hdr);

        self.builder.position_at_end(loop_ext);
        Ok(TypedValue::List(result_alloca))
    }
}
