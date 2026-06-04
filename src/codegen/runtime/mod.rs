// Submodule: runtime

mod print;
mod rc;
mod string;
mod list;
mod map_set;
mod math_random;
mod file_io;

use inkwell::values::PointerValue;

use super::CodeGen;

impl<'ctx> CodeGen<'ctx> {
    #[allow(unused_variables)]
    pub(super) fn define_runtime(&self) -> Result<(), String> {
        eprintln!("[DEBUG] define_runtime: start");
        let i64 = self.i64_ty();
        let f64 = self.f64_ty();
        let void = self.void_ty();
        let ptr = self.ptr_ty();
        let str_ty = self.string_type;
        let b1 = self.bool_ty();
        let i32 = self.context.i32_type();
        let i8 = self.context.i8_type();
        eprintln!("[DEBUG] define_runtime: types created");

        // Declare external C functions
        eprintln!("[DEBUG] define_runtime: declaring printf");
        let printf_fn = self.module.add_function("printf", i32.fn_type(&[ptr.into()], true), None);
        eprintln!("[DEBUG] define_runtime: declaring malloc");
        let malloc_fn = self.module.add_function("malloc", ptr.fn_type(&[i64.into()], false), None);
        let realloc_fn = self.module.add_function("realloc", ptr.fn_type(&[ptr.into(), i64.into()], false), None);
        let free_fn = self.module.add_function("free", void.fn_type(&[ptr.into()], false), None);
        // Declare RC functions early (defined at end of define_runtime)
        let malloc_rc_fn: inkwell::values::FunctionValue<'ctx> = self.module.add_function("atomic_malloc_rc", ptr.fn_type(&[i64.into()], false), None);
        let memcmp_fn = self.module.add_function("memcmp", i32.fn_type(&[ptr.into(), ptr.into(), i64.into()], false), None);
        let utf8_encode_fn = self.module.add_function("atomic_utf8_encode", i64.fn_type(&[i64.into(), ptr.into()], false), None);
        let utf8_byte_len_fn = self.module.add_function("atomic_utf8_byte_len", i64.fn_type(&[i8.into()], false), None);
        let sprintf_fn = self.module.add_function("sprintf", i32.fn_type(&[ptr.into(), ptr.into()], true), None);
        let strlen_fn = self.module.add_function("strlen", i64.fn_type(&[ptr.into()], false), None);
        let memcpy_fn = self.module.add_function("memcpy", ptr.fn_type(&[ptr.into(), ptr.into(), i64.into()], false), None);
        let _pow_fn = self.module.add_function("pow", f64.fn_type(&[f64.into(), f64.into()], false), None);
        let fopen_fn = self.module.add_function("fopen", ptr.fn_type(&[ptr.into(), ptr.into()], false), None);
        let fclose_fn = self.module.add_function("fclose", i32.fn_type(&[ptr.into()], false), None);
        let fread_fn = self.module.add_function("fread", i64.fn_type(&[ptr.into(), i64.into(), i64.into(), ptr.into()], false), None);
        let fwrite_fn = self.module.add_function("fwrite", i64.fn_type(&[ptr.into(), i64.into(), i64.into(), ptr.into()], false), None);
        let fseek_fn = self.module.add_function("fseek", i32.fn_type(&[ptr.into(), i64.into(), i32.into()], false), None);
        let ftell_fn = self.module.add_function("ftell", i64.fn_type(&[ptr.into()], false), None);
        let _remove_fn = self.module.add_function("remove", self.i32_ty().fn_type(&[ptr.into()], false), None);
        let _strtod_fn = self.module.add_function("strtod", f64.fn_type(&[ptr.into(), ptr.into()], false), None);
        let _strftime_fn = self.module.add_function("strftime", i64.fn_type(&[ptr.into(), i64.into(), ptr.into(), ptr.into()], false), None);
        let _strptime_fn = self.module.add_function("strptime", ptr.fn_type(&[ptr.into(), ptr.into(), ptr.into()], false), None);
        // C math functions
        let _sqrt_fn = self.module.add_function("sqrt", f64.fn_type(&[f64.into()], false), None);
        let _sin_fn = self.module.add_function("sin", f64.fn_type(&[f64.into()], false), None);
        let _cos_fn = self.module.add_function("cos", f64.fn_type(&[f64.into()], false), None);
        let _tan_fn = self.module.add_function("tan", f64.fn_type(&[f64.into()], false), None);
        let _asin_fn = self.module.add_function("asin", f64.fn_type(&[f64.into()], false), None);
        let _acos_fn = self.module.add_function("acos", f64.fn_type(&[f64.into()], false), None);
        let _atan_fn = self.module.add_function("atan", f64.fn_type(&[f64.into()], false), None);
        let _atan2_fn = self.module.add_function("atan2", f64.fn_type(&[f64.into(), f64.into()], false), None);
        let _log_fn = self.module.add_function("log", f64.fn_type(&[f64.into()], false), None);
        let _log2_fn = self.module.add_function("log2", f64.fn_type(&[f64.into()], false), None);
        let _log10_fn = self.module.add_function("log10", f64.fn_type(&[f64.into()], false), None);
        let _exp_fn = self.module.add_function("exp", f64.fn_type(&[f64.into()], false), None);
        let _floor_fn = self.module.add_function("floor", f64.fn_type(&[f64.into()], false), None);
        let _ceil_fn = self.module.add_function("ceil", f64.fn_type(&[f64.into()], false), None);
        let _round_fn = self.module.add_function("round", f64.fn_type(&[f64.into()], false), None);
        let _cbrt_fn = self.module.add_function("cbrt", f64.fn_type(&[f64.into()], false), None);

        // ---- pthread / concurrency external declarations ----
        // pthread_t            = unsigned long (8 bytes on 64-bit)
        // pthread_mutex_t      = 40 bytes on Linux x86_64
        // pthread_cond_t       = 48 bytes on Linux x86_64
        // pthread_attr_t       = opaque (use NULL for defaults)
        // pthread_mutexattr_t  = opaque (use NULL for defaults)
        // pthread_condattr_t   = opaque (use NULL for defaults)

        // pthread_create(pthread_t*, attr*, void*(*)(void*), void*) -> i32
        let pthread_create_fn = self.module.add_function(
            "pthread_create",
            i32.fn_type(&[ptr.into(), ptr.into(), ptr.into(), ptr.into()], false), None);
        // pthread_join(pthread_t, void**) -> i32
        let pthread_join_fn = self.module.add_function(
            "pthread_join",
            i32.fn_type(&[i64.into(), ptr.into()], false), None);
        // pthread_detach(pthread_t) -> i32
        let pthread_detach_fn = self.module.add_function(
            "pthread_detach",
            i32.fn_type(&[i64.into()], false), None);

        // pthread_mutex_init(mutex_t*, attr*) -> i32
        let pthread_mutex_init_fn = self.module.add_function(
            "pthread_mutex_init",
            i32.fn_type(&[ptr.into(), ptr.into()], false), None);
        // pthread_mutex_lock(mutex_t*) -> i32
        let pthread_mutex_lock_fn = self.module.add_function(
            "pthread_mutex_lock",
            i32.fn_type(&[ptr.into()], false), None);
        // pthread_mutex_unlock(mutex_t*) -> i32
        let pthread_mutex_unlock_fn = self.module.add_function(
            "pthread_mutex_unlock",
            i32.fn_type(&[ptr.into()], false), None);
        // pthread_mutex_destroy(mutex_t*) -> i32
        let pthread_mutex_destroy_fn = self.module.add_function(
            "pthread_mutex_destroy",
            i32.fn_type(&[ptr.into()], false), None);

        // pthread_cond_init(cond_t*, attr*) -> i32
        let pthread_cond_init_fn = self.module.add_function(
            "pthread_cond_init",
            i32.fn_type(&[ptr.into(), ptr.into()], false), None);
        // pthread_cond_wait(cond_t*, mutex_t*) -> i32
        let pthread_cond_wait_fn = self.module.add_function(
            "pthread_cond_wait",
            i32.fn_type(&[ptr.into(), ptr.into()], false), None);
        // pthread_cond_timedwait(cond_t*, mutex_t*, timespec*) -> i32
        let pthread_cond_timedwait_fn = self.module.add_function(
            "pthread_cond_timedwait",
            i32.fn_type(&[ptr.into(), ptr.into(), ptr.into()], false), None);
        // pthread_cond_signal(cond_t*) -> i32
        let pthread_cond_signal_fn = self.module.add_function(
            "pthread_cond_signal",
            i32.fn_type(&[ptr.into()], false), None);
        // pthread_cond_broadcast(cond_t*) -> i32
        let pthread_cond_broadcast_fn = self.module.add_function(
            "pthread_cond_broadcast",
            i32.fn_type(&[ptr.into()], false), None);
        // pthread_cond_destroy(cond_t*) -> i32
        let pthread_cond_destroy_fn = self.module.add_function(
            "pthread_cond_destroy",
            i32.fn_type(&[ptr.into()], false), None);

        // usleep(useconds_t) -> i32 (for delay)
        let usleep_fn = self.module.add_function(
            "usleep",
            i32.fn_type(&[i32.into()], false), None);

        // pthread_cancel(pthread_t) -> i32 (for withTimeout cancellation)
        let pthread_cancel_fn = self.module.add_function(
            "pthread_cancel",
            i32.fn_type(&[i64.into()], false), None);

        // clock_gettime(clockid_t, timespec*) -> i32 (for timed operations)
        let clock_gettime_fn = self.module.add_function(
            "clock_gettime",
            i32.fn_type(&[i32.into(), ptr.into()], false), None);

        // memmove(dest, src, n) -> void* — for shifting list elements
        let _memmove_fn = self.module.add_function(
            "memmove",
            ptr.fn_type(&[ptr.into(), ptr.into(), i64.into()], false), None);

        // ---- HTTP / networking runtime functions ----
        // atomic_http_request(method: ptr, url: ptr, headers: ptr, body: ptr, body_len: i64) -> ptr
        let _http_request_fn = self.module.add_function(
            "atomic_http_request",
            ptr.fn_type(&[ptr.into(), ptr.into(), ptr.into(), ptr.into(), i64.into()], false), None);
        // atomic_http_free(ptr)
        let _http_free_fn = self.module.add_function(
            "atomic_http_free",
            void.fn_type(&[ptr.into()], false), None);
        // atomic_test_ping() -> i64
        let _ping_fn = self.module.add_function(
            "atomic_test_ping",
            i64.fn_type(&[], false), None);

        // Helper to create a global string constant
        let make_global_str = |name: &str, content: &[u8]| -> PointerValue<'ctx> {
            let arr_ty = i8.array_type(content.len() as u32);
            let global = self.module.add_global(arr_ty, None, name);
            let arr = self.context.const_string(content, false);
            global.set_initializer(&arr);
            global.as_pointer_value()
        };

