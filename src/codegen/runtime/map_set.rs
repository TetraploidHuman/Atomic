use inkwell::values::{BasicValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::super::{CodeGen, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn define_map_basics(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let ptr = self.ptr_ty();
        let list_ty = self.list_type;
        let str_ty = self.string_type;
        let b1 = self.bool_ty();
        let i8 = self.context.i8_type();

        let malloc_rc_fn = self.module.get_function("atomic_malloc_rc").unwrap();
        let realloc_fn = self.module.get_function("realloc").unwrap();
        let memcpy_fn = self.module.get_function("memcpy").unwrap();

        // ---- atomic_map_create(i64 capacity) -> {ptr, i64, i64} ----
        let map_create_fn = self.module.add_function("atomic_map_create", list_ty.fn_type(&[i64.into()], false), None);
        let entry = self.context.append_basic_block(map_create_fn, "entry");
        self.builder.position_at_end(entry);
        let cap = map_create_fn.get_first_param().unwrap().into_int_value();
        let thirty_two = i64.const_int(32, false);
        let data_size = self.builder.build_int_mul(cap, thirty_two, "m_data_size").map_err(llvm_err)?;
        let data = self.builder.build_call(malloc_rc_fn, &[data_size.into()], "m_data").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let zero = i64.const_int(0, false);
        let undef = list_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, data, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, zero, 1, "r2").map_err(llvm_err)?;
        let r3 = self.builder.build_insert_value(r2, cap, 2, "r3").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&r3));

        // ---- atomic_map_insert / atomic_map_get / atomic_map_contains ----
        // atomic_map_insert({ptr,i64,i64}, {i64,ptr}, {i64,ptr}) -> {ptr,i64,i64}
        let mi_fn = self.module.add_function("atomic_map_insert",
            list_ty.fn_type(&[list_ty.into(), str_ty.into(), str_ty.into()], false), None);
        let mi_entry = self.context.append_basic_block(mi_fn, "entry");
        let mi_search = self.context.append_basic_block(mi_fn, "search");
        let mi_body = self.context.append_basic_block(mi_fn, "body");
        let mi_ckey = self.context.append_basic_block(mi_fn, "ckey");
        let mi_update = self.context.append_basic_block(mi_fn, "update");
        let mi_next = self.context.append_basic_block(mi_fn, "next");
        let mi_append_check = self.context.append_basic_block(mi_fn, "append_ck");
        let mi_grow = self.context.append_basic_block(mi_fn, "append_grow");
        let mi_append_store = self.context.append_basic_block(mi_fn, "append_store");

        self.builder.position_at_end(mi_entry);
        let mi_map = mi_fn.get_first_param().unwrap().into_struct_value();
        let mi_key = mi_fn.get_nth_param(1).unwrap().into_struct_value();
        let mi_val = mi_fn.get_nth_param(2).unwrap().into_struct_value();
        let mi_data = self.builder.build_extract_value(mi_map, 0, "d").map_err(llvm_err)?.into_pointer_value();
        let mi_len = self.builder.build_extract_value(mi_map, 1, "l").map_err(llvm_err)?.into_int_value();
        let mi_cap = self.builder.build_extract_value(mi_map, 2, "c").map_err(llvm_err)?.into_int_value();
        let mi_ktag = self.builder.build_extract_value(mi_key, 0, "kt").map_err(llvm_err)?.into_int_value();
        let mi_kptr = self.builder.build_extract_value(mi_key, 1, "kp").map_err(llvm_err)?.into_pointer_value();
        let mi_vtag = self.builder.build_extract_value(mi_val, 0, "vt").map_err(llvm_err)?.into_int_value();
        let mi_vptr = self.builder.build_extract_value(mi_val, 1, "vp").map_err(llvm_err)?.into_pointer_value();
        let mi_kp_i64 = self.builder.build_ptr_to_int(mi_kptr, i64, "kp_i64").map_err(llvm_err)?;
        let mi_vp_i64 = self.builder.build_ptr_to_int(mi_vptr, i64, "vp_i64").map_err(llvm_err)?;
        let mi_di64 = self.builder.build_pointer_cast(mi_data, ptr, "di64").map_err(llvm_err)?;
        let mi_i = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(mi_i, zero).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mi_search);

        self.builder.position_at_end(mi_search);
        let mi_iv = self.builder.build_load(i64, mi_i, "iv").map_err(llvm_err)?.into_int_value();
        let mi_cond = self.builder.build_int_compare(IntPredicate::SLT, mi_iv, mi_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mi_cond, mi_body, mi_append_check);

        self.builder.position_at_end(mi_body);
        let mi_off = self.builder.build_int_mul(mi_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let mi_etp = unsafe { self.builder.build_gep(i64, mi_di64, &[mi_off], "etp").map_err(llvm_err) }?;
        let mi_et = self.builder.build_load(i64, mi_etp, "et").map_err(llvm_err)?.into_int_value();
        let mi_teq = self.builder.build_int_compare(IntPredicate::EQ, mi_et, mi_ktag, "teq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mi_teq, mi_ckey, mi_next);

        self.builder.position_at_end(mi_ckey);
        let mi_off1 = self.builder.build_int_add(mi_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let mi_epp = unsafe { self.builder.build_gep(i64, mi_di64, &[mi_off1], "epp").map_err(llvm_err) }?;
        let mi_ep = self.builder.build_load(i64, mi_epp, "ep").map_err(llvm_err)?.into_int_value();
        let mi_kpz = self.builder.build_int_compare(IntPredicate::EQ, mi_kp_i64, zero, "kpz").map_err(llvm_err)?;
        let mi_ek_undef = str_ty.get_undef();
        let mi_ek1 = self.builder.build_insert_value(mi_ek_undef, mi_et, 0, "ek1").map_err(llvm_err)?;
        let mi_ep_ptr = self.builder.build_int_to_ptr(mi_ep, ptr, "ep_ptr").map_err(llvm_err)?;
        let mi_ek2 = self.builder.build_insert_value(mi_ek1, mi_ep_ptr, 1, "ek2").map_err(llvm_err)?;
        let seq_fn = self.module.get_function("atomic_string_eq").unwrap();
        let mi_seq = self.builder.build_call(seq_fn, &[mi_ek2.as_basic_value_enum().into(), mi_key.into()], "seq").map_err(llvm_err)?;
        let mi_seq_r = mi_seq.try_as_basic_value().basic().unwrap().into_int_value();
        let mi_feq = self.builder.build_select(mi_kpz, mi_teq, mi_seq_r, "feq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mi_feq.into_int_value(), mi_update, mi_next);

        self.builder.position_at_end(mi_update);
        let mi_off2 = self.builder.build_int_add(mi_off, i64.const_int(2, false), "off2").map_err(llvm_err)?;
        let mi_vtp = unsafe { self.builder.build_gep(i64, mi_di64, &[mi_off2], "vtp").map_err(llvm_err) }?;
        self.builder.build_store(mi_vtp, mi_vtag).map_err(llvm_err)?;
        let mi_off3 = self.builder.build_int_add(mi_off, i64.const_int(3, false), "off3").map_err(llvm_err)?;
        let mi_vpp = unsafe { self.builder.build_gep(i64, mi_di64, &[mi_off3], "vpp").map_err(llvm_err) }?;
        self.builder.build_store(mi_vpp, mi_vp_i64).map_err(llvm_err)?;
        let mi_ur = list_ty.get_undef();
        let mi_r1 = self.builder.build_insert_value(mi_ur, mi_data, 0, "r1").map_err(llvm_err)?;
        let mi_r2 = self.builder.build_insert_value(mi_r1, mi_len, 1, "r2").map_err(llvm_err)?;
        let mi_r3 = self.builder.build_insert_value(mi_r2, mi_cap, 2, "r3").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&mi_r3));

        self.builder.position_at_end(mi_next);
        let mi_niv = self.builder.build_int_add(mi_iv, i64.const_int(1, false), "niv").map_err(llvm_err)?;
        self.builder.build_store(mi_i, mi_niv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mi_search);

        self.builder.position_at_end(mi_append_check);
        let need_grow = self.builder.build_int_compare(IntPredicate::SGE, mi_len, mi_cap, "need_grow").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(need_grow, mi_grow, mi_append_store);

        self.builder.position_at_end(mi_grow);
        let min_cap = i64.const_int(4, false);
        let cap_small = self.builder.build_int_compare(IntPredicate::SLT, mi_cap, min_cap, "cap_small").map_err(llvm_err)?;
        let cap2x = self.builder.build_int_mul(mi_cap, i64.const_int(2, false), "cap2x").map_err(llvm_err)?;
        let new_cap = self.builder.build_select(cap_small, min_cap, cap2x, "new_cap").map_err(llvm_err)?.into_int_value();
        let data_size = self.builder.build_int_mul(new_cap, i64.const_int(32, false), "data_size").map_err(llvm_err)?;
        let total_size = self.builder.build_int_add(data_size, i64.const_int(8, false), "total_size").map_err(llvm_err)?;
        let data_int = self.builder.build_ptr_to_int(mi_data, i64, "mi_data_int").map_err(llvm_err)?;
        let rc_offset = i64.const_int(8, false);
        let orig_int = self.builder.build_int_sub(data_int, rc_offset, "mi_orig_int").map_err(llvm_err)?;
        let orig_ptr = self.builder.build_int_to_ptr(orig_int, ptr, "mi_orig_ptr").map_err(llvm_err)?;
        let new_orig = self.builder.build_call(realloc_fn, &[orig_ptr.into(), total_size.into()], "mi_new_orig").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let new_orig_int = self.builder.build_ptr_to_int(new_orig, i64, "mi_new_orig_int").map_err(llvm_err)?;
        let new_data_int = self.builder.build_int_add(new_orig_int, rc_offset, "mi_new_data_int").map_err(llvm_err)?;
        let new_data = self.builder.build_int_to_ptr(new_data_int, ptr, "mi_new_data").map_err(llvm_err)?;
        let new_di64 = self.builder.build_pointer_cast(new_data, ptr, "new_di64").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mi_append_store);

        self.builder.position_at_end(mi_append_store);
        let phi_data = self.builder.build_phi(ptr, "phi_data").map_err(llvm_err)?;
        phi_data.add_incoming(&[(&mi_data, mi_append_check), (&new_data, mi_grow)]);
        let phi_di64 = self.builder.build_phi(ptr, "phi_di64").map_err(llvm_err)?;
        phi_di64.add_incoming(&[(&mi_di64, mi_append_check), (&new_di64, mi_grow)]);
        let phi_cap = self.builder.build_phi(i64, "phi_cap").map_err(llvm_err)?;
        phi_cap.add_incoming(&[(&mi_cap, mi_append_check), (&new_cap, mi_grow)]);
        let final_data = phi_data.as_basic_value().into_pointer_value();
        let final_di64 = phi_di64.as_basic_value().into_pointer_value();
        let final_cap = phi_cap.as_basic_value().into_int_value();
        let mi_lo = self.builder.build_int_mul(mi_len, i64.const_int(4, false), "lo").map_err(llvm_err)?;
        let mi_nkt = unsafe { self.builder.build_gep(i64, final_di64, &[mi_lo], "nkt").map_err(llvm_err) }?;
        self.builder.build_store(mi_nkt, mi_ktag).map_err(llvm_err)?;
        let mi_lo1 = self.builder.build_int_add(mi_lo, i64.const_int(1, false), "lo1").map_err(llvm_err)?;
        let mi_nkp = unsafe { self.builder.build_gep(i64, final_di64, &[mi_lo1], "nkp").map_err(llvm_err) }?;
        self.builder.build_store(mi_nkp, mi_kp_i64).map_err(llvm_err)?;
        let mi_lo2 = self.builder.build_int_add(mi_lo, i64.const_int(2, false), "lo2").map_err(llvm_err)?;
        let mi_nvt = unsafe { self.builder.build_gep(i64, final_di64, &[mi_lo2], "nvt").map_err(llvm_err) }?;
        self.builder.build_store(mi_nvt, mi_vtag).map_err(llvm_err)?;
        let mi_lo3 = self.builder.build_int_add(mi_lo, i64.const_int(3, false), "lo3").map_err(llvm_err)?;
        let mi_nvp = unsafe { self.builder.build_gep(i64, final_di64, &[mi_lo3], "nvp").map_err(llvm_err) }?;
        self.builder.build_store(mi_nvp, mi_vp_i64).map_err(llvm_err)?;
        let mi_nl = self.builder.build_int_add(mi_len, i64.const_int(1, false), "nl").map_err(llvm_err)?;
        let mi_ur2 = list_ty.get_undef();
        let mi_rr1 = self.builder.build_insert_value(mi_ur2, final_data, 0, "rr1").map_err(llvm_err)?;
        let mi_rr2 = self.builder.build_insert_value(mi_rr1, mi_nl, 1, "rr2").map_err(llvm_err)?;
        let mi_rr3 = self.builder.build_insert_value(mi_rr2, final_cap, 2, "rr3").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&mi_rr3));

        // atomic_map_get({ptr,i64,i64}, {i64,ptr}) -> {i64,ptr}
        let mg_fn = self.module.add_function("atomic_map_get",
            str_ty.fn_type(&[list_ty.into(), str_ty.into()], false), None);
        let mg_blocks: Vec<_> = (0..7).map(|i| self.context.append_basic_block(mg_fn, &format!("b{}", i))).collect();
        self.builder.position_at_end(mg_blocks[0]); // entry
        let mg_map = mg_fn.get_first_param().unwrap().into_struct_value();
        let mg_key = mg_fn.get_nth_param(1).unwrap().into_struct_value();
        let mg_data = self.builder.build_extract_value(mg_map, 0, "d").map_err(llvm_err)?.into_pointer_value();
        let mg_len = self.builder.build_extract_value(mg_map, 1, "l").map_err(llvm_err)?.into_int_value();
        let mg_ktag = self.builder.build_extract_value(mg_key, 0, "kt").map_err(llvm_err)?.into_int_value();
        let mg_kptr = self.builder.build_extract_value(mg_key, 1, "kp").map_err(llvm_err)?.into_pointer_value();
        let mg_kp_i64 = self.builder.build_ptr_to_int(mg_kptr, i64, "kp_i64").map_err(llvm_err)?;
        let mg_di64 = self.builder.build_pointer_cast(mg_data, ptr, "di64").map_err(llvm_err)?;
        let mg_i = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(mg_i, zero).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mg_blocks[1]);

        self.builder.position_at_end(mg_blocks[1]); // search
        let mg_iv = self.builder.build_load(i64, mg_i, "iv").map_err(llvm_err)?.into_int_value();
        let mg_cond = self.builder.build_int_compare(IntPredicate::SLT, mg_iv, mg_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mg_cond, mg_blocks[2], mg_blocks[6]);

        self.builder.position_at_end(mg_blocks[2]); // body
        let mg_off = self.builder.build_int_mul(mg_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let mg_etp = unsafe { self.builder.build_gep(i64, mg_di64, &[mg_off], "etp").map_err(llvm_err) }?;
        let mg_et = self.builder.build_load(i64, mg_etp, "et").map_err(llvm_err)?.into_int_value();
        let mg_teq = self.builder.build_int_compare(IntPredicate::EQ, mg_et, mg_ktag, "teq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mg_teq, mg_blocks[3], mg_blocks[5]);

        self.builder.position_at_end(mg_blocks[3]); // ckey
        let mg_off1 = self.builder.build_int_add(mg_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let mg_epp = unsafe { self.builder.build_gep(i64, mg_di64, &[mg_off1], "epp").map_err(llvm_err) }?;
        let mg_ep = self.builder.build_load(i64, mg_epp, "ep").map_err(llvm_err)?.into_int_value();
        let mg_kpz = self.builder.build_int_compare(IntPredicate::EQ, mg_kp_i64, zero, "kpz").map_err(llvm_err)?;
        let mg_ek_undef = str_ty.get_undef();
        let mg_ek1 = self.builder.build_insert_value(mg_ek_undef, mg_et, 0, "ek1").map_err(llvm_err)?;
        let mg_ep_ptr = self.builder.build_int_to_ptr(mg_ep, ptr, "ep_ptr").map_err(llvm_err)?;
        let mg_ek2 = self.builder.build_insert_value(mg_ek1, mg_ep_ptr, 1, "ek2").map_err(llvm_err)?;
        let seq_fn2 = self.module.get_function("atomic_string_eq").unwrap();
        let mg_seq = self.builder.build_call(seq_fn2, &[mg_ek2.as_basic_value_enum().into(), mg_key.into()], "seq").map_err(llvm_err)?;
        let mg_seq_r = mg_seq.try_as_basic_value().basic().unwrap().into_int_value();
        let mg_feq = self.builder.build_select(mg_kpz, mg_teq, mg_seq_r, "feq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mg_feq.into_int_value(), mg_blocks[4], mg_blocks[5]);

        self.builder.position_at_end(mg_blocks[4]); // found
        let mg_off2 = self.builder.build_int_add(mg_off, i64.const_int(2, false), "off2").map_err(llvm_err)?;
        let mg_vtp = unsafe { self.builder.build_gep(i64, mg_di64, &[mg_off2], "vtp").map_err(llvm_err) }?;
        let mg_vt = self.builder.build_load(i64, mg_vtp, "vt").map_err(llvm_err)?.into_int_value();
        let mg_off3 = self.builder.build_int_add(mg_off, i64.const_int(3, false), "off3").map_err(llvm_err)?;
        let mg_vpp = unsafe { self.builder.build_gep(i64, mg_di64, &[mg_off3], "vpp").map_err(llvm_err) }?;
        let mg_vp = self.builder.build_load(i64, mg_vpp, "vp").map_err(llvm_err)?.into_int_value();
        let mg_ur = str_ty.get_undef();
        let mg_r1 = self.builder.build_insert_value(mg_ur, mg_vt, 0, "r1").map_err(llvm_err)?;
        let mg_vp_ptr = self.builder.build_int_to_ptr(mg_vp, ptr, "vp_ptr").map_err(llvm_err)?;
        let mg_r2 = self.builder.build_insert_value(mg_r1, mg_vp_ptr, 1, "r2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&mg_r2));

        self.builder.position_at_end(mg_blocks[5]); // next
        let mg_niv = self.builder.build_int_add(mg_iv, i64.const_int(1, false), "niv").map_err(llvm_err)?;
        self.builder.build_store(mg_i, mg_niv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mg_blocks[1]);

        self.builder.position_at_end(mg_blocks[6]); // not_found
        let mg_ur2 = str_ty.get_undef();
        let mg_nf1 = self.builder.build_insert_value(mg_ur2, zero, 0, "nf1").map_err(llvm_err)?;
        let mg_nf2 = self.builder.build_insert_value(mg_nf1, ptr.const_zero(), 1, "nf2").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&mg_nf2));

        // atomic_map_contains({ptr,i64,i64}, {i64,ptr}) -> i1
        let mc_fn = self.module.add_function("atomic_map_contains",
            b1.fn_type(&[list_ty.into(), str_ty.into()], false), None);
        let mc_blocks: Vec<_> = (0..7).map(|i| self.context.append_basic_block(mc_fn, &format!("b{}", i))).collect();
        self.builder.position_at_end(mc_blocks[0]); // entry
        let mc_map = mc_fn.get_first_param().unwrap().into_struct_value();
        let mc_key = mc_fn.get_nth_param(1).unwrap().into_struct_value();
        let mc_data = self.builder.build_extract_value(mc_map, 0, "d").map_err(llvm_err)?.into_pointer_value();
        let mc_len = self.builder.build_extract_value(mc_map, 1, "l").map_err(llvm_err)?.into_int_value();
        let mc_ktag = self.builder.build_extract_value(mc_key, 0, "kt").map_err(llvm_err)?.into_int_value();
        let mc_kptr = self.builder.build_extract_value(mc_key, 1, "kp").map_err(llvm_err)?.into_pointer_value();
        let mc_kp_i64 = self.builder.build_ptr_to_int(mc_kptr, i64, "kp_i64").map_err(llvm_err)?;
        let mc_di64 = self.builder.build_pointer_cast(mc_data, ptr, "di64").map_err(llvm_err)?;
        let mc_i = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(mc_i, zero).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mc_blocks[1]);

        self.builder.position_at_end(mc_blocks[1]); // search
        let mc_iv = self.builder.build_load(i64, mc_i, "iv").map_err(llvm_err)?.into_int_value();
        let mc_cond = self.builder.build_int_compare(IntPredicate::SLT, mc_iv, mc_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mc_cond, mc_blocks[2], mc_blocks[6]);

        self.builder.position_at_end(mc_blocks[2]); // body
        let mc_off = self.builder.build_int_mul(mc_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let mc_etp = unsafe { self.builder.build_gep(i64, mc_di64, &[mc_off], "etp").map_err(llvm_err) }?;
        let mc_et = self.builder.build_load(i64, mc_etp, "et").map_err(llvm_err)?.into_int_value();
        let mc_teq = self.builder.build_int_compare(IntPredicate::EQ, mc_et, mc_ktag, "teq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mc_teq, mc_blocks[3], mc_blocks[5]);

        self.builder.position_at_end(mc_blocks[3]); // ckey
        let mc_off1 = self.builder.build_int_add(mc_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let mc_epp = unsafe { self.builder.build_gep(i64, mc_di64, &[mc_off1], "epp").map_err(llvm_err) }?;
        let mc_ep = self.builder.build_load(i64, mc_epp, "ep").map_err(llvm_err)?.into_int_value();
        let mc_kpz = self.builder.build_int_compare(IntPredicate::EQ, mc_kp_i64, zero, "kpz").map_err(llvm_err)?;
        let mc_ek_undef = str_ty.get_undef();
        let mc_ek1 = self.builder.build_insert_value(mc_ek_undef, mc_et, 0, "ek1").map_err(llvm_err)?;
        let mc_ep_ptr = self.builder.build_int_to_ptr(mc_ep, ptr, "ep_ptr").map_err(llvm_err)?;
        let mc_ek2 = self.builder.build_insert_value(mc_ek1, mc_ep_ptr, 1, "ek2").map_err(llvm_err)?;
        let seq_fn3 = self.module.get_function("atomic_string_eq").unwrap();
        let mc_seq = self.builder.build_call(seq_fn3, &[mc_ek2.as_basic_value_enum().into(), mc_key.into()], "seq").map_err(llvm_err)?;
        let mc_seq_r = mc_seq.try_as_basic_value().basic().unwrap().into_int_value();
        let mc_feq = self.builder.build_select(mc_kpz, mc_teq, mc_seq_r, "feq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mc_feq.into_int_value(), mc_blocks[4], mc_blocks[5]);

        self.builder.position_at_end(mc_blocks[4]); // found
        let _ = self.builder.build_return(Some(&b1.const_int(1, false)));

        self.builder.position_at_end(mc_blocks[5]); // next
        let mc_niv = self.builder.build_int_add(mc_iv, i64.const_int(1, false), "niv").map_err(llvm_err)?;
        self.builder.build_store(mc_i, mc_niv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mc_blocks[1]);

        self.builder.position_at_end(mc_blocks[6]); // not_found
        let _ = self.builder.build_return(Some(&b1.const_int(0, false)));

        // ---- atomic_map_remove({ptr,i64,i64}, {i64,ptr}) -> {ptr,i64,i64} ----
        let mr_fn = self.module.add_function("atomic_map_remove",
            list_ty.fn_type(&[list_ty.into(), str_ty.into()], false), None);
        let mr_blocks: Vec<_> = (0..8).map(|i| self.context.append_basic_block(mr_fn, &format!("b{}", i))).collect();
        self.builder.position_at_end(mr_blocks[0]); // entry
        let mr_map = mr_fn.get_first_param().unwrap().into_struct_value();
        let mr_key = mr_fn.get_nth_param(1).unwrap().into_struct_value();
        let mr_data = self.builder.build_extract_value(mr_map, 0, "d").map_err(llvm_err)?.into_pointer_value();
        let mr_len = self.builder.build_extract_value(mr_map, 1, "l").map_err(llvm_err)?.into_int_value();
        let mr_cap = self.builder.build_extract_value(mr_map, 2, "c").map_err(llvm_err)?.into_int_value();
        let mr_ktag = self.builder.build_extract_value(mr_key, 0, "kt").map_err(llvm_err)?.into_int_value();
        let mr_kptr = self.builder.build_extract_value(mr_key, 1, "kp").map_err(llvm_err)?.into_pointer_value();
        let mr_kp_i64 = self.builder.build_ptr_to_int(mr_kptr, i64, "kp_i64").map_err(llvm_err)?;
        let mr_di64 = self.builder.build_pointer_cast(mr_data, ptr, "di64").map_err(llvm_err)?;
        let mr_i = self.builder.build_alloca(i64, "i").map_err(llvm_err)?;
        self.builder.build_store(mr_i, zero).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mr_blocks[1]);

        self.builder.position_at_end(mr_blocks[1]); // search
        let mr_iv = self.builder.build_load(i64, mr_i, "iv").map_err(llvm_err)?.into_int_value();
        let mr_cond = self.builder.build_int_compare(IntPredicate::SLT, mr_iv, mr_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mr_cond, mr_blocks[2], mr_blocks[7]);

        self.builder.position_at_end(mr_blocks[2]); // body
        let mr_off = self.builder.build_int_mul(mr_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let mr_etp = unsafe { self.builder.build_gep(i64, mr_di64, &[mr_off], "etp").map_err(llvm_err) }?;
        let mr_et = self.builder.build_load(i64, mr_etp, "et").map_err(llvm_err)?.into_int_value();
        let mr_teq = self.builder.build_int_compare(IntPredicate::EQ, mr_et, mr_ktag, "teq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mr_teq, mr_blocks[3], mr_blocks[6]);

        self.builder.position_at_end(mr_blocks[3]); // ckey
        let mr_off1 = self.builder.build_int_add(mr_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let mr_epp = unsafe { self.builder.build_gep(i64, mr_di64, &[mr_off1], "epp").map_err(llvm_err) }?;
        let mr_ep = self.builder.build_load(i64, mr_epp, "ep").map_err(llvm_err)?.into_int_value();
        let mr_kpz = self.builder.build_int_compare(IntPredicate::EQ, mr_kp_i64, zero, "kpz").map_err(llvm_err)?;
        let mr_ek_undef = str_ty.get_undef();
        let mr_ek1 = self.builder.build_insert_value(mr_ek_undef, mr_et, 0, "ek1").map_err(llvm_err)?;
        let mr_ep_ptr = self.builder.build_int_to_ptr(mr_ep, ptr, "ep_ptr").map_err(llvm_err)?;
        let mr_ek2 = self.builder.build_insert_value(mr_ek1, mr_ep_ptr, 1, "ek2").map_err(llvm_err)?;
        let seq_fn4 = self.module.get_function("atomic_string_eq").unwrap();
        let mr_seq = self.builder.build_call(seq_fn4, &[mr_ek2.as_basic_value_enum().into(), mr_key.into()], "seq").map_err(llvm_err)?;
        let mr_seq_r = mr_seq.try_as_basic_value().basic().unwrap().into_int_value();
        let mr_feq = self.builder.build_select(mr_kpz, mr_teq, mr_seq_r, "feq").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mr_feq.into_int_value(), mr_blocks[4], mr_blocks[6]);

        self.builder.position_at_end(mr_blocks[4]); // remove
        let mr_len_dec = self.builder.build_int_sub(mr_len, i64.const_int(1, false), "len_dec").map_err(llvm_err)?;
        let mr_iv_p1 = self.builder.build_int_add(mr_iv, i64.const_int(1, false), "iv_p1").map_err(llvm_err)?;
        let mr_remaining = self.builder.build_int_sub(mr_len, mr_iv_p1, "remaining").map_err(llvm_err)?;
        let mr_has_remaining = self.builder.build_int_compare(IntPredicate::SGT, mr_remaining, zero, "has_rem").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mr_has_remaining, mr_blocks[5], mr_blocks[7]);

        self.builder.position_at_end(mr_blocks[5]); // shift
        let mr_src_off = self.builder.build_int_mul(mr_iv_p1, i64.const_int(32, false), "src_off").map_err(llvm_err)?;
        let mr_dst_off = self.builder.build_int_mul(mr_iv, i64.const_int(32, false), "dst_off").map_err(llvm_err)?;
        let mr_src = unsafe { self.builder.build_gep(i8, mr_data, &[mr_src_off], "src").map_err(llvm_err) }?;
        let mr_dst = unsafe { self.builder.build_gep(i8, mr_data, &[mr_dst_off], "dst").map_err(llvm_err) }?;
        let mr_rem_bytes = self.builder.build_int_mul(mr_remaining, i64.const_int(32, false), "rem_bytes").map_err(llvm_err)?;
        let _ = self.builder.build_call(memcpy_fn, &[mr_dst.into(), mr_src.into(), mr_rem_bytes.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mr_blocks[7]);

        self.builder.position_at_end(mr_blocks[6]); // next
        let mr_niv = self.builder.build_int_add(mr_iv, i64.const_int(1, false), "niv").map_err(llvm_err)?;
        self.builder.build_store(mr_i, mr_niv).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mr_blocks[1]);

        self.builder.position_at_end(mr_blocks[7]); // done
        let mr_ret_len = self.builder.build_phi(i64, "ret_len").map_err(llvm_err)?;
        mr_ret_len.add_incoming(&[(&mr_len, mr_blocks[1]), (&mr_len_dec, mr_blocks[4]), (&mr_len_dec, mr_blocks[5])]);
        let mr_ret_len_val = mr_ret_len.as_basic_value().into_int_value();
        let mr_ur = list_ty.get_undef();
        let mr_r1 = self.builder.build_insert_value(mr_ur, mr_data, 0, "r1").map_err(llvm_err)?;
        let mr_r2 = self.builder.build_insert_value(mr_r1, mr_ret_len_val, 1, "r2").map_err(llvm_err)?;
        let mr_r3 = self.builder.build_insert_value(mr_r2, mr_cap, 2, "r3").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&mr_r3));

        Ok(())
    }

    pub(super) fn define_map_advanced(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let _f64 = self.f64_ty();
        let ptr = self.ptr_ty();
        let list_ty = self.list_type;
        let str_ty = self.string_type;
        let _b1 = self.bool_ty();
        let _i8 = self.context.i8_type();
        let _i32 = self.context.i32_type();
        // ---- atomic_map_keys({ptr, i64, i64}) -> {ptr, i64, i64} ----
        let mk_fn = self.module.add_function("atomic_map_keys", list_ty.fn_type(&[list_ty.into()], false), None);
        let mk_entry = self.context.append_basic_block(mk_fn, "entry");
        self.builder.position_at_end(mk_entry);
        let mk_in = mk_fn.get_first_param().unwrap().into_struct_value();
        let mk_data = self.builder.build_extract_value(mk_in, 0, "data").map_err(llvm_err)?.into_pointer_value();
        let mk_len = self.builder.build_extract_value(mk_in, 1, "len").map_err(llvm_err)?.into_int_value();
        let mk_data_i64 = self.builder.build_pointer_cast(mk_data, ptr, "data_i64").map_err(llvm_err)?;
        let mk_res = self.call_rt("atomic_list_create", &[i64.const_int(4, false).into()])?;
        let mk_resv = mk_res.try_as_basic_value().basic().unwrap();
        let mk_ra = self.builder.build_alloca(self.list_type, "mk_ra").map_err(llvm_err)?;
        self.builder.build_store(mk_ra, mk_resv).map_err(llvm_err)?;
        let mk_i = self.builder.build_alloca(i64, "mk_i").map_err(llvm_err)?;
        self.builder.build_store(mk_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let mk_loop = self.context.append_basic_block(mk_fn, "loop");
        let mk_body = self.context.append_basic_block(mk_fn, "body");
        let mk_done = self.context.append_basic_block(mk_fn, "done");
        let _ = self.builder.build_unconditional_branch(mk_loop);
        self.builder.position_at_end(mk_loop);
        let mk_iv = self.builder.build_load(i64, mk_i, "iv").map_err(llvm_err)?.into_int_value();
        let mk_cond = self.builder.build_int_compare(IntPredicate::SLT, mk_iv, mk_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mk_cond, mk_body, mk_done);
        self.builder.position_at_end(mk_body);
        // Map entry layout: [key_tag, key_ptr_i64, val_tag, val_ptr_i64] = 4 i64s per entry
        let mk_off = self.builder.build_int_mul(mk_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let mk_ktp = unsafe { self.builder.build_gep(i64, mk_data_i64, &[mk_off], "ktp").map_err(llvm_err) }?;
        let mk_kt = self.builder.build_load(i64, mk_ktp, "kt").map_err(llvm_err)?.into_int_value();
        let mk_off1 = self.builder.build_int_add(mk_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let mk_kpp = unsafe { self.builder.build_gep(i64, mk_data_i64, &[mk_off1], "kpp").map_err(llvm_err) }?;
        let mk_kp_i64 = self.builder.build_load(i64, mk_kpp, "kp_i64").map_err(llvm_err)?.into_int_value();
        let mk_kp = self.builder.build_int_to_ptr(mk_kp_i64, ptr, "kp").map_err(llvm_err)?;
        // Build key fat struct
        let mk_key_undef = self.string_type.get_undef();
        let mk_key1 = self.builder.build_insert_value(mk_key_undef, mk_kt, 0, "ktag").map_err(llvm_err)?;
        let mk_key2 = self.builder.build_insert_value(mk_key1, mk_kp, 1, "kdata").map_err(llvm_err)?;
        // Push key to result
        let mk_cl = self.builder.build_load(self.list_type, mk_ra, "cl").map_err(llvm_err)?.into_struct_value();
        let mk_ps = self.call_rt("atomic_list_push", &[mk_cl.into(), mk_key2.as_basic_value_enum().into()])?;
        self.builder.build_store(mk_ra, mk_ps.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let mk_inc = self.builder.build_int_add(mk_iv, i64.const_int(1, false), "inc").map_err(llvm_err)?;
        self.builder.build_store(mk_i, mk_inc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mk_loop);
        self.builder.position_at_end(mk_done);
        let mk_rt = self.builder.build_load(self.list_type, mk_ra, "mk_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&mk_rt));

        // ---- atomic_map_values({ptr, i64, i64}) -> {ptr, i64, i64} ----
        let mv_fn = self.module.add_function("atomic_map_values", list_ty.fn_type(&[list_ty.into()], false), None);
        let mv_entry = self.context.append_basic_block(mv_fn, "entry");
        self.builder.position_at_end(mv_entry);
        let mv_in = mv_fn.get_first_param().unwrap().into_struct_value();
        let mv_data = self.builder.build_extract_value(mv_in, 0, "data").map_err(llvm_err)?.into_pointer_value();
        let mv_len = self.builder.build_extract_value(mv_in, 1, "len").map_err(llvm_err)?.into_int_value();
        let mv_data_i64 = self.builder.build_pointer_cast(mv_data, ptr, "data_i64").map_err(llvm_err)?;
        let mv_res = self.call_rt("atomic_list_create", &[i64.const_int(4, false).into()])?;
        let mv_resv = mv_res.try_as_basic_value().basic().unwrap();
        let mv_ra = self.builder.build_alloca(self.list_type, "mv_ra").map_err(llvm_err)?;
        self.builder.build_store(mv_ra, mv_resv).map_err(llvm_err)?;
        let mv_i = self.builder.build_alloca(i64, "mv_i").map_err(llvm_err)?;
        self.builder.build_store(mv_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let mv_loop = self.context.append_basic_block(mv_fn, "loop");
        let mv_body = self.context.append_basic_block(mv_fn, "body");
        let mv_done = self.context.append_basic_block(mv_fn, "done");
        let _ = self.builder.build_unconditional_branch(mv_loop);
        self.builder.position_at_end(mv_loop);
        let mv_iv = self.builder.build_load(i64, mv_i, "iv").map_err(llvm_err)?.into_int_value();
        let mv_cond = self.builder.build_int_compare(IntPredicate::SLT, mv_iv, mv_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(mv_cond, mv_body, mv_done);
        self.builder.position_at_end(mv_body);
        // Map entry layout: [key_tag, key_ptr_i64, val_tag, val_ptr_i64]
        let mv_off = self.builder.build_int_mul(mv_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let mv_off2 = self.builder.build_int_add(mv_off, i64.const_int(2, false), "off2").map_err(llvm_err)?;
        let mv_vtp = unsafe { self.builder.build_gep(i64, mv_data_i64, &[mv_off2], "vtp").map_err(llvm_err) }?;
        let mv_vt = self.builder.build_load(i64, mv_vtp, "vt").map_err(llvm_err)?.into_int_value();
        let mv_off3 = self.builder.build_int_add(mv_off, i64.const_int(3, false), "off3").map_err(llvm_err)?;
        let mv_vpp = unsafe { self.builder.build_gep(i64, mv_data_i64, &[mv_off3], "vpp").map_err(llvm_err) }?;
        let mv_vp_i64 = self.builder.build_load(i64, mv_vpp, "vp_i64").map_err(llvm_err)?.into_int_value();
        let mv_vp = self.builder.build_int_to_ptr(mv_vp_i64, ptr, "vp").map_err(llvm_err)?;
        // Build value fat struct
        let mv_val_undef = self.string_type.get_undef();
        let mv_val1 = self.builder.build_insert_value(mv_val_undef, mv_vt, 0, "vtag").map_err(llvm_err)?;
        let mv_val2 = self.builder.build_insert_value(mv_val1, mv_vp, 1, "vdata").map_err(llvm_err)?;
        let mv_cl = self.builder.build_load(self.list_type, mv_ra, "cl").map_err(llvm_err)?.into_struct_value();
        let mv_ps = self.call_rt("atomic_list_push", &[mv_cl.into(), mv_val2.as_basic_value_enum().into()])?;
        self.builder.build_store(mv_ra, mv_ps.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let mv_inc = self.builder.build_int_add(mv_iv, i64.const_int(1, false), "inc").map_err(llvm_err)?;
        self.builder.build_store(mv_i, mv_inc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(mv_loop);
        self.builder.position_at_end(mv_done);
        let mv_rt = self.builder.build_load(self.list_type, mv_ra, "mv_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&mv_rt));

        // ---- atomic_map_entries({ptr, i64, i64}) -> {ptr, i64, i64} ----
        let me_fn = self.module.add_function("atomic_map_entries", list_ty.fn_type(&[list_ty.into()], false), None);
        let me_entry = self.context.append_basic_block(me_fn, "entry");
        self.builder.position_at_end(me_entry);
        let me_in = me_fn.get_first_param().unwrap().into_struct_value();
        let me_data = self.builder.build_extract_value(me_in, 0, "data").map_err(llvm_err)?.into_pointer_value();
        let me_len = self.builder.build_extract_value(me_in, 1, "len").map_err(llvm_err)?.into_int_value();
        let me_data_i64 = self.builder.build_pointer_cast(me_data, ptr, "data_i64").map_err(llvm_err)?;
        let me_res = self.call_rt("atomic_list_create", &[i64.const_int(4, false).into()])?;
        let me_resv = me_res.try_as_basic_value().basic().unwrap();
        let me_ra = self.builder.build_alloca(self.list_type, "me_ra").map_err(llvm_err)?;
        self.builder.build_store(me_ra, me_resv).map_err(llvm_err)?;
        let me_i = self.builder.build_alloca(i64, "me_i").map_err(llvm_err)?;
        self.builder.build_store(me_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let me_loop = self.context.append_basic_block(me_fn, "loop");
        let me_body = self.context.append_basic_block(me_fn, "body");
        let me_done = self.context.append_basic_block(me_fn, "done");
        let _ = self.builder.build_unconditional_branch(me_loop);
        self.builder.position_at_end(me_loop);
        let me_iv = self.builder.build_load(i64, me_i, "iv").map_err(llvm_err)?.into_int_value();
        let me_cond = self.builder.build_int_compare(IntPredicate::SLT, me_iv, me_len, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(me_cond, me_body, me_done);
        self.builder.position_at_end(me_body);
        // Build a tuple fat struct: (key, value)
        // Map entry layout: [key_tag, key_ptr_i64, val_tag, val_ptr_i64]
        let me_off = self.builder.build_int_mul(me_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        // Key fat struct
        let me_ktp = unsafe { self.builder.build_gep(i64, me_data_i64, &[me_off], "ktp").map_err(llvm_err) }?;
        let me_kt = self.builder.build_load(i64, me_ktp, "kt").map_err(llvm_err)?.into_int_value();
        let me_off1 = self.builder.build_int_add(me_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let me_kpp = unsafe { self.builder.build_gep(i64, me_data_i64, &[me_off1], "kpp").map_err(llvm_err) }?;
        let me_kp_i64 = self.builder.build_load(i64, me_kpp, "kp_i64").map_err(llvm_err)?.into_int_value();
        let me_kp = self.builder.build_int_to_ptr(me_kp_i64, ptr, "kp").map_err(llvm_err)?;
        // Value fat struct
        let me_off2 = self.builder.build_int_add(me_off, i64.const_int(2, false), "off2").map_err(llvm_err)?;
        let me_vtp = unsafe { self.builder.build_gep(i64, me_data_i64, &[me_off2], "vtp").map_err(llvm_err) }?;
        let me_vt = self.builder.build_load(i64, me_vtp, "vt").map_err(llvm_err)?.into_int_value();
        let me_off3 = self.builder.build_int_add(me_off, i64.const_int(3, false), "off3").map_err(llvm_err)?;
        let me_vpp = unsafe { self.builder.build_gep(i64, me_data_i64, &[me_off3], "vpp").map_err(llvm_err) }?;
        let me_vp_i64 = self.builder.build_load(i64, me_vpp, "vp_i64").map_err(llvm_err)?.into_int_value();
        let me_vp = self.builder.build_int_to_ptr(me_vp_i64, ptr, "vp").map_err(llvm_err)?;
        // Build tuple: allocate 2 fat structs and point to them
        let me_k_undef = self.string_type.get_undef();
        let me_k1 = self.builder.build_insert_value(me_k_undef, me_kt, 0, "k1").map_err(llvm_err)?;
        let me_k2 = self.builder.build_insert_value(me_k1, me_kp, 1, "k2").map_err(llvm_err)?;
        let me_v_undef = self.string_type.get_undef();
        let me_v1 = self.builder.build_insert_value(me_v_undef, me_vt, 0, "v1").map_err(llvm_err)?;
        let me_v2 = self.builder.build_insert_value(me_v1, me_vp, 1, "v2").map_err(llvm_err)?;
        // Store key+value in a malloc'd block of 2 fat structs
        let me_tuple_ptr = self.builder.build_call(self.module.get_function("malloc").unwrap(), &[i64.const_int(32, false).into()], "tup").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        self.builder.build_store(me_tuple_ptr, me_k2).map_err(llvm_err)?;
        let me_vslot = unsafe { self.builder.build_gep(self.string_type, me_tuple_ptr, &[i64.const_int(1, false)], "vslot").map_err(llvm_err) }?;
        self.builder.build_store(me_vslot, me_v2).map_err(llvm_err)?;
        // Wrap in a fat struct: tag=5 (Struct), data=tuple_ptr
        let me_fat_undef = self.string_type.get_undef();
        let me_fat1 = self.builder.build_insert_value(me_fat_undef, i64.const_int(5, false), 0, "ftag").map_err(llvm_err)?;
        let me_fat2 = self.builder.build_insert_value(me_fat1, me_tuple_ptr, 1, "fdata").map_err(llvm_err)?;
        let me_cl = self.builder.build_load(self.list_type, me_ra, "cl").map_err(llvm_err)?.into_struct_value();
        let me_ps = self.call_rt("atomic_list_push", &[me_cl.into(), me_fat2.as_basic_value_enum().into()])?;
        self.builder.build_store(me_ra, me_ps.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let me_inc = self.builder.build_int_add(me_iv, i64.const_int(1, false), "inc").map_err(llvm_err)?;
        self.builder.build_store(me_i, me_inc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(me_loop);
        self.builder.position_at_end(me_done);
        let me_rt = self.builder.build_load(self.list_type, me_ra, "me_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&me_rt));

        // ---- atomic_set_union({ptr, i64, i64}, {ptr, i64, i64}) -> {ptr, i64, i64} ----
        // Sets use map layout (4×i64 per entry). Result must be in map format.
        let su_fn = self.module.add_function("atomic_set_union", list_ty.fn_type(&[list_ty.into(), list_ty.into()], false), None);
        let su_entry = self.context.append_basic_block(su_fn, "entry");
        self.builder.position_at_end(su_entry);
        let su_a = su_fn.get_first_param().unwrap().into_struct_value();
        let su_b = su_fn.get_nth_param(1).unwrap().into_struct_value();
        let su_alen = self.builder.build_extract_value(su_a, 1, "alen").map_err(llvm_err)?.into_int_value();
        let su_blen = self.builder.build_extract_value(su_b, 1, "blen").map_err(llvm_err)?.into_int_value();
        let su_cap = self.builder.build_int_add(su_alen, su_blen, "cap").map_err(llvm_err)?;
        let su_cap4 = self.builder.build_int_add(su_cap, i64.const_int(4, false), "cap4").map_err(llvm_err)?;
        let map_create_fn = self.module.get_function("atomic_map_create").unwrap();
        let mi_fn = self.module.get_function("atomic_map_insert").unwrap();
        let mc_fn = self.module.get_function("atomic_map_contains").unwrap();
        let su_res = self.builder.build_call(map_create_fn, &[su_cap4.into()], "res").map_err(llvm_err)?;
        let su_resv = su_res.try_as_basic_value().basic().unwrap();
        let su_ra = self.builder.build_alloca(self.list_type, "su_ra").map_err(llvm_err)?;
        self.builder.build_store(su_ra, su_resv).map_err(llvm_err)?;
        let su_null = {
            let u = str_ty.get_undef();
            let u1 = self.builder.build_insert_value(u, i64.const_int(0, false), 0, "n0").map_err(llvm_err)?;
            self.builder.build_insert_value(u1, self.ptr_ty().const_zero(), 1, "n1").map_err(llvm_err)?
        };
        // Helper: build key fat struct from map entry at i64 offset `off`
        let build_key = |builder: &inkwell::builder::Builder<'ctx>, data_i64p: PointerValue<'ctx>, off: IntValue<'ctx>| -> Result<_, String> {
            let tp = unsafe { builder.build_gep(i64, data_i64p, &[off], "tp") }.map_err(llvm_err)?;
            let tag = builder.build_load(i64, tp, "tag").map_err(llvm_err)?.into_int_value();
            let off1 = builder.build_int_add(off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
            let pp = unsafe { builder.build_gep(i64, data_i64p, &[off1], "pp") }.map_err(llvm_err)?;
            let pi = builder.build_load(i64, pp, "pi").map_err(llvm_err)?.into_int_value();
            let pv = builder.build_int_to_ptr(pi, ptr, "pv").map_err(llvm_err)?;
            let u = str_ty.get_undef();
            let u1 = builder.build_insert_value(u, tag, 0, "k1").map_err(llvm_err)?;
            Ok(builder.build_insert_value(u1, pv, 1, "k2").map_err(llvm_err)?)
        };
        // Add all from A
        let su_adata = self.builder.build_extract_value(su_a, 0, "adata").map_err(llvm_err)?.into_pointer_value();
        let su_a_i64p = self.builder.build_pointer_cast(su_adata, ptr, "a_i64p").map_err(llvm_err)?;
        let su_i = self.builder.build_alloca(i64, "su_i").map_err(llvm_err)?;
        self.builder.build_store(su_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let su_loop1 = self.context.append_basic_block(su_fn, "loop1");
        let su_body1 = self.context.append_basic_block(su_fn, "body1");
        let su_done1 = self.context.append_basic_block(su_fn, "done1");
        let _ = self.builder.build_unconditional_branch(su_loop1);
        self.builder.position_at_end(su_loop1);
        let su_iv = self.builder.build_load(i64, su_i, "iv").map_err(llvm_err)?.into_int_value();
        let su_c1 = self.builder.build_int_compare(IntPredicate::SLT, su_iv, su_alen, "c1").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(su_c1, su_body1, su_done1);
        self.builder.position_at_end(su_body1);
        let su_off = self.builder.build_int_mul(su_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let su_key = build_key(&self.builder, su_a_i64p, su_off)?;
        let su_cl1 = self.builder.build_load(self.list_type, su_ra, "cl1").map_err(llvm_err)?.into_struct_value();
        let su_ins = self.builder.build_call(mi_fn, &[su_cl1.into(), su_key.as_basic_value_enum().into(), su_null.as_basic_value_enum().into()], "ins").map_err(llvm_err)?;
        self.builder.build_store(su_ra, su_ins.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let su_inc = self.builder.build_int_add(su_iv, i64.const_int(1, false), "inc").map_err(llvm_err)?;
        self.builder.build_store(su_i, su_inc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(su_loop1);
        // Add from B only if not already in result
        self.builder.position_at_end(su_done1);
        let su_bdata = self.builder.build_extract_value(su_b, 0, "bdata").map_err(llvm_err)?.into_pointer_value();
        let su_b_i64p = self.builder.build_pointer_cast(su_bdata, ptr, "b_i64p").map_err(llvm_err)?;
        let su_j = self.builder.build_alloca(i64, "su_j").map_err(llvm_err)?;
        self.builder.build_store(su_j, i64.const_int(0, false)).map_err(llvm_err)?;
        let su_loop2 = self.context.append_basic_block(su_fn, "loop2");
        let su_body2 = self.context.append_basic_block(su_fn, "body2");
        let su_done2 = self.context.append_basic_block(su_fn, "done2");
        let _ = self.builder.build_unconditional_branch(su_loop2);
        self.builder.position_at_end(su_loop2);
        let su_jv = self.builder.build_load(i64, su_j, "jv").map_err(llvm_err)?.into_int_value();
        let su_c2 = self.builder.build_int_compare(IntPredicate::SLT, su_jv, su_blen, "c2").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(su_c2, su_body2, su_done2);
        self.builder.position_at_end(su_body2);
        let su_boff = self.builder.build_int_mul(su_jv, i64.const_int(4, false), "boff").map_err(llvm_err)?;
        let su_key2 = build_key(&self.builder, su_b_i64p, su_boff)?;
        let su_cl2 = self.builder.build_load(self.list_type, su_ra, "cl2").map_err(llvm_err)?.into_struct_value();
        let su_contains = self.builder.build_call(mc_fn, &[su_cl2.into(), su_key2.as_basic_value_enum().into()], "cont").map_err(llvm_err)?;
        let su_not_cont = self.builder.build_not(su_contains.try_as_basic_value().basic().unwrap().into_int_value(), "nc").map_err(llvm_err)?;
        let su_add = self.context.append_basic_block(su_fn, "add");
        let su_skip = self.context.append_basic_block(su_fn, "skip");
        let _ = self.builder.build_conditional_branch(su_not_cont, su_add, su_skip);
        self.builder.position_at_end(su_add);
        let su_cl3 = self.builder.build_load(self.list_type, su_ra, "cl3").map_err(llvm_err)?.into_struct_value();
        let su_ins2 = self.builder.build_call(mi_fn, &[su_cl3.into(), su_key2.as_basic_value_enum().into(), su_null.as_basic_value_enum().into()], "ins2").map_err(llvm_err)?;
        self.builder.build_store(su_ra, su_ins2.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(su_skip);
        self.builder.position_at_end(su_skip);
        let su_inc2 = self.builder.build_int_add(su_jv, i64.const_int(1, false), "inc2").map_err(llvm_err)?;
        self.builder.build_store(su_j, su_inc2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(su_loop2);
        self.builder.position_at_end(su_done2);
        let su_rt = self.builder.build_load(self.list_type, su_ra, "su_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&su_rt));

        // ---- atomic_set_intersection({ptr, i64, i64}, {ptr, i64, i64}) -> {ptr, i64, i64} ----
        // Sets use map layout (4×i64 per entry). Result must be in map format.
        let si_fn = self.module.add_function("atomic_set_intersection", list_ty.fn_type(&[list_ty.into(), list_ty.into()], false), None);
        let si_entry = self.context.append_basic_block(si_fn, "entry");
        self.builder.position_at_end(si_entry);
        let si_a = si_fn.get_first_param().unwrap().into_struct_value();
        let si_b = si_fn.get_nth_param(1).unwrap().into_struct_value();
        let si_alen = self.builder.build_extract_value(si_a, 1, "alen").map_err(llvm_err)?.into_int_value();
        let si_cap4 = self.builder.build_int_add(si_alen, i64.const_int(4, false), "cap4").map_err(llvm_err)?;
        let map_create_fn = self.module.get_function("atomic_map_create").unwrap();
        let mi_fn = self.module.get_function("atomic_map_insert").unwrap();
        let mc_fn = self.module.get_function("atomic_map_contains").unwrap();
        let si_res = self.builder.build_call(map_create_fn, &[si_cap4.into()], "res").map_err(llvm_err)?;
        let si_resv = si_res.try_as_basic_value().basic().unwrap();
        let si_ra = self.builder.build_alloca(self.list_type, "si_ra").map_err(llvm_err)?;
        self.builder.build_store(si_ra, si_resv).map_err(llvm_err)?;
        let si_null = {
            let u = str_ty.get_undef();
            let u1 = self.builder.build_insert_value(u, i64.const_int(0, false), 0, "n0").map_err(llvm_err)?;
            self.builder.build_insert_value(u1, self.ptr_ty().const_zero(), 1, "n1").map_err(llvm_err)?
        };
        let si_adata = self.builder.build_extract_value(si_a, 0, "adata").map_err(llvm_err)?.into_pointer_value();
        let si_a_i64p = self.builder.build_pointer_cast(si_adata, ptr, "a_i64p").map_err(llvm_err)?;
        let si_i = self.builder.build_alloca(i64, "si_i").map_err(llvm_err)?;
        self.builder.build_store(si_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let si_loop = self.context.append_basic_block(si_fn, "loop");
        let si_body = self.context.append_basic_block(si_fn, "body");
        let si_done = self.context.append_basic_block(si_fn, "done");
        let _ = self.builder.build_unconditional_branch(si_loop);
        self.builder.position_at_end(si_loop);
        let si_iv = self.builder.build_load(i64, si_i, "iv").map_err(llvm_err)?.into_int_value();
        let si_cond = self.builder.build_int_compare(IntPredicate::SLT, si_iv, si_alen, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(si_cond, si_body, si_done);
        self.builder.position_at_end(si_body);
        let si_off = self.builder.build_int_mul(si_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let si_tp = unsafe { self.builder.build_gep(i64, si_a_i64p, &[si_off], "tp").map_err(llvm_err) }?;
        let si_tag = self.builder.build_load(i64, si_tp, "tag").map_err(llvm_err)?.into_int_value();
        let si_off1 = self.builder.build_int_add(si_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let si_pp = unsafe { self.builder.build_gep(i64, si_a_i64p, &[si_off1], "pp").map_err(llvm_err) }?;
        let si_pi = self.builder.build_load(i64, si_pp, "pi").map_err(llvm_err)?.into_int_value();
        let si_pv = self.builder.build_int_to_ptr(si_pi, ptr, "pv").map_err(llvm_err)?;
        let si_key_undef = str_ty.get_undef();
        let si_key1 = self.builder.build_insert_value(si_key_undef, si_tag, 0, "k1").map_err(llvm_err)?;
        let si_key = self.builder.build_insert_value(si_key1, si_pv, 1, "k2").map_err(llvm_err)?;
        // Check if element is in B (use map_contains for correct layout)
        let si_contains = self.builder.build_call(mc_fn, &[si_b.as_basic_value_enum().into(), si_key.as_basic_value_enum().into()], "cont").map_err(llvm_err)?;
        let si_found = si_contains.try_as_basic_value().basic().unwrap().into_int_value();
        let si_add = self.context.append_basic_block(si_fn, "add");
        let si_skip = self.context.append_basic_block(si_fn, "skip");
        let _ = self.builder.build_conditional_branch(si_found, si_add, si_skip);
        self.builder.position_at_end(si_add);
        let si_cl2 = self.builder.build_load(self.list_type, si_ra, "cl2").map_err(llvm_err)?.into_struct_value();
        let si_ins = self.builder.build_call(mi_fn, &[si_cl2.into(), si_key.as_basic_value_enum().into(), si_null.as_basic_value_enum().into()], "ins").map_err(llvm_err)?;
        self.builder.build_store(si_ra, si_ins.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(si_skip);
        self.builder.position_at_end(si_skip);
        let si_inc = self.builder.build_int_add(si_iv, i64.const_int(1, false), "inc").map_err(llvm_err)?;
        self.builder.build_store(si_i, si_inc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(si_loop);
        self.builder.position_at_end(si_done);
        let si_rt = self.builder.build_load(self.list_type, si_ra, "si_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&si_rt));

        // ---- atomic_set_difference({ptr, i64, i64}, {ptr, i64, i64}) -> {ptr, i64, i64} ----
        // Sets use map layout (4×i64 per entry). Result must be in map format.
        let sd_fn = self.module.add_function("atomic_set_difference", list_ty.fn_type(&[list_ty.into(), list_ty.into()], false), None);
        let sd_entry = self.context.append_basic_block(sd_fn, "entry");
        self.builder.position_at_end(sd_entry);
        let sd_a = sd_fn.get_first_param().unwrap().into_struct_value();
        let sd_b = sd_fn.get_nth_param(1).unwrap().into_struct_value();
        let sd_alen = self.builder.build_extract_value(sd_a, 1, "alen").map_err(llvm_err)?.into_int_value();
        let sd_cap4 = self.builder.build_int_add(sd_alen, i64.const_int(4, false), "cap4").map_err(llvm_err)?;
        let map_create_fn = self.module.get_function("atomic_map_create").unwrap();
        let mi_fn = self.module.get_function("atomic_map_insert").unwrap();
        let mc_fn = self.module.get_function("atomic_map_contains").unwrap();
        let sd_res = self.builder.build_call(map_create_fn, &[sd_cap4.into()], "res").map_err(llvm_err)?;
        let sd_resv = sd_res.try_as_basic_value().basic().unwrap();
        let sd_ra = self.builder.build_alloca(self.list_type, "sd_ra").map_err(llvm_err)?;
        self.builder.build_store(sd_ra, sd_resv).map_err(llvm_err)?;
        let sd_null = {
            let u = str_ty.get_undef();
            let u1 = self.builder.build_insert_value(u, i64.const_int(0, false), 0, "n0").map_err(llvm_err)?;
            self.builder.build_insert_value(u1, self.ptr_ty().const_zero(), 1, "n1").map_err(llvm_err)?
        };
        let sd_adata = self.builder.build_extract_value(sd_a, 0, "adata").map_err(llvm_err)?.into_pointer_value();
        let sd_a_i64p = self.builder.build_pointer_cast(sd_adata, ptr, "a_i64p").map_err(llvm_err)?;
        let sd_i = self.builder.build_alloca(i64, "sd_i").map_err(llvm_err)?;
        self.builder.build_store(sd_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let sd_loop = self.context.append_basic_block(sd_fn, "loop");
        let sd_body = self.context.append_basic_block(sd_fn, "body");
        let sd_done = self.context.append_basic_block(sd_fn, "done");
        let _ = self.builder.build_unconditional_branch(sd_loop);
        self.builder.position_at_end(sd_loop);
        let sd_iv = self.builder.build_load(i64, sd_i, "iv").map_err(llvm_err)?.into_int_value();
        let sd_cond = self.builder.build_int_compare(IntPredicate::SLT, sd_iv, sd_alen, "cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(sd_cond, sd_body, sd_done);
        self.builder.position_at_end(sd_body);
        let sd_off = self.builder.build_int_mul(sd_iv, i64.const_int(4, false), "off").map_err(llvm_err)?;
        let sd_tp = unsafe { self.builder.build_gep(i64, sd_a_i64p, &[sd_off], "tp").map_err(llvm_err) }?;
        let sd_tag = self.builder.build_load(i64, sd_tp, "tag").map_err(llvm_err)?.into_int_value();
        let sd_off1 = self.builder.build_int_add(sd_off, i64.const_int(1, false), "off1").map_err(llvm_err)?;
        let sd_pp = unsafe { self.builder.build_gep(i64, sd_a_i64p, &[sd_off1], "pp").map_err(llvm_err) }?;
        let sd_pi = self.builder.build_load(i64, sd_pp, "pi").map_err(llvm_err)?.into_int_value();
        let sd_pv = self.builder.build_int_to_ptr(sd_pi, ptr, "pv").map_err(llvm_err)?;
        let sd_key_undef = str_ty.get_undef();
        let sd_key1 = self.builder.build_insert_value(sd_key_undef, sd_tag, 0, "k1").map_err(llvm_err)?;
        let sd_key = self.builder.build_insert_value(sd_key1, sd_pv, 1, "k2").map_err(llvm_err)?;
        // Check if element is NOT in B (use map_contains for correct layout)
        let sd_contains = self.builder.build_call(mc_fn, &[sd_b.as_basic_value_enum().into(), sd_key.as_basic_value_enum().into()], "cont").map_err(llvm_err)?;
        let sd_not_cont = self.builder.build_not(sd_contains.try_as_basic_value().basic().unwrap().into_int_value(), "nc").map_err(llvm_err)?;
        let sd_add = self.context.append_basic_block(sd_fn, "add");
        let sd_skip = self.context.append_basic_block(sd_fn, "skip");
        let _ = self.builder.build_conditional_branch(sd_not_cont, sd_add, sd_skip);
        self.builder.position_at_end(sd_add);
        let sd_cl2 = self.builder.build_load(self.list_type, sd_ra, "cl2").map_err(llvm_err)?.into_struct_value();
        let sd_ins = self.builder.build_call(mi_fn, &[sd_cl2.into(), sd_key.as_basic_value_enum().into(), sd_null.as_basic_value_enum().into()], "ins").map_err(llvm_err)?;
        self.builder.build_store(sd_ra, sd_ins.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sd_skip);
        self.builder.position_at_end(sd_skip);
        let sd_inc = self.builder.build_int_add(sd_iv, i64.const_int(1, false), "inc").map_err(llvm_err)?;
        self.builder.build_store(sd_i, sd_inc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(sd_loop);
        self.builder.position_at_end(sd_done);
        let sd_rt = self.builder.build_load(self.list_type, sd_ra, "sd_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&sd_rt));

        // ---- atomic_set_is_subset({ptr, i64, i64}, {ptr, i64, i64}) -> i1 ----
        // Sets use map layout: each entry = 4×i64 (key_tag, key_ptr_i64, val_tag, val_ptr_i64).
        // Compare only keys (offsets 0 and 1), skip values (offsets 2 and 3).
        let ss_fn = self.module.add_function("atomic_set_is_subset", self.context.bool_type().fn_type(&[list_ty.into(), list_ty.into()], false), None);
        let ss_entry = self.context.append_basic_block(ss_fn, "entry");
        self.builder.position_at_end(ss_entry);
        let a = ss_fn.get_first_param().unwrap().into_struct_value();
        let b = ss_fn.get_nth_param(1).unwrap().into_struct_value();
        let a_data_ptr = self.builder.build_extract_value(a, 0, "ad").map_err(llvm_err)?.into_pointer_value();
        let alen = self.builder.build_extract_value(a, 1, "al").map_err(llvm_err)?.into_int_value();
        let b_data_ptr = self.builder.build_extract_value(b, 0, "bd").map_err(llvm_err)?.into_pointer_value();
        let blen = self.builder.build_extract_value(b, 1, "bl").map_err(llvm_err)?.into_int_value();
        // Cast data pointers to i64* for 4×i64 entry indexing
        let a_i64p = self.builder.build_pointer_cast(a_data_ptr, ptr, "a_i64p").map_err(llvm_err)?;
        let b_i64p = self.builder.build_pointer_cast(b_data_ptr, ptr, "b_i64p").map_err(llvm_err)?;

        // Outer loop counter
        let oi = self.builder.build_alloca(i64, "oi").map_err(llvm_err)?;
        self.builder.build_store(oi, i64.const_int(0, false)).map_err(llvm_err)?;
        let oloop = self.context.append_basic_block(ss_fn, "oloop");
        let obody = self.context.append_basic_block(ss_fn, "obody");
        let ofound = self.context.append_basic_block(ss_fn, "ofound");
        let oinc = self.context.append_basic_block(ss_fn, "oinc");
        let rtrue = self.context.append_basic_block(ss_fn, "rtrue");
        let rfalse = self.context.append_basic_block(ss_fn, "rfalse");
        let _ = self.builder.build_unconditional_branch(oloop);

        // Outer loop
        self.builder.position_at_end(oloop);
        let oiv = self.builder.build_load(i64, oi, "oiv").map_err(llvm_err)?.into_int_value();
        let ocond = self.builder.build_int_compare(IntPredicate::SLT, oiv, alen, "ocond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(ocond, obody, rtrue);

        // Outer body: load A key at offset oiv*4
        self.builder.position_at_end(obody);
        let a_off = self.builder.build_int_mul(oiv, i64.const_int(4, false), "a_off").map_err(llvm_err)?;
        let a_tag_ptr = unsafe { self.builder.build_gep(i64, a_i64p, &[a_off], "a_tp").map_err(llvm_err) }?;
        let a_tag = self.builder.build_load(i64, a_tag_ptr, "a_tag").map_err(llvm_err)?.into_int_value();
        let a_off1 = self.builder.build_int_add(a_off, i64.const_int(1, false), "a_off1").map_err(llvm_err)?;
        let a_ptr_ptr = unsafe { self.builder.build_gep(i64, a_i64p, &[a_off1], "a_pp").map_err(llvm_err) }?;
        let a_ptr_i64 = self.builder.build_load(i64, a_ptr_ptr, "a_pi").map_err(llvm_err)?.into_int_value();
        let a_is_null = self.builder.build_int_compare(IntPredicate::EQ, a_ptr_i64, i64.const_int(0, false), "a_is_null").map_err(llvm_err)?;

        // Inner loop counter
        let ij = self.builder.build_alloca(i64, "ij").map_err(llvm_err)?;
        self.builder.build_store(ij, i64.const_int(0, false)).map_err(llvm_err)?;
        let iloop = self.context.append_basic_block(ss_fn, "iloop");
        let ibody = self.context.append_basic_block(ss_fn, "ibody");
        let inext = self.context.append_basic_block(ss_fn, "inext");
        let inotfound = self.context.append_basic_block(ss_fn, "inotfound");
        let _ = self.builder.build_unconditional_branch(iloop);

        // Inner loop
        self.builder.position_at_end(iloop);
        let ijv = self.builder.build_load(i64, ij, "ijv").map_err(llvm_err)?.into_int_value();
        let icond = self.builder.build_int_compare(IntPredicate::SLT, ijv, blen, "icond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(icond, ibody, inotfound);

        // Inner body: load B key at offset ijv*4, compare with A key
        self.builder.position_at_end(ibody);
        let b_off = self.builder.build_int_mul(ijv, i64.const_int(4, false), "b_off").map_err(llvm_err)?;
        let b_tag_ptr = unsafe { self.builder.build_gep(i64, b_i64p, &[b_off], "b_tp").map_err(llvm_err) }?;
        let b_tag = self.builder.build_load(i64, b_tag_ptr, "b_tag").map_err(llvm_err)?.into_int_value();
        let b_off1 = self.builder.build_int_add(b_off, i64.const_int(1, false), "b_off1").map_err(llvm_err)?;
        let b_ptr_ptr = unsafe { self.builder.build_gep(i64, b_i64p, &[b_off1], "b_pp").map_err(llvm_err) }?;
        let b_ptr_i64 = self.builder.build_load(i64, b_ptr_ptr, "b_pi").map_err(llvm_err)?.into_int_value();
        let tag_eq = self.builder.build_int_compare(IntPredicate::EQ, a_tag, b_tag, "tag_eq").map_err(llvm_err)?;
        let icontent = self.context.append_basic_block(ss_fn, "icontent");
        let _ = self.builder.build_conditional_branch(tag_eq, icontent, inext);

        // Tags match: check pointer for null vs content
        self.builder.position_at_end(icontent);
        let b_is_null = self.builder.build_int_compare(IntPredicate::EQ, b_ptr_i64, i64.const_int(0, false), "b_is_null").map_err(llvm_err)?;
        let both_null = self.builder.build_and(a_is_null, b_is_null, "both_null").map_err(llvm_err)?;
        let ifound_bb = self.context.append_basic_block(ss_fn, "ifound_bb");
        let istr_bb = self.context.append_basic_block(ss_fn, "istr_bb");
        let _ = self.builder.build_conditional_branch(both_null, ifound_bb, istr_bb);
        // Both null: int/None match
        self.builder.position_at_end(ifound_bb);
        let _ = self.builder.build_unconditional_branch(ofound);
        // At least one pointer non-null: both must be non-null for string compare
        self.builder.position_at_end(istr_bb);
        let a_nn = self.builder.build_not(a_is_null, "a_nn").map_err(llvm_err)?;
        let b_nn = self.builder.build_not(b_is_null, "b_nn").map_err(llvm_err)?;
        let both_nn = self.builder.build_and(a_nn, b_nn, "both_nn").map_err(llvm_err)?;
        let istr_eq = self.context.append_basic_block(ss_fn, "istr_eq");
        let _ = self.builder.build_conditional_branch(both_nn, istr_eq, inext);
        // Build fat structs for string_eq call
        self.builder.position_at_end(istr_eq);
        let a_fat_undef = str_ty.get_undef();
        let a_fat1 = self.builder.build_insert_value(a_fat_undef, a_tag, 0, "af1").map_err(llvm_err)?;
        let a_ptr_val = self.builder.build_int_to_ptr(a_ptr_i64, ptr, "a_ptr").map_err(llvm_err)?;
        let a_fat2 = self.builder.build_insert_value(a_fat1, a_ptr_val, 1, "af2").map_err(llvm_err)?;
        let b_fat_undef = str_ty.get_undef();
        let b_fat1 = self.builder.build_insert_value(b_fat_undef, b_tag, 0, "bf1").map_err(llvm_err)?;
        let b_ptr_val = self.builder.build_int_to_ptr(b_ptr_i64, ptr, "b_ptr").map_err(llvm_err)?;
        let b_fat2 = self.builder.build_insert_value(b_fat1, b_ptr_val, 1, "bf2").map_err(llvm_err)?;
        let sseq_fn = self.module.get_function("atomic_string_eq").unwrap();
        let sseq = self.builder.build_call(sseq_fn, &[a_fat2.as_basic_value_enum().into(), b_fat2.as_basic_value_enum().into()], "sseq").map_err(llvm_err)?;
        let seq_val = sseq.try_as_basic_value().basic().unwrap().into_int_value();
        let istr_found = self.context.append_basic_block(ss_fn, "istr_found");
        let _ = self.builder.build_conditional_branch(seq_val, istr_found, inext);
        self.builder.position_at_end(istr_found);
        let _ = self.builder.build_unconditional_branch(ofound);

        // Increment inner loop
        self.builder.position_at_end(inext);
        let nij = self.builder.build_int_add(ijv, i64.const_int(1, false), "nij").map_err(llvm_err)?;
        self.builder.build_store(ij, nij).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(iloop);

        // Element NOT found in B
        self.builder.position_at_end(inotfound);
        let _ = self.builder.build_unconditional_branch(rfalse);

        // Element found in B: increment outer loop
        self.builder.position_at_end(ofound);
        let _ = self.builder.build_unconditional_branch(oinc);
        self.builder.position_at_end(oinc);
        let noi = self.builder.build_int_add(oiv, i64.const_int(1, false), "noi").map_err(llvm_err)?;
        self.builder.build_store(oi, noi).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(oloop);

        // Results
        self.builder.position_at_end(rfalse);
        let _ = self.builder.build_return(Some(&self.context.bool_type().const_int(0, false)));
        self.builder.position_at_end(rtrue);
        let _ = self.builder.build_return(Some(&self.context.bool_type().const_int(1, false)));

        // ---- atomic_rand_shuffle({ptr, i64, i64}) -> {ptr, i64, i64} ----
        let rs_fn = self.module.add_function("atomic_rand_shuffle", list_ty.fn_type(&[list_ty.into()], false), None);
        let rs_entry = self.context.append_basic_block(rs_fn, "entry");
        self.builder.position_at_end(rs_entry);
        let rs_in = rs_fn.get_first_param().unwrap().into_struct_value();
        let rs_data = self.builder.build_extract_value(rs_in, 0, "data").map_err(llvm_err)?.into_pointer_value();
        let rs_len = self.builder.build_extract_value(rs_in, 1, "len").map_err(llvm_err)?.into_int_value();
        // Copy input list
        let rs_copy = self.call_rt("atomic_list_create", &[i64.const_int(4, false).into()])?;
        let rs_copyv = rs_copy.try_as_basic_value().basic().unwrap();
        let rs_ra = self.builder.build_alloca(self.list_type, "rs_ra").map_err(llvm_err)?;
        self.builder.build_store(rs_ra, rs_copyv).map_err(llvm_err)?;
        // Copy all elements
        let rs_ci = self.builder.build_alloca(i64, "rs_ci").map_err(llvm_err)?;
        self.builder.build_store(rs_ci, i64.const_int(0, false)).map_err(llvm_err)?;
        let rs_cloop = self.context.append_basic_block(rs_fn, "cloop");
        let rs_cbody = self.context.append_basic_block(rs_fn, "cbody");
        let rs_cdone = self.context.append_basic_block(rs_fn, "cdone");
        let _ = self.builder.build_unconditional_branch(rs_cloop);
        self.builder.position_at_end(rs_cloop);
        let rs_civ = self.builder.build_load(i64, rs_ci, "civ").map_err(llvm_err)?.into_int_value();
        let rs_ccond = self.builder.build_int_compare(IntPredicate::SLT, rs_civ, rs_len, "ccond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rs_ccond, rs_cbody, rs_cdone);
        self.builder.position_at_end(rs_cbody);
        let rs_cep = unsafe { self.builder.build_gep(self.string_type, rs_data, &[rs_civ], "cep").map_err(llvm_err) }?;
        let rs_cev = self.builder.build_load(self.string_type, rs_cep, "cev").map_err(llvm_err)?.into_struct_value();
        let rs_ccl = self.builder.build_load(self.list_type, rs_ra, "ccl").map_err(llvm_err)?.into_struct_value();
        let rs_cps = self.call_rt("atomic_list_push", &[rs_ccl.into(), rs_cev.as_basic_value_enum().into()])?;
        self.builder.build_store(rs_ra, rs_cps.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let rs_cinc = self.builder.build_int_add(rs_civ, i64.const_int(1, false), "cinc").map_err(llvm_err)?;
        self.builder.build_store(rs_ci, rs_cinc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rs_cloop);
        self.builder.position_at_end(rs_cdone);
        // Fisher-Yates shuffle: iterate from end to start
        let rs_i = self.builder.build_alloca(i64, "rs_i").map_err(llvm_err)?;
        let rs_len1 = self.builder.build_int_sub(rs_len, i64.const_int(1, false), "len1").map_err(llvm_err)?;
        self.builder.build_store(rs_i, rs_len1).map_err(llvm_err)?;
        let rs_floop = self.context.append_basic_block(rs_fn, "floop");
        let rs_fbody = self.context.append_basic_block(rs_fn, "fbody");
        let rs_fdone = self.context.append_basic_block(rs_fn, "fdone");
        let _ = self.builder.build_unconditional_branch(rs_floop);
        self.builder.position_at_end(rs_floop);
        let rs_iv = self.builder.build_load(i64, rs_i, "iv").map_err(llvm_err)?.into_int_value();
        let rs_fcond = self.builder.build_int_compare(IntPredicate::SGT, rs_iv, i64.const_int(0, false), "fcond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(rs_fcond, rs_fbody, rs_fdone);
        self.builder.position_at_end(rs_fbody);
        // Generate random index [0, i]
        let rs_rand = self.call_rt("atomic_rand_int", &[i64.const_int(0, false).into(), rs_iv.into()])?;
        let rs_j = rs_rand.try_as_basic_value().basic().unwrap().into_int_value();
        // Swap elements at i and j
        let rs_cur = self.builder.build_load(self.list_type, rs_ra, "cur_list").map_err(llvm_err)?.into_struct_value();
        let rs_cur_data = self.builder.build_extract_value(rs_cur, 0, "cur_data").map_err(llvm_err)?.into_pointer_value();
        let rs_epi = unsafe { self.builder.build_gep(self.string_type, rs_cur_data, &[rs_iv], "epi").map_err(llvm_err) }?;
        let rs_epj = unsafe { self.builder.build_gep(self.string_type, rs_cur_data, &[rs_j], "epj").map_err(llvm_err) }?;
        let rs_ei = self.builder.build_load(self.string_type, rs_epi, "ei").map_err(llvm_err)?;
        let rs_ej = self.builder.build_load(self.string_type, rs_epj, "ej").map_err(llvm_err)?;
        self.builder.build_store(rs_epi, rs_ej).map_err(llvm_err)?;
        self.builder.build_store(rs_epj, rs_ei).map_err(llvm_err)?;
        let rs_dec = self.builder.build_int_sub(rs_iv, i64.const_int(1, false), "dec").map_err(llvm_err)?;
        self.builder.build_store(rs_i, rs_dec).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(rs_floop);
        self.builder.position_at_end(rs_fdone);
        let rs_rt = self.builder.build_load(self.list_type, rs_ra, "rs_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&rs_rt));

        // ---- atomic_list_sorted({ptr, i64, i64}) -> {ptr, i64, i64} (Int-only for now) ----
        let srt_fn = self.module.add_function("atomic_list_sorted", list_ty.fn_type(&[list_ty.into()], false), None);
        let srt_entry = self.context.append_basic_block(srt_fn, "entry");
        self.builder.position_at_end(srt_entry);
        let srt_in = srt_fn.get_first_param().unwrap().into_struct_value();
        let srt_data = self.builder.build_extract_value(srt_in, 0, "data").map_err(llvm_err)?.into_pointer_value();
        let srt_len = self.builder.build_extract_value(srt_in, 1, "len").map_err(llvm_err)?.into_int_value();
        // Copy input
        let srt_copy = self.call_rt("atomic_list_create", &[i64.const_int(4, false).into()])?;
        let srt_copyv = srt_copy.try_as_basic_value().basic().unwrap();
        let srt_ra = self.builder.build_alloca(self.list_type, "srt_ra").map_err(llvm_err)?;
        self.builder.build_store(srt_ra, srt_copyv).map_err(llvm_err)?;
        let srt_ci = self.builder.build_alloca(i64, "srt_ci").map_err(llvm_err)?;
        self.builder.build_store(srt_ci, i64.const_int(0, false)).map_err(llvm_err)?;
        let srt_cloop = self.context.append_basic_block(srt_fn, "cloop");
        let srt_cbody = self.context.append_basic_block(srt_fn, "cbody");
        let srt_cdone = self.context.append_basic_block(srt_fn, "cdone");
        let _ = self.builder.build_unconditional_branch(srt_cloop);
        self.builder.position_at_end(srt_cloop);
        let srt_civ = self.builder.build_load(i64, srt_ci, "civ").map_err(llvm_err)?.into_int_value();
        let srt_ccond = self.builder.build_int_compare(IntPredicate::SLT, srt_civ, srt_len, "ccond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(srt_ccond, srt_cbody, srt_cdone);
        self.builder.position_at_end(srt_cbody);
        let srt_cep = unsafe { self.builder.build_gep(self.string_type, srt_data, &[srt_civ], "cep").map_err(llvm_err) }?;
        let srt_cev = self.builder.build_load(self.string_type, srt_cep, "cev").map_err(llvm_err)?.into_struct_value();
        let srt_ccl = self.builder.build_load(self.list_type, srt_ra, "ccl").map_err(llvm_err)?.into_struct_value();
        let srt_cps = self.call_rt("atomic_list_push", &[srt_ccl.into(), srt_cev.as_basic_value_enum().into()])?;
        self.builder.build_store(srt_ra, srt_cps.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
        let srt_cinc = self.builder.build_int_add(srt_civ, i64.const_int(1, false), "cinc").map_err(llvm_err)?;
        self.builder.build_store(srt_ci, srt_cinc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(srt_cloop);
        // Simple bubble sort on the copy
        self.builder.position_at_end(srt_cdone);
        let srt_i = self.builder.build_alloca(i64, "srt_i").map_err(llvm_err)?;
        self.builder.build_store(srt_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let srt_oloop = self.context.append_basic_block(srt_fn, "oloop");
        let srt_obody = self.context.append_basic_block(srt_fn, "obody");
        let srt_odone = self.context.append_basic_block(srt_fn, "odone");
        let _ = self.builder.build_unconditional_branch(srt_oloop);
        self.builder.position_at_end(srt_oloop);
        let srt_iv = self.builder.build_load(i64, srt_i, "iv").map_err(llvm_err)?.into_int_value();
        let srt_ocond = self.builder.build_int_compare(IntPredicate::SLT, srt_iv, srt_len, "ocond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(srt_ocond, srt_obody, srt_odone);
        self.builder.position_at_end(srt_obody);
        let srt_j = self.builder.build_alloca(i64, "srt_j").map_err(llvm_err)?;
        self.builder.build_store(srt_j, i64.const_int(0, false)).map_err(llvm_err)?;
        let srt_len1 = self.builder.build_int_sub(srt_len, i64.const_int(1, false), "len1").map_err(llvm_err)?;
        let srt_iloop = self.context.append_basic_block(srt_fn, "iloop");
        let srt_ibody = self.context.append_basic_block(srt_fn, "ibody");
        let srt_idone = self.context.append_basic_block(srt_fn, "idone");
        let _ = self.builder.build_unconditional_branch(srt_iloop);
        self.builder.position_at_end(srt_iloop);
        let srt_jv = self.builder.build_load(i64, srt_j, "jv").map_err(llvm_err)?.into_int_value();
        let srt_jc = self.builder.build_int_compare(IntPredicate::SLT, srt_jv, srt_len1, "jc").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(srt_jc, srt_ibody, srt_idone);
        self.builder.position_at_end(srt_ibody);
        let srt_cur = self.builder.build_load(self.list_type, srt_ra, "cur").map_err(llvm_err)?.into_struct_value();
        let srt_cur_data = self.builder.build_extract_value(srt_cur, 0, "curd").map_err(llvm_err)?.into_pointer_value();
        let srt_epa = unsafe { self.builder.build_gep(self.string_type, srt_cur_data, &[srt_jv], "epa").map_err(llvm_err) }?;
        let srt_epb = unsafe { self.builder.build_gep(self.string_type, srt_cur_data, &[self.builder.build_int_add(srt_jv, i64.const_int(1, false), "jp1").map_err(llvm_err)?], "epb").map_err(llvm_err) }?;
        let srt_ea = self.builder.build_load(self.string_type, srt_epa, "ea").map_err(llvm_err)?.into_struct_value();
        let srt_eb = self.builder.build_load(self.string_type, srt_epb, "eb").map_err(llvm_err)?.into_struct_value();
        // Compare Int values: extract data pointer as value for Tag=0
        let _srt_ea_tag = self.builder.build_extract_value(srt_ea, 0, "eat").map_err(llvm_err)?.into_int_value();
        let _srt_eb_tag = self.builder.build_extract_value(srt_eb, 0, "ebt").map_err(llvm_err)?.into_int_value();
        let _srt_is_int = self.builder.build_int_compare(IntPredicate::EQ, _srt_ea_tag, i64.const_int(0, false), "isint").map_err(llvm_err)?;
        let srt_ea_ptr = self.builder.build_extract_value(srt_ea, 1, "eap").map_err(llvm_err)?.into_pointer_value();
        let srt_eb_ptr = self.builder.build_extract_value(srt_eb, 1, "ebp").map_err(llvm_err)?.into_pointer_value();
        let srt_ea_int = self.builder.build_ptr_to_int(srt_ea_ptr, i64, "eai").map_err(llvm_err)?;
        let srt_eb_int = self.builder.build_ptr_to_int(srt_eb_ptr, i64, "ebi").map_err(llvm_err)?;
        let srt_swap_needed = self.builder.build_int_compare(IntPredicate::SGT, srt_ea_int, srt_eb_int, "swap").map_err(llvm_err)?;
        let srt_swap = self.context.append_basic_block(srt_fn, "swap");
        let srt_noswap = self.context.append_basic_block(srt_fn, "noswap");
        let _ = self.builder.build_conditional_branch(srt_swap_needed, srt_swap, srt_noswap);
        self.builder.position_at_end(srt_swap);
        self.builder.build_store(srt_epa, srt_eb).map_err(llvm_err)?;
        self.builder.build_store(srt_epb, srt_ea).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(srt_noswap);
        self.builder.position_at_end(srt_noswap);
        let srt_jinc = self.builder.build_int_add(srt_jv, i64.const_int(1, false), "jinc").map_err(llvm_err)?;
        self.builder.build_store(srt_j, srt_jinc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(srt_iloop);
        self.builder.position_at_end(srt_idone);
        let srt_iinc = self.builder.build_int_add(srt_iv, i64.const_int(1, false), "iinc").map_err(llvm_err)?;
        self.builder.build_store(srt_i, srt_iinc).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(srt_oloop);
        self.builder.position_at_end(srt_odone);
        let srt_rt = self.builder.build_load(self.list_type, srt_ra, "srt_rt").map_err(llvm_err)?;
        let _ = self.builder.build_return(Some(&srt_rt));

        Ok(())
    }
}
