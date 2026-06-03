use inkwell::types::BasicTypeEnum;
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, InnerType, llvm_err};
use crate::codegen::Scope;
use atomic::ast::Expr;

impl<'ctx> CodeGen<'ctx> {
    // ---- Coroutine builtins ----

    /// launch { body } — start a coroutine on a real pthread (default scheduler).
    /// launch(io) { body } — start with I/O scheduler.
    /// launch(cpu) { body } — start with CPU scheduler.
    /// Task struct: {pthread: i64, done: i64, cancelled: i64, result_list: {ptr, i64, i64}}
    pub(super) fn builtin_launch(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        // Parse optional scheduler argument
        let scheduler = if !args.is_empty() {
            match &args[0] {
                Expr::Ident(s) if s == "io" => 1i64,
                Expr::Ident(s) if s == "cpu" => 2i64,
                _ => return Err("launch scheduler must be 'io' or 'cpu'".to_string()),
            }
        } else {
            0i64 // default scheduler
        };
        let body = trailing.as_ref().ok_or("launch requires a trailing lambda body")?;
        let body_expr = match body.as_ref() {
            Expr::Lambda { params, body, .. } if params.is_empty() => body.as_ref(),
            _ => return Err("launch expects a block body: launch { ... }".to_string()),
        };

        // 1. Heap-allocate Task struct (so thread can safely write to it after main returns)
        // Compute task struct size via GEP trick
        let task_ty_ptr = self.context.ptr_type(Default::default());
        let null_task_ptr = task_ty_ptr.const_null();
        let task_size_ptr = unsafe { self.builder.build_gep(self.task_type, null_task_ptr, &[self.i64_ty().const_int(1, false)], "task_size_ptr").map_err(llvm_err) }?;
        let task_size = self.builder.build_ptr_to_int(task_size_ptr, self.i64_ty(), "task_size").map_err(llvm_err)?;
        let malloc_fn = self.module.get_function("malloc").unwrap();
        let task_heap = self.builder.build_call(malloc_fn, &[task_size.into()], "task_heap").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let task_undef = self.task_type.get_undef();
        let pthread_zero = self.i64_ty().const_int(0, false);
        let done_zero = self.i64_ty().const_int(0, false);
        let cancelled_zero = self.i64_ty().const_int(0, false);
        let empty_list = self.list_type.get_undef();
        let empty_list_ptr = self.ptr_ty().const_null();
        let empty_list_len = self.i64_ty().const_int(0, false);
        let empty_list_cap = self.i64_ty().const_int(0, false);
        let el0 = self.builder.build_insert_value(empty_list, empty_list_ptr, 0, "el0").map_err(llvm_err)?;
        let el1 = self.builder.build_insert_value(el0, empty_list_len, 1, "el1").map_err(llvm_err)?;
        let el2 = self.builder.build_insert_value(el1, empty_list_cap, 2, "el2").map_err(llvm_err)?;
        let t0 = self.builder.build_insert_value(task_undef, pthread_zero, 0, "t_pt").map_err(llvm_err)?;
        let t1 = self.builder.build_insert_value(t0, done_zero, 1, "t_done").map_err(llvm_err)?;
        let t2 = self.builder.build_insert_value(t1, cancelled_zero, 2, "t_canc").map_err(llvm_err)?;
        let sched_val = self.i64_ty().const_int(scheduler as u64, false);
        let t3 = self.builder.build_insert_value(t2, sched_val, 3, "t_sched").map_err(llvm_err)?;
        let t4 = self.builder.build_insert_value(t3, el2, 4, "t_list").map_err(llvm_err)?;
        self.builder.build_store(task_heap, t4).map_err(llvm_err)?;

        // 2. Compile body into a thread function that creates its own result list
        self.lambda_count += 1;
        let task_name = format!(".task_body_{}", self.lambda_count);
        let fn_type = self.ptr_ty().fn_type(&[self.ptr_ty().into()], false);
        let task_fn = self.module.add_function(&task_name, fn_type, None);
        let entry = self.context.append_basic_block(task_fn, "entry");

        let saved_pos = self.builder.get_insert_block();
        let mut saved_scope = Scope::new();
        std::mem::swap(&mut self.scope, &mut saved_scope);
        self.scope = Scope::new();

        self.builder.position_at_end(entry);
        let task_ptr_param = task_fn.get_first_param().unwrap().into_pointer_value();

        // Compile the body expression
        let result = self.compile_expr(body_expr)?;

        // Create a fresh list INSIDE the thread (avoids cross-thread data issues)
        let cap = self.i64_ty().const_int(1, false);
        let cc = self.call_rt("atomic_list_create", &[cap.into()])?;
        let list_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let rl_alloca = self.builder.build_alloca(self.list_type, "rl_a").map_err(llvm_err)?;
        self.builder.build_store(rl_alloca, list_bv).map_err(llvm_err)?;
        self.push_to_collector(rl_alloca, &result)?;

        // Write done=1 and the new list back to the task struct
        let updated_list = self.builder.build_load(self.list_type, rl_alloca, "ul").map_err(llvm_err)?;
        let task_ptr_cast = self.builder.build_pointer_cast(task_ptr_param, self.context.ptr_type(Default::default()), "task_cast").map_err(llvm_err)?;
        let loaded_task = self.builder.build_load(self.task_type, task_ptr_cast, "ltask").map_err(llvm_err)?.into_struct_value();
        let done_one = self.i64_ty().const_int(1, false);
        let cancelled_val = self.builder.build_extract_value(loaded_task, 2, "cv").map_err(llvm_err)?;
        let pt_val = self.builder.build_extract_value(loaded_task, 0, "pv").map_err(llvm_err)?;
        let sched_val = self.builder.build_extract_value(loaded_task, 3, "sv").map_err(llvm_err)?;
        let undef2 = self.task_type.get_undef();
        let u0 = self.builder.build_insert_value(undef2, pt_val, 0, "u_pt").map_err(llvm_err)?;
        let u1 = self.builder.build_insert_value(u0, done_one, 1, "u_done").map_err(llvm_err)?;
        let u2 = self.builder.build_insert_value(u1, cancelled_val, 2, "u_canc").map_err(llvm_err)?;
        let u3 = self.builder.build_insert_value(u2, sched_val, 3, "u_sched").map_err(llvm_err)?;
        let u4 = self.builder.build_insert_value(u3, updated_list, 4, "u_list").map_err(llvm_err)?;
        self.builder.build_store(task_ptr_cast, u4).map_err(llvm_err)?;

        // Return from thread function
        let current_block = self.builder.get_insert_block().unwrap();
        if current_block.get_terminator().is_none() {
            let null_ret = self.ptr_ty().const_null();
            let _ = self.builder.build_return(Some(&null_ret));
        }

        std::mem::swap(&mut self.scope, &mut saved_scope);
        if let Some(pos) = saved_pos {
            self.builder.position_at_end(pos);
        }

        // 3. Call pthread_create
        let pthread_create_fn = self.module.get_function("pthread_create").unwrap();
        let pthread_field_ptr = self.builder.build_struct_gep(self.task_type, task_heap, 0, "pt_field").map_err(llvm_err)?;
        let fn_as_ptr = task_fn.as_global_value().as_pointer_value();
        let _ = self.builder.build_call(pthread_create_fn, &[
            pthread_field_ptr.into(),
            self.ptr_ty().const_null().into(),
            fn_as_ptr.into(),
            task_heap.into(),
        ], "").map_err(llvm_err)?;

        // 5. If inside coroutineScope, track this task for later join
        if let Some(collector_alloca) = self.coroutine_collector {
            // Store task_heap pointer as i64 in a fat struct {ptr_as_i64, null}
            let task_as_i64 = self.builder.build_ptr_to_int(task_heap, self.i64_ty(), "task_i64").map_err(llvm_err)?;
            let task_fat = self.make_int_fat(task_as_i64)?;
            let cl = self.load_list(collector_alloca)?;
            let cc = self.call_rt("atomic_list_push", &[cl.into(), task_fat.into()])?;
            let nl = cc.try_as_basic_value().basic().ok_or("push failed")?;
            self.builder.build_store(collector_alloca, nl).map_err(llvm_err)?;
        }

        Ok(TypedValue::Task(task_heap))
    }

