use inkwell::IntPredicate;

use super::super::{CodeGen, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn define_print_functions(&self) -> Result<(), String> {
        let i64 = self.i64_ty();
        let f64 = self.f64_ty();
        let void = self.void_ty();
        let ptr = self.ptr_ty();
        let str_ty = self.string_type;
        let b1 = self.bool_ty();

        let printf_fn = self.module.get_function("printf").ok_or("printf not found")?;
        let fmt_int_ptr = self.module.get_global(".fmt_int").ok_or(".fmt_int not found")?.as_pointer_value();
        let fmt_float_ptr = self.module.get_global(".fmt_float").ok_or(".fmt_float not found")?.as_pointer_value();
        let fmt_str_ptr = self.module.get_global(".fmt_str").ok_or(".fmt_str not found")?.as_pointer_value();
        let fmt_nl_ptr = self.module.get_global(".fmt_nl").ok_or(".fmt_nl not found")?.as_pointer_value();
        let str_true_ptr = self.module.get_global(".str_true").ok_or(".str_true not found")?.as_pointer_value();
        let str_false_ptr = self.module.get_global(".str_false").ok_or(".str_false not found")?.as_pointer_value();
        let fmt_lb_ptr = self.module.get_global(".fmt_lb").ok_or(".fmt_lb not found")?.as_pointer_value();
        let fmt_sep_ptr = self.module.get_global(".fmt_sep").ok_or(".fmt_sep not found")?.as_pointer_value();
        let fmt_rb_ptr = self.module.get_global(".fmt_rb").ok_or(".fmt_rb not found")?.as_pointer_value();
        let fmt_task_pre_ptr = self.module.get_global(".fmt_task_pre").ok_or(".fmt_task_pre not found")?.as_pointer_value();
        let fmt_task_mid_ptr = self.module.get_global(".fmt_task_mid").ok_or(".fmt_task_mid not found")?.as_pointer_value();
        let fmt_task_suf_ptr = self.module.get_global(".fmt_task_suf").ok_or(".fmt_task_suf not found")?.as_pointer_value();
        let fmt_struct_ptr = self.module.get_global(".fmt_struct").ok_or(".fmt_struct not found")?.as_pointer_value();
        let str_none_ptr = self.module.get_global(".str_none").ok_or(".str_none not found")?.as_pointer_value();
        let str_some_pre_ptr = self.module.get_global(".str_some_pre").ok_or(".str_some_pre not found")?.as_pointer_value();
        let str_some_suf_ptr = self.module.get_global(".str_some_suf").ok_or(".str_some_suf not found")?.as_pointer_value();

        // ---- atomic_print_int(i64) ----
        let print_int_fn = self.module.add_function("atomic_print_int", void.fn_type(&[i64.into()], false), None);
        let entry = self.context.append_basic_block(print_int_fn, "entry");
        self.builder.position_at_end(entry);
        let n = print_int_fn.get_first_param().unwrap();
        let _ = self.builder.build_call(printf_fn, &[fmt_int_ptr.into(), n.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_print_float(double) ----
        let print_float_fn = self.module.add_function("atomic_print_float", void.fn_type(&[f64.into()], false), None);
        let entry = self.context.append_basic_block(print_float_fn, "entry");
        self.builder.position_at_end(entry);
        let n = print_float_fn.get_first_param().unwrap();
        let _ = self.builder.build_call(printf_fn, &[fmt_float_ptr.into(), n.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_print_bool(i1) ----
        let print_bool_fn = self.module.add_function("atomic_print_bool", void.fn_type(&[b1.into()], false), None);
        let entry = self.context.append_basic_block(print_bool_fn, "entry");
        let true_block = self.context.append_basic_block(print_bool_fn, "true_branch");
        let false_block = self.context.append_basic_block(print_bool_fn, "false_branch");
        self.builder.position_at_end(entry);
        let b = print_bool_fn.get_first_param().unwrap().into_int_value();
        let _ = self.builder.build_conditional_branch(b, true_block, false_block);
        self.builder.position_at_end(true_block);
        let _ = self.builder.build_call(printf_fn, &[fmt_str_ptr.into(), str_true_ptr.into()], "");
        let _ = self.builder.build_return(None);
        self.builder.position_at_end(false_block);
        let _ = self.builder.build_call(printf_fn, &[fmt_str_ptr.into(), str_false_ptr.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_print_string({i64, ptr}) ----
        let print_str_fn = self.module.add_function("atomic_print_string", void.fn_type(&[str_ty.into()], false), None);
        let entry = self.context.append_basic_block(print_str_fn, "entry");
        self.builder.position_at_end(entry);
        let s = print_str_fn.get_first_param().unwrap().into_struct_value();
        let data = self.builder.build_extract_value(s, 1, "data").map_err(llvm_err)?.into_pointer_value();
        let is_null = self.builder.build_is_null(data, "is_null").map_err(llvm_err)?;
        let str_bb = self.context.append_basic_block(print_str_fn, "print_str");
        let int_bb = self.context.append_basic_block(print_str_fn, "print_int");
        let _ = self.builder.build_conditional_branch(is_null, int_bb, str_bb);
        self.builder.position_at_end(str_bb);
        let _ = self.builder.build_call(printf_fn, &[fmt_str_ptr.into(), data.into()], "");
        let _ = self.builder.build_return(None);
        self.builder.position_at_end(int_bb);
        let tag = self.builder.build_extract_value(s, 0, "tag").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_call(printf_fn, &[fmt_int_ptr.into(), tag.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_println() ----
        let println_fn = self.module.add_function("atomic_println", void.fn_type(&[], false), None);
        let entry = self.context.append_basic_block(println_fn, "entry");
        self.builder.position_at_end(entry);
        let _ = self.builder.build_call(printf_fn, &[fmt_nl_ptr.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_list_print({ptr, i64, i64}) ----
        let list_print_fn = self.module.add_function("atomic_list_print", void.fn_type(&[self.list_type.into()], false), None);
        let lp_entry = self.context.append_basic_block(list_print_fn, "entry");
        self.builder.position_at_end(lp_entry);
        let lp_list = list_print_fn.get_first_param().unwrap().into_struct_value();
        let lp_data = self.builder.build_extract_value(lp_list, 0, "data").map_err(llvm_err)?.into_pointer_value();
        let lp_len = self.builder.build_extract_value(lp_list, 1, "len").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_call(printf_fn, &[fmt_lb_ptr.into()], "");
        let lp_i = self.builder.build_alloca(i64, "lpi").map_err(llvm_err)?;
        self.builder.build_store(lp_i, i64.const_int(0, false)).map_err(llvm_err)?;
        let lp_hdr = self.context.append_basic_block(list_print_fn, "lphdr");
        let lp_bdy = self.context.append_basic_block(list_print_fn, "lpbdy");
        let lp_ext = self.context.append_basic_block(list_print_fn, "lpext");
        let _ = self.builder.build_unconditional_branch(lp_hdr);
        self.builder.position_at_end(lp_hdr);
        let lp_iv = self.builder.build_load(i64, lp_i, "lpiv").map_err(llvm_err)?.into_int_value();
        let lp_cond = self.builder.build_int_compare(IntPredicate::SLT, lp_iv, lp_len, "lpcond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(lp_cond, lp_bdy, lp_ext);
        self.builder.position_at_end(lp_bdy);
        let lp_is_first = self.builder.build_int_compare(IntPredicate::EQ, lp_iv, i64.const_int(0, false), "is_first").map_err(llvm_err)?;
        let lp_sep_bb = self.context.append_basic_block(list_print_fn, "lpsep");
        let lp_val_bb = self.context.append_basic_block(list_print_fn, "lpval");
        let _ = self.builder.build_conditional_branch(lp_is_first, lp_val_bb, lp_sep_bb);
        self.builder.position_at_end(lp_sep_bb);
        let _ = self.builder.build_call(printf_fn, &[fmt_sep_ptr.into()], "");
        let _ = self.builder.build_unconditional_branch(lp_val_bb);
        self.builder.position_at_end(lp_val_bb);
        let lp_elem_ptr = unsafe { self.builder.build_gep(self.string_type, lp_data, &[lp_iv], "lpep").map_err(llvm_err) }?;
        let lp_elem = self.builder.build_load(self.string_type, lp_elem_ptr, "lpe").map_err(llvm_err)?.into_struct_value();
        let lp_tag = self.builder.build_extract_value(lp_elem, 0, "lptag").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_call(printf_fn, &[fmt_int_ptr.into(), lp_tag.into()], "");
        let lp_next = self.builder.build_int_add(lp_iv, i64.const_int(1, false), "lpnext").map_err(llvm_err)?;
        self.builder.build_store(lp_i, lp_next).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(lp_hdr);
        self.builder.position_at_end(lp_ext);
        let _ = self.builder.build_call(printf_fn, &[fmt_rb_ptr.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_print_task ----
        let task_print_fn = self.module.add_function("atomic_print_task", void.fn_type(&[self.task_type.into()], false), None);
        let tp_entry = self.context.append_basic_block(task_print_fn, "entry");
        self.builder.position_at_end(tp_entry);
        let tp_task = task_print_fn.get_first_param().unwrap().into_struct_value();
        let tp_done = self.builder.build_extract_value(tp_task, 1, "done").map_err(llvm_err)?;
        let tp_canc = self.builder.build_extract_value(tp_task, 2, "canc").map_err(llvm_err)?;
        let _ = self.builder.build_call(printf_fn, &[fmt_task_pre_ptr.into()], "");
        let _ = self.builder.build_call(printf_fn, &[fmt_int_ptr.into(), tp_done.into()], "");
        let _ = self.builder.build_call(printf_fn, &[fmt_task_mid_ptr.into()], "");
        let _ = self.builder.build_call(printf_fn, &[fmt_int_ptr.into(), tp_canc.into()], "");
        let _ = self.builder.build_call(printf_fn, &[fmt_task_suf_ptr.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_print_struct() ----
        let struct_print_fn = self.module.add_function("atomic_print_struct", void.fn_type(&[], false), None);
        let sp_entry = self.context.append_basic_block(struct_print_fn, "entry");
        self.builder.position_at_end(sp_entry);
        let _ = self.builder.build_call(printf_fn, &[fmt_struct_ptr.into()], "");
        let _ = self.builder.build_return(None);

        // ---- atomic_print_enum({i64, ptr}) ----
        let enum_ty = self.context.struct_type(&[i64.into(), ptr.into()], false);
        let enum_print_fn = self.module.add_function("atomic_print_enum", void.fn_type(&[enum_ty.into()], false), None);
        let ep_entry = self.context.append_basic_block(enum_print_fn, "entry");
        self.builder.position_at_end(ep_entry);
        let ep_enum = enum_print_fn.get_first_param().unwrap().into_struct_value();
        let ep_tag = self.builder.build_extract_value(ep_enum, 0, "tag").map_err(llvm_err)?;
        let ep_data = self.builder.build_extract_value(ep_enum, 1, "data").map_err(llvm_err)?;
        let is_some = self.builder.build_int_compare(IntPredicate::EQ, ep_tag.into_int_value(), i64.const_int(0, false), "is_some").map_err(llvm_err)?;
        let ep_some_bb = self.context.append_basic_block(enum_print_fn, "some");
        let ep_none_bb = self.context.append_basic_block(enum_print_fn, "none");
        let ep_merge_bb = self.context.append_basic_block(enum_print_fn, "merge");
        let _ = self.builder.build_conditional_branch(is_some, ep_some_bb, ep_none_bb);
        self.builder.position_at_end(ep_some_bb);
        let _ = self.builder.build_call(printf_fn, &[str_some_pre_ptr.into()], "");
        let ep_val_ptr = self.builder.build_pointer_cast(ep_data.into_pointer_value(), ptr, "vp").map_err(llvm_err)?;
        let ep_val = self.builder.build_load(i64, ep_val_ptr, "val").map_err(llvm_err)?;
        let _ = self.builder.build_call(printf_fn, &[fmt_int_ptr.into(), ep_val.into()], "");
        let _ = self.builder.build_call(printf_fn, &[str_some_suf_ptr.into()], "");
        let _ = self.builder.build_unconditional_branch(ep_merge_bb);
        self.builder.position_at_end(ep_none_bb);
        let _ = self.builder.build_call(printf_fn, &[str_none_ptr.into()], "");
        let _ = self.builder.build_unconditional_branch(ep_merge_bb);
        self.builder.position_at_end(ep_merge_bb);
        let _ = self.builder.build_return(None);

        // ---- atomic_print_enum_float({i64, ptr}) ----
        let epf_fn = self.module.add_function("atomic_print_enum_float", void.fn_type(&[enum_ty.into()], false), None);
        let epf_entry = self.context.append_basic_block(epf_fn, "entry");
        self.builder.position_at_end(epf_entry);
        let epf_enum = epf_fn.get_first_param().unwrap().into_struct_value();
        let epf_tag = self.builder.build_extract_value(epf_enum, 0, "tag").map_err(llvm_err)?;
        let epf_data = self.builder.build_extract_value(epf_enum, 1, "data").map_err(llvm_err)?;
        let epf_is_some = self.builder.build_int_compare(IntPredicate::EQ, epf_tag.into_int_value(), i64.const_int(0, false), "is_some_f").map_err(llvm_err)?;
        let epf_some_bb = self.context.append_basic_block(epf_fn, "some");
        let epf_none_bb = self.context.append_basic_block(epf_fn, "none");
        let epf_merge_bb = self.context.append_basic_block(epf_fn, "merge");
        let _ = self.builder.build_conditional_branch(epf_is_some, epf_some_bb, epf_none_bb);
        self.builder.position_at_end(epf_some_bb);
        let _ = self.builder.build_call(printf_fn, &[str_some_pre_ptr.into()], "");
        let epf_val_ptr = self.builder.build_pointer_cast(epf_data.into_pointer_value(), ptr, "vpf").map_err(llvm_err)?;
        let epf_val = self.builder.build_load(f64, epf_val_ptr, "valf").map_err(llvm_err)?;
        let _ = self.builder.build_call(printf_fn, &[fmt_float_ptr.into(), epf_val.into()], "");
        let _ = self.builder.build_call(printf_fn, &[str_some_suf_ptr.into()], "");
        let _ = self.builder.build_unconditional_branch(epf_merge_bb);
        self.builder.position_at_end(epf_none_bb);
        let _ = self.builder.build_call(printf_fn, &[str_none_ptr.into()], "");
        let _ = self.builder.build_unconditional_branch(epf_merge_bb);
        self.builder.position_at_end(epf_merge_bb);
        let _ = self.builder.build_return(None);

        Ok(())
    }
}
