use inkwell::IntPredicate;

use super::super::{CodeGen, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn define_rc_functions(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let f64 = self.f64_ty();
        let void = self.void_ty();
        let ptr = self.ptr_ty();
        let i8 = self.context.i8_type();

        // ---- atomic_pow(f64, f64) -> f64 ----
        let pow_fn = self.module.add_function("atomic_pow", f64.fn_type(&[f64.into(), f64.into()], false), None);
        let pow_entry = self.context.append_basic_block(pow_fn, "entry");
        self.builder.position_at_end(pow_entry);
        let pow_base = pow_fn.get_first_param().unwrap().into_float_value();
        let pow_exp = pow_fn.get_nth_param(1).unwrap().into_float_value();
        let pow_c_fn = self.module.get_function("pow").unwrap();
        let pow_r = self.builder.build_call(pow_c_fn, &[pow_base.into(), pow_exp.into()], "r").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_float_value();
        let _ = self.builder.build_return(Some(&pow_r));

        // ---- RC (Reference Counting) runtime ----
        // atomic_rc_inc(i8* ptr): increment refcount at ptr-8. Null-safe.
        let rc_inc_fn = self.module.add_function("atomic_rc_inc", void.fn_type(&[ptr.into()], false), None);
        let rc_inc_entry = self.context.append_basic_block(rc_inc_fn, "entry");
        let rc_inc_do = self.context.append_basic_block(rc_inc_fn, "do_inc");
        let rc_inc_done = self.context.append_basic_block(rc_inc_fn, "done");
        self.builder.position_at_end(rc_inc_entry);
        let rc_inc_ptr = rc_inc_fn.get_first_param().unwrap().into_pointer_value();
        let rc_is_null = self.builder.build_is_null(rc_inc_ptr, "is_null").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rc_is_null, rc_inc_done, rc_inc_do);
        self.builder.position_at_end(rc_inc_do);
        let rc_inc_i64 = self.builder.build_ptr_to_int(rc_inc_ptr, i64, "rc_i64").map_err(llvm_err)?;
        let rc_inc_minus8 = self.builder.build_int_sub(rc_inc_i64, i64.const_int(8, false), "minus8").map_err(llvm_err)?;
        let rc_inc_i64p = self.builder.build_int_to_ptr(rc_inc_minus8, ptr, "rc_i64p").map_err(llvm_err)?;
        let rc_inc_val = self.builder.build_load(self.i64_ty(), rc_inc_i64p, "rc").map_err(llvm_err)?.into_int_value();
        let rc_inc_new = self.builder.build_int_add(rc_inc_val, i64.const_int(1, false), "new_rc").map_err(llvm_err)?;
        let _ = self.builder.build_store(rc_inc_i64p, rc_inc_new).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rc_inc_done);
        self.builder.position_at_end(rc_inc_done);
        let _ = self.builder.build_return(None);

        // atomic_rc_dec(i8* ptr): decrement refcount at ptr-8, free if zero. Null-safe.
        let rc_dec_fn = self.module.add_function("atomic_rc_dec", void.fn_type(&[ptr.into()], false), None);
        let rc_dec_entry = self.context.append_basic_block(rc_dec_fn, "entry");
        let rc_dec_null_bb = self.context.append_basic_block(rc_dec_fn, "null_check");
        let rc_dec_free_bb = self.context.append_basic_block(rc_dec_fn, "do_free");
        let rc_dec_done_bb = self.context.append_basic_block(rc_dec_fn, "done");
        self.builder.position_at_end(rc_dec_entry);
        let rc_dec_ptr = rc_dec_fn.get_first_param().unwrap().into_pointer_value();
        let rc_is_null = self.builder.build_is_null(rc_dec_ptr, "is_null").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rc_is_null, rc_dec_done_bb, rc_dec_null_bb);
        self.builder.position_at_end(rc_dec_null_bb);
        let rc_dec_i64 = self.builder.build_ptr_to_int(rc_dec_ptr, i64, "rc_i64").map_err(llvm_err)?;
        let rc_dec_minus8 = self.builder.build_int_sub(rc_dec_i64, i64.const_int(8, false), "minus8").map_err(llvm_err)?;
        let rc_dec_i64p = self.builder.build_int_to_ptr(rc_dec_minus8, ptr, "rc_i64p").map_err(llvm_err)?;
        let rc_dec_val = self.builder.build_load(self.i64_ty(), rc_dec_i64p, "rc").map_err(llvm_err)?.into_int_value();
        let rc_dec_new = self.builder.build_int_sub(rc_dec_val, i64.const_int(1, false), "new_rc").map_err(llvm_err)?;
        let _ = self.builder.build_store(rc_dec_i64p, rc_dec_new).map_err(llvm_err)?;
        let rc_is_zero = self.builder.build_int_compare(IntPredicate::EQ, rc_dec_new, i64.const_int(0, false), "is_zero").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rc_is_zero, rc_dec_free_bb, rc_dec_done_bb);
        self.builder.position_at_end(rc_dec_free_bb);
        let free_func = self.module.get_function("free").unwrap();
        let rc_dec_free_ptr = self.builder.build_int_to_ptr(rc_dec_minus8, ptr, "free_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_call(free_func, &[rc_dec_free_ptr.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rc_dec_done_bb);
        self.builder.position_at_end(rc_dec_done_bb);
        let _ = self.builder.build_return(None);

        // atomic_malloc_rc body (declared early near malloc)
        let malloc_rc_fn = self.module.get_function("atomic_malloc_rc").unwrap();
        let malloc_rc_entry = self.context.append_basic_block(malloc_rc_fn, "entry");
        self.builder.position_at_end(malloc_rc_entry);
        let malloc_rc_size = malloc_rc_fn.get_first_param().unwrap().into_int_value();
        let malloc_rc_total = self.builder.build_int_add(malloc_rc_size, i64.const_int(8, false), "total").map_err(llvm_err)?;
        let malloc_rc_func = self.module.get_function("malloc").unwrap();
        let malloc_rc_raw = self.builder.build_call(malloc_rc_func, &[malloc_rc_total.into()], "raw").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let malloc_rc_i64p = self.builder.build_pointer_cast(malloc_rc_raw, ptr, "rc_i64p").map_err(llvm_err)?;
        let _ = self.builder.build_store(malloc_rc_i64p, i64.const_int(0, false)).map_err(llvm_err)?;
        let malloc_rc_data = unsafe { self.builder.build_gep(i8, malloc_rc_raw, &[i64.const_int(8, false)], "data").map_err(llvm_err) }?;
        let _ = self.builder.build_return(Some(&malloc_rc_data));

        // atomic_utf8_encode body: encode a Unicode code point into UTF-8 bytes
        let utf8_encode_fn_body = self.module.get_function("atomic_utf8_encode").unwrap();
        let utf8_entry = self.context.append_basic_block(utf8_encode_fn_body, "entry");
        let utf8_1b = self.context.append_basic_block(utf8_encode_fn_body, "one_byte");
        let utf8_2b = self.context.append_basic_block(utf8_encode_fn_body, "two_byte");
        let utf8_3b = self.context.append_basic_block(utf8_encode_fn_body, "three_byte");
        let utf8_4b = self.context.append_basic_block(utf8_encode_fn_body, "four_byte");
        self.builder.position_at_end(utf8_entry);
        let ucode = utf8_encode_fn_body.get_first_param().unwrap().into_int_value();
        let ubuf = utf8_encode_fn_body.get_nth_param(1).unwrap().into_pointer_value();
        let u0x7f = i64.const_int(0x7F, false);
        let u0x7ff = i64.const_int(0x7FF, false);
        let u0xffff = i64.const_int(0xFFFF, false);
        let is_1 = self.builder.build_int_compare(IntPredicate::ULE, ucode, u0x7f, "is1").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_1, utf8_1b, utf8_2b);
        self.builder.position_at_end(utf8_1b);
        let u1 = self.builder.build_int_truncate(ucode, i8, "u1").map_err(llvm_err)?;
        let _ = self.builder.build_store(ubuf, u1).map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&i64.const_int(1, false)));
        self.builder.position_at_end(utf8_2b);
        let is_2 = self.builder.build_int_compare(IntPredicate::ULE, ucode, u0x7ff, "is2").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_2, utf8_3b, utf8_4b);
        self.builder.position_at_end(utf8_3b);
        let u6 = i64.const_int(6, false);
        let ucp6 = self.builder.build_right_shift(ucode, u6, false, "cp6").map_err(llvm_err)?;
        let ulead2 = self.builder.build_or(
            self.builder.build_int_truncate(ucp6, i8, "l2t").map_err(llvm_err)?,
            i8.const_int(0xC0, false), "lead2"
        ).map_err(llvm_err)?;
        let _ = self.builder.build_store(ubuf, ulead2).map_err(llvm_err)?;
        let umask = i64.const_int(0x3F, false);
        let ucont2 = self.builder.build_and(ucode, umask, "cont2").map_err(llvm_err)?;
        let ub2 = self.builder.build_or(
            self.builder.build_int_truncate(ucont2, i8, "c2t").map_err(llvm_err)?,
            i8.const_int(0x80, false), "b2"
        ).map_err(llvm_err)?;
        let ugp1 = unsafe { self.builder.build_gep(i8, ubuf, &[i64.const_int(1, false)], "gp1").map_err(llvm_err) }?;
        let _ = self.builder.build_store(ugp1, ub2).map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&i64.const_int(2, false)));
        self.builder.position_at_end(utf8_4b);
        let is_3 = self.builder.build_int_compare(IntPredicate::ULE, ucode, u0xffff, "is3").map_err(llvm_err)?;
        let utf8_3b_write = self.context.append_basic_block(utf8_encode_fn_body, "three_byte_write");
        let utf8_4b_write = self.context.append_basic_block(utf8_encode_fn_body, "four_byte_write");
        let _ = self.builder.build_conditional_branch(is_3, utf8_3b_write, utf8_4b_write);
        self.builder.position_at_end(utf8_3b_write);
        let u12 = i64.const_int(12, false);
        let ucp12 = self.builder.build_right_shift(ucode, u12, false, "cp12").map_err(llvm_err)?;
        let ulead3 = self.builder.build_or(
            self.builder.build_int_truncate(ucp12, i8, "l3t").map_err(llvm_err)?,
            i8.const_int(0xE0, false), "lead3"
        ).map_err(llvm_err)?;
        let _ = self.builder.build_store(ubuf, ulead3).map_err(llvm_err)?;
        let ucp6b = self.builder.build_right_shift(ucode, u6, false, "cp6b").map_err(llvm_err)?;
        let ucont3_1 = self.builder.build_and(ucp6b, umask, "c3_1").map_err(llvm_err)?;
        let ub3_1 = self.builder.build_or(
            self.builder.build_int_truncate(ucont3_1, i8, "c3_1t").map_err(llvm_err)?,
            i8.const_int(0x80, false), "b3_1"
        ).map_err(llvm_err)?;
        let ugp3_1 = unsafe { self.builder.build_gep(i8, ubuf, &[i64.const_int(1, false)], "gp3_1").map_err(llvm_err) }?;
        let _ = self.builder.build_store(ugp3_1, ub3_1).map_err(llvm_err)?;
        let ucont3_2 = self.builder.build_and(ucode, umask, "c3_2").map_err(llvm_err)?;
        let ub3_2 = self.builder.build_or(
            self.builder.build_int_truncate(ucont3_2, i8, "c3_2t").map_err(llvm_err)?,
            i8.const_int(0x80, false), "b3_2"
        ).map_err(llvm_err)?;
        let ugp3_2 = unsafe { self.builder.build_gep(i8, ubuf, &[i64.const_int(2, false)], "gp3_2").map_err(llvm_err) }?;
        let _ = self.builder.build_store(ugp3_2, ub3_2).map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&i64.const_int(3, false)));
        self.builder.position_at_end(utf8_4b_write);
        let u18 = i64.const_int(18, false);
        let ucp18 = self.builder.build_right_shift(ucode, u18, false, "cp18").map_err(llvm_err)?;
        let ulead4 = self.builder.build_or(
            self.builder.build_int_truncate(ucp18, i8, "l4t").map_err(llvm_err)?,
            i8.const_int(0xF0, false), "lead4"
        ).map_err(llvm_err)?;
        let _ = self.builder.build_store(ubuf, ulead4).map_err(llvm_err)?;
        let u4_12 = i64.const_int(12, false);
        let u4_6 = i64.const_int(6, false);
        let ucp12b4 = self.builder.build_right_shift(ucode, u4_12, false, "cp12b4").map_err(llvm_err)?;
        let ucont4_1 = self.builder.build_and(ucp12b4, umask, "c4_1").map_err(llvm_err)?;
        let ub4_1 = self.builder.build_or(
            self.builder.build_int_truncate(ucont4_1, i8, "c4_1t").map_err(llvm_err)?,
            i8.const_int(0x80, false), "b4_1"
        ).map_err(llvm_err)?;
        let ugp4_1 = unsafe { self.builder.build_gep(i8, ubuf, &[i64.const_int(1, false)], "gp4_1").map_err(llvm_err) }?;
        let _ = self.builder.build_store(ugp4_1, ub4_1).map_err(llvm_err)?;
        let ucp6b4 = self.builder.build_right_shift(ucode, u4_6, false, "cp6b4").map_err(llvm_err)?;
        let ucont4_2 = self.builder.build_and(ucp6b4, umask, "c4_2").map_err(llvm_err)?;
        let ub4_2 = self.builder.build_or(
            self.builder.build_int_truncate(ucont4_2, i8, "c4_2t").map_err(llvm_err)?,
            i8.const_int(0x80, false), "b4_2"
        ).map_err(llvm_err)?;
        let ugp4_2 = unsafe { self.builder.build_gep(i8, ubuf, &[i64.const_int(2, false)], "gp4_2").map_err(llvm_err) }?;
        let _ = self.builder.build_store(ugp4_2, ub4_2).map_err(llvm_err)?;
        let ucont4_3 = self.builder.build_and(ucode, umask, "c4_3").map_err(llvm_err)?;
        let ub4_3 = self.builder.build_or(
            self.builder.build_int_truncate(ucont4_3, i8, "c4_3t").map_err(llvm_err)?,
            i8.const_int(0x80, false), "b4_3"
        ).map_err(llvm_err)?;
        let ugp4_3 = unsafe { self.builder.build_gep(i8, ubuf, &[i64.const_int(3, false)], "gp4_3").map_err(llvm_err) }?;
        let _ = self.builder.build_store(ugp4_3, ub4_3).map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&i64.const_int(4, false)));

        // atomic_utf8_byte_len body: determine UTF-8 byte count from leading byte
        let utf8_bl_fn = self.module.get_function("atomic_utf8_byte_len").unwrap();
        let bl_entry = self.context.append_basic_block(utf8_bl_fn, "entry");
        self.builder.position_at_end(bl_entry);
        let bl_byte = utf8_bl_fn.get_first_param().unwrap().into_int_value();
        let bl_byte_zext = self.builder.build_int_z_extend(bl_byte, i64, "zext").map_err(llvm_err)?;
        let bl_80 = i64.const_int(0x80, false);
        let is_ascii = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_and(bl_byte_zext, bl_80, "and80").map_err(llvm_err)?,
            i64.const_int(0, false), "is_ascii").map_err(llvm_err)?;
        let bl_e0 = i64.const_int(0xE0, false);
        let bl_c0 = i64.const_int(0xC0, false);
        let is_2b = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_and(bl_byte_zext, bl_e0, "andE0").map_err(llvm_err)?,
            bl_c0, "is_2b").map_err(llvm_err)?;
        let bl_f0 = i64.const_int(0xF0, false);
        let bl_e0c = i64.const_int(0xE0, false);
        let is_3b = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_and(bl_byte_zext, bl_f0, "andF0").map_err(llvm_err)?,
            bl_e0c, "is_3b").map_err(llvm_err)?;
        let bl_f8 = i64.const_int(0xF8, false);
        let bl_f0c = i64.const_int(0xF0, false);
        let _is_4b = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_and(bl_byte_zext, bl_f8, "andF8").map_err(llvm_err)?,
            bl_f0c, "is_4b").map_err(llvm_err)?;
        let one = i64.const_int(1, false);
        let two = i64.const_int(2, false);
        let three = i64.const_int(3, false);
        let four = i64.const_int(4, false);
        let bl_s3 = self.builder.build_select(is_3b, three, four, "s3").map_err(llvm_err)?.into_int_value();
        let bl_s2 = self.builder.build_select(is_2b, two, bl_s3, "s2").map_err(llvm_err)?.into_int_value();
        let bl_result = self.builder.build_select(is_ascii, one, bl_s2, "s1").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_return(Some(&bl_result));

        Ok(())
    }
}