        // Create format string globals (all null-terminated)
        let fmt_int_ptr = make_global_str(".fmt_int", b"%ld\0");
        let fmt_float_ptr = make_global_str(".fmt_float", b"%g \0");
        let fmt_str_ptr = make_global_str(".fmt_str", b"%s\0");
        let fmt_nl_ptr = make_global_str(".fmt_nl", b"\n\0");
        let str_true_ptr = make_global_str(".str_true", b"true\0");
        let str_false_ptr = make_global_str(".str_false", b"false\0");
        let fmt_lb_ptr = make_global_str(".fmt_lb", b"[\0");
        let fmt_sep_ptr = make_global_str(".fmt_sep", b", \0");
        let fmt_rb_ptr = make_global_str(".fmt_rb", b"]\0");
        let fmt_task_pre_ptr = make_global_str(".fmt_task_pre", b"Task(done=\0");
        let fmt_task_mid_ptr = make_global_str(".fmt_task_mid", b", cancelled=\0");
        let fmt_task_suf_ptr = make_global_str(".fmt_task_suf", b")\0");
        let fmt_struct_ptr = make_global_str(".fmt_struct", b"<struct>\0");
        let str_none_ptr = make_global_str(".str_none", b"None\0");
        let str_some_pre_ptr = make_global_str(".str_some_pre", b"Some(\0");
        let str_some_suf_ptr = make_global_str(".str_some_suf", b")\0");

        // Save builder position (might be None since no function has been positioned yet)
        let saved_pos = self.builder.get_insert_block();

        self.define_print_functions()?;
        self.define_string_basics()?;
        self.define_math_random()?;

        let list_ty = self.list_type;
        self.define_list_basics()?;
        self.define_file_io()?;

        self.define_map_basics()?;
        self.define_string_advanced()?;

        self.define_list_advanced()?;
        self.define_map_advanced()?;
        self.define_rc_functions()?;

        // Restore builder position
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        eprintln!("[DEBUG] define_runtime: done");
        Ok(())
    }

}