    /// coroutineScope { body } — structured concurrency scope with real pthread join.
    pub(super) fn builtin_coroutine_scope(&mut self, _args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        let body = trailing.as_ref().ok_or("coroutineScope requires a trailing lambda body")?;
        let body_expr = match body.as_ref() {
            Expr::Lambda { params, body, .. } if params.is_empty() => body.as_ref(),
            _ => return Err("coroutineScope expects a block body: coroutineScope { ... }".to_string()),
        };

        // Create collector list for task pointers
        let cap = self.i64_ty().const_int(4, false);
        let cc = self.call_rt("atomic_list_create", &[cap.into()])?;
        let list_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
        let collector_alloca = self.builder.build_alloca(self.list_type, "coro_collector").map_err(llvm_err)?;
        self.builder.build_store(collector_alloca, list_bv).map_err(llvm_err)?;

        // Save previous collector and set new one
        let prev_collector = self.coroutine_collector;
        self.coroutine_collector = Some(collector_alloca);

        // Compile the body (launch calls inside will spawn threads and push task pointers to collector)
        self.compile_expr(body_expr)?;

        // Restore previous collector
        self.coroutine_collector = prev_collector;

        // Join all tasks and collect results
        let collector_list = self.load_list(collector_alloca)?;
        let task_count = self.builder.build_extract_value(collector_list, 1, "tc").map_err(llvm_err)?.into_int_value();
        let task_data = self.builder.build_extract_value(collector_list, 0, "td").map_err(llvm_err)?.into_pointer_value();

        // Create result list
        let result_cap = self.i64_ty().const_int(4, false);
        let rcc = self.call_rt("atomic_list_create", &[result_cap.into()])?;
        let result_list_bv = rcc.try_as_basic_value().basic().ok_or("result list_create failed")?;
        let result_alloca = self.builder.build_alloca(self.list_type, "coro_results").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, result_list_bv).map_err(llvm_err)?;

