use inkwell::IntPredicate;

use super::super::{CodeGen, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn define_string_advanced(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let ptr = self.ptr_ty();
        let str_ty = self.string_type;
        let b1 = self.bool_ty();
        let i8 = self.context.i8_type();
        let i32 = self.context.i32_type();
        let list_ty = self.list_type;
        // ---- atomic_string_split({i64, ptr}, {i64, ptr}) -> {ptr, i64, i64} ----
        // Returns a list of strings by splitting the input on delimiter occurrences.
        let sp_fn = self.module.add_function("atomic_string_split",
            list_ty.fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let sp_entry = self.context.append_basic_block(sp_fn, "entry");
        self.builder.position_at_end(sp_entry);
        let sp_s = sp_fn.get_first_param().unwrap().into_struct_value();
        let sp_delim = sp_fn.get_nth_param(1).unwrap().into_struct_value();
        let sp_slen = self.builder.build_extract_value(sp_s, 0, "slen").map_err(llvm_err)?.into_int_value();
        let sp_sdata = self.builder.build_extract_value(sp_s, 1, "sdata").map_err(llvm_err)?.into_pointer_value();
        let sp_dlen = self.builder.build_extract_value(sp_delim, 0, "dlen").map_err(llvm_err)?.into_int_value();
        let sp_ddata = self.builder.build_extract_value(sp_delim, 1, "ddata").map_err(llvm_err)?.into_pointer_value();

        // Count delimiters
        let sp_count = self.builder.build_alloca(i64, "count").map_err(llvm_err)?;
        let sp_i = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(sp_count, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(sp_i, i64.const_int(0, false)).map_err(llvm_err)?;
        // Need to check dlen > 0 to avoid infinite loops
        let sp_dzero = self.builder.build_int_compare(IntPredicate::EQ, sp_dlen, i64.const_int(0, false), "dzero").map_err(llvm_err)?;
        let sp_cnt_hdr = self.context.append_basic_block(sp_fn, "cnt_hdr");
        let sp_cnt_body = self.context.append_basic_block(sp_fn, "cnt_body");
        let sp_cnt_ck = self.context.append_basic_block(sp_fn, "cnt_ck");
        let sp_cnt_next = self.context.append_basic_block(sp_fn, "cnt_next");
        let sp_cnt_done = self.context.append_basic_block(sp_fn, "cnt_done");
        let _ = self.builder.build_conditional_branch(sp_dzero, sp_cnt_done, sp_cnt_hdr);

        // cnt_hdr: while i + dlen <= slen
        self.builder.position_at_end(sp_cnt_hdr);
        let sp_iv = self.builder.build_load(i64, sp_i, "iv").map_err(llvm_err)?.into_int_value();
        let sp_end = self.builder.build_int_add(sp_iv, sp_dlen, "end").map_err(llvm_err)?;
        let sp_in_range = self.builder.build_int_compare(IntPredicate::ULE, sp_end, sp_slen, "in_range").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(sp_in_range, sp_cnt_body, sp_cnt_done);

        self.builder.position_at_end(sp_cnt_body);
        // Check if substring at pos i matches delimiter
        let sp_src = unsafe { self.builder.build_gep(i8, sp_sdata, &[sp_iv], "src").map_err(llvm_err) }?;
        let sp_mc = self.builder.build_call(self.module.get_function("memcmp").unwrap(), &[sp_src.into(), sp_ddata.into(), sp_dlen.into()], "mc").map_err(llvm_err)?;
        let sp_mcr = sp_mc.try_as_basic_value().basic().unwrap().into_int_value();
        let sp_match = self.builder.build_int_compare(IntPredicate::EQ, sp_mcr, i32.const_int(0, false), "match").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(sp_match, sp_cnt_ck, sp_cnt_next);

        self.builder.position_at_end(sp_cnt_ck);
        let sp_cur = self.builder.build_load(i64, sp_count, "cur").map_err(llvm_err)?.into_int_value();
        let sp_nc = self.builder.build_int_add(sp_cur, i64.const_int(1, false), "nc").map_err(llvm_err)?;
        self.builder.build_store(sp_count, sp_nc).map_err(llvm_err)?;
        // Skip past delimiter
        let sp_ni = self.builder.build_int_add(sp_iv, sp_dlen, "ni").map_err(llvm_err)?;
        self.builder.build_store(sp_i, sp_ni).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sp_cnt_hdr);

        self.builder.position_at_end(sp_cnt_next);
        let sp_ni2 = self.builder.build_int_add(sp_iv, i64.const_int(1, false), "ni2").map_err(llvm_err)?;
        self.builder.build_store(sp_i, sp_ni2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sp_cnt_hdr);

        // cnt_done: create list with capacity = count + 1, then fill
        self.builder.position_at_end(sp_cnt_done);
        let sp_final_cnt = self.builder.build_load(i64, sp_count, "final_cnt").map_err(llvm_err)?.into_int_value();
        let sp_cap = self.builder.build_int_add(sp_final_cnt, i64.const_int(1, false), "cap").map_err(llvm_err)?;
        let _sp_cc = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[i64.const_int(8, false).into()], "list_alloc").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Use the inline list_create approach: allocate list + data in one go
        // Create data array: capacity * 32 bytes per entry (2 * i64 for fat struct)
        let sp_dsize = self.builder.build_int_mul(sp_cap, i64.const_int(16, false), "dsize").map_err(llvm_err)?;
        let sp_data = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[sp_dsize.into()], "data").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // List struct: {data_ptr, len:0, capacity}
        let sp_und = list_ty.get_undef();
        let sp_lr1 = self.builder.build_insert_value(sp_und, sp_data, 0, "lr1").map_err(llvm_err)?;
        let sp_lr2 = self.builder.build_insert_value(sp_lr1, i64.const_int(0, false), 1, "lr2").map_err(llvm_err)?;
        let sp_list_base = self.builder.build_insert_value(sp_lr2, sp_cap, 2, "lr3").map_err(llvm_err)?;
        let sp_list_ptr = self.builder.build_alloca(list_ty, "list_ptr").map_err(llvm_err)?;
        self.builder.build_store(sp_list_ptr, sp_list_base).map_err(llvm_err)?;

        // Reset i to 0, last_start = 0
        let sp_last = self.builder.build_alloca(i64, "last").map_err(llvm_err)?;
        self.builder.build_store(sp_i, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(sp_last, i64.const_int(0, false)).map_err(llvm_err)?;
        let sp_fill_hdr = self.context.append_basic_block(sp_fn, "fill_hdr");
        let sp_fill_body = self.context.append_basic_block(sp_fn, "fill_body");
        let sp_fill_ck2 = self.context.append_basic_block(sp_fn, "fill_ck2");
        let sp_fill_push = self.context.append_basic_block(sp_fn, "fill_push");
        let sp_fill_next = self.context.append_basic_block(sp_fn, "fill_next");
        let sp_fill_last = self.context.append_basic_block(sp_fn, "fill_last");
        let sp_fill_done = self.context.append_basic_block(sp_fn, "fill_done");
        let _ = self.builder.build_conditional_branch(sp_dzero, sp_fill_last, sp_fill_hdr);

        // fill_hdr: while i + dlen <= slen
        self.builder.position_at_end(sp_fill_hdr);
        let sp_iv2 = self.builder.build_load(i64, sp_i, "iv2").map_err(llvm_err)?.into_int_value();
        let sp_end2 = self.builder.build_int_add(sp_iv2, sp_dlen, "end2").map_err(llvm_err)?;
        let sp_in2 = self.builder.build_int_compare(IntPredicate::ULE, sp_end2, sp_slen, "in2").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(sp_in2, sp_fill_body, sp_fill_last);

        self.builder.position_at_end(sp_fill_body);
        let sp_src2 = unsafe { self.builder.build_gep(i8, sp_sdata, &[sp_iv2], "src2").map_err(llvm_err) }?;
        let sp_mc2 = self.builder.build_call(self.module.get_function("memcmp").unwrap(), &[sp_src2.into(), sp_ddata.into(), sp_dlen.into()], "mc2").map_err(llvm_err)?;
        let sp_mcr2 = sp_mc2.try_as_basic_value().basic().unwrap().into_int_value();
        let sp_m2 = self.builder.build_int_compare(IntPredicate::EQ, sp_mcr2, i32.const_int(0, false), "m2").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(sp_m2, sp_fill_ck2, sp_fill_next);

        // fill_ck2: push segment from last to i
        self.builder.position_at_end(sp_fill_ck2);
        let sp_last_v = self.builder.build_load(i64, sp_last, "last_v").map_err(llvm_err)?.into_int_value();
        let sp_seg_len = self.builder.build_int_sub(sp_iv2, sp_last_v, "seg_len").map_err(llvm_err)?;
        // Create substring for this segment
        let sp_salc = self.builder.build_int_add(sp_seg_len, i64.const_int(1, false), "salc").map_err(llvm_err)?;
        let sp_sbuf = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[sp_salc.into()], "sbuf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let sp_ssrc = unsafe { self.builder.build_gep(i8, sp_sdata, &[sp_last_v], "ssrc").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[sp_sbuf.into(), sp_ssrc.into(), sp_seg_len.into()], "").map_err(llvm_err)?;
        let sp_snull = unsafe { self.builder.build_gep(i8, sp_sbuf, &[sp_seg_len], "snull").map_err(llvm_err) }?;
        self.builder.build_store(sp_snull, i8.const_int(0, false)).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sp_fill_push);

        // fill_push: push the new string to list
        self.builder.position_at_end(sp_fill_push);
        // Build fat struct {seg_len, sbuf} for the string
        let sp_fat_undef = str_ty.get_undef();
        let sp_fat1 = self.builder.build_insert_value(sp_fat_undef, sp_seg_len, 0, "fat1").map_err(llvm_err)?;
        let sp_fat2 = self.builder.build_insert_value(sp_fat1, sp_sbuf, 1, "fat2").map_err(llvm_err)?;
        // Load list, push fat struct
        let sp_ll = self.builder.build_load(list_ty, sp_list_ptr, "ll").map_err(llvm_err)?.into_struct_value();
        // Inline list push: get len, check capacity, store at data[len], increment len
        let sp_llen = self.builder.build_extract_value(sp_ll, 1, "llen").map_err(llvm_err)?.into_int_value();
        let sp_ldata = self.builder.build_extract_value(sp_ll, 0, "ldata").map_err(llvm_err)?.into_pointer_value();
        let sp_offset = self.builder.build_int_mul(sp_llen, i64.const_int(16, false), "offset").map_err(llvm_err)?;
        let sp_dst = unsafe { self.builder.build_gep(i8, sp_ldata, &[sp_offset], "dst").map_err(llvm_err) }?;
        let sp_dst_i64 = self.builder.build_pointer_cast(sp_dst, ptr, "dst_i64").map_err(llvm_err)?;
        // Store tag
        let sp_ftag = self.builder.build_extract_value(sp_fat2, 0, "ftag").map_err(llvm_err)?.into_int_value();
        self.builder.build_store(sp_dst_i64, sp_ftag).map_err(llvm_err)?;
        // Store ptr
        let sp_fp = self.builder.build_extract_value(sp_fat2, 1, "fp").map_err(llvm_err)?.into_pointer_value();
        let sp_off1 = self.builder.build_int_add(sp_offset, i64.const_int(8, false), "off1").map_err(llvm_err)?;
        let sp_pp = unsafe { self.builder.build_gep(i8, sp_ldata, &[sp_off1], "pp").map_err(llvm_err) }?;
        let sp_ppi64 = self.builder.build_pointer_cast(sp_pp, ptr, "ppi64").map_err(llvm_err)?;
        let sp_fp_i64 = self.builder.build_ptr_to_int(sp_fp, i64, "fp_i64").map_err(llvm_err)?;
        self.builder.build_store(sp_ppi64, sp_fp_i64).map_err(llvm_err)?;
        // Increment len
        let sp_nlen = self.builder.build_int_add(sp_llen, i64.const_int(1, false), "nlen").map_err(llvm_err)?;
        let sp_nlist_und = list_ty.get_undef();
        let sp_nl1 = self.builder.build_insert_value(sp_nlist_und, sp_ldata, 0, "nl1").map_err(llvm_err)?;
        let sp_nl2 = self.builder.build_insert_value(sp_nl1, sp_nlen, 1, "nl2").map_err(llvm_err)?;
        let sp_nl3 = self.builder.build_insert_value(sp_nl2, sp_cap, 2, "nl3").map_err(llvm_err)?;
        self.builder.build_store(sp_list_ptr, sp_nl3).map_err(llvm_err)?;
        // Update last = i + dlen
        let sp_nlast = self.builder.build_int_add(sp_iv2, sp_dlen, "nlast").map_err(llvm_err)?;
        self.builder.build_store(sp_i, sp_nlast).map_err(llvm_err)?;
        self.builder.build_store(sp_last, sp_nlast).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sp_fill_hdr);

        // fill_next: i += 1
        self.builder.position_at_end(sp_fill_next);
        let sp_ni3 = self.builder.build_int_add(sp_iv2, i64.const_int(1, false), "ni3").map_err(llvm_err)?;
        self.builder.build_store(sp_i, sp_ni3).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sp_fill_hdr);

        // fill_last: push remaining segment from last to slen
        self.builder.position_at_end(sp_fill_last);
        let sp_last_v2 = self.builder.build_load(i64, sp_last, "last_v2").map_err(llvm_err)?.into_int_value();
        let sp_seg_len2 = self.builder.build_int_sub(sp_slen, sp_last_v2, "seg_len2").map_err(llvm_err)?;
        let sp_salc2 = self.builder.build_int_add(sp_seg_len2, i64.const_int(1, false), "salc2").map_err(llvm_err)?;
        let sp_sbuf2 = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[sp_salc2.into()], "sbuf2").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let sp_ssrc2 = unsafe { self.builder.build_gep(i8, sp_sdata, &[sp_last_v2], "ssrc2").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[sp_sbuf2.into(), sp_ssrc2.into(), sp_seg_len2.into()], "").map_err(llvm_err)?;
        let sp_snull2 = unsafe { self.builder.build_gep(i8, sp_sbuf2, &[sp_seg_len2], "snull2").map_err(llvm_err) }?;
        self.builder.build_store(sp_snull2, i8.const_int(0, false)).map_err(llvm_err)?;
        // Build fat struct
        let sp_fat_undef2 = str_ty.get_undef();
        let sp_fat1b = self.builder.build_insert_value(sp_fat_undef2, sp_seg_len2, 0, "fat1b").map_err(llvm_err)?;
        let sp_fat2b = self.builder.build_insert_value(sp_fat1b, sp_sbuf2, 1, "fat2b").map_err(llvm_err)?;
        // Push to list
        let sp_ll2 = self.builder.build_load(list_ty, sp_list_ptr, "ll2").map_err(llvm_err)?.into_struct_value();
        let sp_llen2 = self.builder.build_extract_value(sp_ll2, 1, "llen2").map_err(llvm_err)?.into_int_value();
        let sp_ldata2 = self.builder.build_extract_value(sp_ll2, 0, "ldata2").map_err(llvm_err)?.into_pointer_value();
        let sp_offset2 = self.builder.build_int_mul(sp_llen2, i64.const_int(16, false), "offset2").map_err(llvm_err)?;
        let sp_dst2 = unsafe { self.builder.build_gep(i8, sp_ldata2, &[sp_offset2], "dst2").map_err(llvm_err) }?;
        let sp_dst2_i64 = self.builder.build_pointer_cast(sp_dst2, ptr, "dst2_i64").map_err(llvm_err)?;
        let sp_ftag2 = self.builder.build_extract_value(sp_fat2b, 0, "ftag2").map_err(llvm_err)?.into_int_value();
        self.builder.build_store(sp_dst2_i64, sp_ftag2).map_err(llvm_err)?;
        let sp_fp2 = self.builder.build_extract_value(sp_fat2b, 1, "fp2").map_err(llvm_err)?.into_pointer_value();
        let sp_off1b = self.builder.build_int_add(sp_offset2, i64.const_int(8, false), "off1b").map_err(llvm_err)?;
        let sp_pp2 = unsafe { self.builder.build_gep(i8, sp_ldata2, &[sp_off1b], "pp2").map_err(llvm_err) }?;
        let sp_pp2i64 = self.builder.build_pointer_cast(sp_pp2, ptr, "pp2i64").map_err(llvm_err)?;
        let sp_fp2_i64 = self.builder.build_ptr_to_int(sp_fp2, i64, "fp2_i64").map_err(llvm_err)?;
        self.builder.build_store(sp_pp2i64, sp_fp2_i64).map_err(llvm_err)?;
        let sp_nlen2 = self.builder.build_int_add(sp_llen2, i64.const_int(1, false), "nlen2").map_err(llvm_err)?;
        let sp_nlist_und2 = list_ty.get_undef();
        let sp_nl1b = self.builder.build_insert_value(sp_nlist_und2, sp_ldata2, 0, "nl1b").map_err(llvm_err)?;
        let sp_nl2b = self.builder.build_insert_value(sp_nl1b, sp_nlen2, 1, "nl2b").map_err(llvm_err)?;
        let sp_nl3b = self.builder.build_insert_value(sp_nl2b, sp_cap, 2, "nl3b").map_err(llvm_err)?;
        self.builder.build_store(sp_list_ptr, sp_nl3b).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sp_fill_done);

        // fill_done: return list
        self.builder.position_at_end(sp_fill_done);
        let sp_result = self.builder.build_load(list_ty, sp_list_ptr, "result").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&sp_result));

        // ---- atomic_string_join({ptr, i64, i64}, {i64, ptr}) -> {i64, ptr} ----
        let jn_fn = self.module.add_function("atomic_string_join",
            str_ty.fn_type(&[list_ty.into(), str_ty.into()], false), None);
        let jn_entry = self.context.append_basic_block(jn_fn, "entry");
        self.builder.position_at_end(jn_entry);
        let jn_list = jn_fn.get_first_param().unwrap().into_struct_value();
        let jn_delim = jn_fn.get_nth_param(1).unwrap().into_struct_value();
        let jn_ldata = self.builder.build_extract_value(jn_list, 0, "ldata").map_err(llvm_err)?.into_pointer_value();
        let jn_llen = self.builder.build_extract_value(jn_list, 1, "llen").map_err(llvm_err)?.into_int_value();
        let jn_dlen = self.builder.build_extract_value(jn_delim, 0, "dlen").map_err(llvm_err)?.into_int_value();
        let jn_ddata = self.builder.build_extract_value(jn_delim, 1, "ddata").map_err(llvm_err)?.into_pointer_value();

        // Compute total size
        let jn_total = self.builder.build_alloca(i64, "total").map_err(llvm_err)?;
        self.builder.build_store(jn_total, i64.const_int(0, false)).map_err(llvm_err)?;
        let jn_ji = self.builder.build_alloca(i64, "ji").map_err(llvm_err)?;
        self.builder.build_store(jn_ji, i64.const_int(0, false)).map_err(llvm_err)?;

        let jn_hdr = self.context.append_basic_block(jn_fn, "hdr");
        let jn_body = self.context.append_basic_block(jn_fn, "body");
        let jn_after = self.context.append_basic_block(jn_fn, "after");
        let _ = self.builder.build_unconditional_branch(jn_hdr);

        // Sum all string lengths + delimiter lengths
        self.builder.position_at_end(jn_hdr);
        let jn_iv = self.builder.build_load(i64, jn_ji, "iv").map_err(llvm_err)?.into_int_value();
        let jn_more = self.builder.build_int_compare(IntPredicate::ULT, jn_iv, jn_llen, "more").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(jn_more, jn_body, jn_after);

        self.builder.position_at_end(jn_body);
        let jn_off = self.builder.build_int_mul(jn_iv, i64.const_int(16, false), "off").map_err(llvm_err)?;
        let jn_ep = unsafe { self.builder.build_gep(i8, jn_ldata, &[jn_off], "ep").map_err(llvm_err) }?;
        let jn_epi64 = self.builder.build_pointer_cast(jn_ep, ptr, "epi64").map_err(llvm_err)?;
        let jn_sslen = self.builder.build_load(i64, jn_epi64, "sslen").map_err(llvm_err)?.into_int_value();
        let jn_cur = self.builder.build_load(i64, jn_total, "cur").map_err(llvm_err)?.into_int_value();
        let _jn_add = self.builder.build_int_add(jn_cur, jn_sslen, "add").map_err(llvm_err)?;
        // Add delimiter length if not last element
        let jn_is_last = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_int_add(jn_iv, i64.const_int(1, false), "ivp1").map_err(llvm_err)?,
            jn_llen, "is_last").map_err(llvm_err)?;
        let jn_with_delim = self.builder.build_int_add(jn_sslen, jn_dlen, "with_delim").map_err(llvm_err)?;
        let jn_delta_sv = self.builder.build_select(jn_is_last, jn_sslen, jn_with_delim, "delta").map_err(llvm_err)?;
        let jn_delta = jn_delta_sv.into_int_value();
        let jn_new_total = self.builder.build_int_add(jn_cur, jn_delta, "new_total").map_err(llvm_err)?;
        self.builder.build_store(jn_total, jn_new_total).map_err(llvm_err)?;
        let jn_niv = self.builder.build_int_add(jn_iv, i64.const_int(1, false), "niv").map_err(llvm_err)?;
        self.builder.build_store(jn_ji, jn_niv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(jn_hdr);

        // Allocate and copy
        self.builder.position_at_end(jn_after);
        let jn_final_total = self.builder.build_load(i64, jn_total, "final_total").map_err(llvm_err)?.into_int_value();
        let jn_jalc = self.builder.build_int_add(jn_final_total, i64.const_int(1, false), "jalc").map_err(llvm_err)?;
        let jn_buf = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[jn_jalc.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Reset i, reset write cursor
        let jn_wpos = self.builder.build_alloca(i64, "wpos").map_err(llvm_err)?;
        self.builder.build_store(jn_ji, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(jn_wpos, i64.const_int(0, false)).map_err(llvm_err)?;

        let jn_chdr = self.context.append_basic_block(jn_fn, "chdr");
        let jn_cbody = self.context.append_basic_block(jn_fn, "cbody");
        let jn_cdone = self.context.append_basic_block(jn_fn, "cdone");
        let _ = self.builder.build_unconditional_branch(jn_chdr);

        self.builder.position_at_end(jn_chdr);
        let jn_civ = self.builder.build_load(i64, jn_ji, "civ").map_err(llvm_err)?.into_int_value();
        let jn_cmore = self.builder.build_int_compare(IntPredicate::ULT, jn_civ, jn_llen, "cmore").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(jn_cmore, jn_cbody, jn_cdone);

        self.builder.position_at_end(jn_cbody);
        let jn_coff = self.builder.build_int_mul(jn_civ, i64.const_int(16, false), "coff").map_err(llvm_err)?;
        let jn_cep = unsafe { self.builder.build_gep(i8, jn_ldata, &[jn_coff], "cep").map_err(llvm_err) }?;
        let jn_cepi64 = self.builder.build_pointer_cast(jn_cep, ptr, "cepi64").map_err(llvm_err)?;
        let jn_csslen = self.builder.build_load(i64, jn_cepi64, "csslen").map_err(llvm_err)?.into_int_value();
        let jn_coff1 = self.builder.build_int_add(jn_coff, i64.const_int(8, false), "coff1").map_err(llvm_err)?;
        let jn_cpp = unsafe { self.builder.build_gep(i8, jn_ldata, &[jn_coff1], "cpp").map_err(llvm_err) }?;
        let jn_cppi64 = self.builder.build_pointer_cast(jn_cpp, ptr, "cppi64").map_err(llvm_err)?;
        let jn_cpval = self.builder.build_load(i64, jn_cppi64, "cpval").map_err(llvm_err)?.into_int_value();
        let jn_cp = self.builder.build_int_to_ptr(jn_cpval, ptr, "cp").map_err(llvm_err)?;
        // Copy string data to output at wpos
        let jn_cwp = self.builder.build_load(i64, jn_wpos, "cwp").map_err(llvm_err)?.into_int_value();
        let jn_cdst = unsafe { self.builder.build_gep(i8, jn_buf, &[jn_cwp], "cdst").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[jn_cdst.into(), jn_cp.into(), jn_csslen.into()], "").map_err(llvm_err)?;
        let jn_nwp = self.builder.build_int_add(jn_cwp, jn_csslen, "nwp").map_err(llvm_err)?;
        self.builder.build_store(jn_wpos, jn_nwp).map_err(llvm_err)?;
        // Copy delimiter if not last
        let jn_cis_last = self.builder.build_int_compare(IntPredicate::EQ,
            self.builder.build_int_add(jn_civ, i64.const_int(1, false), "civp1").map_err(llvm_err)?,
            jn_llen, "cis_last").map_err(llvm_err)?;
        let jn_cdel_bb = self.context.append_basic_block(jn_fn, "cdel");
        let jn_cnext_bb = self.context.append_basic_block(jn_fn, "cnext");
        let _ = self.builder.build_conditional_branch(jn_cis_last, jn_cnext_bb, jn_cdel_bb);

        self.builder.position_at_end(jn_cdel_bb);
        let jn_cwp2 = self.builder.build_load(i64, jn_wpos, "cwp2").map_err(llvm_err)?.into_int_value();
        let jn_cdst2 = unsafe { self.builder.build_gep(i8, jn_buf, &[jn_cwp2], "cdst2").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[jn_cdst2.into(), jn_ddata.into(), jn_dlen.into()], "").map_err(llvm_err)?;
        let jn_nwp2 = self.builder.build_int_add(jn_cwp2, jn_dlen, "nwp2").map_err(llvm_err)?;
        self.builder.build_store(jn_wpos, jn_nwp2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(jn_cnext_bb);

        self.builder.position_at_end(jn_cnext_bb);
        let jn_cniv = self.builder.build_int_add(jn_civ, i64.const_int(1, false), "cniv").map_err(llvm_err)?;
        self.builder.build_store(jn_ji, jn_cniv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(jn_chdr);

        // Done: null-terminate and return
        self.builder.position_at_end(jn_cdone);
        let jn_fwp = self.builder.build_load(i64, jn_wpos, "fwp").map_err(llvm_err)?.into_int_value();
        let jn_nullp = unsafe { self.builder.build_gep(i8, jn_buf, &[jn_fwp], "nullp").map_err(llvm_err) }?;
        self.builder.build_store(jn_nullp, i8.const_int(0, false)).map_err(llvm_err)?;
        let jn_und = str_ty.get_undef();
        let jn_r1 = self.builder.build_insert_value(jn_und, jn_fwp, 0, "r1").map_err(llvm_err)?;
        let jn_r2 = self.builder.build_insert_value(jn_r1, jn_buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&jn_r2));

        // ---- atomic_string_replace({i64, ptr}, {i64, ptr}, {i64, ptr}) -> {i64, ptr} ----
        let rp_fn = self.module.add_function("atomic_string_replace",
            str_ty.fn_type(&[str_ty.into(), str_ty.into(), str_ty.into()], false), None);
        let rp_entry = self.context.append_basic_block(rp_fn, "entry");
        self.builder.position_at_end(rp_entry);
        let rp_s = rp_fn.get_first_param().unwrap().into_struct_value();
        let rp_from = rp_fn.get_nth_param(1).unwrap().into_struct_value();
        let rp_to = rp_fn.get_nth_param(2).unwrap().into_struct_value();
        let rp_slen = self.builder.build_extract_value(rp_s, 0, "slen").map_err(llvm_err)?.into_int_value();
        let rp_sdata = self.builder.build_extract_value(rp_s, 1, "sdata").map_err(llvm_err)?.into_pointer_value();
        let rp_flen = self.builder.build_extract_value(rp_from, 0, "flen").map_err(llvm_err)?.into_int_value();
        let rp_fdata = self.builder.build_extract_value(rp_from, 1, "fdata").map_err(llvm_err)?.into_pointer_value();
        let rp_tlen = self.builder.build_extract_value(rp_to, 0, "tlen").map_err(llvm_err)?.into_int_value();
        let rp_tdata = self.builder.build_extract_value(rp_to, 1, "tdata").map_err(llvm_err)?.into_pointer_value();

        // If from is empty, return copy of original
        let rp_fzero = self.builder.build_int_compare(IntPredicate::EQ, rp_flen, i64.const_int(0, false), "fzero").map_err(llvm_err)?;
        let rp_have_from = self.context.append_basic_block(rp_fn, "have_from");
        let rp_copy_ret = self.context.append_basic_block(rp_fn, "copy_ret");
        let _ = self.builder.build_conditional_branch(rp_fzero, rp_copy_ret, rp_have_from);

        // Copy return: just duplicate the original string
        self.builder.position_at_end(rp_copy_ret);
        let rp_calc = self.builder.build_int_add(rp_slen, i64.const_int(1, false), "calc").map_err(llvm_err)?;
        let rp_cbuf = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[rp_calc.into()], "cbuf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[rp_cbuf.into(), rp_sdata.into(), rp_slen.into()], "").map_err(llvm_err)?;
        let rp_cnull = unsafe { self.builder.build_gep(i8, rp_cbuf, &[rp_slen], "cnull").map_err(llvm_err) }?;
        self.builder.build_store(rp_cnull, i8.const_int(0, false)).map_err(llvm_err)?;
        let rp_cund = str_ty.get_undef();
        let rp_cr1 = self.builder.build_insert_value(rp_cund, rp_slen, 0, "cr1").map_err(llvm_err)?;
        let rp_cr2 = self.builder.build_insert_value(rp_cr1, rp_cbuf, 1, "cr2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&rp_cr2));

        // have_from: count occurrences and compute result size
        self.builder.position_at_end(rp_have_from);
        let rp_ri = self.builder.build_alloca(i64, "ri").map_err(llvm_err)?;
        let rp_rlast = self.builder.build_alloca(i64, "rlast").map_err(llvm_err)?;
        let rp_count = self.builder.build_alloca(i64, "rcount").map_err(llvm_err)?;
        self.builder.build_store(rp_ri, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(rp_rlast, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(rp_count, i64.const_int(0, false)).map_err(llvm_err)?;

        let rp_hdr = self.context.append_basic_block(rp_fn, "hdr");
        let rp_body = self.context.append_basic_block(rp_fn, "body");
        let rp_ck = self.context.append_basic_block(rp_fn, "ck");
        let rp_nxt = self.context.append_basic_block(rp_fn, "nxt");
        let rp_build = self.context.append_basic_block(rp_fn, "build");
        let _ = self.builder.build_unconditional_branch(rp_hdr);

        // Scan loop: find matches, count them
        self.builder.position_at_end(rp_hdr);
        let rp_riv = self.builder.build_load(i64, rp_ri, "riv").map_err(llvm_err)?.into_int_value();
        let rp_end = self.builder.build_int_add(rp_riv, rp_flen, "end").map_err(llvm_err)?;
        let rp_ok = self.builder.build_int_compare(IntPredicate::ULE, rp_end, rp_slen, "ok").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rp_ok, rp_body, rp_build);

        self.builder.position_at_end(rp_body);
        let rp_rsrc = unsafe { self.builder.build_gep(i8, rp_sdata, &[rp_riv], "rsrc").map_err(llvm_err) }?;
        let rp_rmc = self.builder.build_call(self.module.get_function("memcmp").unwrap(), &[rp_rsrc.into(), rp_fdata.into(), rp_flen.into()], "rmc").map_err(llvm_err)?;
        let rp_rmcr = rp_rmc.try_as_basic_value().basic().unwrap().into_int_value();
        let rp_rm = self.builder.build_int_compare(IntPredicate::EQ, rp_rmcr, i32.const_int(0, false), "rm").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rp_rm, rp_ck, rp_nxt);

        self.builder.position_at_end(rp_ck);
        let rp_rc = self.builder.build_load(i64, rp_count, "rc").map_err(llvm_err)?.into_int_value();
        let rp_nc = self.builder.build_int_add(rp_rc, i64.const_int(1, false), "nc").map_err(llvm_err)?;
        self.builder.build_store(rp_count, rp_nc).map_err(llvm_err)?;
        let rp_nri = self.builder.build_int_add(rp_riv, rp_flen, "nri").map_err(llvm_err)?;
        self.builder.build_store(rp_ri, rp_nri).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rp_hdr);

        self.builder.position_at_end(rp_nxt);
        let rp_nri2 = self.builder.build_int_add(rp_riv, i64.const_int(1, false), "nri2").map_err(llvm_err)?;
        self.builder.build_store(rp_ri, rp_nri2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rp_hdr);

        // build: allocate and copy with replacements
        self.builder.position_at_end(rp_build);
        let rp_fc = self.builder.build_load(i64, rp_count, "fc").map_err(llvm_err)?.into_int_value();
        // new_len = slen + count * (tlen - flen)
        let rp_diff = self.builder.build_int_sub(rp_tlen, rp_flen, "diff").map_err(llvm_err)?;
        let rp_extra = self.builder.build_int_mul(rp_fc, rp_diff, "extra").map_err(llvm_err)?;
        let rp_nlen = self.builder.build_int_add(rp_slen, rp_extra, "nlen").map_err(llvm_err)?;
        let rp_nalc = self.builder.build_int_add(rp_nlen, i64.const_int(1, false), "nalc").map_err(llvm_err)?;
        let rp_nbuf = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[rp_nalc.into()], "nbuf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();

        // Reset scan
        self.builder.build_store(rp_ri, i64.const_int(0, false)).map_err(llvm_err)?;
        self.builder.build_store(rp_rlast, i64.const_int(0, false)).map_err(llvm_err)?;
        let rp_wpos = self.builder.build_alloca(i64, "wpos").map_err(llvm_err)?;
        self.builder.build_store(rp_wpos, i64.const_int(0, false)).map_err(llvm_err)?;

        let rp_bhdr = self.context.append_basic_block(rp_fn, "bhdr");
        let rp_bbody = self.context.append_basic_block(rp_fn, "bbody");
        let rp_bck = self.context.append_basic_block(rp_fn, "bck");
        let rp_bnxt = self.context.append_basic_block(rp_fn, "bnxt");
        let rp_bfinal = self.context.append_basic_block(rp_fn, "bfinal");
        let rp_bdone = self.context.append_basic_block(rp_fn, "bdone");
        let _ = self.builder.build_unconditional_branch(rp_bhdr);

        self.builder.position_at_end(rp_bhdr);
        let rp_briv = self.builder.build_load(i64, rp_ri, "briv").map_err(llvm_err)?.into_int_value();
        let rp_bend = self.builder.build_int_add(rp_briv, rp_flen, "bend").map_err(llvm_err)?;
        let rp_bok = self.builder.build_int_compare(IntPredicate::ULE, rp_bend, rp_slen, "bok").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rp_bok, rp_bbody, rp_bfinal);

        self.builder.position_at_end(rp_bbody);
        let rp_brsrc = unsafe { self.builder.build_gep(i8, rp_sdata, &[rp_briv], "brsrc").map_err(llvm_err) }?;
        let rp_bmc = self.builder.build_call(self.module.get_function("memcmp").unwrap(), &[rp_brsrc.into(), rp_fdata.into(), rp_flen.into()], "bmc").map_err(llvm_err)?;
        let rp_bmcr = rp_bmc.try_as_basic_value().basic().unwrap().into_int_value();
        let rp_bm = self.builder.build_int_compare(IntPredicate::EQ, rp_bmcr, i32.const_int(0, false), "bm").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rp_bm, rp_bck, rp_bnxt);

        // Match found: copy any non-matched part before it, then copy replacement
        self.builder.position_at_end(rp_bck);
        let rp_blast = self.builder.build_load(i64, rp_rlast, "blast").map_err(llvm_err)?.into_int_value();
        let rp_bgap = self.builder.build_int_sub(rp_briv, rp_blast, "bgap").map_err(llvm_err)?;
        let rp_bwp = self.builder.build_load(i64, rp_wpos, "bwp").map_err(llvm_err)?.into_int_value();
        // Copy gap (non-matched chars before this match)
        let rp_bgsrc = unsafe { self.builder.build_gep(i8, rp_sdata, &[rp_blast], "bgsrc").map_err(llvm_err) }?;
        let rp_bgdst = unsafe { self.builder.build_gep(i8, rp_nbuf, &[rp_bwp], "bgdst").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[rp_bgdst.into(), rp_bgsrc.into(), rp_bgap.into()], "").map_err(llvm_err)?;
        let rp_bnwp1 = self.builder.build_int_add(rp_bwp, rp_bgap, "bnwp1").map_err(llvm_err)?;
        // Copy replacement
        let rp_brdst = unsafe { self.builder.build_gep(i8, rp_nbuf, &[rp_bnwp1], "brdst").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[rp_brdst.into(), rp_tdata.into(), rp_tlen.into()], "").map_err(llvm_err)?;
        let rp_bnwp2 = self.builder.build_int_add(rp_bnwp1, rp_tlen, "bnwp2").map_err(llvm_err)?;
        self.builder.build_store(rp_wpos, rp_bnwp2).map_err(llvm_err)?;
        let rp_bnri = self.builder.build_int_add(rp_briv, rp_flen, "bnri").map_err(llvm_err)?;
        self.builder.build_store(rp_ri, rp_bnri).map_err(llvm_err)?;
        self.builder.build_store(rp_rlast, rp_bnri).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rp_bhdr);

        self.builder.position_at_end(rp_bnxt);
        let rp_bnri2 = self.builder.build_int_add(rp_briv, i64.const_int(1, false), "bnri2").map_err(llvm_err)?;
        self.builder.build_store(rp_ri, rp_bnri2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rp_bhdr);

        // Copy remaining after last match
        self.builder.position_at_end(rp_bfinal);
        let rp_blast2 = self.builder.build_load(i64, rp_rlast, "blast2").map_err(llvm_err)?.into_int_value();
        let rp_brem = self.builder.build_int_sub(rp_slen, rp_blast2, "brem").map_err(llvm_err)?;
        let rp_bwp2 = self.builder.build_load(i64, rp_wpos, "bwp2").map_err(llvm_err)?.into_int_value();
        let rp_brsrc2 = unsafe { self.builder.build_gep(i8, rp_sdata, &[rp_blast2], "brsrc2").map_err(llvm_err) }?;
        let rp_brdst2 = unsafe { self.builder.build_gep(i8, rp_nbuf, &[rp_bwp2], "brdst2").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[rp_brdst2.into(), rp_brsrc2.into(), rp_brem.into()], "").map_err(llvm_err)?;
        let _rp_bnwp3 = self.builder.build_int_add(rp_bwp2, rp_brem, "bnwp3").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rp_bdone);

        self.builder.position_at_end(rp_bdone);
        let rp_fwpos = self.builder.build_load(i64, rp_wpos, "fwpos").map_err(llvm_err)?.into_int_value();
        let rp_bnull = unsafe { self.builder.build_gep(i8, rp_nbuf, &[rp_fwpos], "bnull").map_err(llvm_err) }?;
        self.builder.build_store(rp_bnull, i8.const_int(0, false)).map_err(llvm_err)?;
        let rp_rund = str_ty.get_undef();
        let rp_rr1 = self.builder.build_insert_value(rp_rund, rp_fwpos, 0, "rr1").map_err(llvm_err)?;
        let rp_rr2 = self.builder.build_insert_value(rp_rr1, rp_nbuf, 1, "rr2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&rp_rr2));

        // ---- atomic_string_contains({i64, ptr}, {i64, ptr}) -> i1 ----
        let sc_fn = self.module.add_function("atomic_string_contains",
            b1.fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let sc_entry = self.context.append_basic_block(sc_fn, "entry");
        self.builder.position_at_end(sc_entry);
        let sc_haystack = sc_fn.get_first_param().unwrap().into_struct_value();
        let sc_needle = sc_fn.get_nth_param(1).unwrap().into_struct_value();
        let sc_hlen = self.builder.build_extract_value(sc_haystack, 0, "hlen").map_err(llvm_err)?.into_int_value();
        let sc_hptr = self.builder.build_extract_value(sc_haystack, 1, "hptr").map_err(llvm_err)?.into_pointer_value();
        let sc_nlen = self.builder.build_extract_value(sc_needle, 0, "nlen").map_err(llvm_err)?.into_int_value();
        let sc_nptr = self.builder.build_extract_value(sc_needle, 1, "nptr").map_err(llvm_err)?.into_pointer_value();
        // If needle is empty, return true
        let sc_empty = self.builder.build_int_compare(IntPredicate::EQ, sc_nlen, i64.const_int(0, false), "nempty").map_err(llvm_err)?;
        let sc_len_ok = self.builder.build_int_compare(IntPredicate::SLE, sc_nlen, sc_hlen, "lenok").map_err(llvm_err)?;
        let _sc_can_search = self.builder.build_and(sc_len_ok, self.builder.build_not(sc_empty, "not_empty").map_err(llvm_err)?, "can_search").map_err(llvm_err)?;
        // Brute-force search
        let sc_max = self.builder.build_int_sub(sc_hlen, sc_nlen, "max").map_err(llvm_err)?;
        let sc_loop_bb = self.context.append_basic_block(sc_fn, "sc_loop");
        let sc_found_bb = self.context.append_basic_block(sc_fn, "sc_found");
        let sc_notfound_bb = self.context.append_basic_block(sc_fn, "sc_notfound");
        let _ = self.builder.build_unconditional_branch(sc_loop_bb);
        self.builder.position_at_end(sc_loop_bb);
        let sc_i = self.builder.build_phi(i64, "sc_i").map_err(llvm_err)?;
        // Compare character by character
        let sc_j_loop_bb = self.context.append_basic_block(sc_fn, "sc_jloop");
        let sc_match_bb = self.context.append_basic_block(sc_fn, "sc_match");
        let sc_mismatch_bb = self.context.append_basic_block(sc_fn, "sc_mismatch");
        let _ = self.builder.build_unconditional_branch(sc_j_loop_bb);
        self.builder.position_at_end(sc_j_loop_bb);
        let sc_j = self.builder.build_phi(i64, "sc_j").map_err(llvm_err)?;
        let sc_hidx = self.builder.build_int_add(sc_i.as_basic_value().into_int_value(), sc_j.as_basic_value().into_int_value(), "hidx").map_err(llvm_err)?;
        let sc_hp = unsafe { self.builder.build_gep(i8, sc_hptr, &[sc_hidx], "hp").map_err(llvm_err) }?;
        let sc_hc = self.builder.build_load(i8, sc_hp, "hc").map_err(llvm_err)?.into_int_value();
        let sc_np = unsafe { self.builder.build_gep(i8, sc_nptr, &[sc_j.as_basic_value().into_int_value()], "np").map_err(llvm_err) }?;
        let sc_nc = self.builder.build_load(i8, sc_np, "nc").map_err(llvm_err)?.into_int_value();
        let sc_char_match = self.builder.build_int_compare(IntPredicate::EQ, sc_hc, sc_nc, "char_match").map_err(llvm_err)?;
        let sc_j_next = self.builder.build_int_add(sc_j.as_basic_value().into_int_value(), i64.const_int(1, false), "jnext").map_err(llvm_err)?;
        let sc_j_done = self.builder.build_int_compare(IntPredicate::SGE, sc_j_next, sc_nlen, "jdone").map_err(llvm_err)?;
        sc_j.add_incoming(&[(&i64.const_int(0, false), sc_loop_bb)]);
        let _ = self.builder.build_conditional_branch(sc_char_match, sc_match_bb, sc_mismatch_bb);
        self.builder.position_at_end(sc_match_bb);
        sc_j.add_incoming(&[(&sc_j_next, sc_match_bb)]);
        let _ = self.builder.build_conditional_branch(sc_j_done, sc_found_bb, sc_j_loop_bb);
        self.builder.position_at_end(sc_mismatch_bb);
        let sc_i_next = self.builder.build_int_add(sc_i.as_basic_value().into_int_value(), i64.const_int(1, false), "inext").map_err(llvm_err)?;
        let sc_i_done = self.builder.build_int_compare(IntPredicate::SGT, sc_i_next, sc_max, "idone").map_err(llvm_err)?;
        let sc_i_block = self.builder.get_insert_block().unwrap();
        sc_i.add_incoming(&[(&i64.const_int(0, false), sc_entry), (&sc_i_next, sc_i_block)]);
        let _ = self.builder.build_conditional_branch(sc_i_done, sc_notfound_bb, sc_loop_bb);
        self.builder.position_at_end(sc_found_bb);
        let _ = self.builder.build_return(Some(&b1.const_int(1, false)));
        self.builder.position_at_end(sc_notfound_bb);
        let _ = self.builder.build_return(Some(&b1.const_int(0, false)));

        // ---- atomic_string_repeat({i64, ptr}, i64) -> {i64, ptr} ----
        let sr_fn = self.module.add_function("atomic_string_repeat",
            str_ty.fn_type(&[str_ty.into(), i64.into()], false), None);
        let sr_entry = self.context.append_basic_block(sr_fn, "entry");
        self.builder.position_at_end(sr_entry);
        let sr_str = sr_fn.get_first_param().unwrap().into_struct_value();
        let sr_n = sr_fn.get_nth_param(1).unwrap().into_int_value();
        let sr_slen = self.builder.build_extract_value(sr_str, 0, "slen").map_err(llvm_err)?.into_int_value();
        let sr_sptr = self.builder.build_extract_value(sr_str, 1, "sptr").map_err(llvm_err)?.into_pointer_value();
        let sr_total = self.builder.build_int_mul(sr_slen, sr_n, "total").map_err(llvm_err)?;
        let sr_buf = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[sr_total.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("malloc")?.into_pointer_value();
        // Loop: copy s into buffer n times
        let sr_loop_bb = self.context.append_basic_block(sr_fn, "sr_loop");
        let sr_done_bb = self.context.append_basic_block(sr_fn, "sr_done");
        let _ = self.builder.build_unconditional_branch(sr_loop_bb);
        self.builder.position_at_end(sr_loop_bb);
        let sr_i = self.builder.build_phi(i64, "sr_i").map_err(llvm_err)?;
        let sr_offset = self.builder.build_int_mul(sr_i.as_basic_value().into_int_value(), sr_slen, "offset").map_err(llvm_err)?;
        let sr_dst = unsafe { self.builder.build_gep(i8, sr_buf, &[sr_offset], "dst").map_err(llvm_err) }?;
        let _ = self.builder.build_call(self.module.get_function("memcpy").unwrap(), &[sr_dst.into(), sr_sptr.into(), sr_slen.into()], "").map_err(llvm_err)?;
        let sr_i_next = self.builder.build_int_add(sr_i.as_basic_value().into_int_value(), i64.const_int(1, false), "sri_next").map_err(llvm_err)?;
        let sr_done_cond = self.builder.build_int_compare(IntPredicate::SGE, sr_i_next, sr_n, "srdone").map_err(llvm_err)?;
        let sr_loop_block = self.builder.get_insert_block().unwrap();
        sr_i.add_incoming(&[(&i64.const_int(0, false), sr_entry), (&sr_i_next, sr_loop_block)]);
        let _ = self.builder.build_conditional_branch(sr_done_cond, sr_done_bb, sr_loop_bb);
        self.builder.position_at_end(sr_done_bb);
        let sr_undef = str_ty.get_undef();
        let sr_r1 = self.builder.build_insert_value(sr_undef, sr_total, 0, "r1").map_err(llvm_err)?;
        let sr_r2 = self.builder.build_insert_value(sr_r1, sr_buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&sr_r2));

        // ---- atomic_string_trim_start({i64, ptr}) -> {i64, ptr} ----
        let ts_fn = self.module.add_function("atomic_string_trim_start",
            str_ty.fn_type(&[str_ty.into()], false), None);
        let ts_entry = self.context.append_basic_block(ts_fn, "entry");
        self.builder.position_at_end(ts_entry);
        let ts_str = ts_fn.get_first_param().unwrap().into_struct_value();
        let ts_len = self.builder.build_extract_value(ts_str, 0, "len").map_err(llvm_err)?.into_int_value();
        let ts_ptr = self.builder.build_extract_value(ts_str, 1, "ptr").map_err(llvm_err)?.into_pointer_value();
        let ts_loop_bb = self.context.append_basic_block(ts_fn, "ts_loop");
        let ts_done_bb = self.context.append_basic_block(ts_fn, "ts_done");
        let _ = self.builder.build_unconditional_branch(ts_loop_bb);
        self.builder.position_at_end(ts_loop_bb);
        let ts_i = self.builder.build_phi(i64, "ts_i").map_err(llvm_err)?;
        let ts_cp = unsafe { self.builder.build_gep(i8, ts_ptr, &[ts_i.as_basic_value().into_int_value()], "cp").map_err(llvm_err) }?;
        let ts_c = self.builder.build_load(i8, ts_cp, "c").map_err(llvm_err)?.into_int_value();
        let ts_space = i8.const_int(0x20, false);
        let ts_tab = i8.const_int(0x09, false);
        let ts_nl = i8.const_int(0x0a, false);
        let ts_cr = i8.const_int(0x0d, false);
        let ts_is_space = self.builder.build_int_compare(IntPredicate::EQ, ts_c, ts_space, "is_space").map_err(llvm_err)?;
        let ts_is_tab = self.builder.build_int_compare(IntPredicate::EQ, ts_c, ts_tab, "is_tab").map_err(llvm_err)?;
        let ts_is_nl = self.builder.build_int_compare(IntPredicate::EQ, ts_c, ts_nl, "is_nl").map_err(llvm_err)?;
        let ts_is_cr = self.builder.build_int_compare(IntPredicate::EQ, ts_c, ts_cr, "is_cr").map_err(llvm_err)?;
        let ts_is_ws1 = self.builder.build_or(ts_is_space, ts_is_tab, "ws1").map_err(llvm_err)?;
        let ts_is_ws2 = self.builder.build_or(ts_is_nl, ts_is_cr, "ws2").map_err(llvm_err)?;
        let ts_is_ws = self.builder.build_or(ts_is_ws1, ts_is_ws2, "is_ws").map_err(llvm_err)?;
        let ts_i_next = self.builder.build_int_add(ts_i.as_basic_value().into_int_value(), i64.const_int(1, false), "ts_inext").map_err(llvm_err)?;
        let ts_at_end = self.builder.build_int_compare(IntPredicate::SGE, ts_i_next, ts_len, "at_end").map_err(llvm_err)?;
        let ts_stop = self.builder.build_or(ts_at_end, self.builder.build_not(ts_is_ws, "not_ws").map_err(llvm_err)?, "stop").map_err(llvm_err)?;
        let ts_loop_block = self.builder.get_insert_block().unwrap();
        ts_i.add_incoming(&[(&i64.const_int(0, false), ts_entry), (&ts_i_next, ts_loop_block)]);
        let _ = self.builder.build_conditional_branch(ts_stop, ts_done_bb, ts_loop_bb);
        self.builder.position_at_end(ts_done_bb);
        let ts_start = self.builder.build_phi(i64, "ts_start").map_err(llvm_err)?;
        ts_start.add_incoming(&[(&ts_i.as_basic_value().into_int_value(), ts_loop_block)]);
        // Use start idx as the new start; if start == len, return empty string
        let ts_new_len = self.builder.build_int_sub(ts_len, ts_start.as_basic_value().into_int_value(), "new_len").map_err(llvm_err)?;
        let ts_nptr = unsafe { self.builder.build_gep(i8, ts_ptr, &[ts_start.as_basic_value().into_int_value()], "nptr").map_err(llvm_err) }?;
        let ts_undef = str_ty.get_undef();
        let ts_r1 = self.builder.build_insert_value(ts_undef, ts_new_len, 0, "r1").map_err(llvm_err)?;
        let ts_r2 = self.builder.build_insert_value(ts_r1, ts_nptr, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&ts_r2));

        // ---- atomic_string_trim_end({i64, ptr}) -> {i64, ptr} ----
        let te_fn = self.module.add_function("atomic_string_trim_end",
            str_ty.fn_type(&[str_ty.into()], false), None);
        let te_entry = self.context.append_basic_block(te_fn, "entry");
        self.builder.position_at_end(te_entry);
        let te_str = te_fn.get_first_param().unwrap().into_struct_value();
        let te_len = self.builder.build_extract_value(te_str, 0, "len").map_err(llvm_err)?.into_int_value();
        let te_ptr = self.builder.build_extract_value(te_str, 1, "ptr").map_err(llvm_err)?.into_pointer_value();
        // Start from len-1 and go backwards
        let te_start = self.builder.build_int_sub(te_len, i64.const_int(1, false), "last").map_err(llvm_err)?;
        let te_loop_bb = self.context.append_basic_block(te_fn, "te_loop");
        let te_done_bb = self.context.append_basic_block(te_fn, "te_done");
        let _ = self.builder.build_unconditional_branch(te_loop_bb);
        self.builder.position_at_end(te_loop_bb);
        let te_i = self.builder.build_phi(i64, "te_i").map_err(llvm_err)?;
        let te_cp = unsafe { self.builder.build_gep(i8, te_ptr, &[te_i.as_basic_value().into_int_value()], "cp").map_err(llvm_err) }?;
        let te_c = self.builder.build_load(i8, te_cp, "c").map_err(llvm_err)?.into_int_value();
        let te_is_space = self.builder.build_int_compare(IntPredicate::EQ, te_c, i8.const_int(0x20, false), "is_space").map_err(llvm_err)?;
        let te_is_tab = self.builder.build_int_compare(IntPredicate::EQ, te_c, i8.const_int(0x09, false), "is_tab").map_err(llvm_err)?;
        let te_is_nl = self.builder.build_int_compare(IntPredicate::EQ, te_c, i8.const_int(0x0a, false), "is_nl").map_err(llvm_err)?;
        let te_is_cr = self.builder.build_int_compare(IntPredicate::EQ, te_c, i8.const_int(0x0d, false), "is_cr").map_err(llvm_err)?;
        let te_is_ws1 = self.builder.build_or(te_is_space, te_is_tab, "ws1").map_err(llvm_err)?;
        let te_is_ws2 = self.builder.build_or(te_is_nl, te_is_cr, "ws2").map_err(llvm_err)?;
        let te_is_ws = self.builder.build_or(te_is_ws1, te_is_ws2, "is_ws").map_err(llvm_err)?;
        let te_i_next = self.builder.build_int_sub(te_i.as_basic_value().into_int_value(), i64.const_int(1, false), "te_inext").map_err(llvm_err)?;
        let te_neg = self.builder.build_int_compare(IntPredicate::SLT, te_i_next, i64.const_int(0, false), "neg").map_err(llvm_err)?;
        let te_stop = self.builder.build_or(te_neg, self.builder.build_not(te_is_ws, "not_ws").map_err(llvm_err)?, "stop").map_err(llvm_err)?;
        let te_loop_block = self.builder.get_insert_block().unwrap();
        te_i.add_incoming(&[(&te_start, te_entry), (&te_i_next, te_loop_block)]);
        let _ = self.builder.build_conditional_branch(te_stop, te_done_bb, te_loop_bb);
        self.builder.position_at_end(te_done_bb);
        // te_i is the index of the character we just checked.
        // If it was not whitespace, new_len = te_i + 1.
        // If te_neg was true (all whitespace), te_i = 0 but we need new_len = 0.
        // Check te_neg by checking if te_i_next < 0
        let _te_neg_check = self.builder.build_int_compare(IntPredicate::SLT, te_i.as_basic_value().into_int_value(), i64.const_int(0, false), "neg_check").map_err(llvm_err)?;
        // Re-check: was the character at te_i whitespace?
        // Easier: just re-load and check
        let te_final_cp = unsafe { self.builder.build_gep(i8, te_ptr, &[te_i.as_basic_value().into_int_value()], "fcp").map_err(llvm_err) }?;
        let te_final_c = self.builder.build_load(i8, te_final_cp, "fc").map_err(llvm_err)?.into_int_value();
        let te_final_ws1 = self.builder.build_or(
            self.builder.build_int_compare(IntPredicate::EQ, te_final_c, i8.const_int(0x20, false), "").map_err(llvm_err)?,
            self.builder.build_int_compare(IntPredicate::EQ, te_final_c, i8.const_int(0x09, false), "").map_err(llvm_err)?, "").map_err(llvm_err)?;
        let te_final_ws2 = self.builder.build_or(
            self.builder.build_int_compare(IntPredicate::EQ, te_final_c, i8.const_int(0x0a, false), "").map_err(llvm_err)?,
            self.builder.build_int_compare(IntPredicate::EQ, te_final_c, i8.const_int(0x0d, false), "").map_err(llvm_err)?, "").map_err(llvm_err)?;
        let te_final_ws = self.builder.build_or(te_final_ws1, te_final_ws2, "fws").map_err(llvm_err)?;
        let te_zero_len = i64.const_int(0, false);
        let te_plus1 = self.builder.build_int_add(te_i.as_basic_value().into_int_value(), i64.const_int(1, false), "plus1").map_err(llvm_err)?;
        let te_new_len = self.builder.build_select(te_final_ws, te_zero_len, te_plus1, "new_len").map_err(llvm_err)?.into_int_value();
        let te_undef = str_ty.get_undef();
        let te_r1 = self.builder.build_insert_value(te_undef, te_new_len, 0, "r1").map_err(llvm_err)?;
        let te_r2 = self.builder.build_insert_value(te_r1, te_ptr, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&te_r2));


        Ok(())
    }

    pub(super) fn define_string_basics(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let f64 = self.f64_ty();
        let ptr = self.ptr_ty();
        let str_ty = self.string_type;
        let b1 = self.bool_ty();
        let i8 = self.context.i8_type();
        let i32 = self.context.i32_type();

        let malloc_rc_fn = self.module.get_function("atomic_malloc_rc").unwrap();
        let memcmp_fn = self.module.get_function("memcmp").unwrap();
        let sprintf_fn = self.module.get_function("sprintf").unwrap();
        let strlen_fn = self.module.get_function("strlen").unwrap();

        // ---- atomic_string_create(ptr, i64) -> {i64, ptr} ----
        let str_create_fn = self.module.add_function("atomic_string_create", str_ty.fn_type(&[ptr.into(), i64.into()], false), None);
        let entry = self.context.append_basic_block(str_create_fn, "entry");
        self.builder.position_at_end(entry);
        let data = str_create_fn.get_first_param().unwrap().into_pointer_value();
        let len = str_create_fn.get_nth_param(1).unwrap().into_int_value();
        let one = i64.const_int(1, false);
        let alloc_size = self.builder.build_int_add(len, one, "alloc_size").map_err(llvm_err)?;
        let buf = self.builder.build_call(malloc_rc_fn, &[alloc_size.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let _ = self.builder.build_memcpy(buf, 1, data, 1, len).map_err(llvm_err)?;
        let null_pos = unsafe { self.builder.build_gep(i8, buf, &[len], "null_pos").map_err(llvm_err) }?;
        let zero_byte = i8.const_int(0, false);
        let _ = self.builder.build_store(null_pos, zero_byte).map_err(llvm_err)?;
        let undef = str_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, len, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r2));

        // ---- atomic_string_concat({i64, ptr}, {i64, ptr}) -> {i64, ptr} ----
        let str_concat_fn = self.module.add_function("atomic_string_concat", str_ty.fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let entry = self.context.append_basic_block(str_concat_fn, "entry");
        self.builder.position_at_end(entry);
        let s1 = str_concat_fn.get_first_param().unwrap().into_struct_value();
        let s2 = str_concat_fn.get_nth_param(1).unwrap().into_struct_value();
        let len1 = self.builder.build_extract_value(s1, 0, "len1").map_err(llvm_err)?.into_int_value();
        let data1 = self.builder.build_extract_value(s1, 1, "data1").map_err(llvm_err)?.into_pointer_value();
        let len2 = self.builder.build_extract_value(s2, 0, "len2").map_err(llvm_err)?.into_int_value();
        let data2 = self.builder.build_extract_value(s2, 1, "data2").map_err(llvm_err)?.into_pointer_value();
        let total = self.builder.build_int_add(len1, len2, "total").map_err(llvm_err)?;
        let alloc_size = self.builder.build_int_add(total, i64.const_int(1, false), "alloc_size").map_err(llvm_err)?;
        let buf = self.builder.build_call(malloc_rc_fn, &[alloc_size.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let _ = self.builder.build_memcpy(buf, 1, data1, 1, len1).map_err(llvm_err)?;
        let offset = unsafe { self.builder.build_gep(i8, buf, &[len1], "offset").map_err(llvm_err) }?;
        let _ = self.builder.build_memcpy(offset, 1, data2, 1, len2).map_err(llvm_err)?;
        let null_pos = unsafe { self.builder.build_gep(i8, buf, &[total], "null_pos").map_err(llvm_err) }?;
        self.builder.build_store(null_pos, i8.const_int(0, false)).map_err(llvm_err)?;
        let undef = str_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, total, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r2));

        // ---- atomic_string_eq({i64, ptr}, {i64, ptr}) -> i1 ----
        let str_eq_fn = self.module.add_function("atomic_string_eq", b1.fn_type(&[str_ty.into(), str_ty.into()], false), None);
        let entry_bb = self.context.append_basic_block(str_eq_fn, "entry");
        let compare_bb = self.context.append_basic_block(str_eq_fn, "compare");
        let check_ptr_bb = self.context.append_basic_block(str_eq_fn, "check_ptr");
        let do_memcmp_bb = self.context.append_basic_block(str_eq_fn, "do_memcmp");
        let true_bb = self.context.append_basic_block(str_eq_fn, "true");
        let false_bb = self.context.append_basic_block(str_eq_fn, "false");
        let end_bb = self.context.append_basic_block(str_eq_fn, "end");
        let s1 = str_eq_fn.get_first_param().unwrap().into_struct_value();
        let s2 = str_eq_fn.get_nth_param(1).unwrap().into_struct_value();

        self.builder.position_at_end(entry_bb);
        let len1 = self.builder.build_extract_value(s1, 0, "len1").map_err(llvm_err)?.into_int_value();
        let len2 = self.builder.build_extract_value(s2, 0, "len2").map_err(llvm_err)?.into_int_value();
        let len_eq = self.builder.build_int_compare(IntPredicate::EQ, len1, len2, "len_eq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(len_eq, compare_bb, false_bb);

        self.builder.position_at_end(compare_bb);
        let zero_len = self.i64_ty().const_int(0, false);
        let is_empty = self.builder.build_int_compare(IntPredicate::EQ, len1, zero_len, "is_empty").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_empty, true_bb, check_ptr_bb);

        self.builder.position_at_end(check_ptr_bb);
        let data1 = self.builder.build_extract_value(s1, 1, "data1").map_err(llvm_err)?.into_pointer_value();
        let data2 = self.builder.build_extract_value(s2, 1, "data2").map_err(llvm_err)?.into_pointer_value();
        let null_ptr = self.ptr_ty().const_zero();
        let d1_null = self.builder.build_int_compare(IntPredicate::EQ, data1, null_ptr, "d1_null").map_err(llvm_err)?;
        let d2_null = self.builder.build_int_compare(IntPredicate::EQ, data2, null_ptr, "d2_null").map_err(llvm_err)?;
        let any_null = self.builder.build_or(d1_null, d2_null, "any_null").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(any_null, true_bb, do_memcmp_bb);

        self.builder.position_at_end(do_memcmp_bb);
        let memcmp_call = self.builder.build_call(memcmp_fn, &[data1.into(), data2.into(), len1.into()], "cmp").map_err(llvm_err)?;
        let cmp_result = memcmp_call.try_as_basic_value().basic().unwrap().into_int_value();
        let zero_i32 = i32.const_int(0, false);
        let content_eq = self.builder.build_int_compare(IntPredicate::EQ, cmp_result, zero_i32, "content_eq").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(end_bb);

        self.builder.position_at_end(true_bb);
        let _ = self.builder.build_unconditional_branch(end_bb);

        self.builder.position_at_end(false_bb);
        let _ = self.builder.build_unconditional_branch(end_bb);

        self.builder.position_at_end(end_bb);
        let phi = self.builder.build_phi(b1, "eq_result").map_err(llvm_err)?;
        phi.add_incoming(&[(&b1.const_int(1, false), true_bb), (&b1.const_int(0, false), false_bb), (&content_eq, do_memcmp_bb)]);
        let _ = self.builder.build_return(Some(&phi.as_basic_value()));

        // ---- atomic_string_len({i64, ptr}) -> i64 ----
        let str_len_fn = self.module.add_function("atomic_string_len", i64.fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(str_len_fn, "entry");
        self.builder.position_at_end(entry);
        let sl_s = str_len_fn.get_first_param().unwrap().into_struct_value();
        let sl_len = self.builder.build_extract_value(sl_s, 0, "len").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_return(Some(&sl_len));

        // ---- atomic_int_to_string(i64) -> {i64, ptr} ----
        let int_to_str_fn = self.module.add_function("atomic_int_to_string", str_ty.fn_type(&[i64.into()], false), None);
        let entry = self.context.append_basic_block(int_to_str_fn, "entry");
        self.builder.position_at_end(entry);
        let n = int_to_str_fn.get_first_param().unwrap().into_int_value();
        let buf32 = self.i64_ty().const_int(32, false);
        let buf = self.builder.build_call(malloc_rc_fn, &[buf32.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let fmt_int = self.make_global_str(".fmt_int_str", b"%ld\0");
        let _ = self.builder.build_call(sprintf_fn, &[buf.into(), fmt_int.into(), n.into()], "").map_err(llvm_err)?;
        let len = self.builder.build_call(strlen_fn, &[buf.into()], "len").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let undef = str_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, len, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r2));

        // ---- atomic_float_to_string(f64) -> {i64, ptr} ----
        let float_to_str_fn = self.module.add_function("atomic_float_to_string", str_ty.fn_type(&[f64.into()], false), None);
        let entry = self.context.append_basic_block(float_to_str_fn, "entry");
        self.builder.position_at_end(entry);
        let n = float_to_str_fn.get_first_param().unwrap().into_float_value();
        let buf32 = self.i64_ty().const_int(32, false);
        let buf = self.builder.build_call(malloc_rc_fn, &[buf32.into()], "buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let fmt_float = self.make_global_str(".fmt_float_str", b"%g\0");
        let _ = self.builder.build_call(sprintf_fn, &[buf.into(), fmt_float.into(), n.into()], "").map_err(llvm_err)?;
        let len = self.builder.build_call(strlen_fn, &[buf.into()], "len").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
        let undef = str_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, len, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, buf, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r2));

        Ok(())
    }
}
