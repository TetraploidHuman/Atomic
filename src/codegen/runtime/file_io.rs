use inkwell::values::IntValue;
use inkwell::IntPredicate;

use super::super::{CodeGen, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn define_file_io(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let ptr = self.ptr_ty();
        let str_ty = self.string_type;
        let i32 = self.context.i32_type();
        let i8 = self.context.i8_type();

        // ---- atomic_read_line() -> {i64, ptr, i1} (string + success flag) ----
        // Allocates a 4096-byte buffer and calls fgets. Returns success=0 on EOF.
        let rl_ret_ty = self.context.struct_type(&[i64.into(), ptr.into(), self.bool_ty().into()], false);
        let rl_fn = self.module.add_function("atomic_read_line", rl_ret_ty.fn_type(&[], false), None);
        let _fgets_fn = self.module.add_function("fgets", ptr.fn_type(&[ptr.into(), i32.into(), ptr.into()], false), None);
        let entry = self.context.append_basic_block(rl_fn, "entry");
        self.builder.position_at_end(entry);
        let buf_size = i64.const_int(4096, false);
        let buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[buf_size.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Use external stdin symbol (FILE* from libc, declared as external pointer)
        let stdin_g = self.module.add_global(ptr, None, "stdin");
        // Load the stdin FILE* pointer value from the external global
        let stdin_ptr = self.builder.build_load(ptr, stdin_g.as_pointer_value(), "stdin_ptr").map_err(llvm_err)?.into_pointer_value();
        let fgets_ret = self.builder.build_call(self.module.get_function("fgets").unwrap(), &[buf.into(), i32.const_int(4096, false).into(), stdin_ptr.into()], "").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Check if fgets returned NULL (EOF/error)
        let is_eof = self.builder.build_int_compare(IntPredicate::EQ, fgets_ret, ptr.const_zero(), "is_eof").map_err(llvm_err)?;
        let eof_bb = self.context.append_basic_block(rl_fn, "eof");
        let ok_bb = self.context.append_basic_block(rl_fn, "ok");
        let merge_bb = self.context.append_basic_block(rl_fn, "merge");
        let _ = self.builder.build_conditional_branch(is_eof, eof_bb, ok_bb);
        // EOF path: return {0, null, 0}
        self.builder.position_at_end(eof_bb);
        let eof_undef = rl_ret_ty.get_undef();
        let eof_r1 = self.builder.build_insert_value(eof_undef, i64.const_int(0, false), 0, "eof_len").map_err(llvm_err)?;
        let eof_r2 = self.builder.build_insert_value(eof_r1, ptr.const_zero(), 1, "eof_ptr").map_err(llvm_err)?;
        let eof_r3 = self.builder.build_insert_value(eof_r2, self.bool_ty().const_zero(), 2, "eof_ok").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // OK path: compute length, strip newline
        self.builder.position_at_end(ok_bb);
        let str_len = self.builder.build_call(self.module.get_function("strlen").unwrap(), &[buf.into()], "len").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        // Strip trailing newline if present
        let last_idx = self.builder.build_int_sub(str_len, i64.const_int(1, false), "last_idx").map_err(llvm_err)?;
        let last_ptr = unsafe { self.builder.build_gep(i8, buf, &[last_idx], "last_ptr").map_err(llvm_err) }?;
        let last_ch = self.builder.build_load(i8, last_ptr, "last_ch").map_err(llvm_err)?.into_int_value();
        let is_nl = self.builder.build_int_compare(IntPredicate::EQ, last_ch, i8.const_int(10, false), "is_nl").map_err(llvm_err)?;
        let adj_len = self.builder.build_select(is_nl, last_idx, str_len, "adj_len").map_err(llvm_err)?;
        let ok_undef = rl_ret_ty.get_undef();
        let ok_r1 = self.builder.build_insert_value(ok_undef, adj_len.into_int_value(), 0, "ok_len").map_err(llvm_err)?;
        let ok_r2 = self.builder.build_insert_value(ok_r1, buf, 1, "ok_ptr").map_err(llvm_err)?;
        let ok_r3 = self.builder.build_insert_value(ok_r2, self.bool_ty().const_int(1, false), 2, "ok_ok").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // Merge
        self.builder.position_at_end(merge_bb);
        let rl_phi = self.builder.build_phi(rl_ret_ty, "rl_ret").map_err(llvm_err)?;
        rl_phi.add_incoming(&[(&eof_r3, eof_bb), (&ok_r3, ok_bb)]);
        let _ = self.builder.build_return(Some(&rl_phi.as_basic_value()));

        // ---- atomic_string_to_upper({i64, ptr}) -> {i64, ptr} ----
        let to_upper_fn = self.module.add_function("atomic_string_to_upper", str_ty.fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(to_upper_fn, "entry");
        self.builder.position_at_end(entry);
        let str_param = to_upper_fn.get_first_param().unwrap().into_struct_value();
        let str_len = self.builder.build_extract_value(str_param, 0, "len").map_err(llvm_err)?.into_int_value();
        let str_data = self.builder.build_extract_value(str_param, 1, "data").map_err(llvm_err)?.into_pointer_value();
        let alloc_len = self.builder.build_int_add(str_len, i64.const_int(1, false), "alloc_len").map_err(llvm_err)?;
        let new_buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[alloc_len.into()], "new_buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Loop: for i in 0..len, copy byte, convert if lowercase
        let loop_bb = self.context.append_basic_block(to_upper_fn, "loop");
        let body_bb = self.context.append_basic_block(to_upper_fn, "body");
        let done_bb = self.context.append_basic_block(to_upper_fn, "done");
        let i_alloca = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i64.const_int(0, false)).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_bb);
        self.builder.position_at_end(loop_bb);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let not_done = self.builder.build_int_compare(IntPredicate::ULT, i_val, str_len, "not_done").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(not_done, body_bb, done_bb);
        self.builder.position_at_end(body_bb);
        let src_ptr = unsafe { self.builder.build_gep(i8, str_data, &[i_val], "src_ptr").map_err(llvm_err) }?;
        let c = self.builder.build_load(i8, src_ptr, "c").map_err(llvm_err)?.into_int_value();
        let is_lower = self.builder.build_int_compare(IntPredicate::UGE, c, i8.const_int('a' as u64, false), "ge_a").map_err(llvm_err)?;
        let is_lower2 = self.builder.build_int_compare(IntPredicate::ULE, c, i8.const_int('z' as u64, false), "le_z").map_err(llvm_err)?;
        let is_lower_final = self.builder.build_and(is_lower, is_lower2, "is_lower").map_err(llvm_err)?;
        let upper_c = self.builder.build_int_sub(c, i8.const_int(32, false), "upper_c").map_err(llvm_err)?;
        let conv = self.builder.build_select(is_lower_final, upper_c, c, "conv").map_err(llvm_err)?.into_int_value();
        let dst_ptr = unsafe { self.builder.build_gep(i8, new_buf, &[i_val], "dst_ptr").map_err(llvm_err) }?;
        self.builder.build_store(dst_ptr, conv).map_err(llvm_err)?;
        let next_i = self.builder.build_int_add(i_val, i64.const_int(1, false), "next_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, next_i).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_bb);
        self.builder.position_at_end(done_bb);
        let null_gep = unsafe { self.builder.build_gep(i8, new_buf, &[str_len], "null_ptr").map_err(llvm_err) }?;
        self.builder.build_store(null_gep, i8.const_int(0, false)).map_err(llvm_err)?;
        let undef = str_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, str_len, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, new_buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r2));

        // ---- atomic_string_to_lower({i64, ptr}) -> {i64, ptr} ----
        let to_lower_fn = self.module.add_function("atomic_string_to_lower", str_ty.fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(to_lower_fn, "entry");
        self.builder.position_at_end(entry);
        let str_param = to_lower_fn.get_first_param().unwrap().into_struct_value();
        let str_len = self.builder.build_extract_value(str_param, 0, "len").map_err(llvm_err)?.into_int_value();
        let str_data = self.builder.build_extract_value(str_param, 1, "data").map_err(llvm_err)?.into_pointer_value();
        let alloc_len = self.builder.build_int_add(str_len, i64.const_int(1, false), "alloc_len").map_err(llvm_err)?;
        let new_buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[alloc_len.into()], "new_buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let loop_bb = self.context.append_basic_block(to_lower_fn, "loop");
        let body_bb = self.context.append_basic_block(to_lower_fn, "body");
        let done_bb = self.context.append_basic_block(to_lower_fn, "done");
        let i_alloca = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, i64.const_int(0, false)).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_bb);
        self.builder.position_at_end(loop_bb);
        let i_val = self.builder.build_load(i64, i_alloca, "i_val").map_err(llvm_err)?.into_int_value();
        let not_done = self.builder.build_int_compare(IntPredicate::ULT, i_val, str_len, "not_done").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(not_done, body_bb, done_bb);
        self.builder.position_at_end(body_bb);
        let src_ptr = unsafe { self.builder.build_gep(i8, str_data, &[i_val], "src_ptr").map_err(llvm_err) }?;
        let c = self.builder.build_load(i8, src_ptr, "c").map_err(llvm_err)?.into_int_value();
        let is_upper = self.builder.build_int_compare(IntPredicate::UGE, c, i8.const_int('A' as u64, false), "ge_A").map_err(llvm_err)?;
        let is_upper2 = self.builder.build_int_compare(IntPredicate::ULE, c, i8.const_int('Z' as u64, false), "le_Z").map_err(llvm_err)?;
        let is_upper_final = self.builder.build_and(is_upper, is_upper2, "is_upper").map_err(llvm_err)?;
        let lower_c = self.builder.build_int_add(c, i8.const_int(32, false), "lower_c").map_err(llvm_err)?;
        let conv = self.builder.build_select(is_upper_final, lower_c, c, "conv").map_err(llvm_err)?.into_int_value();
        let dst_ptr = unsafe { self.builder.build_gep(i8, new_buf, &[i_val], "dst_ptr").map_err(llvm_err) }?;
        self.builder.build_store(dst_ptr, conv).map_err(llvm_err)?;
        let next_i = self.builder.build_int_add(i_val, i64.const_int(1, false), "next_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, next_i).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_bb);
        self.builder.position_at_end(done_bb);
        let null_gep = unsafe { self.builder.build_gep(i8, new_buf, &[str_len], "null_ptr").map_err(llvm_err) }?;
        self.builder.build_store(null_gep, i8.const_int(0, false)).map_err(llvm_err)?;
        let undef = str_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, str_len, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, new_buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r2));

        // ---- atomic_string_trim({i64, ptr}) -> {i64, ptr} ----
        let trim_fn = self.module.add_function("atomic_string_trim", str_ty.fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(trim_fn, "entry");
        self.builder.position_at_end(entry);
        let str_param = trim_fn.get_first_param().unwrap().into_struct_value();
        let str_len = self.builder.build_extract_value(str_param, 0, "len").map_err(llvm_err)?.into_int_value();
        let str_data = self.builder.build_extract_value(str_param, 1, "data").map_err(llvm_err)?.into_pointer_value();

        // Helper to build is-whitespace check for a char value
        let build_is_ws = |builder: &inkwell::builder::Builder<'ctx>, c: IntValue<'ctx>|
            -> Result<IntValue<'ctx>, String>
        {
            let is_sp = builder.build_int_compare(IntPredicate::EQ, c, i8.const_int(b' ' as u64, false), "is_sp").map_err(llvm_err)?;
            let is_tab = builder.build_int_compare(IntPredicate::EQ, c, i8.const_int(b'\t' as u64, false), "is_tab").map_err(llvm_err)?;
            let is_nl = builder.build_int_compare(IntPredicate::EQ, c, i8.const_int(b'\n' as u64, false), "is_nl").map_err(llvm_err)?;
            let is_cr = builder.build_int_compare(IntPredicate::EQ, c, i8.const_int(b'\r' as u64, false), "is_cr").map_err(llvm_err)?;
            let ws1 = builder.build_or(is_sp, is_tab, "ws1").map_err(llvm_err)?;
            let ws2 = builder.build_or(is_nl, is_cr, "ws2").map_err(llvm_err)?;
            builder.build_or(ws1, ws2, "is_ws").map_err(llvm_err)
        };

        // Find start (left trim)
        let find_start_hdr = self.context.append_basic_block(trim_fn, "find_start_hdr");
        let find_start_body = self.context.append_basic_block(trim_fn, "find_start_body");
        let start_done = self.context.append_basic_block(trim_fn, "start_done");
        let start_idx = self.builder.build_alloca(i64, "start_idx").map_err(llvm_err)?;
        self.builder.build_store(start_idx, i64.const_int(0, false)).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(find_start_hdr);

        // find_start_hdr: while start < len
        self.builder.position_at_end(find_start_hdr);
        let si = self.builder.build_load(i64, start_idx, "si").map_err(llvm_err)?.into_int_value();
        let si_lt_len = self.builder.build_int_compare(IntPredicate::ULT, si, str_len, "si_lt_len").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(si_lt_len, find_start_body, start_done);

        self.builder.position_at_end(find_start_body);
        let sp = unsafe { self.builder.build_gep(i8, str_data, &[si], "sp").map_err(llvm_err) }?;
        let sc = self.builder.build_load(i8, sp, "sc").map_err(llvm_err)?.into_int_value();
        let is_ws = build_is_ws(&self.builder, sc)?;
        let si_plus1 = self.builder.build_int_add(si, i64.const_int(1, false), "si_plus1").map_err(llvm_err)?;
        let new_si = self.builder.build_select(is_ws, si_plus1, si, "new_si").map_err(llvm_err)?.into_int_value();
        self.builder.build_store(start_idx, new_si).map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_ws, find_start_hdr, start_done);

        // Find end (right trim) - similar loop going backwards
        self.builder.position_at_end(start_done);
        let find_end_hdr = self.context.append_basic_block(trim_fn, "find_end_hdr");
        let find_end_body = self.context.append_basic_block(trim_fn, "find_end_body");
        let end_done = self.context.append_basic_block(trim_fn, "end_done");
        let end_idx = self.builder.build_alloca(i64, "end_idx").map_err(llvm_err)?;
        self.builder.build_store(end_idx, str_len).map_err(llvm_err)?;
        // Load start value here so it dominates uses in end_done
        let final_si = self.builder.build_load(i64, start_idx, "final_si").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_unconditional_branch(find_end_hdr);

        // find_end_hdr: while end > start
        self.builder.position_at_end(find_end_hdr);
        let ei = self.builder.build_load(i64, end_idx, "ei").map_err(llvm_err)?.into_int_value();
        let ei_gt_si = self.builder.build_int_compare(IntPredicate::UGT, ei, final_si, "ei_gt_si").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(ei_gt_si, find_end_body, end_done);

        self.builder.position_at_end(find_end_body);
        let ei_minus1 = self.builder.build_int_sub(ei, i64.const_int(1, false), "ei_minus1").map_err(llvm_err)?;
        let ep = unsafe { self.builder.build_gep(i8, str_data, &[ei_minus1], "ep").map_err(llvm_err) }?;
        let ec = self.builder.build_load(i8, ep, "ec").map_err(llvm_err)?.into_int_value();
        let is_ws = build_is_ws(&self.builder, ec)?;
        let new_ei = self.builder.build_select(is_ws, ei_minus1, ei, "new_ei").map_err(llvm_err)?.into_int_value();
        self.builder.build_store(end_idx, new_ei).map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_ws, find_end_hdr, end_done);

        // end_done: allocate and copy
        self.builder.position_at_end(end_done);
        // Reload end since it might have changed in the loop
        let final_ei = self.builder.build_load(i64, end_idx, "final_ei").map_err(llvm_err)?.into_int_value();
        let new_len = self.builder.build_int_sub(final_ei, final_si, "new_len").map_err(llvm_err)?;
        // Allocate new_len + 1 for null terminator
        let alloc_len = self.builder.build_int_add(new_len, i64.const_int(1, false), "alloc_len").map_err(llvm_err)?;
        let new_buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[alloc_len.into()], "new_buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let src_offset = unsafe { self.builder.build_gep(i8, str_data, &[final_si], "src_offset").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[new_buf.into(), src_offset.into(), new_len.into()], "").map_err(llvm_err)?;
        // Null terminate
        let null_gep = unsafe { self.builder.build_gep(i8, new_buf, &[new_len], "null_ptr").map_err(llvm_err) }?;
        self.builder.build_store(null_gep, i8.const_int(0, false)).map_err(llvm_err)?;
        // Return {new_len, new_buf}
        let undef = str_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, new_len, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, new_buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r2));

        // ---- atomic_string_starts_with({i64, ptr}, {i64, ptr}) -> i1 ----
        let sw_fn = self.module.add_function("atomic_string_starts_with",
            self.bool_ty().fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let sw_entry = self.context.append_basic_block(sw_fn, "entry");
        self.builder.position_at_end(sw_entry);
        let sw_s = sw_fn.get_first_param().unwrap().into_struct_value();
        let sw_pre = sw_fn.get_nth_param(1).unwrap().into_struct_value();
        let sw_slen = self.builder.build_extract_value(sw_s, 0, "slen").map_err(llvm_err)?.into_int_value();
        let sw_plen = self.builder.build_extract_value(sw_pre, 0, "plen").map_err(llvm_err)?.into_int_value();
        let sw_sdata = self.builder.build_extract_value(sw_s, 1, "sdata").map_err(llvm_err)?.into_pointer_value();
        let sw_pdata = self.builder.build_extract_value(sw_pre, 1, "pdata").map_err(llvm_err)?.into_pointer_value();
        let sw_len_ok = self.builder.build_int_compare(IntPredicate::UGE, sw_slen, sw_plen, "len_ok").map_err(llvm_err)?;
        let sw_check = self.context.append_basic_block(sw_fn, "check");
        let sw_cmp = self.context.append_basic_block(sw_fn, "cmp");
        let sw_false = self.context.append_basic_block(sw_fn, "false");
        let sw_done = self.context.append_basic_block(sw_fn, "done");
        let _ = self.builder.build_conditional_branch(sw_len_ok, sw_check, sw_false);
        // check: empty prefix → true, else → cmp
        self.builder.position_at_end(sw_check);
        let sw_pz = self.builder.build_int_compare(IntPredicate::EQ, sw_plen, i64.const_int(0, false), "pz").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(sw_pz, sw_done, sw_cmp);
        // cmp: memcmp
        self.builder.position_at_end(sw_cmp);
        let sw_mc = self.builder.build_call(self.module.get_function("memcmp").unwrap(), &[sw_sdata.into(), sw_pdata.into(), sw_plen.into()], "mc").map_err(llvm_err)?;
        let sw_mcr = sw_mc.try_as_basic_value().basic().unwrap().into_int_value();
        let sw_eq = self.builder.build_int_compare(IntPredicate::EQ, sw_mcr, i32.const_int(0, false), "eq").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sw_done);
        // false
        self.builder.position_at_end(sw_false);
        let _ = self.builder.build_unconditional_branch(sw_done);
        // done: phi [pz from check, eq from cmp, false from false]
        self.builder.position_at_end(sw_done);
        let sw_phi = self.builder.build_phi(self.bool_ty(), "sw_result").map_err(llvm_err)?;
        sw_phi.add_incoming(&[(&sw_pz, sw_check), (&sw_eq, sw_cmp), (&self.bool_ty().const_int(0, false), sw_false)]);
        let _ = self.builder.build_return(Some(&sw_phi.as_basic_value()));

        // ---- atomic_string_ends_with({i64, ptr}, {i64, ptr}) -> i1 ----
        let ew_fn = self.module.add_function("atomic_string_ends_with",
            self.bool_ty().fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let ew_entry = self.context.append_basic_block(ew_fn, "entry");
        self.builder.position_at_end(ew_entry);
        let ew_s = ew_fn.get_first_param().unwrap().into_struct_value();
        let ew_suf = ew_fn.get_nth_param(1).unwrap().into_struct_value();
        let ew_slen = self.builder.build_extract_value(ew_s, 0, "slen").map_err(llvm_err)?.into_int_value();
        let ew_suflen = self.builder.build_extract_value(ew_suf, 0, "suflen").map_err(llvm_err)?.into_int_value();
        let ew_sdata = self.builder.build_extract_value(ew_s, 1, "sdata").map_err(llvm_err)?.into_pointer_value();
        let ew_sufdata = self.builder.build_extract_value(ew_suf, 1, "sufdata").map_err(llvm_err)?.into_pointer_value();
        let ew_len_ok = self.builder.build_int_compare(IntPredicate::UGE, ew_slen, ew_suflen, "len_ok").map_err(llvm_err)?;
        let ew_check = self.context.append_basic_block(ew_fn, "check");
        let ew_cmp = self.context.append_basic_block(ew_fn, "cmp");
        let ew_false = self.context.append_basic_block(ew_fn, "false");
        let ew_done = self.context.append_basic_block(ew_fn, "done");
        let _ = self.builder.build_conditional_branch(ew_len_ok, ew_check, ew_false);
        // check: empty suffix → true, else → cmp
        self.builder.position_at_end(ew_check);
        let ew_sufz = self.builder.build_int_compare(IntPredicate::EQ, ew_suflen, i64.const_int(0, false), "sufz").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(ew_sufz, ew_done, ew_cmp);
        // cmp: memcmp from offset len-suffixlen
        self.builder.position_at_end(ew_cmp);
        let ew_off = self.builder.build_int_sub(ew_slen, ew_suflen, "off").map_err(llvm_err)?;
        let ew_sp = unsafe { self.builder.build_gep(i8, ew_sdata, &[ew_off], "sp").map_err(llvm_err) }?;
        let ew_mc = self.builder.build_call(self.module.get_function("memcmp").unwrap(), &[ew_sp.into(), ew_sufdata.into(), ew_suflen.into()], "mc").map_err(llvm_err)?;
        let ew_mcr = ew_mc.try_as_basic_value().basic().unwrap().into_int_value();
        let ew_eq = self.builder.build_int_compare(IntPredicate::EQ, ew_mcr, i32.const_int(0, false), "eq").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(ew_done);
        // false
        self.builder.position_at_end(ew_false);
        let _ = self.builder.build_unconditional_branch(ew_done);
        // done: phi [sufz from check, eq from cmp, false from false]
        self.builder.position_at_end(ew_done);
        let ew_phi = self.builder.build_phi(self.bool_ty(), "ew_result").map_err(llvm_err)?;
        ew_phi.add_incoming(&[(&ew_sufz, ew_check), (&ew_eq, ew_cmp), (&self.bool_ty().const_int(0, false), ew_false)]);
        let _ = self.builder.build_return(Some(&ew_phi.as_basic_value()));

        // ---- atomic_string_substring({i64, ptr}, i64 start, i64 len) -> {i64, ptr} ----
        let sub_fn = self.module.add_function("atomic_string_substring",
            str_ty.fn_type(&[str_ty.into(), i64.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(sub_fn, "entry");
        self.builder.position_at_end(entry);
        let sub_s = sub_fn.get_first_param().unwrap().into_struct_value();
        let sub_start = sub_fn.get_nth_param(1).unwrap().into_int_value();
        let sub_len = sub_fn.get_nth_param(2).unwrap().into_int_value();
        let sub_slen = self.builder.build_extract_value(sub_s, 0, "slen").map_err(llvm_err)?.into_int_value();
        let sub_sdata = self.builder.build_extract_value(sub_s, 1, "sdata").map_err(llvm_err)?.into_pointer_value();
        // Clamp: if start >= slen, return empty string
        let sub_start_ok = self.builder.build_int_compare(IntPredicate::ULT, sub_start, sub_slen, "start_ok").map_err(llvm_err)?;
        let sub_end = self.builder.build_int_add(sub_start, sub_len, "end").map_err(llvm_err)?;
        let sub_end_ok = self.builder.build_int_compare(IntPredicate::ULE, sub_end, sub_slen, "end_ok").map_err(llvm_err)?;
        let sub_clamped_end = self.builder.build_select(sub_end_ok, sub_end, sub_slen, "clamped_end").map_err(llvm_err)?.into_int_value();
        let sub_actual_len = self.builder.build_int_sub(sub_clamped_end, sub_start, "actual_len").map_err(llvm_err)?;
        let sub_clamped_start = self.builder.build_select(sub_start_ok, sub_start, sub_slen, "clamped_start").map_err(llvm_err)?.into_int_value();
        let _sub_zero_len = self.builder.build_int_compare(IntPredicate::EQ, sub_actual_len, i64.const_int(0, false), "zero_len").map_err(llvm_err)?;
        // Allocate and copy
        let sub_alc = self.builder.build_int_add(sub_actual_len, i64.const_int(1, false), "alc").map_err(llvm_err)?;
        let sub_buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[sub_alc.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let sub_src = unsafe { self.builder.build_gep(i8, sub_sdata, &[sub_clamped_start], "src").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[sub_buf.into(), sub_src.into(), sub_actual_len.into()], "").map_err(llvm_err)?;
        let sub_null = unsafe { self.builder.build_gep(i8, sub_buf, &[sub_actual_len], "null").map_err(llvm_err) }?;
        self.builder.build_store(sub_null, i8.const_int(0, false)).map_err(llvm_err)?;
        let sub_undef = str_ty.get_undef();
        let sub_r1 = self.builder.build_insert_value(sub_undef, sub_actual_len, 0, "r1").map_err(llvm_err)?;
        let sub_r2 = self.builder.build_insert_value(sub_r1, sub_buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&sub_r2));

        // ---- atomic_parse_int({i64, ptr}) -> {i64, i1} (value, success) ----
        let pi_ret_ty = self.context.struct_type(&[i64.into(), self.bool_ty().into()], false);
        let pi_fn = self.module.add_function("atomic_parse_int",
            pi_ret_ty.fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(pi_fn, "entry");
        self.builder.position_at_end(entry);
        let pi_s = pi_fn.get_first_param().unwrap().into_struct_value();
        let pi_len = self.builder.build_extract_value(pi_s, 0, "len").map_err(llvm_err)?.into_int_value();
        let pi_data = self.builder.build_extract_value(pi_s, 1, "data").map_err(llvm_err)?.into_pointer_value();
        // Initialize result=0, sign=1, i=0, valid=0
        let pi_result = self.builder.build_alloca(i64, "result").map_err(llvm_err)?;
        let pi_sign = self.builder.build_alloca(i64, "sign").map_err(llvm_err)?;
        let pi_i = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        let pi_valid = self.builder.build_alloca(self.bool_ty(), "valid").map_err(llvm_err)?;
        self.builder.build_store(pi_result, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(pi_sign, i64.const_int(1, false)).map_err(llvm_err)?;
        self.builder.build_store(pi_i, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(pi_valid, self.bool_ty().const_zero()).map_err(llvm_err)?;
        // Check for leading '-'
        let pi_has_chars = self.builder.build_int_compare(IntPredicate::UGT, pi_len, i64.const_int(0, false), "has_chars").map_err(llvm_err)?;
        let pi_ck = self.context.append_basic_block(pi_fn, "check_sign");
        let pi_setup = self.context.append_basic_block(pi_fn, "setup");
        let pi_loop_hdr = self.context.append_basic_block(pi_fn, "loop_hdr");
        let pi_loop_body = self.context.append_basic_block(pi_fn, "loop_body");
        let pi_done = self.context.append_basic_block(pi_fn, "done");
        let _ = self.builder.build_conditional_branch(pi_has_chars, pi_ck, pi_done);

        // check_sign: check first char for '-', then branch to setup
        self.builder.position_at_end(pi_ck);
        let pi_first = self.builder.build_load(i8, pi_data, "first").map_err(llvm_err)?.into_int_value();
        let pi_is_minus = self.builder.build_int_compare(IntPredicate::EQ, pi_first, i8.const_int(b'-' as u64, false), "is_minus").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(pi_setup);

        // setup: set sign and start index based on whether first char is '-'
        self.builder.position_at_end(pi_setup);
        let pi_sign_val = self.builder.build_select(pi_is_minus, i64.const_int(0xffffffffffffffffu64, true), i64.const_int(1, false), "sign_val").map_err(llvm_err)?.into_int_value();
        let pi_start_i = self.builder.build_select(pi_is_minus, i64.const_int(1, false), i64.const_int(0, false), "start_i").map_err(llvm_err)?.into_int_value();
        self.builder.build_store(pi_sign, pi_sign_val).map_err(llvm_err)?;
        self.builder.build_store(pi_i, pi_start_i).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(pi_loop_hdr);

        self.builder.position_at_end(pi_loop_hdr);
        let pi_iv = self.builder.build_load(i64, pi_i, "iv").map_err(llvm_err)?.into_int_value();
        let pi_not_done = self.builder.build_int_compare(IntPredicate::ULT, pi_iv, pi_len, "not_done").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(pi_not_done, pi_loop_body, pi_done);

        self.builder.position_at_end(pi_loop_body);
        let pi_chp = unsafe { self.builder.build_gep(i8, pi_data, &[pi_iv], "chp").map_err(llvm_err) }?;
        let pi_ch = self.builder.build_load(i8, pi_chp, "ch").map_err(llvm_err)?.into_int_value();
        let pi_is_digit = self.builder.build_int_compare(IntPredicate::UGE, pi_ch, i8.const_int(b'0' as u64, false), "ge0").map_err(llvm_err)?;
        let pi_is_digit2 = self.builder.build_int_compare(IntPredicate::ULE, pi_ch, i8.const_int(b'9' as u64, false), "le9").map_err(llvm_err)?;
        let pi_is_d = self.builder.build_and(pi_is_digit, pi_is_digit2, "is_digit").map_err(llvm_err)?;
        let pi_body_ck = self.context.append_basic_block(pi_fn, "body_ck");
        let pi_body_next = self.context.append_basic_block(pi_fn, "body_next");
        let _ = self.builder.build_conditional_branch(pi_is_d, pi_body_ck, pi_done);

        self.builder.position_at_end(pi_body_ck);
        let pi_cur = self.builder.build_load(i64, pi_result, "cur").map_err(llvm_err)?.into_int_value();
        let pi_mul = self.builder.build_int_mul(pi_cur, i64.const_int(10, false), "mul").map_err(llvm_err)?;
        let pi_dval = self.builder.build_int_sub(pi_ch, i8.const_int(b'0' as u64, false), "dval").map_err(llvm_err)?;
        let pi_dval64 = self.builder.build_int_z_extend(pi_dval, i64, "dval64").map_err(llvm_err)?;
        let pi_add = self.builder.build_int_add(pi_mul, pi_dval64, "add").map_err(llvm_err)?;
        self.builder.build_store(pi_result, pi_add).map_err(llvm_err)?;
        self.builder.build_store(pi_valid, self.bool_ty().const_int(1, false)).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(pi_body_next);

        self.builder.position_at_end(pi_body_next);
        let pi_niv = self.builder.build_int_add(pi_iv, i64.const_int(1, false), "niv").map_err(llvm_err)?;
        self.builder.build_store(pi_i, pi_niv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(pi_loop_hdr);

        self.builder.position_at_end(pi_done);
        let pi_final = self.builder.build_load(i64, pi_result, "final").map_err(llvm_err)?.into_int_value();
        let pi_final_sign = self.builder.build_load(i64, pi_sign, "final_sign").map_err(llvm_err)?.into_int_value();
        let pi_mul_sign = self.builder.build_int_mul(pi_final, pi_final_sign, "mul_sign").map_err(llvm_err)?;
        let pi_valid_val = self.builder.build_load(self.bool_ty(), pi_valid, "valid_val").map_err(llvm_err)?.into_int_value();
        let pi_ret_undef = pi_ret_ty.get_undef();
        let pi_ret1 = self.builder.build_insert_value(pi_ret_undef, pi_mul_sign, 0, "ret_val").map_err(llvm_err)?;
        let pi_ret2 = self.builder.build_insert_value(pi_ret1, pi_valid_val, 1, "ret_ok").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&pi_ret2));

        // ---- atomic_read_file({i64, ptr}) -> {i64, ptr} ----
        let rf_fn = self.module.add_function("atomic_read_file",
            str_ty.fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(rf_fn, "entry");
        self.builder.position_at_end(entry);
        let rf_path_s = rf_fn.get_first_param().unwrap().into_struct_value();
        let rf_path_data = self.builder.build_extract_value(rf_path_s, 1, "path_data").map_err(llvm_err)?.into_pointer_value();
        let rf_mode = self.make_global_str(".rf_mode", b"rb\0");
        let rf_file = self.builder.build_call(self.module.get_function("fopen").unwrap(), &[rf_path_data.into(), rf_mode.into()], "file").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let rf_null = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_ptr_to_int(rf_file, i64, "rf_i64").map_err(llvm_err)?,
            i64.const_int(0, false), "rf_null").map_err(llvm_err)?;
        let rf_open_ok = self.context.append_basic_block(rf_fn, "open_ok");
        let rf_fail = self.context.append_basic_block(rf_fn, "fail");
        let _ = self.builder.build_conditional_branch(rf_null, rf_fail, rf_open_ok);

        // Fail: return empty string
        self.builder.position_at_end(rf_fail);
        let rf_e_undef = str_ty.get_undef();
        let rf_e_r1 = self.builder.build_insert_value(rf_e_undef, i64.const_int(0, false), 0, "r1").map_err(llvm_err)?;
        let rf_e_r2 = self.builder.build_insert_value(rf_e_r1,
            self.builder.build_int_to_ptr(i64.const_int(0, false), ptr, "nullp").map_err(llvm_err)?, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&rf_e_r2));

        // Open ok: seek to end, get size, read, return
        self.builder.position_at_end(rf_open_ok);
        // fseek(file, 0, 2) from end
        let _ = self.builder.build_call(self.module.get_function("fseek").unwrap(), &[rf_file.into(), i64.const_int(0, false).into(), i32.const_int(2, false).into()], "").map_err(llvm_err)?;
        let rf_size = self.builder.build_call(self.module.get_function("ftell").unwrap(), &[rf_file.into()], "size").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        // Rewind
        let _ = self.builder.build_call(self.module.get_function("fseek").unwrap(), &[rf_file.into(), i64.const_int(0, false).into(), i32.const_int(0, false).into()], "").map_err(llvm_err)?;
        // Allocate size+1, read, null-terminate
        let rf_alc = self.builder.build_int_add(rf_size, i64.const_int(1, false), "alc").map_err(llvm_err)?;
        let rf_buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[rf_alc.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let _ = self.builder.build_call(self.module.get_function("fread").unwrap(), &[rf_buf.into(), i64.const_int(1, false).into(), rf_size.into(), rf_file.into()], "").map_err(llvm_err)?;
        let rf_null_gep = unsafe { self.builder.build_gep(i8, rf_buf, &[rf_size], "null_gep").map_err(llvm_err) }?;
        self.builder.build_store(rf_null_gep, i8.const_int(0, false)).map_err(llvm_err)?;
        let _ = self.builder.build_call(self.module.get_function("fclose").unwrap(), &[rf_file.into()], "").map_err(llvm_err)?;
        let rf_und = str_ty.get_undef();
        let rf_r1 = self.builder.build_insert_value(rf_und, rf_size, 0, "r1").map_err(llvm_err)?;
        let rf_r2 = self.builder.build_insert_value(rf_r1, rf_buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&rf_r2));

        // ---- atomic_write_file({i64, ptr}, {i64, ptr}) -> i1 ----
        let wf_fn = self.module.add_function("atomic_write_file",
            self.bool_ty().fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let entry = self.context.append_basic_block(wf_fn, "entry");
        self.builder.position_at_end(entry);
        let wf_path = wf_fn.get_first_param().unwrap().into_struct_value();
        let wf_content = wf_fn.get_nth_param(1).unwrap().into_struct_value();
        let wf_pdata = self.builder.build_extract_value(wf_path, 1, "pdata").map_err(llvm_err)?.into_pointer_value();
        let wf_clen = self.builder.build_extract_value(wf_content, 0, "clen").map_err(llvm_err)?.into_int_value();
        let wf_cdata = self.builder.build_extract_value(wf_content, 1, "cdata").map_err(llvm_err)?.into_pointer_value();
        let wf_wmode = self.make_global_str(".wf_mode", b"wb\0");
        let wf_file = self.builder.build_call(self.module.get_function("fopen").unwrap(), &[wf_pdata.into(), wf_wmode.into()], "file").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let wf_null = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_ptr_to_int(wf_file, i64, "wf_i64").map_err(llvm_err)?,
            i64.const_int(0, false), "wf_null").map_err(llvm_err)?;
        let wf_open_ok = self.context.append_basic_block(wf_fn, "open_ok");
        let wf_fail = self.context.append_basic_block(wf_fn, "wf_fail");
        let wf_done = self.context.append_basic_block(wf_fn, "wf_done");
        let _ = self.builder.build_conditional_branch(wf_null, wf_fail, wf_open_ok);
        self.builder.position_at_end(wf_fail);
        let _ = self.builder.build_unconditional_branch(wf_done);
        self.builder.position_at_end(wf_open_ok);
        let _ = self.builder.build_call(self.module.get_function("fwrite").unwrap(), &[wf_cdata.into(), i64.const_int(1, false).into(), wf_clen.into(), wf_file.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_call(self.module.get_function("fclose").unwrap(), &[wf_file.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(wf_done);
        self.builder.position_at_end(wf_done);
        let wf_phi = self.builder.build_phi(self.bool_ty(), "wf_ok").map_err(llvm_err)?;
        wf_phi.add_incoming(&[(&self.bool_ty().const_int(0, false), wf_fail), (&self.bool_ty().const_int(1, false), wf_open_ok)]);
        let _ = self.builder.build_return(Some(&wf_phi.as_basic_value()));

        // ---- atomic_file_exists({i64, ptr}) -> i1 ----
        let fe_fn = self.module.add_function("atomic_file_exists",
            self.bool_ty().fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(fe_fn, "entry");
        self.builder.position_at_end(entry);
        let fe_path = fe_fn.get_first_param().unwrap().into_struct_value();
        let fe_pdata = self.builder.build_extract_value(fe_path, 1, "pdata").map_err(llvm_err)?.into_pointer_value();
        let fe_mode = self.make_global_str(".fe_mode", b"r\0");
        let fe_file = self.builder.build_call(self.module.get_function("fopen").unwrap(), &[fe_pdata.into(), fe_mode.into()], "file").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let fe_null = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_ptr_to_int(fe_file, i64, "fe_i64").map_err(llvm_err)?,
            i64.const_int(0, false), "fe_null").map_err(llvm_err)?;
        let fe_exists_bb = self.context.append_basic_block(fe_fn, "exists_ok");
        let fe_not_bb = self.context.append_basic_block(fe_fn, "fe_done");
        let _ = self.builder.build_conditional_branch(fe_null, fe_not_bb, fe_exists_bb);
        self.builder.position_at_end(fe_exists_bb);
        let _ = self.builder.build_call(self.module.get_function("fclose").unwrap(), &[fe_file.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(fe_not_bb);
        self.builder.position_at_end(fe_not_bb);
        let fe_phi = self.builder.build_phi(self.bool_ty(), "fe_exists").map_err(llvm_err)?;
        fe_phi.add_incoming(&[(&self.bool_ty().const_int(0, false), entry), (&self.bool_ty().const_int(1, false), fe_exists_bb)]);
        let _ = self.builder.build_return(Some(&fe_phi.as_basic_value()));

        // ---- atomic_file_append({i64, ptr}, {i64, ptr}) -> i1 ----
        let fa_fn = self.module.add_function("atomic_file_append",
            self.bool_ty().fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let entry = self.context.append_basic_block(fa_fn, "entry");
        self.builder.position_at_end(entry);
        let fa_path = fa_fn.get_first_param().unwrap().into_struct_value();
        let fa_content = fa_fn.get_nth_param(1).unwrap().into_struct_value();
        let fa_pdata = self.builder.build_extract_value(fa_path, 1, "pdata").map_err(llvm_err)?.into_pointer_value();
        let fa_clen = self.builder.build_extract_value(fa_content, 0, "clen").map_err(llvm_err)?.into_int_value();
        let fa_cdata = self.builder.build_extract_value(fa_content, 1, "cdata").map_err(llvm_err)?.into_pointer_value();
        let fa_amode = self.make_global_str(".fa_mode", b"a\0");
        let fa_file = self.builder.build_call(self.module.get_function("fopen").unwrap(), &[fa_pdata.into(), fa_amode.into()], "file").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let fa_null = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_ptr_to_int(fa_file, i64, "fa_i64").map_err(llvm_err)?,
            i64.const_int(0, false), "fa_null").map_err(llvm_err)?;
        let fa_open_ok = self.context.append_basic_block(fa_fn, "open_ok");
        let fa_fail = self.context.append_basic_block(fa_fn, "fa_fail");
        let fa_done = self.context.append_basic_block(fa_fn, "fa_done");
        let _ = self.builder.build_conditional_branch(fa_null, fa_fail, fa_open_ok);
        self.builder.position_at_end(fa_fail);
        let _ = self.builder.build_unconditional_branch(fa_done);
        self.builder.position_at_end(fa_open_ok);
        let _ = self.builder.build_call(self.module.get_function("fwrite").unwrap(), &[fa_cdata.into(), i64.const_int(1, false).into(), fa_clen.into(), fa_file.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_call(self.module.get_function("fclose").unwrap(), &[fa_file.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(fa_done);
        self.builder.position_at_end(fa_done);
        let fa_phi = self.builder.build_phi(self.bool_ty(), "fa_ok").map_err(llvm_err)?;
        fa_phi.add_incoming(&[(&self.bool_ty().const_int(0, false), fa_fail), (&self.bool_ty().const_int(1, false), fa_open_ok)]);
        let _ = self.builder.build_return(Some(&fa_phi.as_basic_value()));

        // ---- atomic_file_delete({i64, ptr}) -> i1 ----
        let fd_fn = self.module.add_function("atomic_file_delete",
            self.bool_ty().fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(fd_fn, "entry");
        self.builder.position_at_end(entry);
        let fd_path = fd_fn.get_first_param().unwrap().into_struct_value();
        let fd_pdata = self.builder.build_extract_value(fd_path, 1, "pdata").map_err(llvm_err)?.into_pointer_value();
        let _remove_fn = self.module.get_function("remove").unwrap();
        let fd_ret = self.builder.build_call(self.module.get_function("remove").unwrap(), &[fd_pdata.into()], "ret").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let fd_ok = self.builder.build_int_compare(IntPredicate::EQ, fd_ret, self.i32_ty().const_int(0, false), "fd_ok").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&fd_ok));

        // ---- Streaming File I/O Runtime Functions ----

        // ---- atomic_file_open({i64, ptr}, {i64, ptr}) -> ptr (FILE*) ----
        // Opens a file at path with mode. Returns FILE* (null on failure).
        let fo_fn = self.module.add_function("atomic_file_open",
            ptr.fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let entry = self.context.append_basic_block(fo_fn, "entry");
        self.builder.position_at_end(entry);
        let fo_path = fo_fn.get_first_param().unwrap().into_struct_value();
        let fo_mode = fo_fn.get_nth_param(1).unwrap().into_struct_value();
        let fo_pdata = self.builder.build_extract_value(fo_path, 1, "pdata").map_err(llvm_err)?.into_pointer_value();
        let fo_mdata = self.builder.build_extract_value(fo_mode, 1, "mdata").map_err(llvm_err)?.into_pointer_value();
        let fo_file = self.builder.build_call(self.module.get_function("fopen").unwrap(), &[fo_pdata.into(), fo_mdata.into()], "file").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let _ = self.builder.build_return(Some(&fo_file));

        // ---- atomic_file_close(ptr) -> i32 ----
        // Closes a file handle. Returns 0 on success, EOF on failure.
        let fc_fn = self.module.add_function("atomic_file_close",
            i32.fn_type(&[ptr.into()], false), None);
        let entry = self.context.append_basic_block(fc_fn, "entry");
        self.builder.position_at_end(entry);
        let fc_handle = fc_fn.get_first_param().unwrap().into_pointer_value();
        let fc_ret = self.builder.build_call(self.module.get_function("fclose").unwrap(), &[fc_handle.into()], "ret").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let _ = self.builder.build_return(Some(&fc_ret));

        // ---- atomic_file_eof(ptr) -> i1 ----
        // Checks if file handle is at EOF. Uses feof().
        let feof_c_fn = self.module.add_function("feof", i32.fn_type(&[ptr.into()], false), None);
        let fe_fn = self.module.add_function("atomic_file_eof",
            self.bool_ty().fn_type(&[ptr.into()], false), None);
        let entry = self.context.append_basic_block(fe_fn, "entry");
        self.builder.position_at_end(entry);
        let fe_handle = fe_fn.get_first_param().unwrap().into_pointer_value();
        let fe_ret = self.builder.build_call(feof_c_fn, &[fe_handle.into()], "ret").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let fe_ok = self.builder.build_int_compare(IntPredicate::NE, fe_ret, i32.const_int(0, false), "is_eof").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&fe_ok));

        // ---- atomic_file_read_line(ptr) -> {i64, ptr, i1} (len, data, success) ----
        // Reads one line from file handle. Returns string + success flag (0 on EOF).
        // Uses fgets with a 4096-byte buffer.
        let frl_ret_ty = self.context.struct_type(&[i64.into(), ptr.into(), self.bool_ty().into()], false);
        let frl_fn = self.module.add_function("atomic_file_read_line",
            frl_ret_ty.fn_type(&[ptr.into()], false), None);
        let _fgets_fn = self.module.get_function("fgets").unwrap();
        let entry = self.context.append_basic_block(frl_fn, "entry");
        self.builder.position_at_end(entry);
        let frl_handle = frl_fn.get_first_param().unwrap().into_pointer_value();
        let frl_buf_size = i64.const_int(4096, false);
        let frl_buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[frl_buf_size.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let frl_ret = self.builder.build_call(self.module.get_function("fgets").unwrap(), &[frl_buf.into(), i32.const_int(4096, false).into(), frl_handle.into()], "").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Check if fgets returned NULL (EOF/error)
        let frl_is_eof = self.builder.build_int_compare(IntPredicate::EQ, frl_ret, ptr.const_zero(), "is_eof").map_err(llvm_err)?;
        let frl_eof_bb = self.context.append_basic_block(frl_fn, "eof");
        let frl_ok_bb = self.context.append_basic_block(frl_fn, "ok");
        let frl_merge_bb = self.context.append_basic_block(frl_fn, "merge");
        let _ = self.builder.build_conditional_branch(frl_is_eof, frl_eof_bb, frl_ok_bb);
        // EOF path
        self.builder.position_at_end(frl_eof_bb);
        let frl_e_undef = frl_ret_ty.get_undef();
        let frl_e1 = self.builder.build_insert_value(frl_e_undef, i64.const_int(0, false), 0, "e_len").map_err(llvm_err)?;
        let frl_e2 = self.builder.build_insert_value(frl_e1, ptr.const_zero(), 1, "e_ptr").map_err(llvm_err)?;
        let frl_e3 = self.builder.build_insert_value(frl_e2, self.bool_ty().const_zero(), 2, "e_ok").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(frl_merge_bb);
        // OK path: compute length, strip newline
        self.builder.position_at_end(frl_ok_bb);
        let frl_str_len = self.builder.build_call(self.module.get_function("strlen").unwrap(), &[frl_buf.into()], "len").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let frl_last = self.builder.build_int_sub(frl_str_len, i64.const_int(1, false), "last_idx").map_err(llvm_err)?;
        let frl_last_ptr = unsafe { self.builder.build_gep(i8, frl_buf, &[frl_last], "last_ptr").map_err(llvm_err) }?;
        let frl_last_ch = self.builder.build_load(i8, frl_last_ptr, "last_ch").map_err(llvm_err)?.into_int_value();
        let frl_is_nl = self.builder.build_int_compare(IntPredicate::EQ, frl_last_ch, i8.const_int(10, false), "is_nl").map_err(llvm_err)?;
        let frl_adj_len = self.builder.build_select(frl_is_nl, frl_last, frl_str_len, "adj_len").map_err(llvm_err)?;
        let frl_o_undef = frl_ret_ty.get_undef();
        let frl_o1 = self.builder.build_insert_value(frl_o_undef, frl_adj_len.into_int_value(), 0, "o_len").map_err(llvm_err)?;
        let frl_o2 = self.builder.build_insert_value(frl_o1, frl_buf, 1, "o_ptr").map_err(llvm_err)?;
        let frl_o3 = self.builder.build_insert_value(frl_o2, self.bool_ty().const_int(1, false), 2, "o_ok").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(frl_merge_bb);
        // Merge
        self.builder.position_at_end(frl_merge_bb);
        let frl_phi = self.builder.build_phi(frl_ret_ty, "frl_ret").map_err(llvm_err)?;
        frl_phi.add_incoming(&[(&frl_e3, frl_eof_bb), (&frl_o3, frl_ok_bb)]);
        let _ = self.builder.build_return(Some(&frl_phi.as_basic_value()));

        // ---- atomic_file_read_bytes(ptr, i64) -> {i64, ptr} (actual_len, data) ----
        // Reads up to size bytes from file handle. Returns 0 length on EOF.
        let frb_ret_ty = self.context.struct_type(&[i64.into(), ptr.into()], false);
        let frb_fn = self.module.add_function("atomic_file_read_bytes",
            frb_ret_ty.fn_type(&[ptr.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(frb_fn, "entry");
        self.builder.position_at_end(entry);
        let frb_handle = frb_fn.get_first_param().unwrap().into_pointer_value();
        let frb_size = frb_fn.get_nth_param(1).unwrap().into_int_value();
        let frb_buf = self.builder.build_call(self.module.get_function("atomic_malloc_rc").unwrap(), &[frb_size.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let frb_read = self.builder.build_call(self.module.get_function("fread").unwrap(), &[frb_buf.into(), i64.const_int(1, false).into(), frb_size.into(), frb_handle.into()], "read").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let frb_undef = frb_ret_ty.get_undef();
        let frb_r1 = self.builder.build_insert_value(frb_undef, frb_read, 0, "r_len").map_err(llvm_err)?;
        let frb_r2 = self.builder.build_insert_value(frb_r1, frb_buf, 1, "r_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&frb_r2));

        // ---- atomic_file_write_bytes(ptr, ptr, i64) -> i1 ----
        // Writes data_len bytes from data to file. Returns true on success.
        let fwb_fn = self.module.add_function("atomic_file_write_bytes",
            self.bool_ty().fn_type(&[ptr.into(), ptr.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(fwb_fn, "entry");
        self.builder.position_at_end(entry);
        let fwb_handle = fwb_fn.get_first_param().unwrap().into_pointer_value();
        let fwb_data = fwb_fn.get_nth_param(1).unwrap().into_pointer_value();
        let fwb_len = fwb_fn.get_nth_param(2).unwrap().into_int_value();
        let fwb_written = self.builder.build_call(self.module.get_function("fwrite").unwrap(), &[fwb_data.into(), i64.const_int(1, false).into(), fwb_len.into(), fwb_handle.into()], "written").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let fwb_ok = self.builder.build_int_compare(IntPredicate::EQ, fwb_written, fwb_len, "ok").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&fwb_ok));

        // ---- atomic_file_seek(ptr, i64, i32) -> i1 ----
        // Seeks to position (offset from whence: 0=SET, 1=CUR, 2=END). Returns true on success.
        let fs_fn = self.module.add_function("atomic_file_seek",
            self.bool_ty().fn_type(&[ptr.into(), i64.into(), i32.into()], false), None);
        let entry = self.context.append_basic_block(fs_fn, "entry");
        self.builder.position_at_end(entry);
        let fs_handle = fs_fn.get_first_param().unwrap().into_pointer_value();
        let fs_offset = fs_fn.get_nth_param(1).unwrap().into_int_value();
        let fs_whence = fs_fn.get_nth_param(2).unwrap().into_int_value();
        let fs_ret = self.builder.build_call(self.module.get_function("fseek").unwrap(), &[fs_handle.into(), fs_offset.into(), fs_whence.into()], "ret").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let fs_ok = self.builder.build_int_compare(IntPredicate::EQ, fs_ret, i32.const_int(0, false), "ok").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&fs_ok));

        // ---- atomic_file_tell(ptr) -> i64 ----
        // Returns current file position.
        let ft_fn = self.module.add_function("atomic_file_tell",
            i64.fn_type(&[ptr.into()], false), None);
        let entry = self.context.append_basic_block(ft_fn, "entry");
        self.builder.position_at_end(entry);
        let ft_handle = ft_fn.get_first_param().unwrap().into_pointer_value();
        let ft_ret = self.builder.build_call(self.module.get_function("ftell").unwrap(), &[ft_handle.into()], "ret").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let _ = self.builder.build_return(Some(&ft_ret));

        // ---- atomic_file_flush(ptr) -> i1 ----
        // Flushes file handle. Returns true on success.
        let _fflush_fn = self.module.add_function("fflush", i32.fn_type(&[ptr.into()], false), None);
        let ff_fn = self.module.add_function("atomic_file_flush",
            self.bool_ty().fn_type(&[ptr.into()], false), None);
        let entry = self.context.append_basic_block(ff_fn, "entry");
        self.builder.position_at_end(entry);
        let ff_handle = ff_fn.get_first_param().unwrap().into_pointer_value();
        let ff_ret = self.builder.build_call(self.module.get_function("fflush").unwrap(), &[ff_handle.into()], "ret").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let ff_ok = self.builder.build_int_compare(IntPredicate::EQ, ff_ret, i32.const_int(0, false), "ok").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&ff_ok));

        // ---- atomic_read_dir({i64, ptr}) -> {ptr, i64, i64} ----
        // Uses opendir/readdir/closedir to list directory contents
        let opendir_fn = self.module.add_function("opendir", ptr.fn_type(&[ptr.into()], false), None);
        let readdir_fn = self.module.add_function("readdir", ptr.fn_type(&[ptr.into()], false), None);
        let closedir_fn = self.module.add_function("closedir", self.i32_ty().fn_type(&[ptr.into()], false), None);
        let rd_fn = self.module.add_function("atomic_read_dir", self.list_type.fn_type(&[str_ty.into()], false), None);
        let rd_entry = self.context.append_basic_block(rd_fn, "entry");
        self.builder.position_at_end(rd_entry);
        let rd_path = rd_fn.get_first_param().unwrap().into_struct_value();
        let rd_path_data = self.builder.build_extract_value(rd_path, 1, "path_data").map_err(llvm_err)?.into_pointer_value();
        // Create empty list
        let rd_empty = self.module.get_function("atomic_list_create").unwrap();
        let rd_init = self.builder.build_call(rd_empty, &[i64.const_int(0, false).into()], "rd_init").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_struct_value();
        let rd_dir_ptr = self.builder.build_call(opendir_fn, &[rd_path_data.into()], "dir").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Check if opendir failed (returns NULL)
        let rd_dir_null = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_ptr_to_int(rd_dir_ptr, i64, "").map_err(llvm_err)?,
            self.builder.build_ptr_to_int(ptr.const_null(), i64, "").map_err(llvm_err)?, "dir_null").map_err(llvm_err)?;
        let rd_opendir_ok_bb = self.context.append_basic_block(rd_fn, "dir_ok");
        let rd_opendir_fail_bb = self.context.append_basic_block(rd_fn, "dir_fail");
        let rd_merge_bb = self.context.append_basic_block(rd_fn, "rd_merge");
        let _ = self.builder.build_conditional_branch(rd_dir_null, rd_opendir_fail_bb, rd_opendir_ok_bb);
        // opendir success: loop and read entries
        self.builder.position_at_end(rd_opendir_ok_bb);
        let rd_cur_a = self.builder.build_alloca(self.list_type, "rd_cur").map_err(llvm_err)?;
        self.builder.build_store(rd_cur_a, rd_init).map_err(llvm_err)?;
        let rd_hdr = self.context.append_basic_block(rd_fn, "rd_hdr");
        let rd_bdy = self.context.append_basic_block(rd_fn, "rd_bdy");
        let rd_done = self.context.append_basic_block(rd_fn, "rd_done");
        let _ = self.builder.build_unconditional_branch(rd_hdr);
        self.builder.position_at_end(rd_hdr);
        let rd_ent = self.builder.build_call(readdir_fn, &[rd_dir_ptr.into()], "ent").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let rd_ent_null = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_ptr_to_int(rd_ent, i64, "").map_err(llvm_err)?,
            self.builder.build_ptr_to_int(ptr.const_null(), i64, "").map_err(llvm_err)?, "ent_null").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rd_ent_null, rd_done, rd_bdy);
        self.builder.position_at_end(rd_bdy);
        // d_name is at offset 19 in struct dirent on Linux x86_64
        let rd_name = unsafe { self.builder.build_gep(i8, rd_ent, &[i64.const_int(19, false)], "name").map_err(llvm_err) }?;
        let rd_nlen = self.builder.build_call(self.module.get_function("strlen").unwrap(), &[rd_name.into()], "nlen").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        // Create string
        let rd_asc_fn = self.module.get_function("atomic_string_create").unwrap();
        let rd_new_str = self.builder.build_call(rd_asc_fn, &[rd_name.into(), rd_nlen.into()], "").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_struct_value();
        // Push to list
        let rd_push_fn = self.module.get_function("atomic_list_push").unwrap();
        let rd_cur_list = self.builder.build_load(self.list_type, rd_cur_a, "rd_cur_v").map_err(llvm_err)?;
        let rd_pushed = self.builder.build_call(rd_push_fn, &[rd_cur_list.into(), rd_new_str.into()], "").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_struct_value();
        self.builder.build_store(rd_cur_a, rd_pushed).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rd_hdr);
        // Done reading
        self.builder.position_at_end(rd_done);
        let _ = self.builder.build_call(closedir_fn, &[rd_dir_ptr.into()], "").map_err(llvm_err)?;
        let rd_result = self.builder.build_load(self.list_type, rd_cur_a, "rd_result").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rd_merge_bb);
        // opendir failed: return empty list
        self.builder.position_at_end(rd_opendir_fail_bb);
        let _ = self.builder.build_unconditional_branch(rd_merge_bb);
        // Merge phi
        self.builder.position_at_end(rd_merge_bb);
        let rd_phi = self.builder.build_phi(self.list_type, "rd_phi").map_err(llvm_err)?;
        rd_phi.add_incoming(&[(&rd_result, rd_done), (&rd_init, rd_opendir_fail_bb)]);
        let _ = self.builder.build_return(Some(&rd_phi.as_basic_value()));


        Ok(())
    }
}