        // Loop: for each task in collector, join and collect result
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
        let i_alloca = self.builder.build_alloca(self.i64_ty(), "cs_i").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, self.i64_ty().const_int(0, false)).map_err(llvm_err)?;
        // Allocate cancel-loop index alloca here (dominates all cancel blocks)
        let cj_alloca = self.builder.build_alloca(self.i64_ty(), "cs_cj").map_err(llvm_err)?;

        let loop_hdr = self.context.append_basic_block(current_fn, "cs_hdr");
        let loop_body = self.context.append_basic_block(current_fn, "cs_body");
        let loop_exit = self.context.append_basic_block(current_fn, "cs_exit");

        let _ = self.builder.build_unconditional_branch(loop_hdr);

        self.builder.position_at_end(loop_hdr);
        let i_val = self.builder.build_load(self.i64_ty(), i_alloca, "cs_iv").map_err(llvm_err)?.into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, i_val, task_count, "cs_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cond, loop_body, loop_exit);

        // Create blocks for fast-fail handling
        let cancel_init = self.context.append_basic_block(current_fn, "cs_cancel_init");
        let cancel_loop_hdr = self.context.append_basic_block(current_fn, "cs_cancel_hdr");
        let cancel_loop_body = self.context.append_basic_block(current_fn, "cs_cancel_body");
        let cancel_exit = self.context.append_basic_block(current_fn, "cs_cancel_exit");

        self.builder.position_at_end(loop_body);

        // Load task fat struct from collector[i]
        let elem_gep = unsafe { self.builder.build_gep(self.string_type, task_data, &[i_val], "cs_gep").map_err(llvm_err) }?;
        let elem_fat = self.builder.build_load(self.string_type, elem_gep, "cs_fat").map_err(llvm_err)?.into_struct_value();
        let task_i64 = self.builder.build_extract_value(elem_fat, 0, "cs_ti64").map_err(llvm_err)?.into_int_value();
        let task_ptr = self.builder.build_int_to_ptr(task_i64, self.context.ptr_type(Default::default()), "cs_tp").map_err(llvm_err)?;

        let task_sv = self.builder.build_load(self.task_type, task_ptr, "cs_task").map_err(llvm_err)?.into_struct_value();
        let pthread_val = self.builder.build_extract_value(task_sv, 0, "cs_pt").map_err(llvm_err)?.into_int_value();

        let pthread_join_fn = self.module.get_function("pthread_join").unwrap();
        let null_ptr = self.ptr_ty().const_null();
        let _ = self.builder.build_call(pthread_join_fn, &[pthread_val.into(), null_ptr.into()], "").map_err(llvm_err)?;

        let task_sv2 = self.builder.build_load(self.task_type, task_ptr, "cs_task2").map_err(llvm_err)?.into_struct_value();
        let result_list_sv = self.builder.build_extract_value(task_sv2, 4, "cs_rl").map_err(llvm_err)?.into_struct_value();

        let rl_alloca = self.builder.build_alloca(self.list_type, "cs_rla").map_err(llvm_err)?;
        self.builder.build_store(rl_alloca, result_list_sv).map_err(llvm_err)?;
        let rl_val = self.load_list(rl_alloca)?;
        let zero = self.i64_ty().const_int(0, false);
        let cc = self.call_rt("atomic_list_get", &[rl_val.into(), zero.into()])?;
        let fat = cc.try_as_basic_value().basic().ok_or("get failed")?.into_struct_value();

        // Fast-fail check: tag==1 && data_ptr!=null means Err variant
        let fat_tag = self.builder.build_extract_value(fat, 0, "ff_tag").map_err(llvm_err)?.into_int_value();
        let fat_data = self.builder.build_extract_value(fat, 1, "ff_data").map_err(llvm_err)?.into_pointer_value();
        let is_err_tag = self.builder.build_int_compare(IntPredicate::EQ, fat_tag, self.i64_ty().const_int(1, false), "is_err").map_err(llvm_err)?;
        let data_nonnull = self.builder.build_int_compare(IntPredicate::NE, fat_data, self.ptr_ty().const_null(), "data_ok").map_err(llvm_err)?;
        let is_error = self.builder.build_and(is_err_tag, data_nonnull, "is_error").map_err(llvm_err)?;
        let add_ok_bb = self.context.append_basic_block(current_fn, "cs_add_ok");
        let _ = self.builder.build_conditional_branch(is_error, cancel_init, add_ok_bb);

        // Add OK result to result list
        self.builder.position_at_end(add_ok_bb);
        let cur_results = self.load_list(result_alloca)?;
        let cc2 = self.call_rt("atomic_list_push", &[cur_results.into(), fat.into()])?;
        let new_results = cc2.try_as_basic_value().basic().ok_or("push2 failed")?;
        self.builder.build_store(result_alloca, new_results).map_err(llvm_err)?;
        let next_i = self.builder.build_int_add(i_val, self.i64_ty().const_int(1, false), "cs_ni").map_err(llvm_err)?;
        self.builder.build_store(i_alloca, next_i).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_hdr);

        // Cancel init: compute start index (i+1, skip already-joined task)
        self.builder.position_at_end(cancel_init);
        let cancel_start_i = self.builder.build_int_add(i_val, self.i64_ty().const_int(1, false), "cs_csi").map_err(llvm_err)?;
        self.builder.build_store(cj_alloca, cancel_start_i).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(cancel_loop_hdr);

        // Cancel loop header
        self.builder.position_at_end(cancel_loop_hdr);
        let cj_val = self.builder.build_load(self.i64_ty(), cj_alloca, "cs_cjv").map_err(llvm_err)?.into_int_value();
        let cc_cond = self.builder.build_int_compare(IntPredicate::SLT, cj_val, task_count, "cs_ccond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(cc_cond, cancel_loop_body, cancel_exit);

        // Cancel loop body: cancel one task
        self.builder.position_at_end(cancel_loop_body);
        let c_elem_gep = unsafe { self.builder.build_gep(self.string_type, task_data, &[cj_val], "cs_cgep").map_err(llvm_err) }?;
        let c_elem_fat = self.builder.build_load(self.string_type, c_elem_gep, "cs_cfat").map_err(llvm_err)?.into_struct_value();
        let c_task_i64 = self.builder.build_extract_value(c_elem_fat, 0, "cs_cti64").map_err(llvm_err)?.into_int_value();
        let c_task_ptr = self.builder.build_int_to_ptr(c_task_i64, self.context.ptr_type(Default::default()), "cs_ctp").map_err(llvm_err)?;
        let c_task_sv = self.builder.build_load(self.task_type, c_task_ptr, "cs_ctsk").map_err(llvm_err)?.into_struct_value();
        let c_pt_val = self.builder.build_extract_value(c_task_sv, 0, "cs_cpt").map_err(llvm_err)?.into_int_value();
        let pthread_cancel_fn = self.module.get_function("pthread_cancel").unwrap();
        let _ = self.builder.build_call(pthread_cancel_fn, &[c_pt_val.into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_call(pthread_join_fn, &[c_pt_val.into(), self.ptr_ty().const_null().into()], "").map_err(llvm_err)?;
        let c_next = self.builder.build_int_add(cj_val, self.i64_ty().const_int(1, false), "cs_cn").map_err(llvm_err)?;
        self.builder.build_store(cj_alloca, c_next).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(cancel_loop_hdr);

        // After cancelling all remaining, push the error to result list and exit
        self.builder.position_at_end(cancel_exit);
        let err_results = self.load_list(result_alloca)?;
        let ecc = self.call_rt("atomic_list_push", &[err_results.into(), fat.into()])?;
        let enew = ecc.try_as_basic_value().basic().ok_or("err push failed")?;
        self.builder.build_store(result_alloca, enew).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(loop_exit);

        self.builder.position_at_end(loop_exit);
        Ok(TypedValue::List(result_alloca))
    }

    /// delay(ms) — suspend coroutine for ms milliseconds using usleep.
    pub(super) fn builtin_delay(&mut self, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err("delay expects 1 argument (ms)".to_string());
        }
        let ms_val = self.compile_expr(&args[0])?;
        let ms = match ms_val {
            TypedValue::Int(v) => v,
            _ => return Err("delay: argument must be an Int (milliseconds)".to_string()),
        };
        // usleep takes microseconds: ms * 1000
        let thousand = self.i64_ty().const_int(1000, false);
        let us = self.builder.build_int_mul(ms, thousand, "delay_us").map_err(llvm_err)?;
        // Truncate to i32 for usleep
        let us_i32 = self.builder.build_int_truncate(us, self.i32_ty(), "delay_us32").map_err(llvm_err)?;
        let usleep_fn = self.module.get_function("usleep").unwrap();
        let _ = self.builder.build_call(usleep_fn, &[us_i32.into()], "").map_err(llvm_err)?;
        Ok(TypedValue::Unit)
    }

    /// Push a TypedValue to the collector list (used by launch inside coroutineScope).
    pub(super) fn push_to_collector(&mut self, collector_alloca: inkwell::values::PointerValue<'ctx>, value: &TypedValue<'ctx>) -> Result<(), String> {
        let elem_fat = self.to_fat_struct(value)?;
        let list_val = self.load_list(collector_alloca)?;
        let cc = self.call_rt("atomic_list_push", &[list_val.into(), elem_fat.into()])?;
        let new_list = cc.try_as_basic_value().basic().ok_or("list_push failed")?;
        self.builder.build_store(collector_alloca, new_list).map_err(llvm_err)?;
        Ok(())
    }

    /// withTimeout(ms, { body }) — timeout-controlled coroutine execution using pthread.
    /// Spawns a real pthread for the body, polls until done or timeout.
    /// Returns Ok(result) on success, Err(Timeout) on timeout.
    pub(super) fn builtin_with_timeout(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err("withTimeout expects 2 arguments: timeout(ms) and a trailing lambda".to_string());
        }
        let timeout_ms_val = self.compile_expr(&args[0])?;
        let timeout_ms = match &timeout_ms_val {
            TypedValue::Int(v) => *v,
            _ => return Err("withTimeout: first argument must be Int (milliseconds)".to_string()),
        };
        let body = trailing.as_ref().ok_or("withTimeout requires a trailing lambda body")?;
        let body_expr = match body.as_ref() {
            Expr::Lambda { params, body, .. } if params.is_empty() => body.as_ref().clone(),
            _ => return Err("withTimeout expects a block body: withTimeout(ms) { ... }".to_string()),
        };

        // 1. Heap-allocate Task struct for the thread to write results into
        let task_ty_ptr = self.context.ptr_type(Default::default());
        let null_task_ptr = task_ty_ptr.const_null();
        let task_size_ptr = unsafe { self.builder.build_gep(self.task_type, null_task_ptr, &[self.i64_ty().const_int(1, false)], "wtsz").map_err(llvm_err) }?;
        let task_size = self.builder.build_ptr_to_int(task_size_ptr, self.i64_ty(), "wtsz_i64").map_err(llvm_err)?;
        let malloc_fn = self.module.get_function("malloc").unwrap();
        let task_heap = self.builder.build_call(malloc_fn, &[task_size.into()], "wt_task").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        // Initialize task struct with zeroes
        let task_undef = self.task_type.get_undef();
        let pthread_zero = self.i64_ty().const_int(0, false);
        let done_zero = self.i64_ty().const_int(0, false);
        let cancelled_zero = self.i64_ty().const_int(0, false);
        let empty_list = self.list_type.get_undef();
        let empty_list_ptr = self.ptr_ty().const_null();
        let empty_list_len = self.i64_ty().const_int(0, false);
        let empty_list_cap = self.i64_ty().const_int(0, false);
        let el0 = self.builder.build_insert_value(empty_list, empty_list_ptr, 0, "el0").map_err(llvm_err)?;
        let el1 = self.builder.build_insert_value(el0, empty_list_len, 1, "el1").map_err(llvm_err)?;
        let el2 = self.builder.build_insert_value(el1, empty_list_cap, 2, "el2").map_err(llvm_err)?;
        let t0 = self.builder.build_insert_value(task_undef, pthread_zero, 0, "t0").map_err(llvm_err)?;
        let t1 = self.builder.build_insert_value(t0, done_zero, 1, "t1").map_err(llvm_err)?;
        let t2 = self.builder.build_insert_value(t1, cancelled_zero, 2, "t2").map_err(llvm_err)?;
        let sched_zero = self.i64_ty().const_int(0, false); // default scheduler for withTimeout
        let t3 = self.builder.build_insert_value(t2, sched_zero, 3, "t3_sched").map_err(llvm_err)?;
        let t4 = self.builder.build_insert_value(t3, el2, 4, "t4_list").map_err(llvm_err)?;
        self.builder.build_store(task_heap, t4).map_err(llvm_err)?;

        // 2. Compile body into a thread function
        self.lambda_count += 1;
        let task_name = format!(".wt_body_{}", self.lambda_count);
        let fn_type = self.ptr_ty().fn_type(&[self.ptr_ty().into()], false);
        let task_fn = self.module.add_function(&task_name, fn_type, None);
        let entry = self.context.append_basic_block(task_fn, "entry");

        let saved_pos = self.builder.get_insert_block();
        let mut saved_scope = Scope::new();
        std::mem::swap(&mut self.scope, &mut saved_scope);
        self.scope = Scope::new();

        self.builder.position_at_end(entry);
        let task_ptr_param = task_fn.get_first_param().unwrap().into_pointer_value();

        let result = self.compile_expr(&body_expr)?;

        // Store result in the task's result_list
        let cap = self.i64_ty().const_int(1, false);
        let cc = self.call_rt("atomic_list_create", &[cap.into()])?;
        let list_bv = cc.try_as_basic_value().basic().ok_or("wt list_create failed")?;
        let rl_alloca = self.builder.build_alloca(self.list_type, "wt_rl").map_err(llvm_err)?;
        self.builder.build_store(rl_alloca, list_bv).map_err(llvm_err)?;
        self.push_to_collector(rl_alloca, &result)?;

        // Write done=1 and result_list to task struct
        let updated_list = self.builder.build_load(self.list_type, rl_alloca, "wt_ul").map_err(llvm_err)?;
        let task_ptr_cast = self.builder.build_pointer_cast(task_ptr_param, self.context.ptr_type(Default::default()), "wt_task_cast").map_err(llvm_err)?;
        let loaded_task = self.builder.build_load(self.task_type, task_ptr_cast, "wt_lt").map_err(llvm_err)?.into_struct_value();
        let done_one = self.i64_ty().const_int(1, false);
        let cancelled_val = self.builder.build_extract_value(loaded_task, 2, "wt_cv").map_err(llvm_err)?;
        let pt_val = self.builder.build_extract_value(loaded_task, 0, "wt_pv").map_err(llvm_err)?;
        let undef2 = self.task_type.get_undef();
        let u0 = self.builder.build_insert_value(undef2, pt_val, 0, "u0").map_err(llvm_err)?;
        let u1 = self.builder.build_insert_value(u0, done_one, 1, "u1").map_err(llvm_err)?;
        let u2 = self.builder.build_insert_value(u1, cancelled_val, 2, "u2").map_err(llvm_err)?;
        let wt_sched_val = self.builder.build_extract_value(loaded_task, 3, "wt_sv").map_err(llvm_err)?;
        let u3 = self.builder.build_insert_value(u2, wt_sched_val, 3, "u3_sched").map_err(llvm_err)?;
        let u4 = self.builder.build_insert_value(u3, updated_list, 4, "u4_list").map_err(llvm_err)?;
        self.builder.build_store(task_ptr_cast, u4).map_err(llvm_err)?;
        let null_ret = self.ptr_ty().const_null();
        let _ = self.builder.build_return(Some(&null_ret));

        std::mem::swap(&mut self.scope, &mut saved_scope);
        if let Some(pos) = saved_pos {
            self.builder.position_at_end(pos);
        }

        // 3. Spawn thread with pthread_create
        let pthread_create_fn = self.module.get_function("pthread_create").unwrap();
        let pthread_field_ptr = self.builder.build_struct_gep(self.task_type, task_heap, 0, "wt_ptf").map_err(llvm_err)?;
        let fn_as_ptr = task_fn.as_global_value().as_pointer_value();
        let _ = self.builder.build_call(pthread_create_fn, &[
            pthread_field_ptr.into(),
            self.ptr_ty().const_null().into(),
            fn_as_ptr.into(),
            task_heap.into(),
        ], "").map_err(llvm_err)?;

        // 4. Polling loop: check done flag every 10ms until timeout
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
        let done_field_ptr = self.builder.build_struct_gep(self.task_type, task_heap, 1, "wt_done_ptr").map_err(llvm_err)?;
        let elapsed_alloca = self.builder.build_alloca(self.i64_ty(), "wt_elapsed").map_err(llvm_err)?;
        self.builder.build_store(elapsed_alloca, self.i64_ty().const_int(0, false)).map_err(llvm_err)?;
        let poll_interval = 10_000_i64; // 10ms in microseconds

        let poll_hdr = self.context.append_basic_block(current_fn, "wt_poll_hdr");
        let poll_body = self.context.append_basic_block(current_fn, "wt_poll_body");
        let poll_done = self.context.append_basic_block(current_fn, "wt_poll_done");
        let poll_timeout = self.context.append_basic_block(current_fn, "wt_poll_timeout");

        let _ = self.builder.build_unconditional_branch(poll_hdr);
        self.builder.position_at_end(poll_hdr);
        // Load elapsed and check if >= timeout_ms
        let elapsed = self.builder.build_load(self.i64_ty(), elapsed_alloca, "wt_el").map_err(llvm_err)?.into_int_value();
        let timed_out = self.builder.build_int_compare(IntPredicate::SGE, elapsed, timeout_ms, "wt_to").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(timed_out, poll_timeout, poll_body);

        // Poll body: sleep 10ms, then check done flag
        self.builder.position_at_end(poll_body);
        let usleep_fn = self.module.get_function("usleep").unwrap();
        let _ = self.builder.build_call(usleep_fn, &[self.i32_ty().const_int(poll_interval as u64, false).into()], "").map_err(llvm_err)?;
        // Update elapsed
        let new_elapsed = self.builder.build_int_add(elapsed, self.i64_ty().const_int(10, false), "wt_ne").map_err(llvm_err)?;
        self.builder.build_store(elapsed_alloca, new_elapsed).map_err(llvm_err)?;
        // Check done flag
        let done_val = self.builder.build_load(self.i64_ty(), done_field_ptr, "wt_dv").map_err(llvm_err)?.into_int_value();
        let is_done = self.builder.build_int_compare(IntPredicate::NE, done_val, self.i64_ty().const_int(0, false), "wt_id").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_done, poll_done, poll_hdr);

        // Timeout: cancel thread and return Err(Timeout)
        self.builder.position_at_end(poll_timeout);
        let pthread_cancel_fn = self.module.get_function("pthread_cancel").unwrap();
        let pthread_val_t = self.builder.build_load(self.i64_ty(), pthread_field_ptr, "wt_ptv").map_err(llvm_err)?.into_int_value();
        let _ = self.builder.build_call(pthread_cancel_fn, &[pthread_val_t.into()], "").map_err(llvm_err)?;
        let pthread_join_fn = self.module.get_function("pthread_join").unwrap();
        let _ = self.builder.build_call(pthread_join_fn, &[pthread_val_t.into(), self.ptr_ty().const_null().into()], "").map_err(llvm_err)?;
        // Return Err(Timeout)
        let (timeout_enum, timeout_variant) = self.registry.lookup_variant("Timeout")
            .map(|(ei, vi)| (ei.clone(), vi.clone()))
            .ok_or("TimeoutError enum with Timeout variant required for withTimeout")?;
        let timeout_err = self.compile_enum_construct(&timeout_enum, &timeout_variant, &[])?;
        let err_val = self.to_fat_struct(&timeout_err)?;
        let err_alloca = self.builder.build_alloca(self.string_type, "wt_err").map_err(llvm_err)?;
        self.builder.build_store(err_alloca, err_val).map_err(llvm_err)?;
        let (result_enum, err_variant) = self.registry.lookup_variant("Err")
            .map(|(ei, vi)| (ei.clone(), vi.clone()))
            .ok_or("Result enum with Err variant required for withTimeout")?;
        let err_enum = self.compile_enum_construct(&result_enum, &err_variant, &[])?;
        // Store the timeout error payload into the Err
        let err_enum_ptr = match &err_enum {
            TypedValue::Enum(p, _, ..) => *p,
            _ => return Err("withTimeout: failed to construct Err".to_string()),
        };
        let err_bt: BasicTypeEnum = self.string_type.into();
        let err_loaded = self.builder.build_load(err_bt, err_alloca, "wt_err_ld").map_err(llvm_err)?;
        let err_sv = err_loaded.into_struct_value();
        let err_tag = self.builder.build_extract_value(err_sv, 0, "wt_etag").map_err(llvm_err)?;
        let err_data = self.builder.build_extract_value(err_sv, 1, "wt_edata").map_err(llvm_err)?;
        let undef_err = self.string_type.get_undef();
        let e1 = self.builder.build_insert_value(undef_err, err_tag, 0, "e1").map_err(llvm_err)?;
        let e2 = self.builder.build_insert_value(e1, err_data, 1, "e2").map_err(llvm_err)?;
        self.builder.build_store(err_enum_ptr, e2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(poll_done); // reuse done block to load result

        // Done: pthread_join and return Ok(result)
        self.builder.position_at_end(poll_done);
        // pthread_join if not already joined
        let done_pthread_val = self.builder.build_load(self.i64_ty(), pthread_field_ptr, "wt_dpt").map_err(llvm_err)?.into_int_value();
        // Only join from the success path (not timeout)
        let pt_is_nonzero = self.builder.build_int_compare(IntPredicate::NE, done_pthread_val, self.i64_ty().const_int(0, false), "pt_nz").map_err(llvm_err)?;
        let join_bb = self.context.append_basic_block(current_fn, "wt_join");
        let merge_bb = self.context.append_basic_block(current_fn, "wt_merge");
        let _ = self.builder.build_conditional_branch(pt_is_nonzero, join_bb, merge_bb);
        self.builder.position_at_end(join_bb);
        let _ = self.builder.build_call(pthread_join_fn, &[done_pthread_val.into(), self.ptr_ty().const_null().into()], "").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        self.builder.position_at_end(merge_bb);

        // Load result from task's result_list
        let task_sv = self.builder.build_load(self.task_type, task_heap, "wt_tsk").map_err(llvm_err)?.into_struct_value();
        let result_list_sv = self.builder.build_extract_value(task_sv, 4, "wt_rl").map_err(llvm_err)?.into_struct_value();
        let rla = self.builder.build_alloca(self.list_type, "wt_rla").map_err(llvm_err)?;
        self.builder.build_store(rla, result_list_sv).map_err(llvm_err)?;
        let rl_val = self.load_list(rla)?;
        let zero = self.i64_ty().const_int(0, false);
        let cc = self.call_rt("atomic_list_get", &[rl_val.into(), zero.into()])?;
        let fat = cc.try_as_basic_value().basic().ok_or("wt get failed")?.into_struct_value();

        // Free task heap
        let free_fn = self.module.get_function("free").unwrap();
        let _ = self.builder.build_call(free_fn, &[task_heap.into()], "").map_err(llvm_err)?;

        // Wrap result in Ok(result)
        let (_ok_enum, _ok_variant) = self.registry.lookup_variant("Ok")
            .map(|(ei, vi)| (ei.clone(), vi.clone()))
            .ok_or("Result enum with Ok variant required for withTimeout")?;
        let result_struct = self.string_type.get_undef();
        let r1 = self.builder.build_insert_value(result_struct, fat, 0, "r1").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, self.ptr_ty().const_null(), 1, "r2").map_err(llvm_err)?;
        let fat_alloca = self.builder.build_alloca(self.string_type, "wt_fat").map_err(llvm_err)?;
        self.builder.build_store(fat_alloca, r2).map_err(llvm_err)?;
        let ok_bt: BasicTypeEnum = self.string_type.into();
        // Create Some/Ok wrapper: {tag: 0, data: ptr to fat struct copy}
        let fat_loaded = self.builder.build_load(ok_bt, fat_alloca, "wt_fl").map_err(llvm_err)?.into_struct_value();
        let ok_val_i64 = self.builder.build_extract_value(fat_loaded, 0, "wt_ovi").map_err(llvm_err)?.into_int_value();
        let ok_val_ptr = self.builder.build_extract_value(fat_loaded, 1, "wt_ovp").map_err(llvm_err)?.into_pointer_value();
        // Allocate heap copy of the fat struct data
        let heap_copy = self.builder.build_call(malloc_fn, &[self.i64_ty().const_int(16, false).into()], "wt_hc").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let _ = self.builder.build_store(heap_copy, ok_val_i64).map_err(llvm_err)?;
        let data_ptr = unsafe { self.builder.build_gep(self.i64_ty(), heap_copy, &[self.i64_ty().const_int(1, false)], "wt_dp").map_err(llvm_err) }?;
        let _ = self.builder.build_store(data_ptr, ok_val_ptr).map_err(llvm_err)?;
        let ok_alloca = self.builder.build_alloca(self.string_type, "wt_ok").map_err(llvm_err)?;
        let ok_undef = self.string_type.get_undef();
        let ok_t = self.builder.build_insert_value(ok_undef, self.i64_ty().const_int(0, false), 0, "ok_t").map_err(llvm_err)?;
        let ok_d = self.builder.build_insert_value(ok_t, heap_copy, 1, "ok_d").map_err(llvm_err)?;
        self.builder.build_store(ok_alloca, ok_d).map_err(llvm_err)?;
        Ok(TypedValue::Enum(ok_alloca, self.string_type, InnerType::Int, true))
    }

    /// stream() — create a new Stream<T> channel with mutex + condvar + buffer.
    /// Stream struct (heap-allocated): {mutex: [40 x i8], cond: [48 x i8], closed: i64, list: {ptr, i64, i64}}
    pub(super) fn builtin_stream_create(&mut self) -> Result<TypedValue<'ctx>, String> {
        let stream_ty = self.stream_type;
        let null_ptr = self.context.ptr_type(Default::default()).const_null();
        let size_ptr = unsafe { self.builder.build_gep(stream_ty, null_ptr, &[self.i64_ty().const_int(1, false)], "stream_size_ptr").map_err(llvm_err) }?;
        let stream_size = self.builder.build_ptr_to_int(size_ptr, self.i64_ty(), "stream_size").map_err(llvm_err)?;
        let malloc_fn = self.module.get_function("malloc").unwrap();
        let stream_buf = self.builder.build_call(malloc_fn, &[stream_size.into()], "stream_buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
        let stream_ptr = self.builder.build_pointer_cast(stream_buf, self.context.ptr_type(Default::default()), "stream_ptr").map_err(llvm_err)?;

        // Initialize mutex (field 0)
        let pthread_mutex_init_fn = self.module.get_function("pthread_mutex_init").unwrap();
        let mutex_field_ptr = self.builder.build_struct_gep(stream_ty, stream_ptr, 0, "mutex_field").map_err(llvm_err)?;
        let _ = self.builder.build_call(pthread_mutex_init_fn, &[mutex_field_ptr.into(), self.ptr_ty().const_null().into()], "").map_err(llvm_err)?;

        // Initialize condvar (field 1)
        let pthread_cond_init_fn = self.module.get_function("pthread_cond_init").unwrap();
        let cond_field_ptr = self.builder.build_struct_gep(stream_ty, stream_ptr, 1, "cond_field").map_err(llvm_err)?;
        let _ = self.builder.build_call(pthread_cond_init_fn, &[cond_field_ptr.into(), self.ptr_ty().const_null().into()], "").map_err(llvm_err)?;

        // Initialize closed flag to 0 (field 2)
        let closed_field_ptr = self.builder.build_struct_gep(stream_ty, stream_ptr, 2, "closed_field").map_err(llvm_err)?;
        self.builder.build_store(closed_field_ptr, self.i64_ty().const_int(0, false)).map_err(llvm_err)?;

        // Initialize list (field 3)
        let cap = self.i64_ty().const_int(4, false);
        let cc = self.call_rt("atomic_list_create", &[cap.into()])?;
        let list_bv = cc.try_as_basic_value().basic().ok_or("stream list_create failed")?;
        let list_field_ptr = self.builder.build_struct_gep(stream_ty, stream_ptr, 3, "list_field").map_err(llvm_err)?;
        self.builder.build_store(list_field_ptr, list_bv).map_err(llvm_err)?;

        Ok(TypedValue::Stream(stream_ptr))
    }

    /// Stream operations: send(stream, value), receive(stream), close(stream)
    pub(super) fn builtin_stream_op(&mut self, name: &str, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        match name {
            "send" => {
                if args.len() != 2 {
                    return Err("send expects 2 arguments: stream and value".to_string());
                }
                let stream_val = self.compile_expr(&args[0])?;
                let stream_ptr = match stream_val {
                    TypedValue::Stream(p) => p,
                    _ => return Err("send: first argument must be a Stream".to_string()),
                };
                let value = self.compile_expr(&args[1])?;
                let mutex_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 0, "sm").map_err(llvm_err)?;
                let _ = self.builder.build_call(self.module.get_function("pthread_mutex_lock").unwrap(), &[mutex_ptr.into()], "").map_err(llvm_err)?;
                // Push to list (field 3)
                let list_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 3, "sl").map_err(llvm_err)?;
                self.push_to_collector(list_ptr, &value)?;
                // Signal condvar to wake up waiting receivers
                let cond_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 1, "sc").map_err(llvm_err)?;
                let _ = self.builder.build_call(self.module.get_function("pthread_cond_signal").unwrap(), &[cond_ptr.into()], "").map_err(llvm_err)?;
                // Unlock
                let _ = self.builder.build_call(self.module.get_function("pthread_mutex_unlock").unwrap(), &[mutex_ptr.into()], "").map_err(llvm_err)?;
                Ok(TypedValue::Unit)
            }
            "receive" => {
                if args.len() != 1 {
                    return Err("receive expects 1 argument: stream".to_string());
                }
                let stream_val = self.compile_expr(&args[0])?;
                let stream_ptr = match stream_val {
                    TypedValue::Stream(p) => p,
                    _ => return Err("receive: argument must be a Stream".to_string()),
                };
                let zero = self.i64_ty().const_int(0, false);
                let one = self.i64_ty().const_int(1, false);
                let cur_fn = self.builder.get_insert_block().ok_or("no insert block")?.get_parent().ok_or("no current fn")?;
                let result_alloca = self.builder.build_alloca(self.i64_ty(), "sop_result").map_err(llvm_err)?;
                let lock_fn = self.module.get_function("pthread_mutex_lock").unwrap();
                let unlock_fn = self.module.get_function("pthread_mutex_unlock").unwrap();
                let cond_wait_fn = self.module.get_function("pthread_cond_wait").unwrap();
                let mutex_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 0, "rm").map_err(llvm_err)?;
                let cond_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 1, "rc").map_err(llvm_err)?;
                let closed_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 2, "rc_closed").map_err(llvm_err)?;
                let list_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 3, "rl").map_err(llvm_err)?;
                let merge_bb = self.context.append_basic_block(cur_fn, "sop_merge");
                let _ = self.builder.build_call(lock_fn, &[mutex_ptr.into()], "").map_err(llvm_err)?;
                // Wait loop: while list is empty and not closed, cond_wait
                let wait_loop_bb = self.context.append_basic_block(cur_fn, "sop_wait_loop");
                let got_data_bb = self.context.append_basic_block(cur_fn, "sop_got_data");
                let empty_bb = self.context.append_basic_block(cur_fn, "sop_empty");
                let _ = self.builder.build_unconditional_branch(wait_loop_bb);
                self.builder.position_at_end(wait_loop_bb);
                let list_val = self.load_list(list_ptr)?;
                let len = self.builder.build_extract_value(list_val, 1, "len").map_err(llvm_err)?.into_int_value();
                let has_data = self.builder.build_int_compare(IntPredicate::SGT, len, zero, "has_data").map_err(llvm_err)?;
                let _ = self.builder.build_conditional_branch(has_data, got_data_bb, empty_bb);
                // Empty: check closed
                self.builder.position_at_end(empty_bb);
                let closed_val = self.builder.build_load(self.i64_ty(), closed_ptr, "closed_val").map_err(llvm_err)?.into_int_value();
                let is_closed = self.builder.build_int_compare(IntPredicate::NE, closed_val, zero, "is_closed").map_err(llvm_err)?;
                let do_wait_bb = self.context.append_basic_block(cur_fn, "sop_cond_wait");
                let ret_zero_bb = self.context.append_basic_block(cur_fn, "sop_ret_zero");
                let _ = self.builder.build_conditional_branch(is_closed, ret_zero_bb, do_wait_bb);
                self.builder.position_at_end(do_wait_bb);
                let _ = self.builder.build_call(cond_wait_fn, &[cond_ptr.into(), mutex_ptr.into()], "").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(wait_loop_bb);
                // Closed & empty: return 0
                self.builder.position_at_end(ret_zero_bb);
                let _ = self.builder.build_call(unlock_fn, &[mutex_ptr.into()], "").map_err(llvm_err)?;
                self.builder.build_store(result_alloca, zero).map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_bb);
                // Got data: extract, shift, unlock
                self.builder.position_at_end(got_data_bb);
                // Re-load list_val in this block (can't use value from wait_loop across cond_wait)
                let lv2 = self.load_list(list_ptr)?;
                let fat = self.call_rt("atomic_list_get", &[lv2.into(), zero.into()])?;
                let fat = fat.try_as_basic_value().basic().ok_or("receive get failed")?.into_struct_value();
                let tag = self.builder.build_extract_value(fat, 0, "tag").map_err(llvm_err)?.into_int_value();
                let data_ptr = self.builder.build_extract_value(lv2, 0, "data").map_err(llvm_err)?.into_pointer_value();
                let len2 = self.builder.build_extract_value(lv2, 1, "len").map_err(llvm_err)?.into_int_value();
                let cap = self.builder.build_extract_value(lv2, 2, "cap").map_err(llvm_err)?.into_int_value();
                let new_len = self.builder.build_int_sub(len2, one, "new_len").map_err(llvm_err)?;
                let has_more = self.builder.build_int_compare(IntPredicate::SGT, len2, one, "has_more").map_err(llvm_err)?;
                let shift_bb = self.context.append_basic_block(cur_fn, "sop_shift_bb");
                let done_bb = self.context.append_basic_block(cur_fn, "sop_shift_done");
                let _ = self.builder.build_conditional_branch(has_more, shift_bb, done_bb);
                self.builder.position_at_end(shift_bb);
                let mm_fn = self.module.get_function("memmove").unwrap();
                let src_ptr = unsafe { self.builder.build_gep(self.string_type, data_ptr, &[one], "src").map_err(llvm_err) }?;
                let elem_size = self.i64_ty().const_int(16, false);
                let move_bytes = self.builder.build_int_mul(new_len, elem_size, "move_bytes").map_err(llvm_err)?;
                let _ = self.builder.build_call(mm_fn, &[data_ptr.into(), src_ptr.into(), move_bytes.into()], "").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(done_bb);
                self.builder.position_at_end(done_bb);
                let undef = self.list_type.get_undef();
                let r1 = self.builder.build_insert_value(undef, data_ptr, 0, "sr1").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, new_len, 1, "sr2").map_err(llvm_err)?;
                let r3 = self.builder.build_insert_value(r2, cap, 2, "sr3").map_err(llvm_err)?;
                self.builder.build_store(list_ptr, r3).map_err(llvm_err)?;
                let _ = self.builder.build_call(unlock_fn, &[mutex_ptr.into()], "").map_err(llvm_err)?;
                self.builder.build_store(result_alloca, tag).map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_bb);
                // Merge: load result and return
                self.builder.position_at_end(merge_bb);
                let result = self.builder.build_load(self.i64_ty(), result_alloca, "sop_load_result").map_err(llvm_err)?.into_int_value();
                Ok(TypedValue::Int(result))
            }
            "close" => {
                if args.len() != 1 {
                    return Err("close expects 1 argument: stream".to_string());
                }
                let stream_val = self.compile_expr(&args[0])?;
                let stream_ptr = match stream_val {
                    TypedValue::Stream(p) => p,
                    _ => return Err("close: argument must be a Stream".to_string()),
                };
                // Lock mutex, set closed=1, broadcast to wake all waiters, unlock
                let mutex_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 0, "cm").map_err(llvm_err)?;
                let _ = self.builder.build_call(self.module.get_function("pthread_mutex_lock").unwrap(), &[mutex_ptr.into()], "").map_err(llvm_err)?;
                let closed_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 2, "cc").map_err(llvm_err)?;
                self.builder.build_store(closed_ptr, self.i64_ty().const_int(1, false)).map_err(llvm_err)?;
                let cond_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 1, "ccond").map_err(llvm_err)?;
                let _ = self.builder.build_call(self.module.get_function("pthread_cond_broadcast").unwrap(), &[cond_ptr.into()], "").map_err(llvm_err)?;
                let _ = self.builder.build_call(self.module.get_function("pthread_mutex_unlock").unwrap(), &[mutex_ptr.into()], "").map_err(llvm_err)?;
                Ok(TypedValue::Unit)
            }
            _ => Err(format!("Unknown Stream operation: {}", name)),
        }
    }

    /// Task operations: cancel(task), is_done(task), is_cancelled(task), wait(task)
    /// Task struct: {pthread: i64, done: i64, cancelled: i64, result_list: {ptr, i64, i64}}
    pub(super) fn builtin_task_op(&mut self, name: &str, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!("{} expects 1 argument: task", name));
        }
        let task_val = self.compile_expr(&args[0])?;
        let task_ptr = match task_val {
            TypedValue::Task(p) => p,
            _ => return Err(format!("{}: argument must be a Task", name)),
        };
        let tv = self.builder.build_load(self.task_type, task_ptr, "task_val").map_err(llvm_err)?.into_struct_value();
        match name {
            "cancel" => {
                let cancelled_one = self.i64_ty().const_int(1, false);
                let updated = self.builder.build_insert_value(tv, cancelled_one, 2, "t_canc_set").map_err(llvm_err)?;
                self.builder.build_store(task_ptr, updated).map_err(llvm_err)?;
                Ok(TypedValue::Unit)
            }
            "is_done" => {
                let done = self.builder.build_extract_value(tv, 1, "is_done").map_err(llvm_err)?.into_int_value();
                let is_true = self.builder.build_int_compare(IntPredicate::NE, done, self.i64_ty().const_int(0, false), "done_bool").map_err(llvm_err)?;
                Ok(TypedValue::Bool(is_true))
            }
            "is_cancelled" => {
                let cancelled = self.builder.build_extract_value(tv, 2, "is_canc").map_err(llvm_err)?.into_int_value();
                let is_true = self.builder.build_int_compare(IntPredicate::NE, cancelled, self.i64_ty().const_int(0, false), "canc_bool").map_err(llvm_err)?;
                Ok(TypedValue::Bool(is_true))
            }
            "wait" => {
                // pthread_join the task, then extract result
                let pthread_val = self.builder.build_extract_value(tv, 0, "pt").map_err(llvm_err)?.into_int_value();
                let pthread_join_fn = self.module.get_function("pthread_join").unwrap();
                let null_ptr = self.ptr_ty().const_null();
                let _ = self.builder.build_call(pthread_join_fn, &[pthread_val.into(), null_ptr.into()], "").map_err(llvm_err)?;
                // Reload task struct after join (thread updated done, result_list fields)
                let tv2 = self.builder.build_load(self.task_type, task_ptr, "task_val2").map_err(llvm_err)?.into_struct_value();
                // Extract result list from task struct field 4
                let result_list = self.builder.build_extract_value(tv2, 4, "wait_list").map_err(llvm_err)?.into_struct_value();
                let list_alloca = self.builder.build_alloca(self.list_type, "wait_l").map_err(llvm_err)?;
                self.builder.build_store(list_alloca, result_list).map_err(llvm_err)?;
                let list_val = self.load_list(list_alloca)?;
                let zero = self.i64_ty().const_int(0, false);
                let cc = self.call_rt("atomic_list_get", &[list_val.into(), zero.into()])?;
                let fat = cc.try_as_basic_value().basic().ok_or("wait get failed")?.into_struct_value();
                let tag = self.builder.build_extract_value(fat, 0, "tag").map_err(llvm_err)?.into_int_value();
                Ok(TypedValue::Int(tag))
            }
            _ => Err(format!("Unknown Task operation: {}", name)),
        }
    }
}
