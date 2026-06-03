use atomic::ast::*;
use inkwell::values::{BasicValue, IntValue, PointerValue};
use inkwell::{IntPredicate, FloatPredicate};

use super::{CodeGen, TypedValue, llvm_err, InnerType};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn builtin_stdlib(&mut self, name: &str, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        // Delegate string/char builtins to compile_string_builtin
        match name {
            "char_at" | "char_code" | "chars" | "code_to_char" | "ends_with"
            | "from_cstring" | "is_alpha" | "join" | "parse_int" | "replace"
            | "split" | "split_lines" | "starts_with" | "string_contains"
            | "string_repeat" | "substring" | "to_char" | "to_cstring"
            | "to_lower" | "to_string" | "to_upper" | "trim" | "trim_end"
            | "trim_start" => {
                return self.compile_string_builtin(name, args);
            }
            _ => {}
        }

        match name {
            "to" => {
                if args.len() != 2 {
                    return Err("to expects 2 arguments".to_string());
                }
                self.compile_tuple(&[(None, args[0].clone()), (None, args[1].clone())])
            }
            "len" => {
                if args.len() != 1 {
                    return Err("len expects 1 argument".to_string());
                }
                let val = self.compile_expr(&args[0])?;
                match val {
                    TypedValue::List(ptr) => {
                        let list = self.load_list(ptr)?;
                        let len = self.builder.build_extract_value(list, 1, "len")
                            .map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(len))
                    }
                    TypedValue::LazyList(ptr) => {
                        let ll_sv = self.builder.build_load(self.lazylist_type, ptr, "len_ll").map_err(llvm_err)?.into_struct_value();
                        let take_count = self.builder.build_extract_value(ll_sv, 3, "len_tc").map_err(llvm_err)?.into_int_value();
                        // If take_count > 0, that's the length. If 0 (no step fn), it's 1.
                        // If -1 (infinite), return -1.
                        let zero = self.i64_ty().const_int(0, false);
                        let one = self.i64_ty().const_int(1, false);
                        let is_zero = self.builder.build_int_compare(IntPredicate::EQ, take_count, zero, "tc_zero").map_err(llvm_err)?;
                        let result_len = self.builder.build_select(is_zero, one, take_count, "ll_len").map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(result_len))
                    }
                    TypedValue::Str(ptr) => {
                        let str_val = self.load_string(ptr)?;
                        let len = self.builder.build_extract_value(str_val, 0, "slen")
                            .map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(len))
                    }
                    TypedValue::Map(ptr) | TypedValue::Set(ptr) => {
                        let m = self.load_list(ptr)?;
                        let len = self.builder.build_extract_value(m, 1, "len")
                            .map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(len))
                    }
                    _ => Err("len: argument must be a list, string, map, set, or lazy list".to_string()),
                }
            }
            "is_empty" => {
                if args.len() != 1 {
                    return Err("is_empty expects 1 argument".to_string());
                }
                let val = self.compile_expr(&args[0])?;
                let len = match val {
                    TypedValue::List(ptr) => {
                        let list = self.load_list(ptr)?;
                        self.builder.build_extract_value(list, 1, "len").map_err(llvm_err)?.into_int_value()
                    }
                    TypedValue::LazyList(_) => {
                        // A LazyList always has at least the head element, so never empty
                        self.i64_ty().const_int(1, false)
                    }
                    TypedValue::Str(ptr) => {
                        let str_val = self.load_string(ptr)?;
                        self.builder.build_extract_value(str_val, 0, "slen").map_err(llvm_err)?.into_int_value()
                    }
                    TypedValue::Map(ptr) | TypedValue::Set(ptr) => {
                        let m = self.load_list(ptr)?;
                        self.builder.build_extract_value(m, 1, "len").map_err(llvm_err)?.into_int_value()
                    }
                    _ => return Err("is_empty: argument must be a list, string, map, set, or lazy list".to_string()),
                };
                let zero = self.i64_ty().const_int(0, false);
                let is_empty = self.builder.build_int_compare(IntPredicate::EQ, len, zero, "is_empty")
                    .map_err(llvm_err)?;
                Ok(TypedValue::Bool(is_empty))
            }
            "append" => {
                if args.len() != 2 {
                    return Err("append expects 2 arguments (list, element)".to_string());
                }
                let list_val = self.compile_expr(&args[0])?;
                let list_ptr = match list_val {
                    TypedValue::List(p) => p,
                    _ => return Err("append: first argument must be a list".to_string()),
                };
                let elem_val = self.compile_expr(&args[1])?;
                let elem_fat = self.to_fat_struct(&elem_val)?;
                let list = self.load_list(list_ptr)?;
                let cc = self.call_rt("atomic_list_push", &[list.into(), elem_fat.into()])?;
                let new_list = cc.try_as_basic_value().basic().ok_or("list_push failed")?;
                let alloca = self.builder.build_alloca(self.list_type, "appended").map_err(llvm_err)?;
                self.builder.build_store(alloca, new_list).map_err(llvm_err)?;
                Ok(TypedValue::List(alloca))
            }
            "concat" => {
                if args.len() != 2 {
                    return Err("concat expects 2 arguments".to_string());
                }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    (TypedValue::Str(p1), TypedValue::Str(p2)) => {
                        let s1 = self.load_string(*p1)?;
                        let s2 = self.load_string(*p2)?;
                        let cc = self.call_rt("atomic_string_concat", &[s1.into(), s2.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("string_concat failed")?;
                        let alloca = self.builder.build_alloca(self.string_type, "concat_str").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::Str(alloca))
                    }
                    _ => Err("concat: arguments must be strings".to_string()),
                }
            }
            "read_line" => {
                if !args.is_empty() {
                    return Err("read_line expects no arguments".to_string());
                }
                let cc = self.call_rt("atomic_read_line", &[])?;
                let result_struct = cc.try_as_basic_value().basic().ok_or("read_line failed")?.into_struct_value();
                // Extract string {i64, ptr} and success flag i1
                let str_len = self.builder.build_extract_value(result_struct, 0, "slen").map_err(llvm_err)?.into_int_value();
                let str_ptr = self.builder.build_extract_value(result_struct, 1, "sptr").map_err(llvm_err)?.into_pointer_value();
                let ok = self.builder.build_extract_value(result_struct, 2, "ok").map_err(llvm_err)?.into_int_value();
                // Build the string fat struct and store in alloca
                let line_undef = self.string_type.get_undef();
                let line1 = self.builder.build_insert_value(line_undef, str_len, 0, "l_len").map_err(llvm_err)?;
                let line_val = self.builder.build_insert_value(line1, str_ptr, 1, "l_ptr").map_err(llvm_err)?;
                let fat_alloca = self.builder.build_alloca(self.string_type, "line").map_err(llvm_err)?;
                self.builder.build_store(fat_alloca, line_val).map_err(llvm_err)?;
                let flag_alloca = self.builder.build_alloca(self.bool_ty(), "line_ok").map_err(llvm_err)?;
                self.builder.build_store(flag_alloca, ok).map_err(llvm_err)?;
                self.build_option_from_fat_struct(fat_alloca, flag_alloca, InnerType::Str)
            }
            "read_file" => {
                if args.len() != 1 {
                    return Err("read_file expects 1 argument (path)".to_string());
                }
                let path = self.compile_expr(&args[0])?;
                match path {
                    TypedValue::Str(pp) => {
                        let pv = self.load_string(pp)?;
                        let cc = self.call_rt("atomic_read_file", &[pv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("read_file failed")?;
                        let alloca = self.builder.build_alloca(self.string_type, "content").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::Str(alloca))
                    }
                    _ => Err("read_file: argument must be a string".to_string()),
                }
            }
            "write_file" => {
                if args.len() != 2 {
                    return Err("write_file expects 2 arguments (path, content)".to_string());
                }
                let path = self.compile_expr(&args[0])?;
                let content = self.compile_expr(&args[1])?;
                match (&path, &content) {
                    (TypedValue::Str(pp), TypedValue::Str(cp)) => {
                        let pv = self.load_string(*pp)?;
                        let cv = self.load_string(*cp)?;
                        let cc = self.call_rt("atomic_write_file", &[pv.into(), cv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("write_file failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("write_file: arguments must be strings".to_string()),
                }
            }
            "append_file" => {
                if args.len() != 2 {
                    return Err("append_file expects 2 arguments (path, content)".to_string());
                }
                let path = self.compile_expr(&args[0])?;
                let content = self.compile_expr(&args[1])?;
                match (&path, &content) {
                    (TypedValue::Str(pp), TypedValue::Str(cp)) => {
                        let pv = self.load_string(*pp)?;
                        let cv = self.load_string(*cp)?;
                        let cc = self.call_rt("atomic_file_append", &[pv.into(), cv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("append_file failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("append_file: arguments must be strings".to_string()),
                }
            }
            "exists" => {
                if args.len() != 1 {
                    return Err("exists expects 1 argument (path)".to_string());
                }
                let path = self.compile_expr(&args[0])?;
                match path {
                    TypedValue::Str(pp) => {
                        let pv = self.load_string(pp)?;
                        let cc = self.call_rt("atomic_file_exists", &[pv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("exists failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("exists: argument must be a string".to_string()),
                }
            }
            "delete_file" => {
                if args.len() != 1 {
                    return Err("delete_file expects 1 argument (path)".to_string());
                }
                let path = self.compile_expr(&args[0])?;
                match path {
                    TypedValue::Str(pp) => {
                        let pv = self.load_string(pp)?;
                        let cc = self.call_rt("atomic_file_delete", &[pv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("delete_file failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("delete_file: argument must be a string".to_string()),
                }
            }
            // ---- Streaming File I/O ----
            "open_file" => {
                if args.len() != 2 {
                    return Err("open_file expects 2 arguments (path, mode)".to_string());
                }
                let path = self.compile_expr(&args[0])?;
                let mode = self.compile_expr(&args[1])?;
                match (&path, &mode) {
                    (TypedValue::Str(pp), TypedValue::Str(mp)) => {
                        let path_s = self.load_string(*pp)?;
                        let mode_s = self.load_string(*mp)?;
                        let cc = self.call_rt("atomic_file_open", &[path_s.into(), mode_s.into()])?;
                        let file_ptr = cc.try_as_basic_value().basic().ok_or("open_file failed")?.into_pointer_value();
                        Ok(TypedValue::FileHandle(file_ptr))
                    }
                    _ => Err("open_file: arguments must be strings (path, mode)".to_string()),
                }
            }
            "close_file" => {
                if args.len() != 1 {
                    return Err("close_file expects 1 argument (file)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                match file {
                    TypedValue::FileHandle(p) => {
                        let cc = self.call_rt("atomic_file_close", &[p.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("close_file failed")?.into_int_value();
                        let ok = self.builder.build_int_compare(
                            IntPredicate::EQ, result, self.i32_ty().const_int(0, false), "ok"
                        ).map_err(llvm_err)?;
                        Ok(TypedValue::Bool(ok))
                    }
                    _ => Err("close_file: argument must be a FileHandle".to_string()),
                }
            }
            "is_eof" => {
                if args.len() != 1 {
                    return Err("is_eof expects 1 argument (file)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                match file {
                    TypedValue::FileHandle(p) => {
                        let cc = self.call_rt("atomic_file_eof", &[p.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("is_eof failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("is_eof: argument must be a FileHandle".to_string()),
                }
            }
            "file_read_line" => {
                if args.len() != 1 {
                    return Err("file_read_line expects 1 argument (file)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                match file {
                    TypedValue::FileHandle(p) => {
                        let cc = self.call_rt("atomic_file_read_line", &[p.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("file_read_line failed")?.into_struct_value();
                        // Build string from len+ptr
                        let len = self.builder.build_extract_value(result, 0, "len").map_err(llvm_err)?.into_int_value();
                        let data = self.builder.build_extract_value(result, 1, "data").map_err(llvm_err)?.into_pointer_value();
                        let str_struct = self.call_rt("atomic_string_create", &[data.into(), len.into()])?;
                        let str_val = str_struct.try_as_basic_value().basic().ok_or("string_create failed")?;
                        let str_alloca = self.builder.build_alloca(self.string_type, "str_tmp").map_err(llvm_err)?;
                        self.builder.build_store(str_alloca, str_val).map_err(llvm_err)?;
                        Ok(TypedValue::Str(str_alloca))
                    }
                    _ => Err("file_read_line: argument must be a FileHandle".to_string()),
                }
            }
            "file_read_bytes" => {
                if args.len() != 2 {
                    return Err("file_read_bytes expects 2 arguments (file, size)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                let size = self.compile_expr(&args[1])?;
                match (&file, &size) {
                    (TypedValue::FileHandle(p), TypedValue::Int(s)) => {
                        let cc = self.call_rt("atomic_file_read_bytes", &[(*p).into(), (*s).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("file_read_bytes failed")?.into_struct_value();
                        let len = self.builder.build_extract_value(result, 0, "len").map_err(llvm_err)?.into_int_value();
                        let data = self.builder.build_extract_value(result, 1, "data").map_err(llvm_err)?.into_pointer_value();
                        let str_struct = self.call_rt("atomic_string_create", &[data.into(), len.into()])?;
                        let str_val = str_struct.try_as_basic_value().basic().ok_or("string_create failed")?;
                        let str_alloca = self.builder.build_alloca(self.string_type, "rb_tmp").map_err(llvm_err)?;
                        self.builder.build_store(str_alloca, str_val).map_err(llvm_err)?;
                        Ok(TypedValue::Str(str_alloca))
                    }
                    _ => Err("file_read_bytes: arguments must be (FileHandle, Int)".to_string()),
                }
            }
            "file_write" => {
                if args.len() != 2 {
                    return Err("file_write expects 2 arguments (file, data)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                let data = self.compile_expr(&args[1])?;
                match (&file, &data) {
                    (TypedValue::FileHandle(fp), TypedValue::Str(dp)) => {
                        let data_s = self.load_string(*dp)?;
                        let data_len = self.builder.build_extract_value(data_s, 0, "dlen").map_err(llvm_err)?.into_int_value();
                        let data_ptr = self.builder.build_extract_value(data_s, 1, "dptr").map_err(llvm_err)?.into_pointer_value();
                        let cc = self.call_rt("atomic_file_write_bytes", &[(*fp).into(), data_ptr.into(), data_len.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("file_write failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("file_write: arguments must be (FileHandle, String)".to_string()),
                }
            }
            "file_write_line" => {
                if args.len() != 2 {
                    return Err("file_write_line expects 2 arguments (file, data)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                let data = self.compile_expr(&args[1])?;
                match (&file, &data) {
                    (TypedValue::FileHandle(fp), TypedValue::Str(dp)) => {
                        let data_s = self.load_string(*dp)?;
                        let data_len = self.builder.build_extract_value(data_s, 0, "dlen").map_err(llvm_err)?.into_int_value();
                        let data_ptr = self.builder.build_extract_value(data_s, 1, "dptr").map_err(llvm_err)?.into_pointer_value();
                        // Write data first
                        let cc1 = self.call_rt("atomic_file_write_bytes", &[(*fp).into(), data_ptr.into(), data_len.into()])?;
                        // Write newline: create a buffer with "\n\0"
                        let malloc_fn = self.module.get_function("malloc").unwrap();
                        let nl_len = self.i64_ty().const_int(1, false);
                        let nl_buf = self.builder.build_call(malloc_fn,
                            &[self.i64_ty().const_int(2, false).into()], "nl_buf"
                        ).map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
                        self.builder.build_store(nl_buf, self.context.i8_type().const_int(10, false)).map_err(llvm_err)?;
                        let _ = self.call_rt("atomic_file_write_bytes", &[(*fp).into(), nl_buf.into(), nl_len.into()])?;
                        let result = cc1.try_as_basic_value().basic().ok_or("file_write_line failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("file_write_line: arguments must be (FileHandle, String)".to_string()),
                }
            }
            "file_flush" => {
                if args.len() != 1 {
                    return Err("file_flush expects 1 argument (file)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                match file {
                    TypedValue::FileHandle(p) => {
                        let cc = self.call_rt("atomic_file_flush", &[p.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("file_flush failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("file_flush: argument must be a FileHandle".to_string()),
                }
            }
            "file_seek" => {
                if args.len() != 3 {
                    return Err("file_seek expects 3 arguments (file, offset, whence)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                let offset = self.compile_expr(&args[1])?;
                let whence = self.compile_expr(&args[2])?;
                match (&file, &offset, &whence) {
                    (TypedValue::FileHandle(p), TypedValue::Int(o), TypedValue::Int(w)) => {
                        let w32 = self.builder.build_int_truncate(*w, self.i32_ty(), "w32").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_file_seek", &[(*p).into(), (*o).into(), w32.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("file_seek failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("file_seek: arguments must be (FileHandle, Int, Int)".to_string()),
                }
            }
            "file_tell" => {
                if args.len() != 1 {
                    return Err("file_tell expects 1 argument (file)".to_string());
                }
                let file = self.compile_expr(&args[0])?;
                match file {
                    TypedValue::FileHandle(p) => {
                        let cc = self.call_rt("atomic_file_tell", &[p.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("file_tell failed")?.into_int_value();
                        Ok(TypedValue::Int(result))
                    }
                    _ => Err("file_tell: argument must be a FileHandle".to_string()),
                }
            }
            "rand_int" => {
                if args.len() != 2 {
                    return Err("rand_int expects 2 arguments (min, max)".to_string());
                }
                let min = self.compile_expr(&args[0])?;
                let max = self.compile_expr(&args[1])?;
                let min_bv = min.to_bv().ok_or("min must be a basic value")?;
                let max_bv = max.to_bv().ok_or("max must be a basic value")?;
                let cc = self.call_rt("atomic_rand_int", &[min_bv.into(), max_bv.into()])?;
                let result = cc.try_as_basic_value().basic().ok_or("rand_int failed")?.into_int_value();
                Ok(TypedValue::Int(result))
            }
            "rand_float" => {
                if !args.is_empty() {
                    return Err("rand_float expects no arguments".to_string());
                }
                let cc = self.call_rt("atomic_rand_float", &[])?;
                let result = cc.try_as_basic_value().basic().ok_or("rand_float failed")?.into_float_value();
                Ok(TypedValue::Float(result))
            }
            "abs" => {
                if args.len() != 1 {
                    return Err("abs expects 1 argument".to_string());
                }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Int(iv) => {
                        let zero = self.i64_ty().const_int(0, false);
                        let neg = self.builder.build_int_neg(iv, "neg").map_err(llvm_err)?;
                        let is_neg = self.builder.build_int_compare(IntPredicate::SLT, iv, zero, "is_neg").map_err(llvm_err)?;
                        let result = self.builder.build_select(is_neg, neg, iv, "abs_result").map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(result))
                    }
                    TypedValue::Float(fv) => {
                        let zero = self.f64_ty().const_float(0.0);
                        let neg = self.builder.build_float_neg(fv, "neg").map_err(llvm_err)?;
                        let is_neg = self.builder.build_float_compare(FloatPredicate::OLT, fv, zero, "is_neg").map_err(llvm_err)?;
                        let result = self.builder.build_select(is_neg, neg, fv, "fabs_result").map_err(llvm_err)?.into_float_value();
                        Ok(TypedValue::Float(result))
                    }
                    _ => Err("abs: argument must be Int or Float".to_string()),
                }
            }
            "min" => {
                if args.len() != 2 {
                    return Err("min expects 2 arguments".to_string());
                }
                let a = self.compile_expr(&args[0])?;
                let b = self.compile_expr(&args[1])?;
                match (&a, &b) {
                    (TypedValue::Int(av), TypedValue::Int(bv)) => {
                        let is_lt = self.builder.build_int_compare(IntPredicate::SLT, *av, *bv, "is_lt").map_err(llvm_err)?;
                        let result = self.builder.build_select(is_lt, *av, *bv, "min_result").map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(result))
                    }
                    (TypedValue::Float(av), TypedValue::Float(bv)) => {
                        let is_lt = self.builder.build_float_compare(FloatPredicate::OLT, *av, *bv, "is_lt").map_err(llvm_err)?;
                        let result = self.builder.build_select(is_lt, *av, *bv, "fmin_result").map_err(llvm_err)?.into_float_value();
                        Ok(TypedValue::Float(result))
                    }
                    _ => Err("min: arguments must be both Int or both Float".to_string()),
                }
            }
            "max" => {
                if args.len() != 2 {
                    return Err("max expects 2 arguments".to_string());
                }
                let a = self.compile_expr(&args[0])?;
                let b = self.compile_expr(&args[1])?;
                match (&a, &b) {
                    (TypedValue::Int(av), TypedValue::Int(bv)) => {
                        let is_gt = self.builder.build_int_compare(IntPredicate::SGT, *av, *bv, "is_gt").map_err(llvm_err)?;
                        let result = self.builder.build_select(is_gt, *av, *bv, "max_result").map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(result))
                    }
                    (TypedValue::Float(av), TypedValue::Float(bv)) => {
                        let is_gt = self.builder.build_float_compare(FloatPredicate::OGT, *av, *bv, "is_gt").map_err(llvm_err)?;
                        let result = self.builder.build_select(is_gt, *av, *bv, "fmax_result").map_err(llvm_err)?.into_float_value();
                        Ok(TypedValue::Float(result))
                    }
                    _ => Err("max: arguments must be both Int or both Float".to_string()),
                }
            }
            "sqrt" => {
                if args.len() != 1 { return Err("sqrt expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let sqrt_fn = self.module.get_function("sqrt").unwrap();
                let r = self.builder.build_call(sqrt_fn, &[fv.into()], "sqrt").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("sqrt failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "cbrt" => {
                if args.len() != 1 { return Err("cbrt expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let cbrt_fn = self.module.get_function("cbrt").unwrap();
                let r = self.builder.build_call(cbrt_fn, &[fv.into()], "cbrt").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("cbrt failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "sin" => {
                if args.len() != 1 { return Err("sin expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("sin").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "sin").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("sin failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "cos" => {
                if args.len() != 1 { return Err("cos expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("cos").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "cos").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("cos failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "tan" => {
                if args.len() != 1 { return Err("tan expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("tan").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "tan").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("tan failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "asin" => {
                if args.len() != 1 { return Err("asin expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("asin").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "asin").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("asin failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "acos" => {
                if args.len() != 1 { return Err("acos expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("acos").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "acos").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("acos failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "atan" => {
                if args.len() != 1 { return Err("atan expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("atan").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "atan").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("atan failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "atan2" => {
                if args.len() != 2 { return Err("atan2 expects 2 arguments".to_string()); }
                let y = self.compile_expr(&args[0])?;
                let x = self.compile_expr(&args[1])?;
                let yv = self.typed_to_float(&y)?;
                let xv = self.typed_to_float(&x)?;
                let f = self.module.get_function("atan2").unwrap();
                let r = self.builder.build_call(f, &[yv.into(), xv.into()], "atan2").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("atan2 failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "log" => {
                if args.len() != 1 { return Err("log expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("log").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "log").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("log failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "log2" => {
                if args.len() != 1 { return Err("log2 expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("log2").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "log2").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("log2 failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "log10" => {
                if args.len() != 1 { return Err("log10 expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("log10").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "log10").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("log10 failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "exp" => {
                if args.len() != 1 { return Err("exp expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("exp").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "exp").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("exp failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "floor" => {
                if args.len() != 1 { return Err("floor expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("floor").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "floor").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("floor failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "ceil" => {
                if args.len() != 1 { return Err("ceil expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("ceil").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "ceil").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("ceil failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "round" => {
                if args.len() != 1 { return Err("round expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let f = self.module.get_function("round").unwrap();
                let r = self.builder.build_call(f, &[fv.into()], "round").map_err(llvm_err)?.try_as_basic_value().basic().ok_or("round failed")?.into_float_value();
                Ok(TypedValue::Float(r))
            }
            "pi" => {
                if !args.is_empty() { return Err("pi expects no arguments".to_string()); }
                let pi_val = self.f64_ty().const_float(std::f64::consts::PI);
                Ok(TypedValue::Float(pi_val))
            }
            "e" => {
                if !args.is_empty() { return Err("e expects no arguments".to_string()); }
                let e_val = self.f64_ty().const_float(std::f64::consts::E);
                Ok(TypedValue::Float(e_val))
            }
            "clamp" => {
                if args.len() != 3 { return Err("clamp expects 3 arguments (value, min, max)".to_string()); }
                let val = self.compile_expr(&args[0])?;
                let min = self.compile_expr(&args[1])?;
                let max = self.compile_expr(&args[2])?;
                match (&val, &min, &max) {
                    (TypedValue::Int(vv), TypedValue::Int(mn), TypedValue::Int(mx)) => {
                        let lt_min = self.builder.build_int_compare(IntPredicate::SLT, *vv, *mn, "lt_min").map_err(llvm_err)?;
                        let r1 = self.builder.build_select(lt_min, *mn, *vv, "clamp1").map_err(llvm_err)?.into_int_value();
                        let gt_max = self.builder.build_int_compare(IntPredicate::SGT, r1, *mx, "gt_max").map_err(llvm_err)?;
                        let r2 = self.builder.build_select(gt_max, *mx, r1, "clamp2").map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(r2))
                    }
                    (TypedValue::Float(vv), TypedValue::Float(mn), TypedValue::Float(mx)) => {
                        let lt_min = self.builder.build_float_compare(FloatPredicate::OLT, *vv, *mn, "lt_min").map_err(llvm_err)?;
                        let r1 = self.builder.build_select(lt_min, *mn, *vv, "clamp1").map_err(llvm_err)?.into_float_value();
                        let gt_max = self.builder.build_float_compare(FloatPredicate::OGT, r1, *mx, "gt_max").map_err(llvm_err)?;
                        let r2 = self.builder.build_select(gt_max, *mx, r1, "clamp2").map_err(llvm_err)?.into_float_value();
                        Ok(TypedValue::Float(r2))
                    }
                    _ => Err("clamp: arguments must be all Int or all Float".to_string()),
                }
            }
            "is_nan" => {
                if args.len() != 1 { return Err("is_nan expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let is_nan = self.builder.build_float_compare(FloatPredicate::UNO, fv, fv, "is_nan").map_err(llvm_err)?;
                Ok(TypedValue::Bool(is_nan))
            }
            "is_infinite" => {
                if args.len() != 1 { return Err("is_infinite expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let fv = self.typed_to_float(&v)?;
                let inf = self.f64_ty().const_float(f64::INFINITY);
                let is_pos_inf = self.builder.build_float_compare(FloatPredicate::OEQ, fv, inf, "is_pos_inf").map_err(llvm_err)?;
                let neg_inf = self.f64_ty().const_float(f64::NEG_INFINITY);
                let is_neg_inf = self.builder.build_float_compare(FloatPredicate::OEQ, fv, neg_inf, "is_neg_inf").map_err(llvm_err)?;
                let is_inf = self.builder.build_or(is_pos_inf, is_neg_inf, "is_inf").map_err(llvm_err)?;
                Ok(TypedValue::Bool(is_inf))
            }
            "panic" => {
                if args.len() != 1 { return Err("panic expects 1 argument (message)".to_string()); }
                let msg = self.compile_expr(&args[0])?;
                match msg {
                    TypedValue::Str(sp) => {
                        let sv = self.load_string(sp)?;
                        let _ = self.call_rt("atomic_print_string", &[sv.into()])?;
                        let _ = self.call_rt("atomic_println", &[])?;
                        // Call exit(1)
                        let exit_fn = self.module.get_function("exit");
                        if exit_fn.is_none() {
                            let _ = self.module.add_function("exit", self.void_ty().fn_type(&[self.i32_ty().into()], false), None);
                        }
                        let exit_fn = self.module.get_function("exit").unwrap();
                        let one = self.i32_ty().const_int(1, false);
                        let _ = self.builder.build_call(exit_fn, &[one.into()], "").map_err(llvm_err)?;
                        self.builder.build_unreachable().map_err(llvm_err)?;
                        Ok(TypedValue::Unit)
                    }
                    _ => Err("panic: argument must be a string".to_string()),
                }
            }
            "assert" => {
                if args.len() != 2 { return Err("assert expects 2 arguments (condition, message)".to_string()); }
                let cond = self.compile_expr(&args[0])?;
                let cond_bool = match cond {
                    TypedValue::Bool(b) => b,
                    _ => return Err("assert: first argument must be a Bool".to_string()),
                };
                let current_fn = self.builder.get_insert_block().unwrap().get_parent().ok_or("block has no parent function")?;
                let assert_ok_bb = self.context.append_basic_block(current_fn, "assert_ok");
                let assert_fail_bb = self.context.append_basic_block(current_fn, "assert_fail");
                let assert_merge_bb = self.context.append_basic_block(current_fn, "assert_merge");
                let _ = self.builder.build_conditional_branch(cond_bool, assert_ok_bb, assert_fail_bb);
                // Fail: print message and exit
                self.builder.position_at_end(assert_fail_bb);
                let msg = self.compile_expr(&args[1])?;
                match msg {
                    TypedValue::Str(sp) => {
                        let sv = self.load_string(sp)?;
                        let prefix = self.compile_string_literal("Assertion failed: ")?;
                        let prefix_sv = match prefix {
                            TypedValue::Str(pp) => self.load_string(pp)?,
                            _ => return Err("internal error".to_string()),
                        };
                        let cc = self.call_rt("atomic_string_concat", &[prefix_sv.into(), sv.into()])?;
                        let full = cc.try_as_basic_value().basic().ok_or("concat failed")?;
                        let _ = self.call_rt("atomic_print_string", &[full.into()])?;
                        let _ = self.call_rt("atomic_println", &[])?;
                        let exit_fn = self.module.get_function("exit");
                        if exit_fn.is_none() {
                            let _ = self.module.add_function("exit", self.void_ty().fn_type(&[self.i32_ty().into()], false), None);
                        }
                        let exit_fn = self.module.get_function("exit").unwrap();
                        let _ = self.builder.build_call(exit_fn, &[self.i32_ty().const_int(1, false).into()], "").map_err(llvm_err)?;
                        self.builder.build_unreachable().map_err(llvm_err)?;
                    }
                    _ => return Err("assert: second argument must be a string".to_string()),
                }
                // Ok: continue
                self.builder.position_at_end(assert_ok_bb);
                let _ = self.builder.build_unconditional_branch(assert_merge_bb);
                self.builder.position_at_end(assert_merge_bb);
                Ok(TypedValue::Unit)
            }
            "head" => {
                if args.len() != 1 { return Err("head expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) | TypedValue::LazyList(lp) => {
                        let list_val = self.load_list(lp)?;
                        let len = self.builder.build_extract_value(list_val, 1, "len").map_err(llvm_err)?.into_int_value();
                        let zero = self.i64_ty().const_int(0, false);
                        let empty = self.builder.build_int_compare(IntPredicate::EQ, len, zero, "empty").map_err(llvm_err)?;
                        let result_ty = self.string_type;
                        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                        let some_bb = self.context.append_basic_block(current_fn, "head_some");
                        let none_bb = self.context.append_basic_block(current_fn, "head_none");
                        let merge_bb = self.context.append_basic_block(current_fn, "head_merge");
                        let _ = self.builder.build_conditional_branch(empty, none_bb, some_bb);
                        // Some block: wrap element in Option::Some
                        self.builder.position_at_end(some_bb);
                        let elem = self.call_rt("atomic_list_get", &[list_val.into(), zero.into()])?;
                        let elem_bv = elem.try_as_basic_value().basic().ok_or("get failed")?;
                        let fat_size = self.i64_ty().const_int(16, false);
                        let fat_heap = self.malloc_rc(fat_size)?;
                        self.builder.build_store(fat_heap, elem_bv).map_err(llvm_err)?;
                        self.rc_inc(fat_heap)?;
                        let some_struct = {
                            let undef = result_ty.get_undef();
                            let r1 = self.builder.build_insert_value(undef, zero, 0, "some_tag").map_err(llvm_err)?;
                            self.builder.build_insert_value(r1, fat_heap, 1, "some_data").map_err(llvm_err)?
                        };
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // None block: build None enum
                        self.builder.position_at_end(none_bb);
                        let none_struct = {
                            let undef = result_ty.get_undef();
                            let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(1, false), 0, "none_tag").map_err(llvm_err)?;
                            self.builder.build_insert_value(r1, self.ptr_ty().const_zero(), 1, "none_data").map_err(llvm_err)?
                        };
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // Merge
                        self.builder.position_at_end(merge_bb);
                        let phi = self.builder.build_phi(result_ty, "head_result").map_err(llvm_err)?;
                        phi.add_incoming(&[(&some_struct, some_bb), (&none_struct, none_bb)]);
                        let alloca = self.builder.build_alloca(result_ty, "head").map_err(llvm_err)?;
                        self.builder.build_store(alloca, phi.as_basic_value()).map_err(llvm_err)?;
                        Ok(TypedValue::Enum(alloca, result_ty, InnerType::Int, true))
                    }
                    _ => Err("head: argument must be a list".to_string()),
                }
            }
            "last" => {
                if args.len() != 1 { return Err("last expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) | TypedValue::LazyList(lp) => {
                        let list_val = self.load_list(lp)?;
                        let len = self.builder.build_extract_value(list_val, 1, "len").map_err(llvm_err)?.into_int_value();
                        let zero = self.i64_ty().const_int(0, false);
                        let empty = self.builder.build_int_compare(IntPredicate::EQ, len, zero, "empty").map_err(llvm_err)?;
                        let last_idx = self.builder.build_int_sub(len, self.i64_ty().const_int(1, false), "last_idx").map_err(llvm_err)?;
                        let result_ty = self.string_type;
                        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                        let some_bb = self.context.append_basic_block(current_fn, "last_some");
                        let none_bb = self.context.append_basic_block(current_fn, "last_none");
                        let merge_bb = self.context.append_basic_block(current_fn, "last_merge");
                        let _ = self.builder.build_conditional_branch(empty, none_bb, some_bb);
                        // Some block: wrap element in Option::Some
                        self.builder.position_at_end(some_bb);
                        let elem = self.call_rt("atomic_list_get", &[list_val.into(), last_idx.into()])?;
                        let elem_bv = elem.try_as_basic_value().basic().ok_or("get failed")?;
                        let fat_size = self.i64_ty().const_int(16, false);
                        let fat_heap = self.malloc_rc(fat_size)?;
                        self.builder.build_store(fat_heap, elem_bv).map_err(llvm_err)?;
                        self.rc_inc(fat_heap)?;
                        let some_struct = {
                            let undef = result_ty.get_undef();
                            let r1 = self.builder.build_insert_value(undef, zero, 0, "some_tag").map_err(llvm_err)?;
                            self.builder.build_insert_value(r1, fat_heap, 1, "some_data").map_err(llvm_err)?
                        };
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // None block: build None enum
                        self.builder.position_at_end(none_bb);
                        let none_struct = {
                            let undef = result_ty.get_undef();
                            let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(1, false), 0, "none_tag").map_err(llvm_err)?;
                            self.builder.build_insert_value(r1, self.ptr_ty().const_zero(), 1, "none_data").map_err(llvm_err)?
                        };
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // Merge
                        self.builder.position_at_end(merge_bb);
                        let phi = self.builder.build_phi(result_ty, "last_result").map_err(llvm_err)?;
                        phi.add_incoming(&[(&some_struct, some_bb), (&none_struct, none_bb)]);
                        let alloca = self.builder.build_alloca(result_ty, "last").map_err(llvm_err)?;
                        self.builder.build_store(alloca, phi.as_basic_value()).map_err(llvm_err)?;
                        Ok(TypedValue::Enum(alloca, result_ty, InnerType::Int, true))
                    }
                    _ => Err("last: argument must be a list".to_string()),
                }
            }
            "get" => {
                if args.len() != 2 { return Err("get expects 2 arguments (list, index)".to_string()); }
                let list_val = self.compile_expr(&args[0])?;
                let idx_val = self.compile_expr(&args[1])?;
                match (&list_val, &idx_val) {
                    (TypedValue::List(lp), TypedValue::Int(iv)) => {
                        let lv = self.load_list(*lp)?;
                        let len = self.builder.build_extract_value(lv, 1, "len").map_err(llvm_err)?.into_int_value();
                        let zero = self.i64_ty().const_int(0, false);
                        let neg = self.builder.build_int_compare(IntPredicate::SLT, *iv, zero, "neg").map_err(llvm_err)?;
                        let ge_len = self.builder.build_int_compare(IntPredicate::SGE, *iv, len, "ge_len").map_err(llvm_err)?;
                        let oob = self.builder.build_or(neg, ge_len, "oob").map_err(llvm_err)?;
                        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                        let some_bb = self.context.append_basic_block(current_fn, "get_some");
                        let none_bb = self.context.append_basic_block(current_fn, "get_none");
                        let merge_bb = self.context.append_basic_block(current_fn, "get_merge");
                        let _ = self.builder.build_conditional_branch(oob, none_bb, some_bb);
                        // Some block: wrap element in Option::Some
                        self.builder.position_at_end(some_bb);
                        let elem = self.call_rt("atomic_list_get", &[lv.into(), (*iv).into()])?;
                        let elem_bv = elem.try_as_basic_value().basic().ok_or("get failed")?;
                        // Allocate heap memory for the fat value and store it
                        let fat_size = self.i64_ty().const_int(16, false);
                        let fat_heap = self.malloc_rc(fat_size)?;
                        self.builder.build_store(fat_heap, elem_bv).map_err(llvm_err)?;
                        self.rc_inc(fat_heap)?;
                        // Build Some enum: {tag: 0, data: fat_heap}
                        let some_struct = {
                            let undef = self.string_type.get_undef();
                            let tag = self.i64_ty().const_int(0, false);
                            let r1 = self.builder.build_insert_value(undef, tag, 0, "some_tag").map_err(llvm_err)?;
                            self.builder.build_insert_value(r1, fat_heap, 1, "some_data").map_err(llvm_err)?
                        };
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // None block: build None enum {tag: 1, data: null}
                        self.builder.position_at_end(none_bb);
                        let none_struct = {
                            let undef = self.string_type.get_undef();
                            let tag = self.i64_ty().const_int(1, false);
                            let r1 = self.builder.build_insert_value(undef, tag, 0, "none_tag").map_err(llvm_err)?;
                            self.builder.build_insert_value(r1, self.ptr_ty().const_zero(), 1, "none_data").map_err(llvm_err)?
                        };
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // Merge
                        self.builder.position_at_end(merge_bb);
                        let phi = self.builder.build_phi(self.string_type, "get_result").map_err(llvm_err)?;
                        phi.add_incoming(&[(&some_struct, some_bb), (&none_struct, none_bb)]);
                        let alloca = self.builder.build_alloca(self.string_type, "get").map_err(llvm_err)?;
                        self.builder.build_store(alloca, phi.as_basic_value()).map_err(llvm_err)?;
                        Ok(TypedValue::Enum(alloca, self.string_type, InnerType::Int, true))
                    }
                    _ => Err("get: first argument must be a list, second an Int".to_string()),
                }
            }
            "reverse" => {
                if args.len() != 1 { return Err("reverse expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let cc = self.call_rt("atomic_list_reverse", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("reverse failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "rev").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("reverse: argument must be a list".to_string()),
                }
            }
            "contains" => {
                if args.len() != 2 { return Err("contains expects 2 arguments (list, element)".to_string()); }
                let list_val = self.compile_expr(&args[0])?;
                let elem_val = self.compile_expr(&args[1])?;
                match (&list_val, &elem_val) {
                    (TypedValue::List(lp), _) => {
                        let lv = self.load_list(*lp)?;
                        let fat = self.to_fat_struct(&elem_val)?;
                        let cc = self.call_rt("atomic_list_contains", &[lv.into(), fat.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("contains failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    (TypedValue::Set(sp), _) => {
                        let lv = self.load_list(*sp)?;
                        let fat = self.to_fat_struct(&elem_val)?;
                        let cc = self.call_rt("atomic_list_contains", &[lv.into(), fat.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("contains failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("contains: first argument must be a list or set".to_string()),
                }
            }
            "contains_key" => {
                if args.len() != 2 { return Err("contains_key expects 2 arguments (map, key)".to_string()); }
                let map_val = self.compile_expr(&args[0])?;
                let key_val = self.compile_expr(&args[1])?;
                match &map_val {
                    TypedValue::Map(mp) => {
                        let lv = self.load_list(*mp)?;
                        let key_fat = self.to_fat_struct(&key_val)?;
                        let cc = self.call_rt("atomic_map_contains", &[lv.into(), key_fat.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("map_contains failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("contains_key: first argument must be a map".to_string()),
                }
            }
            "prepend" => {
                if args.len() != 2 { return Err("prepend expects 2 arguments (element, list)".to_string()); }
                let elem_val = self.compile_expr(&args[0])?;
                let list_val = self.compile_expr(&args[1])?;
                match list_val {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let len_bv = self.builder.build_extract_value(lv, 1, "len").map_err(llvm_err)?.into_int_value();
                        let new_cap = self.builder.build_int_add(len_bv, self.i64_ty().const_int(4, false), "new_cap").map_err(llvm_err)?;
                        let new_list = self.call_rt("atomic_list_create", &[new_cap.into()])?;
                        let new_list_bv = new_list.try_as_basic_value().basic().ok_or("create failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "prepend").map_err(llvm_err)?;
                        self.builder.build_store(alloca, new_list_bv).map_err(llvm_err)?;
                        // Push element first
                        let fat = self.to_fat_struct(&elem_val)?;
                        let lv2 = self.load_list(alloca)?;
                        let pushed1 = self.call_rt("atomic_list_push", &[lv2.into(), fat.into()])?;
                        let pb1 = pushed1.try_as_basic_value().basic().ok_or("push1 failed")?;
                        self.builder.build_store(alloca, pb1).map_err(llvm_err)?;
                        // Then push all original elements
                        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                        let pred_block = self.builder.get_insert_block().ok_or("no insert block")?;
                        let loop_bb = self.context.append_basic_block(current_fn, "prepend_loop");
                        let done_bb = self.context.append_basic_block(current_fn, "prepend_done");
                        let _ = self.builder.build_unconditional_branch(loop_bb);
                        self.builder.position_at_end(loop_bb);
                        let i = self.builder.build_phi(self.i64_ty(), "pp_i").map_err(llvm_err)?;
                        let lv_orig = self.load_list(lp)?;
                        let lv_cur = self.load_list(alloca)?;
                        let elem = self.call_rt("atomic_list_get", &[lv_orig.into(), i.as_basic_value().into_int_value().into()])?;
                        let elem_bv = elem.try_as_basic_value().basic().ok_or("get failed")?;
                        let pushed = self.call_rt("atomic_list_push", &[lv_cur.into(), elem_bv.into()])?;
                        let pb = pushed.try_as_basic_value().basic().ok_or("push2 failed")?;
                        self.builder.build_store(alloca, pb).map_err(llvm_err)?;
                        let ni = self.builder.build_int_add(i.as_basic_value().into_int_value(), self.i64_ty().const_int(1, false), "pp_ni").map_err(llvm_err)?;
                        let done_cond = self.builder.build_int_compare(IntPredicate::SGE, ni, len_bv, "pp_done").map_err(llvm_err)?;
                        let loop_block = self.builder.get_insert_block().unwrap();
                        i.add_incoming(&[(&self.i64_ty().const_int(0, false), pred_block), (&ni, loop_block)]);
                        let _ = self.builder.build_conditional_branch(done_cond, done_bb, loop_bb);
                        self.builder.position_at_end(done_bb);
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("prepend: second argument must be a list".to_string()),
                }
            }
            "take" => {
                if args.len() != 2 { return Err("take expects 2 arguments (list, n)".to_string()); }
                let list_val = self.compile_expr(&args[0])?;
                let n_val = self.compile_expr(&args[1])?;
                match (&list_val, &n_val) {
                    (TypedValue::List(lp), TypedValue::Int(nv)) => {
                        let lv = self.load_list(*lp)?;
                        let cc = self.call_rt("atomic_list_take", &[lv.into(), (*nv).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("take failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "take").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("take: first argument must be a list, second an Int".to_string()),
                }
            }
            "drop" => {
                if args.len() != 2 { return Err("drop expects 2 arguments (list, n)".to_string()); }
                let list_val = self.compile_expr(&args[0])?;
                let n_val = self.compile_expr(&args[1])?;
                match (&list_val, &n_val) {
                    (TypedValue::List(lp), TypedValue::Int(nv)) => {
                        let lv = self.load_list(*lp)?;
                        let cc = self.call_rt("atomic_list_drop", &[lv.into(), (*nv).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("drop failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "drop").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("drop: first argument must be a list, second an Int".to_string()),
                }
            }
            "range" => {
                if args.len() != 2 { return Err("range expects 2 arguments (start, end)".to_string()); }
                let start = self.compile_expr(&args[0])?;
                let end = self.compile_expr(&args[1])?;
                match (&start, &end) {
                    (TypedValue::Int(sv), TypedValue::Int(ev)) => {
                        let cc = self.call_rt("atomic_list_range", &[(*sv).into(), (*ev).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("range failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "range").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("range: arguments must be Int".to_string()),
                }
            }
            "repeat" => {
                if args.len() != 2 { return Err("repeat expects 2 arguments (value, count)".to_string()); }
                let val = self.compile_expr(&args[0])?;
                let count = self.compile_expr(&args[1])?;
                match count {
                    TypedValue::Int(cv) => {
                        let cap = self.i64_ty().const_int(4, false);
                        let new_list = self.call_rt("atomic_list_create", &[cap.into()])?;
                        let new_list_bv = new_list.try_as_basic_value().basic().ok_or("create failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "repeat").map_err(llvm_err)?;
                        self.builder.build_store(alloca, new_list_bv).map_err(llvm_err)?;
                        let fat = self.to_fat_struct(&val)?;
                        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                        let pred_block = self.builder.get_insert_block().ok_or("no insert block")?;
                        let loop_bb = self.context.append_basic_block(current_fn, "repeat_loop");
                        let done_bb = self.context.append_basic_block(current_fn, "repeat_done");
                        let _ = self.builder.build_unconditional_branch(loop_bb);
                        self.builder.position_at_end(loop_bb);
                        let i = self.builder.build_phi(self.i64_ty(), "rep_i").map_err(llvm_err)?;
                        let lv = self.load_list(alloca)?;
                        let pushed = self.call_rt("atomic_list_push", &[lv.into(), fat.into()])?;
                        let pb = pushed.try_as_basic_value().basic().ok_or("push failed")?;
                        self.builder.build_store(alloca, pb).map_err(llvm_err)?;
                        let ni = self.builder.build_int_add(i.as_basic_value().into_int_value(), self.i64_ty().const_int(1, false), "rep_ni").map_err(llvm_err)?;
                        let done_cond = self.builder.build_int_compare(IntPredicate::SGE, ni, cv, "rep_done").map_err(llvm_err)?;
                        let loop_block = self.builder.get_insert_block().unwrap();
                        i.add_incoming(&[(&self.i64_ty().const_int(0, false), pred_block), (&ni, loop_block)]);
                        let _ = self.builder.build_conditional_branch(done_cond, done_bb, loop_bb);
                        self.builder.position_at_end(done_bb);
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("repeat: second argument must be Int".to_string()),
                }
            }
            "tail" => {
                if args.len() != 1 { return Err("tail expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let len = self.builder.build_extract_value(lv, 1, "len").map_err(llvm_err)?.into_int_value();
                        let is_empty = self.builder.build_int_compare(IntPredicate::EQ, len, self.i64_ty().const_int(0, false), "empty").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_list_tail", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("tail failed")?.into_struct_value();
                        self.build_option_list(result, is_empty)
                    }
                    _ => Err("tail: argument must be a list".to_string()),
                }
            }
            "zip" => {
                if args.len() != 2 { return Err("zip expects 2 arguments (list1, list2)".to_string()); }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    (TypedValue::List(lp1), TypedValue::List(lp2)) => {
                        let lv1 = self.load_list(*lp1)?;
                        let lv2 = self.load_list(*lp2)?;
                        let cc = self.call_rt("atomic_list_zip", &[lv1.into(), lv2.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("zip failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "zip").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("zip: arguments must be lists".to_string()),
                }
            }
            "index_of" => {
                if args.len() != 2 { return Err("index_of expects 2 arguments".to_string()); }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    // index_of(element, list) -> Option<Int>
                    (elem, TypedValue::List(lp)) => {
                        let lv = self.load_list(*lp)?;
                        let fat = self.to_fat_struct(elem)?;
                        let cc = self.call_rt("atomic_list_index_of", &[lv.into(), fat.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("index_of failed")?.into_int_value();
                        let found = self.builder.build_int_compare(IntPredicate::SGE, result, self.i64_ty().const_int(0, false), "found").map_err(llvm_err)?;
                        self.build_option_int(result, found)
                    }
                    // index_of(substring, string) -> Option<Int>
                    (TypedValue::Str(sp1), TypedValue::Str(sp2)) => {
                        let sv1 = self.load_string(*sp1)?;
                        let sv2 = self.load_string(*sp2)?;
                        // runtime expects (haystack, needle), so swap: sv2 is haystack, sv1 is needle
                        let cc = self.call_rt("atomic_string_index_of", &[sv2.into(), sv1.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("index_of failed")?.into_int_value();
                        let neg_one = self.i64_ty().const_int((-1i64) as u64, true);
                        let found = self.builder.build_int_compare(IntPredicate::NE, result, neg_one, "found").map_err(llvm_err)?;
                        self.build_option_int(result, found)
                    }
                    _ => Err("index_of: first arg must be (element, list) or (substring, string)".to_string()),
                }
            }
            "init" => {
                if args.len() != 1 { return Err("init expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let len = self.builder.build_extract_value(lv, 1, "len").map_err(llvm_err)?.into_int_value();
                        let is_empty = self.builder.build_int_compare(IntPredicate::EQ, len, self.i64_ty().const_int(0, false), "empty").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_list_init", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("init failed")?.into_struct_value();
                        self.build_option_list(result, is_empty)
                    }
                    _ => Err("init: argument must be a list".to_string()),
                }
            }
            "set_to_list" => {
                if args.len() != 1 { return Err("set_to_list expects 1 argument (set)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Set(p) => Ok(TypedValue::List(p)),
                    _ => Err("set_to_list: argument must be a set".to_string()),
                }
            }
            "set_from_list" => {
                if args.len() != 1 { return Err("set_from_list expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(p) => Ok(TypedValue::Set(p)),
                    _ => Err("set_from_list: argument must be a list".to_string()),
                }
            }
            "from_list" => {
                if args.len() != 1 { return Err("from_list expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(p) => Ok(TypedValue::Set(p)),
                    _ => Err("from_list: argument must be a list".to_string()),
                }
            }
            "today" => {
                if !args.is_empty() { return Err("today expects no arguments".to_string()); }
                // Call C time() and localtime_r() to get real current date
                self.emit_today_now(false)
            }
            "now" => {
                if !args.is_empty() { return Err("now expects no arguments".to_string()); }
                self.emit_today_now(true)
            }
            // DateTime/Date field accessors
            "year" | "month" | "day" | "hour" | "minute" | "second" => {
                if args.len() != 1 { return Err(format!("{} expects 1 argument", name)); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Struct(p, st) => {
                        let field_idx = match name {
                            "year" => 0, "month" => 1, "day" => 2,
                            "hour" => 3, "minute" => 4, "second" => 5, _ => return Err("bad field".to_string()),
                        };
                        let fptr = self.builder.build_struct_gep(st, p, field_idx, "fptr").map_err(llvm_err)?;
                        let val = self.builder.build_load(self.i64_ty(), fptr, "val").map_err(llvm_err)?.into_int_value();
                        Ok(TypedValue::Int(val))
                    }
                    _ => Err(format!("{}: argument must be a Date or DateTime struct", name)),
                }
            }
            "add_days" => {
                if args.len() != 2 { return Err("add_days expects 2 arguments (date, days)".to_string()); }
                let d = self.compile_expr(&args[0])?;
                let days = self.compile_expr(&args[1])?;
                let days_bv = days.to_bv().ok_or("days must be Int")?;
                match d {
                    TypedValue::Struct(p, st) => {
                        // Create a new Date struct with added days
                        let alloca = self.builder.build_alloca(st, "new_date").map_err(llvm_err)?;
                        for i in 0..3u32 {
                            let fptr = self.builder.build_struct_gep(st, p, i, "fptr").map_err(llvm_err)?;
                            let fval = self.builder.build_load(self.i64_ty(), fptr, "fval").map_err(llvm_err)?.into_int_value();
                            let new_val = if i == 2 {
                                self.builder.build_int_add(fval, days_bv.into_int_value(), "new_day").map_err(llvm_err)?.into()
                            } else {
                                fval
                            };
                            let dfptr = self.builder.build_struct_gep(st, alloca, i, "dfptr").map_err(llvm_err)?;
                            self.builder.build_store(dfptr, new_val).map_err(llvm_err)?;
                        }
                        Ok(TypedValue::Struct(alloca, st))
                    }
                    _ => Err("add_days: first argument must be a Date struct".to_string()),
                }
            }
            "add_hours" => {
                if args.len() != 2 { return Err("add_hours expects 2 arguments (datetime, hours)".to_string()); }
                let d = self.compile_expr(&args[0])?;
                let hours = self.compile_expr(&args[1])?;
                let hours_bv = hours.to_bv().ok_or("hours must be Int")?;
                match d {
                    TypedValue::Struct(p, st) => {
                        let alloca = self.builder.build_alloca(st, "new_dt").map_err(llvm_err)?;
                        for i in 0..6u32 {
                            let fptr = self.builder.build_struct_gep(st, p, i, "fptr").map_err(llvm_err)?;
                            let fval = self.builder.build_load(self.i64_ty(), fptr, "fval").map_err(llvm_err)?.into_int_value();
                            let new_val = if i == 3 {
                                self.builder.build_int_add(fval, hours_bv.into_int_value(), "new_hour").map_err(llvm_err)?.into()
                            } else {
                                fval
                            };
                            let dfptr = self.builder.build_struct_gep(st, alloca, i, "dfptr").map_err(llvm_err)?;
                            self.builder.build_store(dfptr, new_val).map_err(llvm_err)?;
                        }
                        Ok(TypedValue::Struct(alloca, st))
                    }
                    _ => Err("add_hours: first argument must be a DateTime struct".to_string()),
                }
            }
            "rand_choice" => {
                if args.len() != 1 { return Err("rand_choice expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let len = self.builder.build_extract_value(lv, 1, "len").map_err(llvm_err)?.into_int_value();
                        let empty = self.builder.build_int_compare(IntPredicate::EQ, len, self.i64_ty().const_int(0, false), "empty").map_err(llvm_err)?;
                        let current_fn = self.builder.get_insert_block().unwrap().get_parent().ok_or("block has no parent function")?;
                        let has_elem = self.context.append_basic_block(current_fn, "has_elem");
                        let no_elem = self.context.append_basic_block(current_fn, "no_elem");
                        let merge = self.context.append_basic_block(current_fn, "merge");
                        let _ = self.builder.build_conditional_branch(empty, no_elem, has_elem);
                        // No element: return None (tag=0)
                        self.builder.position_at_end(no_elem);
                        let none_fat = self.string_type.get_undef();
                        let none1 = self.builder.build_insert_value(none_fat, self.i64_ty().const_int(0, false), 0, "none_tag").map_err(llvm_err)?;
                        let none2 = self.builder.build_insert_value(none1, self.ptr_ty().const_zero(), 1, "none_data").map_err(llvm_err)?;
                        let _ = self.builder.build_unconditional_branch(merge);
                        let none_block = self.builder.get_insert_block().unwrap();
                        // Has element: pick random index
                        self.builder.position_at_end(has_elem);
                        let idx = self.builder.build_int_sub(len, self.i64_ty().const_int(1, false), "max_idx").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_rand_int", &[self.i64_ty().const_int(0, false).into(), idx.into()])?;
                        let ri = cc.try_as_basic_value().basic().unwrap().into_int_value();
                        let data = self.builder.build_extract_value(lv, 0, "data").map_err(llvm_err)?.into_pointer_value();
                        let ep = unsafe { self.builder.build_gep(self.string_type, data, &[ri], "ep").map_err(llvm_err) }?;
                        let elem = self.builder.build_load(self.string_type, ep, "elem").map_err(llvm_err)?.into_struct_value();
                        // Wrap in Some: tag=1, data=ptr to elem copy
                        let malloc = self.module.get_function("malloc").unwrap();
                        let some_ptr = self.builder.build_call(malloc, &[self.i64_ty().const_int(16, false).into()], "some").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
                        self.builder.build_store(some_ptr, elem).map_err(llvm_err)?;
                        let some_fat = self.string_type.get_undef();
                        let some1 = self.builder.build_insert_value(some_fat, self.i64_ty().const_int(1, false), 0, "some_tag").map_err(llvm_err)?;
                        let some2 = self.builder.build_insert_value(some1, some_ptr, 1, "some_data").map_err(llvm_err)?;
                        let _ = self.builder.build_unconditional_branch(merge);
                        let some_block = self.builder.get_insert_block().unwrap();
                        // Merge
                        self.builder.position_at_end(merge);
                        let phi = self.builder.build_phi(self.string_type, "choice").map_err(llvm_err)?;
                        phi.add_incoming(&[(&none2.as_basic_value_enum(), none_block), (&some2.as_basic_value_enum(), some_block)]);
                        // Return as fat struct (Tag=EnumKind(3), data=ptr to fat value)
                        let opt_alloca = self.builder.build_alloca(self.string_type, "opt").map_err(llvm_err)?;
                        self.builder.build_store(opt_alloca, phi.as_basic_value()).map_err(llvm_err)?;
                        Ok(TypedValue::List(opt_alloca)) // Reuse List type for the result
                    }
                    _ => Err("rand_choice: argument must be a list".to_string()),
                }
            }
            "to_int" => {
                if args.len() != 1 { return Err("to_int expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Int(iv) => self.build_option_int(iv, self.bool_ty().const_int(1, false)),
                    TypedValue::Float(fv) => {
                        let i = self.builder.build_float_to_signed_int(fv, self.i64_ty(), "ftoi").map_err(llvm_err)?;
                        self.build_option_int(i, self.bool_ty().const_int(1, false))
                    }
                    TypedValue::Str(sp) => {
                        let sv = self.load_string(sp)?;
                        let cc = self.call_rt("atomic_parse_int", &[sv.into()])?;
                        let result_struct = cc.try_as_basic_value().basic().ok_or("parse_int failed")?.into_struct_value();
                        let val = self.builder.build_extract_value(result_struct, 0, "val").map_err(llvm_err)?.into_int_value();
                        let ok = self.builder.build_extract_value(result_struct, 1, "ok").map_err(llvm_err)?.into_int_value();
                        self.build_option_int(val, ok)
                    }
                    _ => Err("to_int: cannot convert to Int".to_string()),
                }
            }
            "to_float" => {
                if args.len() != 1 { return Err("to_float expects 1 argument".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let always_true = self.bool_ty().const_int(1, false);
                match v {
                    TypedValue::Float(fv) => self.build_option_float(fv, always_true),
                    TypedValue::Int(iv) => {
                        let f = self.builder.build_signed_int_to_float(iv, self.f64_ty(), "itof").map_err(llvm_err)?;
                        self.build_option_float(f, always_true)
                    }
                    TypedValue::Str(sp) => {
                        let sv = self.load_string(sp)?;
                        let len = self.builder.build_extract_value(sv, 0, "len").map_err(llvm_err)?.into_int_value();
                        let has_chars = self.builder.build_int_compare(IntPredicate::UGT, len, self.i64_ty().const_int(0, false), "has_chars").map_err(llvm_err)?;
                        // Check first char is digit, '-', '+', or '.'
                        let data_ptr = self.builder.build_extract_value(sv, 1, "dptr").map_err(llvm_err)?.into_pointer_value();
                        let first_char = self.builder.build_load(self.context.i8_type(), data_ptr, "first_char").map_err(llvm_err)?.into_int_value();
                        let is_digit = self.builder.build_int_compare(IntPredicate::UGE, first_char, self.context.i8_type().const_int(b'0' as u64, false), "isd").map_err(llvm_err)?;
                        let le9 = self.builder.build_int_compare(IntPredicate::ULE, first_char, self.context.i8_type().const_int(b'9' as u64, false), "le9").map_err(llvm_err)?;
                        let is_d = self.builder.build_and(is_digit, le9, "is_digit").map_err(llvm_err)?;
                        let is_minus = self.builder.build_int_compare(IntPredicate::EQ, first_char, self.context.i8_type().const_int(b'-' as u64, false), "is_minus").map_err(llvm_err)?;
                        let is_plus = self.builder.build_int_compare(IntPredicate::EQ, first_char, self.context.i8_type().const_int(b'+' as u64, false), "is_plus").map_err(llvm_err)?;
                        let is_dot = self.builder.build_int_compare(IntPredicate::EQ, first_char, self.context.i8_type().const_int(b'.' as u64, false), "is_dot").map_err(llvm_err)?;
                        let is_sign = self.builder.build_or(is_minus, is_plus, "is_sign").map_err(llvm_err)?;
                        let is_num_start = self.builder.build_or(is_d, is_sign, "is_num1").map_err(llvm_err)?;
                        let is_valid = self.builder.build_or(is_num_start, is_dot, "is_valid").map_err(llvm_err)?;
                        let ok = self.builder.build_and(has_chars, is_valid, "ok").map_err(llvm_err)?;
                        let strtod_fn = self.module.get_function("strtod").unwrap();
                        let null_ptr = self.ptr_ty().const_zero();
                        let result = self.builder.build_call(strtod_fn, &[data_ptr.into(), null_ptr.into()], "fval").map_err(llvm_err)?
                            .try_as_basic_value().basic().ok_or("strtod failed")?.into_float_value();
                        self.build_option_float(result, ok)
                    }
                    _ => Err("to_float: cannot convert to Float".to_string()),
                }
            }
            "with_index" => {
                if args.len() != 1 { return Err("with_index expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let cc = self.call_rt("atomic_list_with_index", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("with_index failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "wi").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("with_index: argument must be a list".to_string()),
                }
            }
            "unique" => {
                if args.len() != 1 { return Err("unique expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let cc = self.call_rt("atomic_list_unique", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("unique failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "unique").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("unique: argument must be a list".to_string()),
                }
            }
            "slice" => {
                if args.len() != 3 { return Err("slice expects 3 arguments (collection, start, end)".to_string()); }
                let coll_v = self.compile_expr(&args[0])?;
                let start_v = self.compile_expr(&args[1])?;
                let end_v = self.compile_expr(&args[2])?;
                match (&coll_v, &start_v, &end_v) {
                    // slice(List<T>, Int, Int) -> List<T>  with [start, end) semantics
                    (TypedValue::List(lp), TypedValue::Int(sv), TypedValue::Int(ev)) => {
                        let lv = self.load_list(*lp)?;
                        let cc = self.call_rt("atomic_list_slice", &[lv.into(), (*sv).into(), (*ev).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("slice failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "slice").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    // slice(String, Int, Int) -> String  with [start, end) semantics
                    (TypedValue::Str(sp), TypedValue::Int(sv), TypedValue::Int(ev)) => {
                        let str_val = self.load_string(*sp)?;
                        let len = self.builder.build_int_sub(*ev, *sv, "slice_len").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_string_substring",
                            &[str_val.into(), (*sv).into(), len.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("slice failed")?;
                        let alloca = self.builder.build_alloca(self.string_type, "slice_str").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::Str(alloca))
                    }
                    _ => Err("slice: first argument must be a list or string, second and third Int".to_string()),
                }
            }
            "flatten" => {
                if args.len() != 1 { return Err("flatten expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let cc = self.call_rt("atomic_list_flatten", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("flatten failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "flatten").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("flatten: argument must be a list".to_string()),
                }
            }
            "split_at" => {
                if args.len() != 2 { return Err("split_at expects 2 arguments (list, index)".to_string()); }
                let list_v = self.compile_expr(&args[0])?;
                let idx_v = self.compile_expr(&args[1])?;
                match (&list_v, &idx_v) {
                    (TypedValue::List(lp), TypedValue::Int(iv)) => {
                        let lv = self.load_list(*lp)?;
                        let cc = self.call_rt("atomic_list_split_at", &[lv.into(), (*iv).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("split_at failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "split_at").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("split_at: first argument must be a list, second Int".to_string()),
                }
            }
            "chunks" => {
                if args.len() != 2 { return Err("chunks expects 2 arguments (list, size)".to_string()); }
                let list_v = self.compile_expr(&args[0])?;
                let size_v = self.compile_expr(&args[1])?;
                match (&list_v, &size_v) {
                    (TypedValue::List(lp), TypedValue::Int(sv)) => {
                        let lv = self.load_list(*lp)?;
                        let cc = self.call_rt("atomic_list_chunks", &[lv.into(), (*sv).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("chunks failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "chunks").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("chunks: first argument must be a list, second Int".to_string()),
                }
            }
            "windows" => {
                if args.len() != 2 { return Err("windows expects 2 arguments (list, size)".to_string()); }
                let list_v = self.compile_expr(&args[0])?;
                let size_v = self.compile_expr(&args[1])?;
                match (&list_v, &size_v) {
                    (TypedValue::List(lp), TypedValue::Int(sv)) => {
                        let lv = self.load_list(*lp)?;
                        let cc = self.call_rt("atomic_list_windows", &[lv.into(), (*sv).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("windows failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "windows").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("windows: first argument must be a list, second Int".to_string()),
                }
            }
            "pow" => {
                if args.len() != 2 { return Err("pow expects 2 arguments".to_string()); }
                let base = self.compile_expr(&args[0])?;
                let exp = self.compile_expr(&args[1])?;
                match (&base, &exp) {
                    (TypedValue::Float(bv), TypedValue::Float(ev)) => {
                        let cc = self.call_rt("atomic_pow", &[(*bv).into(), (*ev).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("pow failed")?.into_float_value();
                        Ok(TypedValue::Float(result))
                    }
                    (TypedValue::Int(bv), TypedValue::Int(ev)) => {
                        let bf = self.builder.build_signed_int_to_float(*bv, self.f64_ty(), "bf").map_err(llvm_err)?;
                        let ef = self.builder.build_signed_int_to_float(*ev, self.f64_ty(), "ef").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_pow", &[bf.into(), ef.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("pow failed")?.into_float_value();
                        Ok(TypedValue::Float(result))
                    }
                    // Mixed Int/Float → promote Int to Float
                    (TypedValue::Int(bv), TypedValue::Float(ev)) => {
                        let bf = self.builder.build_signed_int_to_float(*bv, self.f64_ty(), "bf").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_pow", &[bf.into(), (*ev).into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("pow failed")?.into_float_value();
                        Ok(TypedValue::Float(result))
                    }
                    (TypedValue::Float(bv), TypedValue::Int(ev)) => {
                        let ef = self.builder.build_signed_int_to_float(*ev, self.f64_ty(), "ef").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_pow", &[(*bv).into(), ef.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("pow failed")?.into_float_value();
                        Ok(TypedValue::Float(result))
                    }
                    _ => Err("pow: arguments must be numeric".to_string()),
                }
            }
            "map_keys" => {
                if args.len() != 1 { return Err("map_keys expects 1 argument (map)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Map(mp) => {
                        let mv = self.load_list(mp)?;
                        let cc = self.call_rt("atomic_map_keys", &[mv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("map_keys failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "keys").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("map_keys: argument must be a map".to_string()),
                }
            }
            "map_values" => {
                if args.len() != 1 { return Err("map_values expects 1 argument (map)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Map(mp) => {
                        let mv = self.load_list(mp)?;
                        let cc = self.call_rt("atomic_map_values", &[mv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("map_values failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "values").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("map_values: argument must be a map".to_string()),
                }
            }
            "map_entries" => {
                if args.len() != 1 { return Err("map_entries expects 1 argument (map)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Map(mp) => {
                        let mv = self.load_list(mp)?;
                        let cc = self.call_rt("atomic_map_entries", &[mv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("map_entries failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "entries").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("map_entries: argument must be a map".to_string()),
                }
            }
            "map_union" => {
                if args.len() != 2 { return Err("map_union expects 2 arguments (map1, map2)".to_string()); }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    (TypedValue::Map(mp1), TypedValue::Map(mp2)) => {
                        let mv1 = self.load_list(*mp1)?;
                        let mv2 = self.load_list(*mp2)?;
                        let cc = self.call_rt("atomic_map_union", &[mv1.into(), mv2.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("map_union failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "map_union").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::Map(alloca))
                    }
                    _ => Err("map_union: arguments must be maps".to_string()),
                }
            }
            "set_union" => {
                if args.len() != 2 { return Err("set_union expects 2 arguments (set1, set2)".to_string()); }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    (TypedValue::Set(sp1), TypedValue::Set(sp2)) => {
                        let sv1 = self.load_list(*sp1)?;
                        let sv2 = self.load_list(*sp2)?;
                        let cc = self.call_rt("atomic_set_union", &[sv1.into(), sv2.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("set_union failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "union").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::Set(alloca))
                    }
                    _ => Err("set_union: arguments must be sets".to_string()),
                }
            }
            "set_intersection" => {
                if args.len() != 2 { return Err("set_intersection expects 2 arguments (set1, set2)".to_string()); }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    (TypedValue::Set(sp1), TypedValue::Set(sp2)) => {
                        let sv1 = self.load_list(*sp1)?;
                        let sv2 = self.load_list(*sp2)?;
                        let cc = self.call_rt("atomic_set_intersection", &[sv1.into(), sv2.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("set_intersection failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "intersection").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::Set(alloca))
                    }
                    _ => Err("set_intersection: arguments must be sets".to_string()),
                }
            }
            "set_difference" => {
                if args.len() != 2 { return Err("set_difference expects 2 arguments (set1, set2)".to_string()); }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    (TypedValue::Set(sp1), TypedValue::Set(sp2)) => {
                        let sv1 = self.load_list(*sp1)?;
                        let sv2 = self.load_list(*sp2)?;
                        let cc = self.call_rt("atomic_set_difference", &[sv1.into(), sv2.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("set_difference failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "difference").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::Set(alloca))
                    }
                    _ => Err("set_difference: arguments must be sets".to_string()),
                }
            }
            "set_is_subset" => {
                if args.len() != 2 { return Err("set_is_subset expects 2 arguments (set1, set2)".to_string()); }
                let v1 = self.compile_expr(&args[0])?;
                let v2 = self.compile_expr(&args[1])?;
                match (&v1, &v2) {
                    (TypedValue::Set(sp1), TypedValue::Set(sp2)) => {
                        let sv1 = self.load_list(*sp1)?;
                        let sv2 = self.load_list(*sp2)?;
                        let cc = self.call_rt("atomic_set_is_subset", &[sv1.into(), sv2.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("set_is_subset failed")?.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("set_is_subset: arguments must be sets".to_string()),
                }
            }
            "rand_shuffle" => {
                if args.len() != 1 { return Err("rand_shuffle expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let cc = self.call_rt("atomic_rand_shuffle", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("rand_shuffle failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "shuffled").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("rand_shuffle: argument must be a list".to_string()),
                }
            }
            "sorted" => {
                if args.len() != 1 { return Err("sorted expects 1 argument (list)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::List(lp) => {
                        let lv = self.load_list(lp)?;
                        let cc = self.call_rt("atomic_list_sorted", &[lv.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("sorted failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "sorted").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("sorted: argument must be a list".to_string()),
                }
            }
            "read_dir" => {
                if args.len() != 1 { return Err("read_dir expects 1 argument (path)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                match v {
                    TypedValue::Str(p) => {
                        let s = self.load_string(p)?;
                        let cc = self.call_rt("atomic_read_dir", &[s.into()])?;
                        let result = cc.try_as_basic_value().basic().ok_or("read_dir failed")?;
                        let alloca = self.builder.build_alloca(self.list_type, "read_dir").map_err(llvm_err)?;
                        self.builder.build_store(alloca, result).map_err(llvm_err)?;
                        Ok(TypedValue::List(alloca))
                    }
                    _ => Err("read_dir: argument must be a string".to_string()),
                }
            }
            "identity" => {
                if args.len() != 1 { return Err("identity expects 1 argument".to_string()); }
                self.compile_expr(&args[0])
            }
            "compose" => {
                if args.len() != 3 { return Err("compose expects 3 arguments (f, g, x)".to_string()); }
                // compose(f, g, x) = f(g(x))
                let inner = Expr::Call {
                    func: Box::new(args[1].clone()),
                    args: vec![args[2].clone()],
                    trailing_lambda: None,
                };
                let outer = Expr::Call {
                    func: Box::new(args[0].clone()),
                    args: vec![inner],
                    trailing_lambda: None,
                };
                self.compile_expr(&outer)
            }
            "diff_days" => {
                if args.len() != 2 { return Err("diff_days expects 2 arguments (date1, date2)".to_string()); }
                let d1 = self.compile_expr(&args[0])?;
                let d2 = self.compile_expr(&args[1])?;
                let (p1, st1) = match d1 { TypedValue::Struct(p, st) => (p, st), _ => return Err("diff_days: arguments must be Date structs".to_string()) };
                let (p2, st2) = match d2 { TypedValue::Struct(p, st) => (p, st), _ => return Err("diff_days: arguments must be Date structs".to_string()) };
                let i64_ty = self.i64_ty();
                // Julian Day Number: JDN = D + (153*m+2)/5 + 365*y + y/4 - y/100 + y/400 - 32045
                // where a = (14-M)/12, y = Y+4800-a, m = M+12*a-3
                let jdn = |yp: PointerValue<'ctx>, sty: inkwell::types::StructType<'ctx>| -> Result<IntValue<'ctx>, String> {
                    let y_ptr = self.builder.build_struct_gep(sty, yp, 0, "j_y").map_err(llvm_err)?;
                    let y_val = self.builder.build_load(i64_ty, y_ptr, "j_yv").map_err(llvm_err)?.into_int_value();
                    let m_ptr = self.builder.build_struct_gep(sty, yp, 1, "j_m").map_err(llvm_err)?;
                    let m_val = self.builder.build_load(i64_ty, m_ptr, "j_mv").map_err(llvm_err)?.into_int_value();
                    let d_ptr = self.builder.build_struct_gep(sty, yp, 2, "j_d").map_err(llvm_err)?;
                    let d_val = self.builder.build_load(i64_ty, d_ptr, "j_dv").map_err(llvm_err)?.into_int_value();
                    let c12 = i64_ty.const_int(12, false);
                    let c14 = i64_ty.const_int(14, false);
                    let c4800 = i64_ty.const_int(4800, false);
                    let c3 = i64_ty.const_int(3, false);
                    let c4 = i64_ty.const_int(4, false);
                    let c100 = i64_ty.const_int(100, false);
                    let c400 = i64_ty.const_int(400, false);
                    let c153 = i64_ty.const_int(153, false);
                    let c2 = i64_ty.const_int(2, false);
                    let c5 = i64_ty.const_int(5, false);
                    let c365 = i64_ty.const_int(365, false);
                    let c32045 = i64_ty.const_int(32045, false);
                    // a = (14 - M) / 12
                    let a = self.builder.build_int_signed_div(self.builder.build_int_sub(c14, m_val, "t_a1").map_err(llvm_err)?, c12, "a").map_err(llvm_err)?;
                    // y = Y + 4800 - a
                    let y = self.builder.build_int_sub(self.builder.build_int_add(y_val, c4800, "t_y1").map_err(llvm_err)?, a, "y").map_err(llvm_err)?;
                    // m = M + 12*a - 3
                    let m = self.builder.build_int_sub(self.builder.build_int_add(m_val, self.builder.build_int_mul(c12, a, "t_m1").map_err(llvm_err)?, "t_m2").map_err(llvm_err)?, c3, "m").map_err(llvm_err)?;
                    // term1 = (153*m + 2) / 5
                    let term1 = self.builder.build_int_signed_div(self.builder.build_int_add(self.builder.build_int_mul(c153, m, "t_t1a").map_err(llvm_err)?, c2, "t_t1b").map_err(llvm_err)?, c5, "term1").map_err(llvm_err)?;
                    // term2 = 365*y
                    let term2 = self.builder.build_int_mul(c365, y, "term2").map_err(llvm_err)?;
                    // term3 = y/4
                    let term3 = self.builder.build_int_signed_div(y, c4, "term3").map_err(llvm_err)?;
                    // term4 = y/100
                    let term4 = self.builder.build_int_signed_div(y, c100, "term4").map_err(llvm_err)?;
                    // term5 = y/400
                    let term5 = self.builder.build_int_signed_div(y, c400, "term5").map_err(llvm_err)?;
                    // JDN = D + term1 + term2 + term3 - term4 + term5 - 32045
                    let s1 = self.builder.build_int_add(d_val, term1, "s1").map_err(llvm_err)?;
                    let s2 = self.builder.build_int_add(s1, term2, "s2").map_err(llvm_err)?;
                    let s3 = self.builder.build_int_add(s2, term3, "s3").map_err(llvm_err)?;
                    let s4 = self.builder.build_int_sub(s3, term4, "s4").map_err(llvm_err)?;
                    let s5 = self.builder.build_int_add(s4, term5, "s5").map_err(llvm_err)?;
                    let jdn_val = self.builder.build_int_sub(s5, c32045, "jdn").map_err(llvm_err)?;
                    Ok(jdn_val)
                };
                let j1 = jdn(p1, st1)?;
                let j2 = jdn(p2, st2)?;
                let diff = self.builder.build_int_sub(j1, j2, "diff").map_err(llvm_err)?;
                let zero = i64_ty.const_int(0, false);
                let nd = self.builder.build_int_neg(diff, "nd").map_err(llvm_err)?;
                let is_neg = self.builder.build_int_compare(IntPredicate::SLT, diff, zero, "is_neg").map_err(llvm_err)?;
                let abs_diff = self.builder.build_select(is_neg, nd, diff, "abs_diff").map_err(llvm_err)?.into_int_value();
                Ok(TypedValue::Int(abs_diff))
            }
            "weekday" => {
                if args.len() != 1 { return Err("weekday expects 1 argument (date)".to_string()); }
                let d = self.compile_expr(&args[0])?;
                match d {
                    TypedValue::Struct(p, st) => {
                        // Use mktime to compute proper weekday
                        // Build struct tm: {i32 x 9}
                        let i32_ty = self.context.i32_type();
                        let tm_ty = self.context.struct_type(&[i32_ty.into(); 9], false);
                        let tm_a = self.builder.build_alloca(tm_ty, "tm").map_err(llvm_err)?;
                        let i64_ty = self.i64_ty();
                        // Extract year, month, day from Date struct
                        let yp = self.builder.build_struct_gep(st, p, 0, "w_yp").map_err(llvm_err)?;
                        let yv = self.builder.build_load(i64_ty, yp, "w_yv").map_err(llvm_err)?.into_int_value();
                        let mp = self.builder.build_struct_gep(st, p, 1, "w_mp").map_err(llvm_err)?;
                        let mv = self.builder.build_load(i64_ty, mp, "w_mv").map_err(llvm_err)?.into_int_value();
                        let dp = self.builder.build_struct_gep(st, p, 2, "w_dp").map_err(llvm_err)?;
                        let dv = self.builder.build_load(i64_ty, dp, "w_dv").map_err(llvm_err)?.into_int_value();
                        // tm_sec = 0
                        let f0 = self.builder.build_struct_gep(tm_ty, tm_a, 0, "f0").map_err(llvm_err)?;
                        self.builder.build_store(f0, i32_ty.const_int(0, false)).map_err(llvm_err)?;
                        // tm_min = 0
                        let f1 = self.builder.build_struct_gep(tm_ty, tm_a, 1, "f1").map_err(llvm_err)?;
                        self.builder.build_store(f1, i32_ty.const_int(0, false)).map_err(llvm_err)?;
                        // tm_hour = 12 (noon, avoid DST issues)
                        let f2 = self.builder.build_struct_gep(tm_ty, tm_a, 2, "f2").map_err(llvm_err)?;
                        self.builder.build_store(f2, i32_ty.const_int(12, false)).map_err(llvm_err)?;
                        // tm_mday = day
                        let f3 = self.builder.build_struct_gep(tm_ty, tm_a, 3, "f3").map_err(llvm_err)?;
                        let dv32 = self.builder.build_int_truncate(dv, i32_ty, "dv32").map_err(llvm_err)?;
                        self.builder.build_store(f3, dv32).map_err(llvm_err)?;
                        // tm_mon = month - 1
                        let f4 = self.builder.build_struct_gep(tm_ty, tm_a, 4, "f4").map_err(llvm_err)?;
                        let mon_minus = self.builder.build_int_sub(mv, i64_ty.const_int(1, false), "mon_minus").map_err(llvm_err)?;
                        let mon32 = self.builder.build_int_truncate(mon_minus, i32_ty, "mon32").map_err(llvm_err)?;
                        self.builder.build_store(f4, mon32).map_err(llvm_err)?;
                        // tm_year = year - 1900
                        let f5 = self.builder.build_struct_gep(tm_ty, tm_a, 5, "f5").map_err(llvm_err)?;
                        let y_minus = self.builder.build_int_sub(yv, i64_ty.const_int(1900, false), "y_minus").map_err(llvm_err)?;
                        let y32 = self.builder.build_int_truncate(y_minus, i32_ty, "y32").map_err(llvm_err)?;
                        self.builder.build_store(f5, y32).map_err(llvm_err)?;
                        // Remaining fields init to 0
                        for i in 6..9u32 {
                            let f = self.builder.build_struct_gep(tm_ty, tm_a, i, "f").map_err(llvm_err)?;
                            self.builder.build_store(f, i32_ty.const_int(0, false)).map_err(llvm_err)?;
                        }
                        // Call mktime
                        let mktime_fn = self.module.get_function("mktime")
                            .unwrap_or_else(|| self.module.add_function("mktime", self.i64_ty().fn_type(&[self.ptr_ty().into()], false), None));
                        let _ = self.builder.build_call(mktime_fn, &[tm_a.into()], "").map_err(llvm_err)?;
                        // Read tm_wday (field 6)
                        let wf = self.builder.build_struct_gep(tm_ty, tm_a, 6, "wf").map_err(llvm_err)?;
                        let wday32 = self.builder.build_load(i32_ty, wf, "wday").map_err(llvm_err)?.into_int_value();
                        // Convert: C wday 0=Sunday -> Atomic 1=Monday..7=Sunday
                        // Atomic weekday: 1=Monday, 7=Sunday
                        // C: 0=Sun,1=Mon,2=Tue,3=Wed,4=Thu,5=Fri,6=Sat
                        // Map: C=0->7, C=1->1, C=2->2, C=3->3, C=4->4, C=5->5, C=6->6
                        let wd_c0 = self.builder.build_int_compare(IntPredicate::EQ, wday32, i32_ty.const_int(0, false), "wd_c0").map_err(llvm_err)?;
                        let wd32 = self.builder.build_select(wd_c0, i32_ty.const_int(7, false), wday32, "wd").map_err(llvm_err)?.into_int_value();
                        let wd = self.builder.build_int_s_extend(wd32, i64_ty, "wd64").map_err(llvm_err)?;
                        Ok(TypedValue::Int(wd))
                    }
                    _ => Err("weekday: argument must be a Date struct".to_string()),
                }
            }
            "sum" => {
                if args.len() != 1 { return Err("sum expects 1 argument (list)".to_string()); }
                let list_val = self.compile_expr(&args[0])?;
                let list_ptr = match list_val {
                    TypedValue::List(p) => p,
                    _ => return Err("sum: argument must be a list".to_string()),
                };
                let list = self.load_list(list_ptr)?;
                let len = self.list_len_val(list)?;
                let data = self.list_data_ptr(list)?;
                let current = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
                let sum_a = self.builder.build_alloca(self.i64_ty(), "sum").map_err(llvm_err)?;
                self.builder.build_store(sum_a, self.i64_ty().const_int(0, false)).map_err(llvm_err)?;
                let i_a = self.builder.build_alloca(self.i64_ty(), "i").map_err(llvm_err)?;
                self.builder.build_store(i_a, self.i64_ty().const_int(0, false)).map_err(llvm_err)?;
                let hdr = self.context.append_basic_block(current, "sum_hdr");
                let bdy = self.context.append_basic_block(current, "sum_bdy");
                let ext = self.context.append_basic_block(current, "sum_ext");
                let _ = self.builder.build_unconditional_branch(hdr);
                self.builder.position_at_end(hdr);
                let iv = self.builder.build_load(self.i64_ty(), i_a, "iv").map_err(llvm_err)?.into_int_value();
                let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, len, "cond").map_err(llvm_err)?;
                let _ = self.builder.build_conditional_branch(cond, bdy, ext);
                self.builder.position_at_end(bdy);
                let ep = unsafe { self.builder.build_gep(self.string_type, data, &[iv], "ep").map_err(llvm_err) }?;
                let ev = self.builder.build_load(self.string_type, ep, "ev").map_err(llvm_err)?;
                let etag = self.builder.build_extract_value(ev.into_struct_value(), 0, "etag").map_err(llvm_err)?.into_int_value();
                let cur = self.builder.build_load(self.i64_ty(), sum_a, "cur").map_err(llvm_err)?.into_int_value();
                let new_sum = self.builder.build_int_add(cur, etag, "new_sum").map_err(llvm_err)?;
                self.builder.build_store(sum_a, new_sum).map_err(llvm_err)?;
                let ni = self.builder.build_int_add(iv, self.i64_ty().const_int(1, false), "ni").map_err(llvm_err)?;
                self.builder.build_store(i_a, ni).map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(hdr);
                self.builder.position_at_end(ext);
                let result = self.builder.build_load(self.i64_ty(), sum_a, "result").map_err(llvm_err)?;
                Ok(TypedValue::Int(result.into_int_value()))
            }
            "product" => {
                if args.len() != 1 { return Err("product expects 1 argument (list)".to_string()); }
                let list_val = self.compile_expr(&args[0])?;
                let list_ptr = match list_val {
                    TypedValue::List(p) => p,
                    _ => return Err("product: argument must be a list".to_string()),
                };
                let list = self.load_list(list_ptr)?;
                let len = self.list_len_val(list)?;
                let data = self.list_data_ptr(list)?;
                let current = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
                let prod_a = self.builder.build_alloca(self.i64_ty(), "prod").map_err(llvm_err)?;
                self.builder.build_store(prod_a, self.i64_ty().const_int(1, false)).map_err(llvm_err)?;
                let i_a = self.builder.build_alloca(self.i64_ty(), "i").map_err(llvm_err)?;
                self.builder.build_store(i_a, self.i64_ty().const_int(0, false)).map_err(llvm_err)?;
                let hdr = self.context.append_basic_block(current, "prod_hdr");
                let bdy = self.context.append_basic_block(current, "prod_bdy");
                let ext = self.context.append_basic_block(current, "prod_ext");
                let _ = self.builder.build_unconditional_branch(hdr);
                self.builder.position_at_end(hdr);
                let iv = self.builder.build_load(self.i64_ty(), i_a, "iv").map_err(llvm_err)?.into_int_value();
                let cond = self.builder.build_int_compare(IntPredicate::SLT, iv, len, "cond").map_err(llvm_err)?;
                let _ = self.builder.build_conditional_branch(cond, bdy, ext);
                self.builder.position_at_end(bdy);
                let ep = unsafe { self.builder.build_gep(self.string_type, data, &[iv], "ep").map_err(llvm_err) }?;
                let ev = self.builder.build_load(self.string_type, ep, "ev").map_err(llvm_err)?;
                let etag = self.builder.build_extract_value(ev.into_struct_value(), 0, "etag").map_err(llvm_err)?.into_int_value();
                let cur = self.builder.build_load(self.i64_ty(), prod_a, "cur").map_err(llvm_err)?.into_int_value();
                let new_prod = self.builder.build_int_mul(cur, etag, "new_prod").map_err(llvm_err)?;
                self.builder.build_store(prod_a, new_prod).map_err(llvm_err)?;
                let ni = self.builder.build_int_add(iv, self.i64_ty().const_int(1, false), "ni").map_err(llvm_err)?;
                self.builder.build_store(i_a, ni).map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(hdr);
                self.builder.position_at_end(ext);
                let result = self.builder.build_load(self.i64_ty(), prod_a, "result").map_err(llvm_err)?;
                Ok(TypedValue::Int(result.into_int_value()))
            }
            "digits" => {
                // digits(n) -> List<Int>: decimal digits of abs(n), MSD first. 0 -> [0].
                if args.len() != 1 { return Err("digits expects 1 argument (int)".to_string()); }
                let v = self.compile_expr(&args[0])?;
                let n = match v {
                    TypedValue::Int(iv) => iv,
                    _ => return Err("digits: argument must be an int".to_string()),
                };
                let ten = self.i64_ty().const_int(10, false);
                let zero = self.i64_ty().const_int(0, false);
                let one = self.i64_ty().const_int(1, false);
                // abs_n = n < 0 ? -n : n
                let neg = self.builder.build_int_neg(n, "neg").map_err(llvm_err)?;
                let is_neg = self.builder.build_int_compare(IntPredicate::SLT, n, zero, "is_neg").map_err(llvm_err)?;
                let abs_n = self.builder.build_select(is_neg, neg, n, "abs_n").map_err(llvm_err)?.into_int_value();
                let is_zero = self.builder.build_int_compare(IntPredicate::EQ, n, zero, "is0").map_err(llvm_err)?;
                let current = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
                // Count digits via repeated division
                let dc_a = self.builder.build_alloca(self.i64_ty(), "dc").map_err(llvm_err)?;
                self.builder.build_store(dc_a, zero).map_err(llvm_err)?;
                let tmp_a = self.builder.build_alloca(self.i64_ty(), "tmp").map_err(llvm_err)?;
                self.builder.build_store(tmp_a, abs_n).map_err(llvm_err)?;
                let cnt_hdr = self.context.append_basic_block(current, "dc_hdr");
                let cnt_bdy = self.context.append_basic_block(current, "dc_bdy");
                let cnt_ext = self.context.append_basic_block(current, "dc_ext");
                let _ = self.builder.build_unconditional_branch(cnt_hdr);
                self.builder.position_at_end(cnt_hdr);
                let tv = self.builder.build_load(self.i64_ty(), tmp_a, "tv").map_err(llvm_err)?.into_int_value();
                let gt0 = self.builder.build_int_compare(IntPredicate::SGT, tv, zero, "gt0").map_err(llvm_err)?;
                let _ = self.builder.build_conditional_branch(gt0, cnt_bdy, cnt_ext);
                self.builder.position_at_end(cnt_bdy);
                let dv = self.builder.build_load(self.i64_ty(), dc_a, "dv").map_err(llvm_err)?.into_int_value();
                let nd = self.builder.build_int_add(dv, one, "nd").map_err(llvm_err)?;
                self.builder.build_store(dc_a, nd).map_err(llvm_err)?;
                let nt = self.builder.build_int_signed_div(tv, ten, "nt").map_err(llvm_err)?;
                self.builder.build_store(tmp_a, nt).map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(cnt_hdr);
                self.builder.position_at_end(cnt_ext);
                let ndigits = self.builder.build_load(self.i64_ty(), dc_a, "nd").map_err(llvm_err)?.into_int_value();
                // 0 -> 1 digit
                let final_dc = self.builder.build_select(is_zero, one, ndigits, "fdc").map_err(llvm_err)?.into_int_value();
                // Create result list with capacity = final_dc
                let cc = self.call_rt("atomic_list_create", &[final_dc.into()])?;
                let res_bv = cc.try_as_basic_value().basic().ok_or("list_create failed")?;
                let res_a = self.builder.build_alloca(self.list_type, "digits_res").map_err(llvm_err)?;
                self.builder.build_store(res_a, res_bv).map_err(llvm_err)?;
                // Compute 10^(ndigits-1) iteratively
                let pow_a = self.builder.build_alloca(self.i64_ty(), "pow10").map_err(llvm_err)?;
                self.builder.build_store(pow_a, one).map_err(llvm_err)?;
                let pi_a = self.builder.build_alloca(self.i64_ty(), "pi").map_err(llvm_err)?;
                self.builder.build_store(pi_a, one).map_err(llvm_err)?;
                let pow_hdr = self.context.append_basic_block(current, "pow_hdr");
                let pow_bdy = self.context.append_basic_block(current, "pow_bdy");
                let pow_ext = self.context.append_basic_block(current, "pow_ext");
                let _ = self.builder.build_unconditional_branch(pow_hdr);
                self.builder.position_at_end(pow_hdr);
                let piv = self.builder.build_load(self.i64_ty(), pi_a, "piv").map_err(llvm_err)?.into_int_value();
                let plt = self.builder.build_int_compare(IntPredicate::SLT, piv, final_dc, "plt").map_err(llvm_err)?;
                let _ = self.builder.build_conditional_branch(plt, pow_bdy, pow_ext);
                self.builder.position_at_end(pow_bdy);
                let pv = self.builder.build_load(self.i64_ty(), pow_a, "pv").map_err(llvm_err)?.into_int_value();
                let npv = self.builder.build_int_mul(pv, ten, "npv").map_err(llvm_err)?;
                self.builder.build_store(pow_a, npv).map_err(llvm_err)?;
                let npi = self.builder.build_int_add(piv, one, "npi").map_err(llvm_err)?;
                self.builder.build_store(pi_a, npi).map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(pow_hdr);
                self.builder.position_at_end(pow_ext);
                let pow10 = self.builder.build_load(self.i64_ty(), pow_a, "pow10").map_err(llvm_err)?.into_int_value();
                // Extract digits MSD-first: for i in 0..ndigits { d = (abs_n / pow10) % 10; push; pow10 /= 10 }
                self.builder.build_store(tmp_a, abs_n).map_err(llvm_err)?;
                let di_a = self.builder.build_alloca(self.i64_ty(), "di").map_err(llvm_err)?;
                self.builder.build_store(di_a, zero).map_err(llvm_err)?;
                let p10_a = self.builder.build_alloca(self.i64_ty(), "p10").map_err(llvm_err)?;
                self.builder.build_store(p10_a, pow10).map_err(llvm_err)?;
                let fill_hdr = self.context.append_basic_block(current, "fill_hdr");
                let fill_bdy = self.context.append_basic_block(current, "fill_bdy");
                let fill_ext = self.context.append_basic_block(current, "fill_ext");
                let _ = self.builder.build_unconditional_branch(fill_hdr);
                self.builder.position_at_end(fill_hdr);
                let div = self.builder.build_load(self.i64_ty(), di_a, "div").map_err(llvm_err)?.into_int_value();
                let flt = self.builder.build_int_compare(IntPredicate::SLT, div, final_dc, "flt").map_err(llvm_err)?;
                let _ = self.builder.build_conditional_branch(flt, fill_bdy, fill_ext);
                self.builder.position_at_end(fill_bdy);
                let cur_pow = self.builder.build_load(self.i64_ty(), p10_a, "cur_pow").map_err(llvm_err)?.into_int_value();
                let cur_n = self.builder.build_load(self.i64_ty(), tmp_a, "cur_n").map_err(llvm_err)?.into_int_value();
                let q = self.builder.build_int_signed_div(cur_n, cur_pow, "q").map_err(llvm_err)?;
                let digit = self.builder.build_int_signed_rem(q, ten, "digit").map_err(llvm_err)?;
                // Build fat struct {digit, null} and push
                let undef = self.string_type.get_undef();
                let d1 = self.builder.build_insert_value(undef, digit, 0, "d1").map_err(llvm_err)?;
                let d2 = self.builder.build_insert_value(d1, self.ptr_ty().const_zero(), 1, "d2").map_err(llvm_err)?;
                let rl = self.builder.build_load(self.list_type, res_a, "rl").map_err(llvm_err)?.into_struct_value();
                let rp = self.call_rt("atomic_list_push", &[rl.into(), d2.as_basic_value_enum().into()])?;
                self.builder.build_store(res_a, rp.try_as_basic_value().basic().unwrap()).map_err(llvm_err)?;
                // Advance: i++, pow10 /= 10
                let ndi = self.builder.build_int_add(div, one, "ndi").map_err(llvm_err)?;
                self.builder.build_store(di_a, ndi).map_err(llvm_err)?;
                let np10 = self.builder.build_int_signed_div(cur_pow, ten, "np10").map_err(llvm_err)?;
                self.builder.build_store(p10_a, np10).map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(fill_hdr);
                self.builder.position_at_end(fill_ext);
                Ok(TypedValue::List(res_a))
            }
            "now_utc" => {
                if !args.is_empty() { return Err("now_utc expects no arguments".to_string()); }
                let sty = self.context.struct_type(&[self.i64_ty().into(); 6], false);
                let alloca = self.builder.build_alloca(sty, "now_utc").map_err(llvm_err)?;
                let time_fn = self.module.get_function("time").unwrap();
                let null_ptr = self.ptr_ty().const_null();
                let ts = self.builder.build_call(time_fn, &[null_ptr.into()], "ts").map_err(llvm_err)?;
                let ts_val = ts.try_as_basic_value().basic().unwrap().into_int_value();
                let gmtime_fn = self.module.get_function("gmtime_r").unwrap();
                let tm_ptr = self.builder.build_alloca(sty, "tm").map_err(llvm_err)?;
                let gmtime_call = self.builder.build_call(gmtime_fn, &[ts_val.into(), tm_ptr.into()], "").map_err(llvm_err)?;
                let _ = gmtime_call.try_as_basic_value().basic();
                // Copy tm struct to result (year+1900, month, day, hour, min, sec)
                for i in 0..6u32 {
                    let src_p = self.builder.build_struct_gep(sty, tm_ptr, i, "tm_f").map_err(llvm_err)?;
                    let val = self.builder.build_load(self.i64_ty(), src_p, "val").map_err(llvm_err)?;
                    let dst_p = self.builder.build_struct_gep(sty, alloca, i, "dst_f").map_err(llvm_err)?;
                    self.builder.build_store(dst_p, val).map_err(llvm_err)?;
                }
                // Fix year: tm_year is years since 1900
                let yp = self.builder.build_struct_gep(sty, alloca, 0, "yp").map_err(llvm_err)?;
                let yv = self.builder.build_load(self.i64_ty(), yp, "yv").map_err(llvm_err)?.into_int_value();
                let ya = self.builder.build_int_add(yv, self.i64_ty().const_int(1900, false), "ya").map_err(llvm_err)?;
                self.builder.build_store(yp, ya).map_err(llvm_err)?;
                Ok(TypedValue::Struct(alloca, sty))
            }
            "diff_seconds" => {
                if args.len() != 2 { return Err("diff_seconds expects 2 arguments (dt1, dt2)".to_string()); }
                let d1 = self.compile_expr(&args[0])?;
                let d2 = self.compile_expr(&args[1])?;
                let (p1, st1) = match d1 { TypedValue::Struct(p, st) => (p, st), _ => return Err("diff_seconds: arguments must be DateTime structs".to_string()) };
                let (p2, _st2) = match d2 { TypedValue::Struct(p, st) => (p, st), _ => return Err("diff_seconds: arguments must be DateTime structs".to_string()) };
                let i64_ty = self.i64_ty();
                // Approximate seconds from year/month/day/hour/min/sec
                let extract = |builder: &inkwell::builder::Builder<'ctx>, p: PointerValue<'ctx>, st: inkwell::types::StructType<'ctx>| -> Result<IntValue<'ctx>, String> {
                    let yp = builder.build_struct_gep(st, p, 0, "yp").map_err(llvm_err)?;
                    let y = builder.build_load(i64_ty, yp, "y").map_err(llvm_err)?.into_int_value();
                    let mp = builder.build_struct_gep(st, p, 1, "mp").map_err(llvm_err)?;
                    let m = builder.build_load(i64_ty, mp, "m").map_err(llvm_err)?.into_int_value();
                    let dp = builder.build_struct_gep(st, p, 2, "dp").map_err(llvm_err)?;
                    let d = builder.build_load(i64_ty, dp, "d").map_err(llvm_err)?.into_int_value();
                    let hp = builder.build_struct_gep(st, p, 3, "hp").map_err(llvm_err)?;
                    let h = builder.build_load(i64_ty, hp, "h").map_err(llvm_err)?.into_int_value();
                    let minp = builder.build_struct_gep(st, p, 4, "minp").map_err(llvm_err)?;
                    let minv = builder.build_load(i64_ty, minp, "min").map_err(llvm_err)?.into_int_value();
                    let sp = builder.build_struct_gep(st, p, 5, "sp").map_err(llvm_err)?;
                    let s = builder.build_load(i64_ty, sp, "s").map_err(llvm_err)?.into_int_value();
                    let d365 = builder.build_int_mul(y, i64_ty.const_int(365, false), "d365").map_err(llvm_err)?;
                    let d30 = builder.build_int_mul(m, i64_ty.const_int(30, false), "d30").map_err(llvm_err)?;
                    let days = builder.build_int_add(builder.build_int_add(d365, d30, "d1").map_err(llvm_err)?, d, "d2").map_err(llvm_err)?;
                    let secs_per_day = i64_ty.const_int(86400, false);
                    let ds = builder.build_int_mul(days, secs_per_day, "ds").map_err(llvm_err)?;
                    let hs = builder.build_int_mul(h, i64_ty.const_int(3600, false), "hs").map_err(llvm_err)?;
                    let ms = builder.build_int_mul(minv, i64_ty.const_int(60, false), "ms").map_err(llvm_err)?;
                    let total = builder.build_int_add(builder.build_int_add(builder.build_int_add(ds, hs, "t1").map_err(llvm_err)?, ms, "t2").map_err(llvm_err)?, s, "t3").map_err(llvm_err)?;
                    Ok(total)
                };
                let t1 = extract(&self.builder, p1, st1)?;
                let t2 = extract(&self.builder, p2, st1)?;
                let diff = self.builder.build_int_sub(t1, t2, "diff").map_err(llvm_err)?;
                // Absolute value
                let zero = self.i64_ty().const_int(0, false);
                let nd = self.builder.build_int_neg(diff, "nd").map_err(llvm_err)?;
                let is_neg = self.builder.build_int_compare(IntPredicate::SLT, diff, zero, "is_neg").map_err(llvm_err)?;
                let abs_diff = self.builder.build_select(is_neg, nd, diff, "abs_diff").map_err(llvm_err)?.into_int_value();
                Ok(TypedValue::Int(abs_diff))
            }
            "format" => {
                if args.len() != 2 { return Err("format expects 2 arguments (datetime, format_str)".to_string()); }
                let dt = self.compile_expr(&args[0])?;
                let fmt = self.compile_expr(&args[1])?;
                match (&dt, &fmt) {
                    (TypedValue::Struct(dt_ptr, dt_st), TypedValue::Str(fmt_ptr)) => {
                        let fmt_val = self.load_string(*fmt_ptr)?;
                        let fmt_data = self.builder.build_extract_value(fmt_val, 1, "fmt_data").map_err(llvm_err)?.into_pointer_value();
                        // Extract DateTime fields: {year, month, day, hour, minute, second}
                        let extract_field = |i: u32| -> Result<IntValue, String> {
                            let fptr = self.builder.build_struct_gep(*dt_st, *dt_ptr, i, "dt_f").map_err(llvm_err)?;
                            let val = self.builder.build_load(self.i64_ty(), fptr, "dt_v").map_err(llvm_err)?.into_int_value();
                            Ok(val)
                        };
                        let year = extract_field(0)?;
                        let month = extract_field(1)?;
                        let day = extract_field(2)?;
                        let hour = extract_field(3)?;
                        let minute = extract_field(4)?;
                        let second = extract_field(5)?;
                        // Build struct tm: {i32 x 9}
                        let i32 = self.context.i32_type();
                        let tm_ty = self.context.struct_type(&[i32.into(); 9], false);
                        let tm_a = self.builder.build_alloca(tm_ty, "tm").map_err(llvm_err)?;
                        // tm_sec = second
                        let tm_sec = self.builder.build_int_truncate(second, i32, "tm_sec").map_err(llvm_err)?;
                        let f0 = self.builder.build_struct_gep(tm_ty, tm_a, 0, "f0").map_err(llvm_err)?;
                        self.builder.build_store(f0, tm_sec).map_err(llvm_err)?;
                        // tm_min = minute
                        let tm_min = self.builder.build_int_truncate(minute, i32, "tm_min").map_err(llvm_err)?;
                        let f1 = self.builder.build_struct_gep(tm_ty, tm_a, 1, "f1").map_err(llvm_err)?;
                        self.builder.build_store(f1, tm_min).map_err(llvm_err)?;
                        // tm_hour = hour
                        let tm_hour = self.builder.build_int_truncate(hour, i32, "tm_hour").map_err(llvm_err)?;
                        let f2 = self.builder.build_struct_gep(tm_ty, tm_a, 2, "f2").map_err(llvm_err)?;
                        self.builder.build_store(f2, tm_hour).map_err(llvm_err)?;
                        // tm_mday = day
                        let tm_mday = self.builder.build_int_truncate(day, i32, "tm_mday").map_err(llvm_err)?;
                        let f3 = self.builder.build_struct_gep(tm_ty, tm_a, 3, "f3").map_err(llvm_err)?;
                        self.builder.build_store(f3, tm_mday).map_err(llvm_err)?;
                        // tm_mon = month - 1
                        let mon_minus = self.builder.build_int_sub(month, self.i64_ty().const_int(1, false), "mon_minus").map_err(llvm_err)?;
                        let tm_mon = self.builder.build_int_truncate(mon_minus, i32, "tm_mon").map_err(llvm_err)?;
                        let f4 = self.builder.build_struct_gep(tm_ty, tm_a, 4, "f4").map_err(llvm_err)?;
                        self.builder.build_store(f4, tm_mon).map_err(llvm_err)?;
                        // tm_year = year - 1900
                        let year_minus = self.builder.build_int_sub(year, self.i64_ty().const_int(1900, false), "year_minus").map_err(llvm_err)?;
                        let tm_year = self.builder.build_int_truncate(year_minus, i32, "tm_year").map_err(llvm_err)?;
                        let f5 = self.builder.build_struct_gep(tm_ty, tm_a, 5, "f5").map_err(llvm_err)?;
                        self.builder.build_store(f5, tm_year).map_err(llvm_err)?;
                        // tm_wday = 0
                        let f6 = self.builder.build_struct_gep(tm_ty, tm_a, 6, "f6").map_err(llvm_err)?;
                        self.builder.build_store(f6, i32.const_int(0, false)).map_err(llvm_err)?;
                        // tm_yday = 0
                        let f7 = self.builder.build_struct_gep(tm_ty, tm_a, 7, "f7").map_err(llvm_err)?;
                        self.builder.build_store(f7, i32.const_int(0, false)).map_err(llvm_err)?;
                        // tm_isdst = -1
                        let f8 = self.builder.build_struct_gep(tm_ty, tm_a, 8, "f8").map_err(llvm_err)?;
                        self.builder.build_store(f8, i32.const_int(0xFFFFFFFFu64 as u64, false)).map_err(llvm_err)?;
                        // Allocate buffer and call strftime
                        let buf_size = self.i64_ty().const_int(256, false);
                        let malloc_fn = self.module.get_function("malloc").unwrap();
                        let buf = self.builder.build_call(malloc_fn, &[buf_size.into()], "fmt_buf").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
                        let strftime_fn = self.module.get_function("strftime").unwrap();
                        let _ = self.builder.build_call(strftime_fn, &[buf.into(), buf_size.into(), fmt_data.into(), tm_a.into()], "").map_err(llvm_err)?;
                        // Build Atomic string: {i64, i8*} with strlen
                        let strlen_fn = self.module.get_function("strlen").unwrap();
                        let len = self.builder.build_call(strlen_fn, &[buf.into()], "fmt_len").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
                        let fat = self.string_type.get_undef();
                        let r1 = self.builder.build_insert_value(fat, len, 0, "r1").map_err(llvm_err)?;
                        let r2 = self.builder.build_insert_value(r1, buf, 1, "r2").map_err(llvm_err)?;
                        let alloca = self.builder.build_alloca(self.string_type, "fmt_str").map_err(llvm_err)?;
                        self.builder.build_store(alloca, r2).map_err(llvm_err)?;
                        Ok(TypedValue::Str(alloca))
                    }
                    _ => Err("format: expects (DateTime, String)".to_string()),
                }
            }
            "parse_date" => {
                if args.len() != 2 { return Err("parse_date expects 2 arguments (format_str, date_str)".to_string()); }
                let fmt_v = self.compile_expr(&args[0])?;
                let date_v = self.compile_expr(&args[1])?;
                match (&fmt_v, &date_v) {
                    (TypedValue::Str(_fmt_ptr), TypedValue::Str(date_ptr)) => {
                        let date_val = self.load_string(*date_ptr)?;
                        let date_data = self.builder.build_extract_value(date_val, 1, "pd_date").map_err(llvm_err)?.into_pointer_value();
                        // Use sscanf to parse the date string with format "%d-%d-%d"
                        let i32_ty = self.context.i32_type();
                        let sscanf_ty = self.i32_ty().fn_type(&[self.ptr_ty().into(), self.ptr_ty().into()], true);
                        let sscanf_fn = self.module.get_function("sscanf")
                            .unwrap_or_else(|| self.module.add_function("sscanf", sscanf_ty, None));
                        // Stack-allocate year, month, day as i32
                        let y_ptr = self.builder.build_alloca(i32_ty, "pd_y").map_err(llvm_err)?;
                        let m_ptr = self.builder.build_alloca(i32_ty, "pd_m").map_err(llvm_err)?;
                        let d_ptr = self.builder.build_alloca(i32_ty, "pd_d").map_err(llvm_err)?;
                        let fmt_str = self.builder.build_global_string_ptr("%d-%d-%d", "pd_fmt").map_err(llvm_err)?;
                        let ret = self.builder.build_call(sscanf_fn, &[
                            date_data.into(),
                            fmt_str.as_pointer_value().into(),
                            y_ptr.into(),
                            m_ptr.into(),
                            d_ptr.into(),
                        ], "pd_ret").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_int_value();
                        let ok = self.builder.build_int_compare(IntPredicate::EQ, ret, i32_ty.const_int(3, false), "pd_ok").map_err(llvm_err)?;
                        // Build Option<Date>
                        let enum_ty = self.context.struct_type(&[self.i64_ty().into(), self.ptr_ty().into()], false);
                        let some_sty = self.named_structs.get("Date")
                            .copied()
                            .unwrap_or_else(|| self.context.struct_type(&[self.i64_ty().into(); 3], false));
                        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                        let some_bb = self.context.append_basic_block(current_fn, "pd_some");
                        let none_bb = self.context.append_basic_block(current_fn, "pd_none");
                        let merge_bb = self.context.append_basic_block(current_fn, "pd_merge");
                        let _ = self.builder.build_conditional_branch(ok, some_bb, none_bb);
                        // Some branch
                        self.builder.position_at_end(some_bb);
                        let y_val = self.builder.build_load(i32_ty, y_ptr, "pd_yv").map_err(llvm_err)?.into_int_value();
                        let m_val = self.builder.build_load(i32_ty, m_ptr, "pd_mv").map_err(llvm_err)?.into_int_value();
                        let d_val = self.builder.build_load(i32_ty, d_ptr, "pd_dv").map_err(llvm_err)?.into_int_value();
                        let year_i64 = self.builder.build_int_s_extend(y_val, self.i64_ty(), "py").map_err(llvm_err)?;
                        let month_i64 = self.builder.build_int_s_extend(m_val, self.i64_ty(), "pm").map_err(llvm_err)?;
                        let day_i64 = self.builder.build_int_s_extend(d_val, self.i64_ty(), "pd").map_err(llvm_err)?;
                        let date_size = self.i64_ty().const_int(24, false);
                        let malloc_fn = self.module.get_function("malloc").unwrap();
                        let heap = self.builder.build_call(malloc_fn, &[date_size.into()], "pd_heap").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
                        let dp = self.builder.build_pointer_cast(heap, self.ptr_ty(), "dp").map_err(llvm_err)?;
                        let yp = self.builder.build_struct_gep(some_sty, dp, 0, "yp").map_err(llvm_err)?;
                        self.builder.build_store(yp, year_i64).map_err(llvm_err)?;
                        let mp = self.builder.build_struct_gep(some_sty, dp, 1, "mp").map_err(llvm_err)?;
                        self.builder.build_store(mp, month_i64).map_err(llvm_err)?;
                        let dap = self.builder.build_struct_gep(some_sty, dp, 2, "dap").map_err(llvm_err)?;
                        self.builder.build_store(dap, day_i64).map_err(llvm_err)?;
                        let undef = enum_ty.get_undef();
                        let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(0, false), 0, "r1").map_err(llvm_err)?;
                        let r2 = self.builder.build_insert_value(r1, heap, 1, "r2").map_err(llvm_err)?;
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // None branch
                        self.builder.position_at_end(none_bb);
                        let undef2 = enum_ty.get_undef();
                        let r3 = self.builder.build_insert_value(undef2, self.i64_ty().const_int(1, false), 0, "r3").map_err(llvm_err)?;
                        let r4 = self.builder.build_insert_value(r3, self.ptr_ty().const_null(), 1, "r4").map_err(llvm_err)?;
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // Merge with phi
                        self.builder.position_at_end(merge_bb);
                        let phi = self.builder.build_phi(enum_ty, "pd_phi").map_err(llvm_err)?;
                        phi.add_incoming(&[(&r2, some_bb), (&r4, none_bb)]);
                        let result_alloca = self.builder.build_alloca(enum_ty, "pd_result").map_err(llvm_err)?;
                        self.builder.build_store(result_alloca, phi.as_basic_value()).map_err(llvm_err)?;
                        Ok(TypedValue::Enum(result_alloca, enum_ty, InnerType::Int, false))
                    }
                    _ => Err("parse_date: expects (String, String)".to_string()),
                }
            }
            "date" => {
                if args.len() != 3 { return Err("date expects 3 arguments (year, month, day)".to_string()); }
                let yv = self.compile_expr(&args[0])?;
                let mv = self.compile_expr(&args[1])?;
                let dv = self.compile_expr(&args[2])?;
                let y = yv.to_bv().ok_or("year must be Int")?.into_int_value();
                let m = mv.to_bv().ok_or("month must be Int")?.into_int_value();
                let d = dv.to_bv().ok_or("day must be Int")?.into_int_value();
                let i64_ty = self.i64_ty();
                let zero = i64_ty.const_int(0, false);
                let one = i64_ty.const_int(1, false);
                // year >= 1
                let y_ok = self.builder.build_int_compare(IntPredicate::SGE, y, one, "y_ok").map_err(llvm_err)?;
                // 1 <= month <= 12
                let m_ge1 = self.builder.build_int_compare(IntPredicate::SGE, m, one, "m_ge").map_err(llvm_err)?;
                let m_le12 = self.builder.build_int_compare(IntPredicate::SLE, m, i64_ty.const_int(12, false), "m_le").map_err(llvm_err)?;
                let m_ok = self.builder.build_and(m_ge1, m_le12, "m_ok").map_err(llvm_err)?;
                // Leap year: (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
                let y_mod4 = self.builder.build_int_signed_rem(y, i64_ty.const_int(4, false), "ym4").map_err(llvm_err)?;
                let y_mod100 = self.builder.build_int_signed_rem(y, i64_ty.const_int(100, false), "ym100").map_err(llvm_err)?;
                let y_mod400 = self.builder.build_int_signed_rem(y, i64_ty.const_int(400, false), "ym400").map_err(llvm_err)?;
                let div4_ok = self.builder.build_int_compare(IntPredicate::EQ, y_mod4, zero, "d4").map_err(llvm_err)?;
                let div100_ok = self.builder.build_int_compare(IntPredicate::NE, y_mod100, zero, "d100").map_err(llvm_err)?;
                let div400_ok = self.builder.build_int_compare(IntPredicate::EQ, y_mod400, zero, "d400").map_err(llvm_err)?;
                let leap_part1 = self.builder.build_and(div4_ok, div100_ok, "lp1").map_err(llvm_err)?;
                let is_leap = self.builder.build_or(leap_part1, div400_ok, "is_leap").map_err(llvm_err)?;
                // feb_days = is_leap ? 29 : 28
                let feb_days = self.builder.build_select(is_leap, i64_ty.const_int(29, false), i64_ty.const_int(28, false), "feb").map_err(llvm_err)?.into_int_value();
                // max_days based on month:
                // month 2 -> feb_days
                // month 4,6,9,11 -> 30
                // month 1,3,5,7,8,10,12 -> 31
                let is_feb = self.builder.build_int_compare(IntPredicate::EQ, m, i64_ty.const_int(2, false), "is_feb").map_err(llvm_err)?;
                let is_30d = {
                    let m4 = self.builder.build_int_compare(IntPredicate::EQ, m, i64_ty.const_int(4, false), "m4").map_err(llvm_err)?;
                    let m6 = self.builder.build_int_compare(IntPredicate::EQ, m, i64_ty.const_int(6, false), "m6").map_err(llvm_err)?;
                    let m9 = self.builder.build_int_compare(IntPredicate::EQ, m, i64_ty.const_int(9, false), "m9").map_err(llvm_err)?;
                    let m11 = self.builder.build_int_compare(IntPredicate::EQ, m, i64_ty.const_int(11, false), "m11").map_err(llvm_err)?;
                    let t1 = self.builder.build_or(m4, m6, "t1").map_err(llvm_err)?;
                    let t2 = self.builder.build_or(m9, m11, "t2").map_err(llvm_err)?;
                    self.builder.build_or(t1, t2, "is_30d").map_err(llvm_err)?
                };
                let max_days_30or31 = self.builder.build_select(is_30d, i64_ty.const_int(30, false), i64_ty.const_int(31, false), "md_30or31").map_err(llvm_err)?.into_int_value();
                let max_days = self.builder.build_select(is_feb, feb_days, max_days_30or31, "max_days").map_err(llvm_err)?.into_int_value();
                let d_ge1 = self.builder.build_int_compare(IntPredicate::SGE, d, one, "d_ge").map_err(llvm_err)?;
                let d_le_max = self.builder.build_int_compare(IntPredicate::SLE, d, max_days, "d_le").map_err(llvm_err)?;
                let d_ok = self.builder.build_and(d_ge1, d_le_max, "d_ok").map_err(llvm_err)?;
                let ym_ok = self.builder.build_and(y_ok, m_ok, "ym_ok").map_err(llvm_err)?;
                let is_valid = self.builder.build_and(ym_ok, d_ok, "is_valid").map_err(llvm_err)?;
                // Build Option<Date>
                let enum_ty = self.context.struct_type(&[i64_ty.into(), self.ptr_ty().into()], false);
                let date_sty = self.named_structs.get("Date").copied()
                    .unwrap_or_else(|| self.context.struct_type(&[i64_ty.into(); 3], false));
                let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                let some_bb = self.context.append_basic_block(current_fn, "d_some");
                let none_bb = self.context.append_basic_block(current_fn, "d_none");
                let merge_bb = self.context.append_basic_block(current_fn, "d_merge");
                let _ = self.builder.build_conditional_branch(is_valid, some_bb, none_bb);
                self.builder.position_at_end(some_bb);
                let date_size = i64_ty.const_int(24, false);
                let malloc_fn = self.module.get_function("malloc").unwrap();
                let heap = self.builder.build_call(malloc_fn, &[date_size.into()], "d_heap").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
                let yp = self.builder.build_struct_gep(date_sty, heap, 0, "d_yp").map_err(llvm_err)?;
                self.builder.build_store(yp, y).map_err(llvm_err)?;
                let mp = self.builder.build_struct_gep(date_sty, heap, 1, "d_mp").map_err(llvm_err)?;
                self.builder.build_store(mp, m).map_err(llvm_err)?;
                let dp = self.builder.build_struct_gep(date_sty, heap, 2, "d_dp").map_err(llvm_err)?;
                self.builder.build_store(dp, d).map_err(llvm_err)?;
                let undef = enum_ty.get_undef();
                let r1 = self.builder.build_insert_value(undef, i64_ty.const_int(0, false), 0, "r1").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, heap, 1, "r2").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_bb);
                self.builder.position_at_end(none_bb);
                let undef2 = enum_ty.get_undef();
                let r3 = self.builder.build_insert_value(undef2, i64_ty.const_int(1, false), 0, "r3").map_err(llvm_err)?;
                let r4 = self.builder.build_insert_value(r3, self.ptr_ty().const_null(), 1, "r4").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_bb);
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(enum_ty, "d_phi").map_err(llvm_err)?;
                phi.add_incoming(&[(&r2, some_bb), (&r4, none_bb)]);
                let result_alloca = self.builder.build_alloca(enum_ty, "d_result").map_err(llvm_err)?;
                self.builder.build_store(result_alloca, phi.as_basic_value()).map_err(llvm_err)?;
                Ok(TypedValue::Enum(result_alloca, enum_ty, InnerType::Int, false))
            }
            "datetime" => {
                if args.len() != 6 { return Err("datetime expects 6 arguments (year, month, day, hour, minute, second)".to_string()); }
                let yv = self.compile_expr(&args[0])?;
                let mov = self.compile_expr(&args[1])?;
                let dv = self.compile_expr(&args[2])?;
                let hv = self.compile_expr(&args[3])?;
                let minv = self.compile_expr(&args[4])?;
                let sv = self.compile_expr(&args[5])?;
                let y = yv.to_bv().ok_or("year must be Int")?.into_int_value();
                let mo = mov.to_bv().ok_or("month must be Int")?.into_int_value();
                let d = dv.to_bv().ok_or("day must be Int")?.into_int_value();
                let h = hv.to_bv().ok_or("hour must be Int")?.into_int_value();
                let min = minv.to_bv().ok_or("minute must be Int")?.into_int_value();
                let s = sv.to_bv().ok_or("second must be Int")?.into_int_value();
                let i64_ty = self.i64_ty();
                let zero = i64_ty.const_int(0, false);
                let one = i64_ty.const_int(1, false);
                // Validate year, month, day (same as date)
                let y_ok = self.builder.build_int_compare(IntPredicate::SGE, y, one, "y_ok").map_err(llvm_err)?;
                let m_ge1 = self.builder.build_int_compare(IntPredicate::SGE, mo, one, "m_ge").map_err(llvm_err)?;
                let m_le12 = self.builder.build_int_compare(IntPredicate::SLE, mo, i64_ty.const_int(12, false), "m_le").map_err(llvm_err)?;
                let m_ok = self.builder.build_and(m_ge1, m_le12, "m_ok").map_err(llvm_err)?;
                let y_mod4 = self.builder.build_int_signed_rem(y, i64_ty.const_int(4, false), "ym4").map_err(llvm_err)?;
                let y_mod100 = self.builder.build_int_signed_rem(y, i64_ty.const_int(100, false), "ym100").map_err(llvm_err)?;
                let y_mod400 = self.builder.build_int_signed_rem(y, i64_ty.const_int(400, false), "ym400").map_err(llvm_err)?;
                let div4_ok = self.builder.build_int_compare(IntPredicate::EQ, y_mod4, zero, "d4").map_err(llvm_err)?;
                let div100_ok = self.builder.build_int_compare(IntPredicate::NE, y_mod100, zero, "d100").map_err(llvm_err)?;
                let div400_ok = self.builder.build_int_compare(IntPredicate::EQ, y_mod400, zero, "d400").map_err(llvm_err)?;
                let leap_part1 = self.builder.build_and(div4_ok, div100_ok, "lp1").map_err(llvm_err)?;
                let is_leap = self.builder.build_or(leap_part1, div400_ok, "is_leap").map_err(llvm_err)?;
                let feb_days = self.builder.build_select(is_leap, i64_ty.const_int(29, false), i64_ty.const_int(28, false), "feb").map_err(llvm_err)?.into_int_value();
                let is_feb = self.builder.build_int_compare(IntPredicate::EQ, mo, i64_ty.const_int(2, false), "is_feb").map_err(llvm_err)?;
                let m4 = self.builder.build_int_compare(IntPredicate::EQ, mo, i64_ty.const_int(4, false), "m4").map_err(llvm_err)?;
                let m6 = self.builder.build_int_compare(IntPredicate::EQ, mo, i64_ty.const_int(6, false), "m6").map_err(llvm_err)?;
                let m9 = self.builder.build_int_compare(IntPredicate::EQ, mo, i64_ty.const_int(9, false), "m9").map_err(llvm_err)?;
                let m11 = self.builder.build_int_compare(IntPredicate::EQ, mo, i64_ty.const_int(11, false), "m11").map_err(llvm_err)?;
                let t1 = self.builder.build_or(m4, m6, "t1").map_err(llvm_err)?;
                let t2 = self.builder.build_or(m9, m11, "t2").map_err(llvm_err)?;
                let is_30d = self.builder.build_or(t1, t2, "is_30d").map_err(llvm_err)?;
                let max_days_30or31 = self.builder.build_select(is_30d, i64_ty.const_int(30, false), i64_ty.const_int(31, false), "md_30or31").map_err(llvm_err)?.into_int_value();
                let max_days = self.builder.build_select(is_feb, feb_days, max_days_30or31, "max_days").map_err(llvm_err)?.into_int_value();
                let d_ge1 = self.builder.build_int_compare(IntPredicate::SGE, d, one, "d_ge").map_err(llvm_err)?;
                let d_le_max = self.builder.build_int_compare(IntPredicate::SLE, d, max_days, "d_le").map_err(llvm_err)?;
                let d_ok = self.builder.build_and(d_ge1, d_le_max, "d_ok").map_err(llvm_err)?;
                // hour 0-23, minute 0-59, second 0-59
                let h_ge0 = self.builder.build_int_compare(IntPredicate::SGE, h, zero, "h_ge").map_err(llvm_err)?;
                let h_le23 = self.builder.build_int_compare(IntPredicate::SLE, h, i64_ty.const_int(23, false), "h_le").map_err(llvm_err)?;
                let h_ok = self.builder.build_and(h_ge0, h_le23, "h_ok").map_err(llvm_err)?;
                let min_ge0 = self.builder.build_int_compare(IntPredicate::SGE, min, zero, "min_ge").map_err(llvm_err)?;
                let min_le59 = self.builder.build_int_compare(IntPredicate::SLE, min, i64_ty.const_int(59, false), "min_le").map_err(llvm_err)?;
                let min_ok = self.builder.build_and(min_ge0, min_le59, "min_ok").map_err(llvm_err)?;
                let s_ge0 = self.builder.build_int_compare(IntPredicate::SGE, s, zero, "s_ge").map_err(llvm_err)?;
                let s_le59 = self.builder.build_int_compare(IntPredicate::SLE, s, i64_ty.const_int(59, false), "s_le").map_err(llvm_err)?;
                let s_ok = self.builder.build_and(s_ge0, s_le59, "s_ok").map_err(llvm_err)?;
                let ym_ok = self.builder.build_and(y_ok, m_ok, "ym_ok").map_err(llvm_err)?;
                let ymd_ok = self.builder.build_and(ym_ok, d_ok, "ymd_ok").map_err(llvm_err)?;
                let hms_ok = self.builder.build_and(self.builder.build_and(h_ok, min_ok, "hm_ok").map_err(llvm_err)?, s_ok, "hms_ok").map_err(llvm_err)?;
                let is_valid = self.builder.build_and(ymd_ok, hms_ok, "is_valid").map_err(llvm_err)?;
                // Build Option<DateTime>
                let enum_ty = self.context.struct_type(&[i64_ty.into(), self.ptr_ty().into()], false);
                let dt_sty = self.named_structs.get("DateTime").copied()
                    .unwrap_or_else(|| self.context.struct_type(&[i64_ty.into(); 6], false));
                let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no fn")?;
                let some_bb = self.context.append_basic_block(current_fn, "dt_some");
                let none_bb = self.context.append_basic_block(current_fn, "dt_none");
                let merge_bb = self.context.append_basic_block(current_fn, "dt_merge");
                let _ = self.builder.build_conditional_branch(is_valid, some_bb, none_bb);
                self.builder.position_at_end(some_bb);
                let dt_size = i64_ty.const_int(48, false); // 6 * 8 bytes
                let malloc_fn = self.module.get_function("malloc").unwrap();
                let heap = self.builder.build_call(malloc_fn, &[dt_size.into()], "dt_heap").map_err(llvm_err)?.try_as_basic_value().basic().unwrap().into_pointer_value();
                for (i, val) in [y, mo, d, h, min, s].iter().enumerate() {
                    let fp = self.builder.build_struct_gep(dt_sty, heap, i as u32, "dt_f").map_err(llvm_err)?;
                    self.builder.build_store(fp, *val).map_err(llvm_err)?;
                }
                let undef = enum_ty.get_undef();
                let r1 = self.builder.build_insert_value(undef, i64_ty.const_int(0, false), 0, "r1").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, heap, 1, "r2").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_bb);
                self.builder.position_at_end(none_bb);
                let undef2 = enum_ty.get_undef();
                let r3 = self.builder.build_insert_value(undef2, i64_ty.const_int(1, false), 0, "r3").map_err(llvm_err)?;
                let r4 = self.builder.build_insert_value(r3, self.ptr_ty().const_null(), 1, "r4").map_err(llvm_err)?;
                let _ = self.builder.build_unconditional_branch(merge_bb);
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(enum_ty, "dt_phi").map_err(llvm_err)?;
                phi.add_incoming(&[(&r2, some_bb), (&r4, none_bb)]);
                let result_alloca = self.builder.build_alloca(enum_ty, "dt_result").map_err(llvm_err)?;
                self.builder.build_store(result_alloca, phi.as_basic_value()).map_err(llvm_err)?;
                Ok(TypedValue::Enum(result_alloca, enum_ty, InnerType::Int, false))
            }
            "Random_new" => {
                if args.len() != 1 { return Err("Random_new expects 1 argument (seed)".to_string()); }
                let seed_v = self.compile_expr(&args[0])?;
                let seed = seed_v.to_bv().ok_or("seed must be Int")?.into_int_value();
                // Random struct is just {i64} wrapping the seed
                let rand_sty = self.context.struct_type(&[self.i64_ty().into()], false);
                let alloca = self.builder.build_alloca(rand_sty, "rand").map_err(llvm_err)?;
                let f0 = self.builder.build_struct_gep(rand_sty, alloca, 0, "f0").map_err(llvm_err)?;
                self.builder.build_store(f0, seed).map_err(llvm_err)?;
                Ok(TypedValue::Struct(alloca, rand_sty))
            }
            "next_int" => {
                if args.len() != 3 { return Err("next_int expects 3 arguments (random, min, max)".to_string()); }
                let rng_v = self.compile_expr(&args[0])?;
                let min_v = self.compile_expr(&args[1])?;
                let max_v = self.compile_expr(&args[2])?;
                let (rng_ptr, rng_st) = match rng_v {
                    TypedValue::Struct(p, st) => (p, st),
                    _ => return Err("next_int: first argument must be a Random struct".to_string()),
                };
                let min = min_v.to_bv().ok_or("min must be Int")?.into_int_value();
                let max = max_v.to_bv().ok_or("max must be Int")?.into_int_value();
                let i64_ty = self.i64_ty();
                // Load current seed
                let f0 = self.builder.build_struct_gep(rng_st, rng_ptr, 0, "f0").map_err(llvm_err)?;
                let seed = self.builder.build_load(i64_ty, f0, "seed").map_err(llvm_err)?.into_int_value();
                // xorshift64 PRNG
                // x ^= x << 13; x ^= x >> 7; x ^= x << 17
                let c13 = i64_ty.const_int(13, false);
                let c7 = i64_ty.const_int(7, false);
                let c17 = i64_ty.const_int(17, false);
                let x1 = self.builder.build_xor(seed,
                    self.builder.build_left_shift(seed, c13, "s1").map_err(llvm_err)?, "x1").map_err(llvm_err)?;
                let x2 = self.builder.build_xor(x1,
                    self.builder.build_right_shift(x1, c7, false, "s2").map_err(llvm_err)?, "x2").map_err(llvm_err)?;
                let x3 = self.builder.build_xor(x2,
                    self.builder.build_left_shift(x2, c17, "s3").map_err(llvm_err)?, "x3").map_err(llvm_err)?;
                // Ensure non-zero (degenerates to 0 otherwise)
                let zero = i64_ty.const_int(0, false);
                let is_zero = self.builder.build_int_compare(IntPredicate::EQ, x3, zero, "is_zero").map_err(llvm_err)?;
                let new_seed = self.builder.build_select(is_zero, i64_ty.const_int(1, false), x3, "new_seed").map_err(llvm_err)?.into_int_value();
                // Compute value in [min, max] range
                let range = self.builder.build_int_sub(max, min, "range").map_err(llvm_err)?;
                let range_plus_1 = self.builder.build_int_add(range, i64_ty.const_int(1, false), "rp1").map_err(llvm_err)?;
                // Use unsigned remainder for proper range mapping
                let value = self.builder.build_int_unsigned_rem(new_seed, range_plus_1, "val_mod").map_err(llvm_err)?;
                let result = self.builder.build_int_add(value, min, "result").map_err(llvm_err)?;
                // Build result tuple (Random, Int)
                let rand_sty = rng_st;
                let tuple_sty = self.context.struct_type(&[rand_sty.into(), i64_ty.into()], false);
                let tup_alloca = self.builder.build_alloca(tuple_sty, "tup").map_err(llvm_err)?;
                // Store new Random
                let rng_field = self.builder.build_struct_gep(tuple_sty, tup_alloca, 0, "rf").map_err(llvm_err)?;
                self.builder.build_store(rng_field, new_seed).map_err(llvm_err)?;
                // Store int result
                let int_field = self.builder.build_struct_gep(tuple_sty, tup_alloca, 1, "inf").map_err(llvm_err)?;
                self.builder.build_store(int_field, result).map_err(llvm_err)?;
                Ok(TypedValue::Struct(tup_alloca, tuple_sty))
            }
            "flip" => {
                if args.len() != 3 { return Err("flip expects 3 arguments (f, a, b)".to_string()); }
                // flip(f, a, b) = f(b, a)
                let call = Expr::Call {
                    func: Box::new(args[0].clone()),
                    args: vec![args[2].clone(), args[1].clone()],
                    trailing_lambda: None,
                };
                self.compile_expr(&call)
            }
            "constant" => {
                if args.len() != 2 { return Err("constant expects 2 arguments (a, b)".to_string()); }
                // constant(a, b) = a (returns first argument, ignores second)
                self.compile_expr(&args[0])
            }
            "uncurry" => {
                if args.len() != 3 { return Err("uncurry expects 3 arguments (f, a, b)".to_string()); }
                // uncurry(f, a, b) = f(a)(b)
                let inner = Expr::Call {
                    func: Box::new(args[0].clone()),
                    args: vec![args[1].clone()],
                    trailing_lambda: None,
                };
                let outer = Expr::Call {
                    func: Box::new(inner),
                    args: vec![args[2].clone()],
                    trailing_lambda: None,
                };
                self.compile_expr(&outer)
            }
            "curry" => {
                if args.len() != 2 { return Err("curry expects 2 arguments (f, a)".to_string()); }
                // curry(f, a) → creates a lambda |b| f(a, b)
                // We implement this by compiling the partial application as a lambda expression
                let lambda = Expr::Lambda {
                    params: vec!["b".to_string()],
                    body: Box::new(Expr::Call {
                        func: Box::new(args[0].clone()),
                        args: vec![args[1].clone(), Expr::Ident("b".to_string())],
                        trailing_lambda: None,
                    }),
                    implicit_it: false,
                };
                self.compile_expr(&lambda)
            }
            // ---- Option/Result convenience methods ----
            "is_some" => {
                if args.len() != 1 { return Err("is_some expects 1 argument (option)".to_string()); }
                self.builtin_enum_is_tag(&args[0], 0)
            }
            "is_none" => {
                if args.len() != 1 { return Err("is_none expects 1 argument (option)".to_string()); }
                self.builtin_enum_is_tag(&args[0], 1)
            }
            "is_ok" => {
                if args.len() != 1 { return Err("is_ok expects 1 argument (result)".to_string()); }
                self.builtin_enum_is_tag(&args[0], 0)
            }
            "is_err" => {
                if args.len() != 1 { return Err("is_err expects 1 argument (result)".to_string()); }
                self.builtin_enum_is_tag(&args[0], 1)
            }
            "unwrap_or" => {
                if args.len() != 2 { return Err("unwrap_or expects 2 arguments (enum, default)".to_string()); }
                self.builtin_unwrap_or(&args[0], &args[1])
            }
            "unwrap" => {
                if args.len() != 1 { return Err("unwrap expects 1 argument (enum)".to_string()); }
                self.builtin_unwrap(&args[0])
            }
            "or_else" => {
                if args.len() != 2 { return Err("or_else expects 2 arguments (enum, handler)".to_string()); }
                self.builtin_or_else(&args[0], &args[1])
            }
            "ok" => {
                if args.len() != 2 { return Err("ok expects 2 arguments (option, error_value)".to_string()); }
                self.builtin_ok(&args[0], &args[1])
            }
            // ---- LazyList operations ----
            "to_list" => {
                if args.len() != 1 { return Err("to_list expects 1 argument (lazy_list or set)".to_string()); }
                self.builtin_to_list(&args[0])
            }
            "to_lazy_list" => {
                if args.len() != 1 { return Err("to_lazy_list expects 1 argument (list)".to_string()); }
                self.builtin_to_lazy_list(&args[0])
            }
            "lazy_take" => {
                if args.len() != 2 { return Err("lazy_take expects 2 arguments (n, lazy_list)".to_string()); }
                self.builtin_lazy_take(&args[0], &args[1])
            }
            "lazy_drop" => {
                if args.len() != 2 { return Err("lazy_drop expects 2 arguments (n, lazy_list)".to_string()); }
                self.builtin_lazy_drop(&args[0], &args[1])
            }
            "lazy_map" => {
                if args.len() != 2 { return Err("lazy_map expects 2 arguments (fn, lazy_list)".to_string()); }
                self.builtin_lazy_map(&args[0], &args[1])
            }
            "lazy_filter" => {
                if args.len() != 2 { return Err("lazy_filter expects 2 arguments (fn, lazy_list)".to_string()); }
                self.builtin_lazy_filter(&args[0], &args[1])
            }
            "lazy_take_while" => {
                if args.len() != 2 { return Err("lazy_take_while expects 2 arguments (fn, lazy_list)".to_string()); }
                self.builtin_lazy_take_while(&args[0], &args[1])
            }
            "lazy_head" => {
                if args.len() != 1 { return Err("lazy_head expects 1 argument (lazy_list)".to_string()); }
                self.builtin_lazy_head(&args[0])
            }
            "lazy_zip" => {
                if args.len() != 2 { return Err("lazy_zip expects 2 arguments (lazy1, lazy2)".to_string()); }
                self.builtin_lazy_zip(&args[0], &args[1])
            }
            "is_null" => {
                if args.len() != 1 { return Err("is_null expects 1 argument".to_string()); }
                self.builtin_is_null(&args[0])
            }
            "deref" => {
                if args.len() != 1 { return Err("deref expects 1 argument".to_string()); }
                self.builtin_deref(&args[0])
            }
            "ping" => {
                let result = self.call_rt("atomic_test_ping", &[])?;
                let val = result.try_as_basic_value().basic()
                    .ok_or("ping call failed")?.into_int_value();
                Ok(TypedValue::Int(val))
            }
            "httpRequest" => {
                if args.len() != 4 { return Err("httpRequest expects 4 arguments (method, url, headers, body)".to_string()); }
                self.builtin_http_request(&args[0], &args[1], &args[2], &args[3])
            }
            _ => Err(format!("Unknown builtin: {}", name)),
        }
    }

    /// Emit real date/time by calling C time() and localtime_r().
    /// When `include_time` is true, returns DateTime {year, month, day, hour, minute, second};
    /// otherwise returns Date {year, month, day}.
    pub(super) fn emit_today_now(&mut self, include_time: bool) -> Result<TypedValue<'ctx>, String> {
        let i64 = self.i64_ty();
        let i32 = self.i32_ty();
        let ptr = self.ptr_ty();

        // Declare time(3) if not already declared: time_t time(time_t *tloc)
        let time_fn = self.module.get_function("time")
            .unwrap_or_else(|| self.module.add_function("time", i64.fn_type(&[ptr.into()], false), None));

        // Declare localtime_r(3) if not already declared: struct tm *localtime_r(const time_t *timep, struct tm *result)
        let loc_fn = self.module.get_function("localtime_r")
            .unwrap_or_else(|| self.module.add_function("localtime_r", ptr.fn_type(&[ptr.into(), ptr.into()], false), None));

        // struct tm = {i32, i32, i32, i32, i32, i32, i32, i32, i32}
        let tm_ty = self.context.struct_type(&[
            i32.into(), i32.into(), i32.into(), i32.into(), i32.into(),
            i32.into(), i32.into(), i32.into(), i32.into(),
        ], false);

        // Call time(NULL) — pass null for tloc
        let null_ptr = ptr.const_zero();
        let now_ts = self.builder.build_call(time_fn, &[null_ptr.into()], "now_ts").map_err(llvm_err)?
            .try_as_basic_value().basic().ok_or("time() call failed")?;

        // Allocate struct tm on stack, zero-init
        let tm_a = self.builder.build_alloca(tm_ty, "tm_buf").map_err(llvm_err)?;
        let zero_i32 = i32.const_int(0, false);
        for i in 0..9u32 {
            let fp = self.builder.build_struct_gep(tm_ty, tm_a, i, "tm_f").map_err(llvm_err)?;
            self.builder.build_store(fp, zero_i32).map_err(llvm_err)?;
        }

        // Allocate time_t for passing to localtime_r
        let ts_a = self.builder.build_alloca(i64, "ts_buf").map_err(llvm_err)?;
        self.builder.build_store(ts_a, now_ts).map_err(llvm_err)?;

        // Call localtime_r(&ts, &tm)
        let _ = self.builder.build_call(loc_fn, &[ts_a.into(), tm_a.into()], "").map_err(llvm_err)?;

        // Load fields from struct tm
        // tm_year: years since 1900 → actual year = tm_year + 1900
        let tm_year_p = self.builder.build_struct_gep(tm_ty, tm_a, 5, "tm_year_p").map_err(llvm_err)?;
        let tm_year = self.builder.build_load(i32, tm_year_p, "tm_year").map_err(llvm_err)?.into_int_value();
        let year = self.builder.build_int_add(
            self.builder.build_int_s_extend(tm_year, i64, "year_ext").map_err(llvm_err)?,
            i64.const_int(1900, false), "year"
        ).map_err(llvm_err)?;

        // tm_mon: 0-11 → month = tm_mon + 1
        let tm_mon_p = self.builder.build_struct_gep(tm_ty, tm_a, 4, "tm_mon_p").map_err(llvm_err)?;
        let tm_mon = self.builder.build_load(i32, tm_mon_p, "tm_mon").map_err(llvm_err)?.into_int_value();
        let month = self.builder.build_int_add(
            self.builder.build_int_s_extend(tm_mon, i64, "mon_ext").map_err(llvm_err)?,
            i64.const_int(1, false), "month"
        ).map_err(llvm_err)?;

        // tm_mday: 1-31
        let tm_day_p = self.builder.build_struct_gep(tm_ty, tm_a, 3, "tm_day_p").map_err(llvm_err)?;
        let tm_day = self.builder.build_load(i32, tm_day_p, "tm_day").map_err(llvm_err)?.into_int_value();
        let day = self.builder.build_int_s_extend(tm_day, i64, "day_ext").map_err(llvm_err)?;

        if include_time {
            let dt_struct = self.named_structs.get("DateTime")
                .or_else(|| self.anon_structs.values().find(|s| s.get_field_types().len() == 6));
            match dt_struct {
                Some(sty) => {
                    let sty = *sty;
                    let alloca = self.builder.build_alloca(sty, "now").map_err(llvm_err)?;
                    // Store year, month, day
                    for (i, val) in [(0u32, year), (1, month), (2, day)].iter() {
                        let fp = self.builder.build_struct_gep(sty, alloca, *i, "f").map_err(llvm_err)?;
                        self.builder.build_store(fp, *val).map_err(llvm_err)?;
                    }
                    // tm_hour: 0-23
                    let tm_h_p = self.builder.build_struct_gep(tm_ty, tm_a, 2, "tm_h_p").map_err(llvm_err)?;
                    let tm_h = self.builder.build_load(i32, tm_h_p, "tm_h").map_err(llvm_err)?.into_int_value();
                    let hour = self.builder.build_int_s_extend(tm_h, i64, "h_ext").map_err(llvm_err)?;
                    // tm_min: 0-59
                    let tm_m_p = self.builder.build_struct_gep(tm_ty, tm_a, 1, "tm_min_p").map_err(llvm_err)?;
                    let tm_m = self.builder.build_load(i32, tm_m_p, "tm_m").map_err(llvm_err)?.into_int_value();
                    let min = self.builder.build_int_s_extend(tm_m, i64, "m_ext").map_err(llvm_err)?;
                    // tm_sec: 0-60
                    let tm_s_p = self.builder.build_struct_gep(tm_ty, tm_a, 0, "tm_s_p").map_err(llvm_err)?;
                    let tm_s = self.builder.build_load(i32, tm_s_p, "tm_s").map_err(llvm_err)?.into_int_value();
                    let sec = self.builder.build_int_s_extend(tm_s, i64, "s_ext").map_err(llvm_err)?;
                    for (i, val) in [(3u32, hour), (4, min), (5, sec)].iter() {
                        let fp = self.builder.build_struct_gep(sty, alloca, *i, "f").map_err(llvm_err)?;
                        self.builder.build_store(fp, *val).map_err(llvm_err)?;
                    }
                    Ok(TypedValue::Struct(alloca, sty))
                }
                None => Err("now: DateTime type not defined".to_string()),
            }
        } else {
            let date_struct = self.named_structs.get("Date")
                .or_else(|| self.anon_structs.values().find(|s| s.get_field_types().len() == 3));
            match date_struct {
                Some(sty) => {
                    let sty = *sty;
                    let alloca = self.builder.build_alloca(sty, "today").map_err(llvm_err)?;
                    for (i, val) in [(0u32, year), (1, month), (2, day)].iter() {
                        let fp = self.builder.build_struct_gep(sty, alloca, *i, "f").map_err(llvm_err)?;
                        self.builder.build_store(fp, *val).map_err(llvm_err)?;
                    }
                    Ok(TypedValue::Struct(alloca, sty))
                }
                None => Err("today: Date type not defined".to_string()),
            }
        }
    }

    /// to_cstring(str) -> CString: allocate a null-terminated copy of the string
    pub(super) fn builtin_to_cstring(&mut self, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(expr)?;
        match val {
            TypedValue::Str(ptr) => {
                let str_val = self.load_string(ptr)?;
                let len = self.builder.build_extract_value(str_val, 0, "len")
                    .map_err(llvm_err)?.into_int_value();
                let data = self.builder.build_extract_value(str_val, 1, "data")
                    .map_err(llvm_err)?.into_pointer_value();
                // allocate len + 1 bytes for null-terminated copy
                let size = self.builder.build_int_add(len, self.i64_ty().const_int(1, false), "cstr_size")
                    .map_err(llvm_err)?;
                let cstr = self.call_rt("malloc", &[size.into()])?;
                let cstr_ptr = cstr.try_as_basic_value().basic().ok_or("malloc failed")?.into_pointer_value();
                // memcpy the string data (dest, src, len)
                let _ = self.builder.build_memcpy(cstr_ptr, 1, data, 1, len).map_err(llvm_err)?;
                // null terminate
                let null_pos = unsafe { self.builder.build_gep(self.context.i8_type(), cstr_ptr, &[len], "null_pos") }.map_err(llvm_err)?;
                self.builder.build_store(null_pos, self.context.i8_type().const_int(0, false))
                    .map_err(llvm_err)?;
                Ok(TypedValue::CString(cstr_ptr))
            }
            _ => Err("to_cstring: argument must be a String".to_string()),
        }
    }

    /// from_cstring(cstr) -> String: read a null-terminated C string
    pub(super) fn builtin_from_cstring(&mut self, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(expr)?;
        match val {
            TypedValue::CString(ptr) | TypedValue::Ptr(ptr) | TypedValue::FileHandle(ptr) => {
                // strlen - FileHandle is treated as a pointer for null check
                if matches!(val, TypedValue::FileHandle(_)) {
                    return Err("from_cstring: cannot convert FileHandle to string".to_string());
                }
                // strlen
                let len_val = self.call_rt("strlen", &[ptr.into()])?;
                let len = len_val.try_as_basic_value().basic().ok_or("strlen failed")?.into_int_value();
                // allocate Atomic string
                let str_struct = self.call_rt("atomic_string_create", &[ptr.into(), len.into()])?;
                let str_val = str_struct.try_as_basic_value().basic().ok_or("string_create failed")?;
                let alloca = self.builder.build_alloca(self.string_type, "from_cstr").map_err(llvm_err)?;
                self.builder.build_store(alloca, str_val).map_err(llvm_err)?;
                Ok(TypedValue::Str(alloca))
            }
            _ => Err("from_cstring: argument must be a CString or Ptr".to_string()),
        }
    }

    /// is_null(ptr) -> Bool: check if a Ptr or CString is null
    pub(super) fn builtin_is_null(&mut self, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        if !self.in_unsafe {
            return Err("is_null can only be used inside an unsafe block".to_string());
        }
        let val = self.compile_expr(expr)?;
        match val {
            TypedValue::Ptr(p) | TypedValue::CString(p) | TypedValue::FileHandle(p) => {
                let null_ptr = self.context.ptr_type(inkwell::AddressSpace::default()).const_zero();
                let is_null = self.builder.build_int_compare(
                    IntPredicate::EQ, p, null_ptr, "is_null"
                ).map_err(llvm_err)?;
                Ok(TypedValue::Bool(is_null))
            }
            _ => Err("is_null: argument must be a Ptr, CString, or FileHandle".to_string()),
        }
    }

    /// deref(ptr) -> T: dereference a typed pointer (unsafe)
    pub(super) fn builtin_deref(&mut self, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        if !self.in_unsafe {
            return Err("deref can only be used inside an unsafe block".to_string());
        }
        let val = self.compile_expr(expr)?;
        match val {
            TypedValue::Ptr(p) => {
                // Load as i64 (most common FFI use case)
                let loaded = self.builder.build_load(self.i64_ty(), p, "deref").map_err(llvm_err)?;
                Ok(TypedValue::Int(loaded.into_int_value()))
            }
            _ => Err("deref: argument must be a Ptr".to_string()),
        }
    }

    /// httpRequest(method: String, url: String, headers: String, body: String) -> String
    /// Converts each String arg to CString, calls atomic_http_request, returns result as String.
    pub(super) fn builtin_http_request(
        &mut self,
        method: &Expr,
        url: &Expr,
        headers: &Expr,
        body: &Expr,
    ) -> Result<TypedValue<'ctx>, String> {
        // Delegate to existing to_cstring for each arg
        let method_cstr = self.builtin_to_cstring(method)?;
        let url_cstr = self.builtin_to_cstring(url)?;
        let headers_cstr = self.builtin_to_cstring(headers)?;
        let body_cstr = self.builtin_to_cstring(body)?;

        let method_ptr = match method_cstr {
            TypedValue::CString(p) => p,
            _ => return Err("httpRequest: method must be String".to_string()),
        };
        let url_ptr = match url_cstr {
            TypedValue::CString(p) => p,
            _ => return Err("httpRequest: url must be String".to_string()),
        };
        let headers_ptr = match headers_cstr {
            TypedValue::CString(p) => p,
            _ => return Err("httpRequest: headers must be String".to_string()),
        };
        let body_ptr = match body_cstr {
            TypedValue::CString(p) => p,
            _ => return Err("httpRequest: body must be String".to_string()),
        };

        // Use strlen to get body length (safe since we just null-terminated it)
        let body_len_val = self.call_rt("strlen", &[body_ptr.into()])?;
        let body_len = body_len_val.try_as_basic_value().basic()
            .ok_or("strlen failed")?.into_int_value();

        // Call atomic_http_request(method, url, headers, body, body_len)
        let req_fn = self.module.get_function("atomic_http_request")
            .unwrap();
        let call_result = self.builder.build_call(req_fn, &[
            method_ptr.into(), url_ptr.into(), headers_ptr.into(),
            body_ptr.into(), body_len.into(),
        ], "http_result").map_err(llvm_err)?;
        let result_ptr = call_result.try_as_basic_value().basic()
            .ok_or("call failed")?.into_pointer_value();

        // Free temp CStrings
        let free_fn = self.module.get_function("free")
            .ok_or("free not found in module")?;
        for ptr in &[method_ptr, url_ptr, headers_ptr, body_ptr] {
            let _ = self.builder.build_call(free_fn, &[(*ptr).into()], "");
        }

        // Convert result CString -> String (from_cstring logic inline)
        let res_len_val = self.call_rt("strlen", &[result_ptr.into()])?;
        let res_len = res_len_val.try_as_basic_value().basic()
            .ok_or("strlen failed")?.into_int_value();
        let str_struct = self.call_rt("atomic_string_create", &[result_ptr.into(), res_len.into()])?;
        let str_val = str_struct.try_as_basic_value().basic()
            .ok_or("string_create failed")?;
        let alloca = self.builder.build_alloca(self.string_type, "http_resp")
            .map_err(llvm_err)?;
        self.builder.build_store(alloca, str_val).map_err(llvm_err)?;

        // Free C result string via atomic_http_free
        let http_free_fn = self.module.get_function("atomic_http_free")
            .unwrap();
        let _ = self.builder.build_call(http_free_fn, &[result_ptr.into()], "");

        Ok(TypedValue::Str(alloca))
    }
}
