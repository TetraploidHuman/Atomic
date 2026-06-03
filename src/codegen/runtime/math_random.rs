use inkwell::IntPredicate;

use super::super::{CodeGen, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn define_math_random(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let f64 = self.f64_ty();
        let _ptr = self.ptr_ty();
        let _i32 = self.context.i32_type();
        let _i8 = self.context.i8_type();

        // ---- atomic_int_pow(i64, i64) -> i64 (exponentiation by squaring) ----
        let int_pow_fn = self.module.add_function("atomic_int_pow", i64.fn_type(&[i64.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(int_pow_fn, "entry");
        let loop_bb = self.context.append_basic_block(int_pow_fn, "loop");
        let odd_bb = self.context.append_basic_block(int_pow_fn, "odd");
        let after_mul_bb = self.context.append_basic_block(int_pow_fn, "after_mul");
        let done_bb = self.context.append_basic_block(int_pow_fn, "done");

        let base = int_pow_fn.get_first_param().unwrap().into_int_value();
        let exp = int_pow_fn.get_nth_param(1).unwrap().into_int_value();

        self.builder.position_at_end(entry);
        let result_alloca = self.builder.build_alloca(i64, "result").map_err(llvm_err)?;
        let b_alloca = self.builder.build_alloca(i64, "b").map_err(llvm_err)?;
        let e_alloca = self.builder.build_alloca(i64, "e").map_err(llvm_err)?;
        let one = i64.const_int(1, false);
        let zero = i64.const_int(0, false);
        self.builder.build_store(result_alloca, one).map_err(llvm_err)?;
        self.builder.build_store(b_alloca, base).map_err(llvm_err)?;
        self.builder.build_store(e_alloca, exp).map_err(llvm_err)?;
        let exp_neg = self.builder.build_int_compare(IntPredicate::SLT, exp, zero, "neg").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(exp_neg, done_bb, loop_bb);

        // loop: while e > 0
        self.builder.position_at_end(loop_bb);
        let e_cur = self.builder.build_load(i64, e_alloca, "e_cur").map_err(llvm_err)?.into_int_value();
        let e_gt_zero = self.builder.build_int_compare(IntPredicate::SGT, e_cur, zero, "gt").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(e_gt_zero, odd_bb, done_bb);

        // odd: if e & 1 then result *= b
        self.builder.position_at_end(odd_bb);
        let e_val = self.builder.build_load(i64, e_alloca, "e_val").map_err(llvm_err)?.into_int_value();
        let is_odd = self.builder.build_and(e_val, one, "odd").map_err(llvm_err)?;
        let odd_cond = self.builder.build_int_compare(IntPredicate::EQ, is_odd, one, "odd_cmp").map_err(llvm_err)?;
        let mul_bb = self.context.append_basic_block(int_pow_fn, "mul");
        let _ = self.builder.build_conditional_branch(odd_cond, mul_bb, after_mul_bb);

        // mul: result *= b
        self.builder.position_at_end(mul_bb);
        let cur_result = self.builder.build_load(i64, result_alloca, "cur_r").map_err(llvm_err)?.into_int_value();
        let cur_b = self.builder.build_load(i64, b_alloca, "cur_b").map_err(llvm_err)?.into_int_value();
        let new_result = self.builder.build_int_mul(cur_result, cur_b, "mul_r").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, new_result).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(after_mul_bb);

        // after_mul: b *= b; e >>= 1
        self.builder.position_at_end(after_mul_bb);
        let b_val = self.builder.build_load(i64, b_alloca, "b_val").map_err(llvm_err)?.into_int_value();
        let b_sq = self.builder.build_int_mul(b_val, b_val, "sq").map_err(llvm_err)?;
        self.builder.build_store(b_alloca, b_sq).map_err(llvm_err)?;
        let e_val2 = self.builder.build_load(i64, e_alloca, "e_val2").map_err(llvm_err)?.into_int_value();
        let two = i64.const_int(2, false);
        let e_half = self.builder.build_int_signed_div(e_val2, two, "half").map_err(llvm_err)?;
        self.builder.build_store(e_alloca, e_half).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_bb);

        // done: return result
        self.builder.position_at_end(done_bb);
        let done_val = self.builder.build_load(i64, result_alloca, "done_val").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_return(Some(&done_val));
        // ---- abs(i64) -> i64 ----
        let abs_fn = self.module.add_function("abs", i64.fn_type(&[i64.into()], false), None);
        let entry = self.context.append_basic_block(abs_fn, "entry");
        self.builder.position_at_end(entry);
        let x = abs_fn.get_first_param().unwrap().into_int_value();
        let neg = self.builder.build_int_neg(x, "neg").map_err(llvm_err)?;
        let is_neg = self.builder.build_int_compare(IntPredicate::SLT, x, i64.const_int(0, false), "is_neg").map_err(llvm_err)?;
        let result = self.builder.build_select(is_neg, neg, x, "abs_result").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&result.into_int_value()));

        // ---- min(i64, i64) -> i64 ----
        let min_fn = self.module.add_function("min", i64.fn_type(&[i64.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(min_fn, "entry");
        self.builder.position_at_end(entry);
        let a = min_fn.get_first_param().unwrap().into_int_value();
        let b = min_fn.get_nth_param(1).unwrap().into_int_value();
        let lt = self.builder.build_int_compare(IntPredicate::SLT, a, b, "lt").map_err(llvm_err)?;
        let min_result = self.builder.build_select(lt, a, b, "min_result").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&min_result.into_int_value()));

        // ---- max(i64, i64) -> i64 ----
        let max_fn = self.module.add_function("max", i64.fn_type(&[i64.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(max_fn, "entry");
        self.builder.position_at_end(entry);
        let ma = max_fn.get_first_param().unwrap().into_int_value();
        let mb = max_fn.get_nth_param(1).unwrap().into_int_value();
        let gt = self.builder.build_int_compare(IntPredicate::SGT, ma, mb, "gt").map_err(llvm_err)?;
        let max_result = self.builder.build_select(gt, ma, mb, "max_result").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&max_result.into_int_value()));
        // ---- atomic_rand_init() ----
        // Simple LCG state: uses a global i64 seed initialized to 1
        let rand_seed_g = self.module.add_global(i64, None, "atomic_rand_seed");
        rand_seed_g.set_initializer(&i64.const_int(123456789, false));

        // ---- atomic_rand_int(i64 min, i64 max) -> i64 ----
        let ri_fn = self.module.add_function("atomic_rand_int",
            i64.fn_type(&[i64.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(ri_fn, "entry");
        self.builder.position_at_end(entry);
        let ri_min = ri_fn.get_first_param().unwrap().into_int_value();
        let ri_max = ri_fn.get_nth_param(1).unwrap().into_int_value();
        // LCG: seed = seed * 1103515245 + 12345
        let ri_seed_ptr = rand_seed_g.as_pointer_value();
        let ri_old_seed = self.builder.build_load(i64, ri_seed_ptr, "old_seed").map_err(llvm_err)?.into_int_value();
        let ri_mul = self.builder.build_int_mul(ri_old_seed, i64.const_int(1103515245, false), "mul").map_err(llvm_err)?;
        let ri_new_seed = self.builder.build_int_add(ri_mul, i64.const_int(12345, false), "new_seed").map_err(llvm_err)?;
        self.builder.build_store(ri_seed_ptr, ri_new_seed).map_err(llvm_err)?;
        // range = max - min + 1
        let ri_range = self.builder.build_int_sub(ri_max, ri_min, "sub").map_err(llvm_err)?;
        let ri_range1 = self.builder.build_int_add(ri_range, i64.const_int(1, false), "range1").map_err(llvm_err)?;
        // result = min + (new_seed % range)
        let _ri_range_pos = self.builder.build_int_compare(IntPredicate::SGT, ri_range1, i64.const_int(0, false), "pos").map_err(llvm_err)?;
        // Use unsigned remainder to avoid negative issues
        let ri_rem = self.builder.build_int_unsigned_rem(ri_new_seed, ri_range1, "rem").map_err(llvm_err)?;
        let ri_zero = self.builder.build_int_compare(IntPredicate::ULE, ri_range1, i64.const_int(0, false), "zero_range").map_err(llvm_err)?;
        // If range <= 0, return min
        let ri_result = self.builder.build_select(ri_zero, ri_min,
            self.builder.build_int_add(ri_min, ri_rem, "add").map_err(llvm_err)?,
            "result").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_return(Some(&ri_result));

        // ---- atomic_rand_float() -> f64 ----
        let rf_fn = self.module.add_function("atomic_rand_float",
            f64.fn_type(&[], false), None);
        let entry = self.context.append_basic_block(rf_fn, "entry");
        self.builder.position_at_end(entry);
        // Use the same LCG seed, return value in [0, 1)
        let rf_seed_ptr = rand_seed_g.as_pointer_value();
        let rf_old_seed = self.builder.build_load(i64, rf_seed_ptr, "old_seed").map_err(llvm_err)?.into_int_value();
        let rf_mul = self.builder.build_int_mul(rf_old_seed, i64.const_int(1103515245, false), "mul").map_err(llvm_err)?;
        let rf_new_seed = self.builder.build_int_add(rf_mul, i64.const_int(12345, false), "new_seed").map_err(llvm_err)?;
        self.builder.build_store(rf_seed_ptr, rf_new_seed).map_err(llvm_err)?;
        // Convert to float: (new_seed & 0x7fffffffffffffff) / 0x7fffffffffffffff
        let rf_mask = i64.const_int(0x7fffffffffffffff_u64, false);
        let rf_masked = self.builder.build_and(rf_new_seed, rf_mask, "masked").map_err(llvm_err)?;
        let rf_f64 = self.builder.build_unsigned_int_to_float(rf_masked, f64, "f64").map_err(llvm_err)?;
        let rf_divisor = f64.const_float(0x7fffffffffffffff_u64 as f64);
        let rf_result = self.builder.build_float_div(rf_f64, rf_divisor, "result").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&rf_result));


        Ok(())
    }
}
