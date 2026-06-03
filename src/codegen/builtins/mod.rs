// Submodule: builtins

mod stdlib;
mod enum_option;
mod lazy;
mod concurrency;
mod list_hof;
mod map_set;
mod string;

use atomic::ast::*;
use inkwell::values::{BasicMetadataValueEnum, IntValue, FloatValue, PointerValue, StructValue};
use inkwell::types::{BasicTypeEnum, BasicMetadataTypeEnum};
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, llvm_err, InnerType};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn compile_call(&mut self, func: &Expr, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        // Handle named function calls (including builtins)
        if let Expr::Ident(name) = func {
            // If this name is a function variable in scope, dispatch via indirect call
            // (takes precedence over builtins to allow passing builtins as function references)
            if let Some(scope_var) = self.scope.get(name) {
                if scope_var.kind == super::ValKind::Fn {
                    // Fall through to higher-order call path below
                    let target = self.compile_expr(func)?;
                    return self.compile_indirect_call(target, args, trailing);
                }
            }
            if name == "print" || name == "println" {
                return self.builtin_print(name, args);
            }
            if name == "__list" {
                return self.builtin_list(args);
            }
            if name == "lazy_list" {
                return self.builtin_lazy_list(args, trailing);
            }
            if name == "launch" {
                return self.builtin_launch(args, trailing);
            }
            if name == "coroutineScope" {
                return self.builtin_coroutine_scope(args, trailing);
            }
            if name == "delay" {
                return self.builtin_delay(args);
            }
            if name == "withTimeout" {
                return self.builtin_with_timeout(args, trailing);
            }
            // Stream<T> operations
            if name == "stream" {
                return self.builtin_stream_create();
            }
            if name == "send" || name == "receive" || name == "close" {
                return self.builtin_stream_op(name, args);
            }
            // Task<T> operations
            if name == "cancel" || name == "is_done" || name == "is_cancelled" || name == "wait" {
                return self.builtin_task_op(name, args);
            }
            if name == "len" || name == "is_empty" || name == "append" || name == "concat"
                || name == "to_upper" || name == "to_lower" || name == "trim"
                || name == "read_line" || name == "starts_with" || name == "ends_with"
                || name == "substring" || name == "parse_int"
                || name == "read_file" || name == "write_file"
                || name == "append_file" || name == "exists" || name == "delete_file"
                || name == "open_file" || name == "close_file" || name == "is_eof"
                || name == "file_read_line" || name == "file_read_bytes"
                || name == "file_write" || name == "file_write_line"
                || name == "file_flush" || name == "file_seek" || name == "file_tell"
                || name == "rand_int" || name == "rand_float"
                || name == "split" || name == "join" || name == "replace"
                || name == "abs" || name == "min" || name == "max"
                || name == "sqrt" || name == "cbrt"
                || name == "sin" || name == "cos" || name == "tan"
                || name == "asin" || name == "acos" || name == "atan" || name == "atan2"
                || name == "log" || name == "log2" || name == "log10" || name == "exp"
                || name == "floor" || name == "ceil" || name == "round"
                || name == "pi" || name == "e"
                || name == "clamp" || name == "is_nan" || name == "is_infinite"
                || name == "panic" || name == "assert" || name == "to_string"
                || name == "head" || name == "last" || name == "get"
                || name == "reverse" || name == "contains" || name == "contains_key" || name == "prepend"
                || name == "take" || name == "drop" || name == "range" || name == "repeat"
                || name == "trim_start" || name == "trim_end" || name == "string_contains"
                || name == "string_repeat" || name == "now" || name == "today"
                || name == "tail" || name == "zip" || name == "split_lines"
                || name == "index_of"
                || name == "year" || name == "month" || name == "day"
                || name == "hour" || name == "minute" || name == "second"
                || name == "add_days" || name == "add_hours"
                || name == "rand_choice" || name == "rand_shuffle"
                || name == "to_char" || name == "char_code"
                || name == "to_int" || name == "to_float"
                || name == "init" || name == "chars"
                || name == "set_to_list" || name == "set_from_list" || name == "from_list"
                || name == "with_index" || name == "unique" || name == "slice"
                || name == "flatten" || name == "split_at" || name == "chunks" || name == "windows"
                || name == "pow"
                || name == "map_keys" || name == "map_values" || name == "map_entries"
                || name == "map_union"
                || name == "set_union" || name == "set_intersection" || name == "set_difference"
                || name == "set_is_subset" || name == "set_insert" || name == "set_remove"
                || name == "rand_shuffle" || name == "sorted"
                || name == "read_dir"
                || name == "identity" || name == "compose"
                || name == "diff_days" || name == "weekday"
                || name == "sum" || name == "product" || name == "digits"
                || name == "char_at" || name == "is_alpha" || name == "code_to_char"
                || name == "now_utc" || name == "diff_seconds"
                || name == "flip" || name == "constant" || name == "uncurry" || name == "curry"
                || name == "is_some" || name == "is_none" || name == "is_ok" || name == "is_err"
                || name == "unwrap_or" || name == "unwrap" || name == "or_else" || name == "ok"
                || name == "to_lazy_list" || name == "lazy_take" || name == "lazy_drop"
                || name == "lazy_map" || name == "lazy_filter" || name == "lazy_take_while"
                || name == "lazy_head" || name == "lazy_zip" || name == "to_list"
                || name == "format" || name == "parse_date"
                || name == "date" || name == "datetime"
                || name == "Random_new" || name == "next_int"
                || name == "to_cstring" || name == "from_cstring"
                || name == "is_null" || name == "deref"
                || name == "to"
                || name == "httpRequest"
                || name == "ping"
            {
                // Handle trailing lambda for lazy_map/filter/take_while:
                // lazy_map(ll) { fn } → args becomes [fn, ll]
                if trailing.is_some() && (name == "lazy_map" || name == "lazy_filter" || name == "lazy_take_while") {
                    let mut new_args = vec![*trailing.clone().ok_or("no trailing lambda")?];
                    new_args.extend_from_slice(args);
                    return self.builtin_stdlib(name, &new_args);
                }
                return self.builtin_stdlib(name, args);
            }
            // Handle enum variant constructors: Some(42), Ok(val), Err(e), etc.
            if let Some((enum_info, variant)) = self.registry.lookup_variant(name).map(|(ei, vi)| (ei.clone(), vi.clone())) {
                if !variant.params.is_empty() {
                    return self.compile_enum_construct(&enum_info, &variant, args);
                }
                // Unit variant without args: simply construct
                if args.is_empty() {
                    return self.compile_enum_construct(&enum_info, &variant, &[]);
                }
                return Err(format!("Variant '{}' takes no arguments but {} were given", name, args.len()));
            }
            // Handle flatMap/flatMapResult for Option/Result inline (avoids untyped callback issues)
            if name == "flatMap" || name == "flatMapResult" || name == "flat_map" {
                let is_enum_op = if trailing.is_some() || args.len() >= 2 {
                    let enum_arg = if trailing.is_some() { &args[0] } else { &args[1] };
                    matches!(self.compile_expr(enum_arg), Ok(TypedValue::Enum(_, _, InnerType::Int, false)))
                } else {
                    false
                };
                if is_enum_op {
                    if name == "flatMap" {
                        return self.builtin_flat_map(args, trailing);
                    } else {
                        return self.builtin_flat_map_result(args, trailing);
                    }
                }
                // Not an enum op — fall through to module function lookup (stdlib)
            }
            if name == "map" || name == "filter" || name == "fold" {
                let list_arg_idx: Option<usize> = if name == "map" || name == "filter" {
                    if trailing.is_some() { Some(0) } else if args.len() >= 2 { Some(1) } else { None }
                } else if name == "fold" {
                    if trailing.is_some() && args.len() >= 2 { Some(1) } else if args.len() >= 3 { Some(1) } else { None }
                } else {
                    None
                };
                let is_list_op = list_arg_idx.map_or(false, |idx| {
                    idx < args.len() && matches!(self.compile_expr(&args[idx]), Ok(TypedValue::List(_)))
                });
                if is_list_op {
                    if name == "map" {
                        return self.builtin_map(args, trailing);
                    } else if name == "filter" {
                        return self.builtin_filter(args, trailing);
                    } else if name == "fold" {
                        return self.builtin_fold(args, trailing);
                    }
                }
                // Also check if it's an enum op (Option/Result map)
                if name == "map" {
                    let is_enum_op = if trailing.is_some() || args.len() >= 2 {
                        let enum_arg = if trailing.is_some() { &args[0] } else { &args[1] };
                        matches!(self.compile_expr(enum_arg), Ok(TypedValue::Enum(_, _, InnerType::Int, false)))
                    } else {
                        false
                    };
                    if is_enum_op {
                        return self.builtin_enum_map(args, trailing);
                    }
                }
            }
            // flat_map for lists: flat_map(fn, list) or flat_map(list) { lambda }
            if name == "flat_map" {
                let list_arg_idx: Option<usize> = if trailing.is_some() { Some(0) } else if args.len() >= 2 { Some(1) } else { None };
                let is_list_op = list_arg_idx.map_or(false, |idx| {
                    idx < args.len() && matches!(self.compile_expr(&args[idx]), Ok(TypedValue::List(_)))
                });
                if is_list_op {
                    return self.builtin_flat_map_list(args, trailing);
                }
            }
            // Callback-based list functions
            if name == "any" || name == "all" || name == "find" || name == "find_index"
                || name == "reduce" || name == "fold_right" || name == "take_while"
                || name == "drop_while" || name == "sorted_by"
                || name == "partition" || name == "count"
            {
                let list_arg_idx: Option<usize> = if name == "fold_right" {
                    if trailing.is_some() && args.len() >= 2 { Some(1) } else if args.len() >= 3 { Some(1) } else { None }
                } else {
                    if trailing.is_some() { Some(0) } else if args.len() >= 2 { Some(1) } else { None }
                };
                let is_list_op = list_arg_idx.map_or(false, |idx| {
                    idx < args.len() && matches!(self.compile_expr(&args[idx]), Ok(TypedValue::List(_)))
                });
                if is_list_op {
                    return self.builtin_callback_list(name, args, trailing);
                }
            }
            // Callback-based map functions
            if name == "map_filter" || name == "map_map_values" || name == "map_fold" {
                // Find which argument is a Map
                let map_idx = (0..args.len()).find(|&i| {
                    self.compile_expr(&args[i]).map_or(false, |v| matches!(v, TypedValue::Map(_)))
                });
                if map_idx.is_some() {
                    return self.builtin_callback_map(name, args, trailing);
                }
            }

            // Check if it's an enum variant constructor: Some(42), None, etc.
            let variant_info = self.registry.lookup_variant(name)
                .map(|(ei, vi)| (ei.clone(), vi.clone()));
            if let Some((enum_info, variant)) = variant_info {
                return self.compile_enum_construct(&enum_info, &variant, args);
            }

            // Try overloaded dispatch first if the name has overloads
            if let Some(overloads) = self.overloaded_functions.get(name).cloned() {
                // Compile args to determine their runtime types
                let arg_vals: Vec<TypedValue<'ctx>> = args.iter()
                    .map(|a| self.compile_expr(a))
                    .collect::<Result<_, _>>()?;

                // Map TypedValue to type name for mangling
                let arg_type_names: Vec<String> = arg_vals.iter()
                    .map(|v| self.typed_value_type_name(v))
                    .collect();
                let mangled = if arg_type_names.is_empty() {
                    name.clone()
                } else {
                    format!("{}_{}", name, arg_type_names.join("_"))
                };

                // Find matching overload
                let fn_name = overloads.iter()
                    .find(|(_, mn)| *mn == mangled)
                    .map(|(_, mn)| mn)
                    .or_else(|| {
                        // Exact match not found; try fallback: if all args are Int,
                        // it might be an untyped call — use the first overload
                        overloads.first().map(|(_, mn)| mn)
                    })
                    .ok_or_else(|| format!(
                        "No matching overload of '{}' for argument types: {:?}",
                        name, arg_type_names
                    ))?;

                let fn_val = self.module.get_function(fn_name)
                    .ok_or_else(|| format!("Overloaded function '{}' not found", fn_name))?;
                let fn_type = fn_val.get_type();
                let param_tys = fn_type.get_param_types();
                let mut ca: Vec<BasicMetadataValueEnum> = Vec::new();
                for (i, av) in arg_vals.iter().enumerate() {
                    let bv = av.to_bv().unwrap_or_else(|| {
                        // For complex types, we need to load from alloca
                        match av {
                            TypedValue::Str(ptr) => {
                                let ld = self.builder.build_load(self.string_type, *ptr, "arg_str").unwrap();
                                ld.into()
                            }
                            TypedValue::List(ptr) | TypedValue::Map(ptr) | TypedValue::Set(ptr) => {
                                let ld = self.builder.build_load(self.list_type, *ptr, "arg_list").unwrap();
                                ld.into()
                            }
                            TypedValue::LazyList(ptr) => {
                                let ld = self.builder.build_load(self.lazylist_type, *ptr, "arg_ll").unwrap();
                                ld.into()
                            }
                            TypedValue::Task(ptr) => {
                                let ld = self.builder.build_load(self.task_type, *ptr, "arg_task").unwrap();
                                ld.into()
                            }
                            TypedValue::Stream(ptr) => {
                                // Stream is a heap pointer; extract list from field 1 for arg passing
                                let lf = self.builder.build_struct_gep(self.stream_type, *ptr, 3, "arg_slf").unwrap();
                                let ld = self.builder.build_load(self.list_type, lf, "arg_sl").unwrap();
                                ld.into()
                            }
                            TypedValue::Struct(ptr, st) => {
                                let ld = self.builder.build_load(*st, *ptr, "arg_struct").unwrap();
                                ld.into()
                            }
                            TypedValue::Enum(ptr, et, ..) => {
                                let ld = self.builder.build_load(*et, *ptr, "arg_enum").unwrap();
                                ld.into()
                            }
                            TypedValue::CString(p) | TypedValue::Ptr(p) | TypedValue::FileHandle(p) => {
                                (*p).into()
                            }
                            _ => {
                                // Fallback: use zero int
                                self.i64_ty().const_int(0, false).into()
                            }
                        }
                    });
                    let casted = self.coerce_arg(bv, param_tys.get(i))?;
                    ca.push(casted.into());
                }
                if let Some(lam) = trailing {
                    let bv = self.compile_and_load(lam)?;
                    let casted = self.coerce_arg(bv, param_tys.get(args.len()))?;
                    ca.push(casted.into());
                }

                let cc = self.builder.build_call(fn_val, &ca, "").map_err(llvm_err)?;
                return match cc.try_as_basic_value().basic() {
                    Some(bv) => self.bv_to_typed(bv),
                    None => Ok(TypedValue::Unit),
                };
            }

            // Try direct call if function exists in module
            if self.module.get_function(name).is_some() {
                let fn_val = self.module.get_function(name).unwrap();
                let fn_type = fn_val.get_type();
                let param_tys = fn_type.get_param_types();
                let mut ca: Vec<BasicMetadataValueEnum> = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    let bv = self.compile_and_load(a)?;
                    let casted = self.coerce_arg(bv, param_tys.get(i))?;
                    ca.push(casted.into());
                }
                if let Some(lam) = trailing {
                    let bv = self.compile_and_load(lam)?;
                    let casted = self.coerce_arg(bv, param_tys.get(args.len()))?;
                    ca.push(casted.into());
                }

                let cc = self.builder.build_call(fn_val, &ca, "").map_err(llvm_err)?;
                return match cc.try_as_basic_value().basic() {
                    Some(bv) => self.bv_to_typed(bv),
                    None => Ok(TypedValue::Unit),
                };
            }
            // Not a module function - fall through to higher-order path (it might be a variable holding a lambda)
        }

        // Module-qualified call: module.function(args) → module_function(args)
        if let Expr::FieldAccess(module_expr, method) = func {
            if let Expr::Ident(module_name) = module_expr.as_ref() {
                // List.of(...) → List[...] (equivalent to list literal)
                if module_name == "List" && method == "of" {
                    return self.builtin_list(args);
                }
                // Set.of(...) → Set literal
                if module_name == "Set" && method == "of" {
                    return self.builtin_set_of(args);
                }
                let mangled = format!("{}_{}", module_name, method);
                // Check if mangled name is a builtin
                if mangled == "Random_new" || mangled == "Random_next_int" {
                    let new_func = Expr::Ident(mangled);
                    return self.compile_call(&new_func, args, trailing);
                }
                if self.module.get_function(&mangled).is_some() {
                    let fn_val = self.module.get_function(&mangled).unwrap();
                    let fn_type = fn_val.get_type();
                    let param_tys = fn_type.get_param_types();
                    let mut ca: Vec<BasicMetadataValueEnum> = Vec::new();
                    for (i, a) in args.iter().enumerate() {
                        let bv = self.compile_and_load(a)?;
                        let casted = self.coerce_arg(bv, param_tys.get(i))?;
                        ca.push(casted.into());
                    }
                    if let Some(lam) = trailing {
                        let bv = self.compile_and_load(lam)?;
                        let casted = self.coerce_arg(bv, param_tys.get(args.len()))?;
                        ca.push(casted.into());
                    }
                    let cc = self.builder.build_call(fn_val, &ca, "").map_err(llvm_err)?;
                    return match cc.try_as_basic_value().basic() {
                        Some(bv) => self.bv_to_typed(bv),
                        None => Ok(TypedValue::Unit),
                    };
                }
            }
        }

        // UFCS method call: receiver.method(args) → TypeName_method(receiver, args)
        if let Expr::FieldAccess(receiver, method) = func {
            let recv_val = self.compile_expr(receiver)?;
            let type_name = self.type_name_from_typed_value(&recv_val);

            // Handle Map builtin methods inline
            if matches!(recv_val, TypedValue::Map(_)) {
                if method == "insert" {
                    return self.builtin_map_insert(receiver, args);
                }
                if method == "remove" {
                    return self.builtin_map_remove(receiver, args);
                }
                if method == "contains" {
                    return self.builtin_map_contains(receiver, args);
                }
                if method == "len" || method == "is_empty" {
                    let new_func = Expr::Ident(method.to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                }
                if method == "keys" {
                    let new_func = Expr::Ident("map_keys".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                }
                if method == "values" {
                    // map.values() -> get all values as a list
                    let new_func = Expr::Ident("map_values".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                }
                if method == "map_values" {
                    // map.map_values(transform) -> map_map_values(map, transform)
                    let new_func = Expr::Ident("map_map_values".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], trailing);
                }
                if method == "entries" {
                    let new_func = Expr::Ident("map_entries".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                }
                if method == "union" {
                    if args.len() != 1 { return Err("map.union expects 1 argument (other map)".to_string()); }
                    let new_func = Expr::Ident("map_union".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                }
                if method == "filter" {
                    // map.filter(predicate) -> map_filter(map, predicate)
                    let new_func = Expr::Ident("map_filter".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], trailing);
                }
                if method == "fold" {
                    // map.fold(init, folder) -> map_fold(map, init, folder)
                    let new_func = Expr::Ident("map_fold".to_string());
                    let mut new_args = vec![receiver.as_ref().clone()];
                    new_args.extend(args.iter().cloned());
                    return self.compile_call(&new_func, &new_args, trailing);
                }
            }
            // Handle Set builtin methods inline
            if matches!(recv_val, TypedValue::Set(_)) {
                if method == "insert" {
                    return self.builtin_set_insert(receiver, args);
                }
                if method == "remove" {
                    return self.builtin_set_remove(receiver, args);
                }
                if method == "contains" {
                    return self.builtin_set_contains(receiver, args);
                }
                if method == "len" || method == "is_empty" {
                    let new_func = Expr::Ident(method.to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                }
                if method == "union" {
                    if args.len() != 1 { return Err("set.union expects 1 argument (other set)".to_string()); }
                    let new_func = Expr::Ident("set_union".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                }
                if method == "intersection" {
                    if args.len() != 1 { return Err("set.intersection expects 1 argument (other set)".to_string()); }
                    let new_func = Expr::Ident("set_intersection".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                }
                if method == "difference" {
                    if args.len() != 1 { return Err("set.difference expects 1 argument (other set)".to_string()); }
                    let new_func = Expr::Ident("set_difference".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                }
                if method == "is_subset" {
                    if args.len() != 1 { return Err("set.is_subset expects 1 argument (other set)".to_string()); }
                    let new_func = Expr::Ident("set_is_subset".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                }
                if method == "to_list" {
                    let new_func = Expr::Ident("to_list".to_string());
                    return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                }
            }
            // Handle Range builtin methods inline (range is a Struct with 3 i64 fields)
            if let TypedValue::Struct(_, st) = &recv_val {
                if *st == self.range_type {
                    match method.as_str() {
                        "contains" => {
                            if args.len() != 1 { return Err("range.contains expects 1 argument".to_string()); }
                            return self.builtin_range_contains(receiver, &args[0]);
                        }
                        "toList" => {
                            if !args.is_empty() { return Err("range.toList expects no arguments".to_string()); }
                            return self.builtin_range_to_list(receiver);
                        }
                        _ => return Err(format!("Method '{}' not found on Range", method)),
                    }
                }
            }
            // Handle Option/Result builtin methods inline
            if matches!(recv_val, TypedValue::Enum(..)) {
                match method.as_str() {
                    "is_some" | "is_none" | "is_ok" | "is_err" => {
                        let new_func = Expr::Ident(method.to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    "unwrap_or" => {
                        if args.len() != 1 { return Err("unwrap_or expects 1 argument".to_string()); }
                        let new_func = Expr::Ident("unwrap_or".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                    }
                    "unwrap" => {
                        let new_func = Expr::Ident("unwrap".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    "or_else" => {
                        if args.len() != 1 { return Err("or_else expects 1 argument".to_string()); }
                        let new_func = Expr::Ident("or_else".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                    }
                    "ok" => {
                        if args.len() != 1 { return Err("ok expects 1 argument (error value)".to_string()); }
                        let new_func = Expr::Ident("ok".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                    }
                    "map" | "flat_map" => {
                        let new_func = Expr::Ident(method.to_string());
                        let mut new_args = vec![receiver.as_ref().clone()];
                        new_args.extend(args.iter().cloned());
                        return self.compile_call(&new_func, &new_args, trailing);
                    }
                    _ => return Err(format!("Method '{}' not found on Option/Result", method)),
                }
            }
            // Handle LazyList builtin methods inline
            if matches!(recv_val, TypedValue::LazyList(_)) {
                match method.as_str() {
                    "to_list" => {
                        let new_func = Expr::Ident("to_list".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    "to_lazy_list" => {
                        let new_func = Expr::Ident("to_lazy_list".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    "take" => {
                        if args.len() != 1 { return Err("lazy.take expects 1 argument (n)".to_string()); }
                        let new_func = Expr::Ident("lazy_take".to_string());
                        return self.compile_call(&new_func, &[args[0].clone(), receiver.as_ref().clone()], &None);
                    }
                    "drop" => {
                        if args.len() != 1 { return Err("lazy.drop expects 1 argument (n)".to_string()); }
                        let new_func = Expr::Ident("lazy_drop".to_string());
                        return self.compile_call(&new_func, &[args[0].clone(), receiver.as_ref().clone()], &None);
                    }
                    "map" => {
                        let new_func = Expr::Ident("lazy_map".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], trailing);
                    }
                    "filter" => {
                        let new_func = Expr::Ident("lazy_filter".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], trailing);
                    }
                    "take_while" => {
                        let new_func = Expr::Ident("lazy_take_while".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], trailing);
                    }
                    "head" => {
                        let new_func = Expr::Ident("lazy_head".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    "zip" => {
                        if args.len() != 1 { return Err("lazy.zip expects 1 argument (other)".to_string()); }
                        let new_func = Expr::Ident("lazy_zip".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                    }
                    _ => return Err(format!("Method '{}' not found on LazyList", method)),
                }
            }
            // Handle String builtin methods inline
            if matches!(recv_val, TypedValue::Str(_)) {
                match method.as_str() {
                    // No-arg methods
                    "len" | "is_empty" | "to_upper" | "to_lower" | "trim"
                    | "trim_start" | "trim_end" | "chars" | "split_lines"
                    | "to_int" | "to_float" => {
                        let new_func = Expr::Ident(method.to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    // Single-arg methods (method(string, arg))
                    "split" | "starts_with" | "ends_with" | "index_of"
                    | "replace" | "slice" | "repeat" | "contains" => {
                        if args.len() != 1 { return Err(format!("string.{} expects 1 argument", method)); }
                        let mapped = match method.as_str() {
                            "contains" => "string_contains",
                            "repeat" => "string_repeat",
                            "slice" => "slice",
                            other => other,
                        };
                        let new_func = Expr::Ident(mapped.to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                    }
                    // substring(string, start, len)
                    "substring" => {
                        if args.len() != 2 { return Err("string.substring expects 2 arguments (start, length)".to_string()); }
                        let new_func = Expr::Ident("substring".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone(), args[1].clone()], &None);
                    }
                    "join" => {
                        // string.join(list) = join(string, list)
                        if args.len() != 1 { return Err("string.join expects 1 argument (list)".to_string()); }
                        let new_func = Expr::Ident("join".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                    }
                    "to_cstring" => {
                        let new_func = Expr::Ident("to_cstring".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    _ => return Err(format!("Method '{}' not found on String", method)),
                }
            }
            // Handle Ptr/CString builtin methods inline
            if matches!(recv_val, TypedValue::Ptr(_) | TypedValue::CString(_) | TypedValue::FileHandle(_)) {
                match method.as_str() {
                    "is_null" => {
                        let new_func = Expr::Ident("is_null".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    "deref" => {
                        let new_func = Expr::Ident("deref".to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    _ => return Err(format!("Method '{}' not found on Ptr/CString", method)),
                }
            }
            // Handle Stream builtin methods inline
            if matches!(recv_val, TypedValue::Stream(_)) {
                match method.as_str() {
                    "send" => {
                        if args.len() != 1 { return Err("stream.send expects 1 argument: value".to_string()); }
                        let stream_ptr = match recv_val {
                            TypedValue::Stream(p) => p,
                            _ => unreachable!(),
                        };
                        let value = self.compile_expr(&args[0])?;
                        // Lock mutex (field 0)
                        let mutex_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 0, "sm").map_err(llvm_err)?;
                        let lock_fn = self.module.get_function("pthread_mutex_lock").unwrap();
                        let _ = self.builder.build_call(lock_fn, &[mutex_ptr.into()], "").map_err(llvm_err)?;
                        // Push to list (field 3)
                        let list_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 3, "sl").map_err(llvm_err)?;
                        self.push_to_collector(list_ptr, &value)?;
                        // Signal condvar to wake up waiting receivers
                        let cond_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 1, "sc").map_err(llvm_err)?;
                        let cond_sig_fn = self.module.get_function("pthread_cond_signal").unwrap();
                        let _ = self.builder.build_call(cond_sig_fn, &[cond_ptr.into()], "").map_err(llvm_err)?;
                        // Unlock mutex
                        let unlock_fn = self.module.get_function("pthread_mutex_unlock").unwrap();
                        let _ = self.builder.build_call(unlock_fn, &[mutex_ptr.into()], "").map_err(llvm_err)?;
                        return Ok(TypedValue::Unit);
                    }
                    "receive" => {
                        let stream_ptr = match recv_val {
                            TypedValue::Stream(p) => p,
                            _ => unreachable!(),
                        };
                        let zero = self.i64_ty().const_int(0, false);
                        let one = self.i64_ty().const_int(1, false);
                        let cur_fn = self.builder.get_insert_block().ok_or("no insert block")?.get_parent().ok_or("no current fn")?;
                        let result_alloca = self.builder.build_alloca(self.i64_ty(), "ufcs_recv_result").map_err(llvm_err)?;
                        let lock_fn = self.module.get_function("pthread_mutex_lock").unwrap();
                        let unlock_fn = self.module.get_function("pthread_mutex_unlock").unwrap();
                        let cond_wait_fn = self.module.get_function("pthread_cond_wait").unwrap();
                        let mutex_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 0, "rm").map_err(llvm_err)?;
                        let cond_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 1, "rc").map_err(llvm_err)?;
                        let closed_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 2, "rc_closed").map_err(llvm_err)?;
                        let list_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 3, "rl").map_err(llvm_err)?;
                        let merge_bb = self.context.append_basic_block(cur_fn, "ufcs_merge");
                        let _ = self.builder.build_call(lock_fn, &[mutex_ptr.into()], "").map_err(llvm_err)?;
                        // Wait loop: while list is empty and not closed, cond_wait
                        let wait_loop_bb = self.context.append_basic_block(cur_fn, "stream_wait_loop");
                        let got_data_bb = self.context.append_basic_block(cur_fn, "stream_got_data");
                        let empty_closed_bb = self.context.append_basic_block(cur_fn, "stream_empty_closed");
                        let _ = self.builder.build_unconditional_branch(wait_loop_bb);
                        self.builder.position_at_end(wait_loop_bb);
                        let list_val = self.load_list(list_ptr)?;
                        let len = self.builder.build_extract_value(list_val, 1, "len").map_err(llvm_err)?.into_int_value();
                        let has_data = self.builder.build_int_compare(IntPredicate::SGT, len, zero, "has_data").map_err(llvm_err)?;
                        let _ = self.builder.build_conditional_branch(has_data, got_data_bb, empty_closed_bb);
                        // Empty: check if closed
                        self.builder.position_at_end(empty_closed_bb);
                        let closed_val = self.builder.build_load(self.i64_ty(), closed_ptr, "closed_val").map_err(llvm_err)?.into_int_value();
                        let is_closed = self.builder.build_int_compare(IntPredicate::NE, closed_val, zero, "is_closed").map_err(llvm_err)?;
                        let do_wait_bb = self.context.append_basic_block(cur_fn, "do_cond_wait");
                        let return_zero_bb = self.context.append_basic_block(cur_fn, "ret_closed");
                        let _ = self.builder.build_conditional_branch(is_closed, return_zero_bb, do_wait_bb);
                        self.builder.position_at_end(do_wait_bb);
                        let _ = self.builder.build_call(cond_wait_fn, &[cond_ptr.into(), mutex_ptr.into()], "").map_err(llvm_err)?;
                        let _ = self.builder.build_unconditional_branch(wait_loop_bb);
                        // Return 0 when closed & empty
                        self.builder.position_at_end(return_zero_bb);
                        let _ = self.builder.build_call(unlock_fn, &[mutex_ptr.into()], "").map_err(llvm_err)?;
                        self.builder.build_store(result_alloca, zero).map_err(llvm_err)?;
                        let _ = self.builder.build_unconditional_branch(merge_bb);
                        // Got data: extract, shift, unlock
                        self.builder.position_at_end(got_data_bb);
                        let lv2 = self.load_list(list_ptr)?;
                        let fat = self.call_rt("atomic_list_get", &[lv2.into(), zero.into()])?;
                        let fat = fat.try_as_basic_value().basic().ok_or("receive get failed")?.into_struct_value();
                        let tag = self.builder.build_extract_value(fat, 0, "tag").map_err(llvm_err)?.into_int_value();
                        let data_ptr = self.builder.build_extract_value(lv2, 0, "data").map_err(llvm_err)?.into_pointer_value();
                        let len2 = self.builder.build_extract_value(lv2, 1, "len").map_err(llvm_err)?.into_int_value();
                        let cap = self.builder.build_extract_value(lv2, 2, "cap").map_err(llvm_err)?.into_int_value();
                        let new_len = self.builder.build_int_sub(len2, one, "new_len").map_err(llvm_err)?;
                        let has_more = self.builder.build_int_compare(IntPredicate::SGT, len2, one, "has_more").map_err(llvm_err)?;
                        let shift_bb = self.context.append_basic_block(cur_fn, "shift_bb");
                        let done_bb = self.context.append_basic_block(cur_fn, "shift_done");
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
                        // Merge: load result
                        self.builder.position_at_end(merge_bb);
                        let result = self.builder.build_load(self.i64_ty(), result_alloca, "ufcs_load_result").map_err(llvm_err)?.into_int_value();
                        return Ok(TypedValue::Int(result));
                    }
                    "close" => {
                        let stream_ptr = match recv_val {
                            TypedValue::Stream(p) => p,
                            _ => unreachable!(),
                        };
                        let mutex_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 0, "cm").map_err(llvm_err)?;
                        let _ = self.builder.build_call(self.module.get_function("pthread_mutex_lock").unwrap(), &[mutex_ptr.into()], "").map_err(llvm_err)?;
                        let closed_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 2, "cc").map_err(llvm_err)?;
                        self.builder.build_store(closed_ptr, self.i64_ty().const_int(1, false)).map_err(llvm_err)?;
                        let cond_ptr = self.builder.build_struct_gep(self.stream_type, stream_ptr, 1, "ccond").map_err(llvm_err)?;
                        let _ = self.builder.build_call(self.module.get_function("pthread_cond_broadcast").unwrap(), &[cond_ptr.into()], "").map_err(llvm_err)?;
                        let _ = self.builder.build_call(self.module.get_function("pthread_mutex_unlock").unwrap(), &[mutex_ptr.into()], "").map_err(llvm_err)?;
                        return Ok(TypedValue::Unit);
                    }
                    _ => return Err(format!("Method '{}' not found on Stream", method)),
                }
            }
            // Handle Task builtin methods inline
            // Task struct: {pthread: i64, done: i64, cancelled: i64, result_list: {ptr, i64, i64}}
            if matches!(recv_val, TypedValue::Task(_)) {
                let task_ptr = match recv_val {
                    TypedValue::Task(p) => p,
                    _ => unreachable!(),
                };
                let task_val = self.builder.build_load(self.task_type, task_ptr, "task_val").map_err(llvm_err)?.into_struct_value();
                match method.as_str() {
                    "cancel" => {
                        let cancelled_one = self.i64_ty().const_int(1, false);
                        let updated = self.builder.build_insert_value(task_val, cancelled_one, 2, "t_canc_set").map_err(llvm_err)?;
                        self.builder.build_store(task_ptr, updated).map_err(llvm_err)?;
                        return Ok(TypedValue::Unit);
                    }
                    "is_done" => {
                        let done = self.builder.build_extract_value(task_val, 1, "is_done").map_err(llvm_err)?.into_int_value();
                        let is_true = self.builder.build_int_compare(IntPredicate::NE, done, self.i64_ty().const_int(0, false), "done_bool").map_err(llvm_err)?;
                        return Ok(TypedValue::Bool(is_true));
                    }
                    "is_cancelled" => {
                        let cancelled = self.builder.build_extract_value(task_val, 2, "is_canc").map_err(llvm_err)?.into_int_value();
                        let is_true = self.builder.build_int_compare(IntPredicate::NE, cancelled, self.i64_ty().const_int(0, false), "canc_bool").map_err(llvm_err)?;
                        return Ok(TypedValue::Bool(is_true));
                    }
                    "wait" => {
                        // pthread_join then reload task (thread updates result_list)
                        let pthread_val = self.builder.build_extract_value(task_val, 0, "pt").map_err(llvm_err)?.into_int_value();
                        let pthread_join_fn = self.module.get_function("pthread_join").unwrap();
                        let null_ptr = self.ptr_ty().const_null();
                        let _ = self.builder.build_call(pthread_join_fn, &[pthread_val.into(), null_ptr.into()], "").map_err(llvm_err)?;
                        let task_val2 = self.builder.build_load(self.task_type, task_ptr, "task_val2").map_err(llvm_err)?.into_struct_value();
                        let result_list = self.builder.build_extract_value(task_val2, 4, "wait_list").map_err(llvm_err)?.into_struct_value();
                        let list_alloca = self.builder.build_alloca(self.list_type, "wait_l").map_err(llvm_err)?;
                        self.builder.build_store(list_alloca, result_list).map_err(llvm_err)?;
                        let list_val = self.load_list(list_alloca)?;
                        let zero = self.i64_ty().const_int(0, false);
                        let cc = self.call_rt("atomic_list_get", &[list_val.into(), zero.into()])?;
                        let fat = cc.try_as_basic_value().basic().ok_or("wait get failed")?.into_struct_value();
                        let tag = self.builder.build_extract_value(fat, 0, "tag").map_err(llvm_err)?.into_int_value();
                        return Ok(TypedValue::Int(tag));
                    }
                    _ => return Err(format!("Method '{}' not found on Task", method)),
                }
            }
            // Handle List builtin methods inline — UFCS: list.method(args) ≡ method(list, args...)
            if matches!(recv_val, TypedValue::List(_) | TypedValue::LazyList(_)) {
                match method.as_str() {
                    // No-arg methods: f(list)
                    "len" | "is_empty" | "head" | "last" | "tail" | "init"
                    | "reverse" | "sum" | "product" | "sorted" | "flatten"
                    | "unique" | "to_list" | "to_lazy_list" => {
                        let new_func = Expr::Ident(method.to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], &None);
                    }
                    // Single-arg methods: f(list, arg) — dispatch to builtin_stdlib
                    "get" | "contains" | "take" | "drop" | "append" | "prepend"
                    | "index_of" | "slice" | "split_at" | "chunks" | "windows"
                    | "repeat" | "with_index" | "zip" | "count" | "partition" => {
                        if args.len() != 1 { return Err(format!("list.{} expects 1 argument", method)); }
                        let new_func = Expr::Ident(method.to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone(), args[0].clone()], &None);
                    }
                    // map, filter, fold, any, all, find, reduce, fold_right, take_while, drop_while, flat_map, sorted_by
                    "map" | "filter" | "any" | "all" | "find" | "reduce"
                    | "take_while" | "drop_while" | "flat_map" | "fold_right"
                    | "sorted_by" | "find_index" => {
                        let new_func = Expr::Ident(method.to_string());
                        return self.compile_call(&new_func, &[receiver.as_ref().clone()], trailing);
                    }
                    "fold" => {
                        if args.len() < 1 { return Err("list.fold expects at least 1 argument (init)".to_string()); }
                        let new_func = Expr::Ident("fold".to_string());
                        let mut new_args = vec![receiver.as_ref().clone()];
                        new_args.extend(args.iter().cloned());
                        return self.compile_call(&new_func, &new_args, trailing);
                    }
                    _ => return Err(format!("Method '{}' not found on List", method)),
                }
            }

            let lookup_key = format!("{}.{}", type_name, method);
            if let Some(fn_name) = self.extension_methods.get(&lookup_key).cloned() {
                let fn_val = self.module.get_function(&fn_name)
                    .ok_or_else(|| format!("Extension method '{}' not found", fn_name))?;
                let fn_type = fn_val.get_type();
                let param_tys = fn_type.get_param_types();
                let mut ca: Vec<BasicMetadataValueEnum> = Vec::new();
                let recv_bv = self.compile_and_load(receiver)?;
                let casted_recv = self.coerce_arg(recv_bv, param_tys.first())?;
                ca.push(casted_recv.into());
                for (i, a) in args.iter().enumerate() {
                    let bv = self.compile_and_load(a)?;
                    let casted = self.coerce_arg(bv, param_tys.get(i + 1))?;
                    ca.push(casted.into());
                }
                if let Some(lam) = trailing {
                    let bv = self.compile_and_load(lam)?;
                    let casted = self.coerce_arg(bv, param_tys.get(args.len() + 1))?;
                    ca.push(casted.into());
                }
                let cc = self.builder.build_call(fn_val, &ca, "").map_err(llvm_err)?;
                return match cc.try_as_basic_value().basic() {
                    Some(bv) => self.bv_to_typed(bv),
                    None => Ok(TypedValue::Unit),
                };
            }
            // If receiver is Map/Set/Stream/Task and no builtin/extension method matched, error out
            if matches!(recv_val, TypedValue::Map(_) | TypedValue::Set(_) | TypedValue::Stream(_) | TypedValue::Task(_)) {
                return Err(format!("Method '{}' not found on type '{}'", method, type_name));
            }
        }

        // Higher-order call: compile the call target expression
        let target = self.compile_expr(func)?;
        self.compile_indirect_call(target, args, trailing)
    }

    /// Perform an indirect function call through a TypedValue::Fn or TypedValue::Int.
    pub(super) fn compile_indirect_call(&mut self, target: TypedValue<'ctx>, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        match target {
            TypedValue::Fn(fn_ptr, fn_type) => {
                let mut ca: Vec<BasicMetadataValueEnum> = Vec::new();
                for a in args {
                    let bv = self.compile_and_load(a)?;
                    ca.push(bv.into());
                }
                if let Some(lam) = trailing {
                    let bv = self.compile_and_load(lam)?;
                    ca.push(bv.into());
                }

                let cc = self.builder.build_indirect_call(fn_type, fn_ptr, &ca, "indirect")
                    .map_err(llvm_err)?;
                match cc.try_as_basic_value().basic() {
                    Some(bv) => self.unpack_fat_return(bv, fn_type.get_return_type()),
                    None => Ok(TypedValue::Unit),
                }
            }
            // Handle untyped parameters (fallback to Int) used as function callbacks.
            // Use fat return type to preserve enum/string/struct values through the
            // untyped boundary. The named fat_return_type is distinct from enum types,
            // so bv_to_typed won't confuse packed scalars with enums.
            TypedValue::Int(iv) => {
                let total_args = args.len() + trailing.as_ref().map_or(0, |_| 1);
                let param_tys: Vec<BasicMetadataTypeEnum<'ctx>> =
                    (0..total_args).map(|_| self.i64_ty().into()).collect();
                let ret_ty = self.fat_return_type;
                let fn_type = ret_ty.fn_type(&param_tys, false);
                let ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
                let fn_ptr = self.builder.build_int_to_ptr(iv, ptr_type, "fn_ptr_cast")
                    .map_err(llvm_err)?;
                let mut ca: Vec<BasicMetadataValueEnum> = Vec::new();
                for a in args {
                    let bv = self.compile_and_load(a)?;
                    ca.push(bv.into());
                }
                if let Some(lam) = trailing {
                    let bv = self.compile_and_load(lam)?;
                    ca.push(bv.into());
                }
                let cc = self.builder.build_indirect_call(fn_type, fn_ptr, &ca, "indirect_untyped")
                    .map_err(llvm_err)?;
                match cc.try_as_basic_value().basic() {
                    Some(bv) => self.unpack_fat_return(bv, Some(BasicTypeEnum::StructType(ret_ty))),
                    None => Ok(TypedValue::Unit),
                }
            }
            _ => Err("Call target is not a function".to_string()),
        }
    }

    pub(super) fn builtin_print(&mut self, name: &str, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.is_empty() {
            if name == "println" { let _ = self.call_rt("atomic_println", &[]); }
            return Ok(TypedValue::Unit);
        }
        let v = self.compile_expr(&args[0])?;
        match &v {
            TypedValue::Int(_) => { if let Some(bv) = v.to_bv() { let _ = self.call_rt("atomic_print_int", &[bv.into()]); } }
            TypedValue::Float(_) => { if let Some(bv) = v.to_bv() { let _ = self.call_rt("atomic_print_float", &[bv.into()]); } }
            TypedValue::Bool(_) => { if let Some(bv) = v.to_bv() { let _ = self.call_rt("atomic_print_bool", &[bv.into()]); } }
            TypedValue::Str(ptr) => { let _ = self.call_rt_with_str("atomic_print_string", *ptr); }
            TypedValue::Fn(_, _) => { /* print fn pointer as int */ if let Some(bv) = v.to_bv() { let _ = self.call_rt("atomic_print_int", &[bv.into()]); } }
            TypedValue::List(ptr) | TypedValue::Set(ptr) | TypedValue::Map(ptr) => {
                let list = self.load_list(*ptr)?;
                let _ = self.call_rt("atomic_list_print", &[list.into()]);
            }
            TypedValue::Task(ptr) => {
                let task_val = self.builder.build_load(self.task_type, *ptr, "print_task").map_err(llvm_err)?;
                let _ = self.call_rt("atomic_print_task", &[task_val.into()]);
            }
            TypedValue::Stream(ptr) => {
                // Stream is {mutex, cond, closed, list}; load list from field 3
                let list_field = self.builder.build_struct_gep(self.stream_type, *ptr, 3, "print_sl_field").map_err(llvm_err)?;
                let list_val = self.builder.build_load(self.list_type, list_field, "print_sl").map_err(llvm_err)?;
                let _ = self.call_rt("atomic_list_print", &[list_val.into()]);
            }
            TypedValue::LazyList(ptr) => {
                let list_val = self.builder.build_load(self.list_type, *ptr, "print_ll").map_err(llvm_err)?;
                let _ = self.call_rt("atomic_list_print", &[list_val.into()]);
            }
            TypedValue::CString(_p) | TypedValue::Ptr(_p) | TypedValue::FileHandle(_p) => {
                // Print pointer value as hex
                if let Some(bv) = v.to_bv() { let _ = self.call_rt("atomic_print_int", &[bv.into()]); }
            }
            TypedValue::Struct(_, _) => {
                let _ = self.call_rt("atomic_print_struct", &[]);
            }
            TypedValue::Enum(ptr, _, inner_type, _) => {
                let enum_st = self.context.struct_type(
                    &[self.i64_ty().into(), self.ptr_ty().into()], false,
                );
                let loaded = self.builder.build_load(enum_st, *ptr, "print_enum_ld")
                    .map_err(llvm_err)?;
                if *inner_type == InnerType::Float {
                    let _ = self.call_rt("atomic_print_enum_float", &[loaded.into()]);
                } else {
                    let _ = self.call_rt("atomic_print_enum", &[loaded.into()]);
                }
            }
            TypedValue::Unit => {}
        }
        if name == "println" { let _ = self.call_rt("atomic_println", &[]); }
        Ok(TypedValue::Unit)
    }

    pub(super) fn builtin_list(&mut self, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        let len = self.i64_ty().const_int(args.len() as u64, false);
        let cc = self.call_rt("atomic_list_create", &[len.into()])?;
        let list_bv = cc.try_as_basic_value().basic()
            .ok_or("list_create failed")?;
        let list_alloca = self.builder.build_alloca(self.list_type, "list_tmp").map_err(llvm_err)?;
        self.builder.build_store(list_alloca, list_bv).map_err(llvm_err)?;

        for arg in args {
            let v = self.compile_expr(arg)?;
            let elem_fat = self.to_fat_struct(&v)?;
            let list_val = self.load_list(list_alloca)?;
            let cc = self.call_rt("atomic_list_push", &[list_val.into(), elem_fat.into()])?;
            let new_list = cc.try_as_basic_value().basic()
                .ok_or("list_push failed")?;
            self.builder.build_store(list_alloca, new_list).map_err(llvm_err)?;
        }

        Ok(TypedValue::List(list_alloca))
    }



    /// Build Option<T> from fat struct alloca + found flag -> TypedValue::Enum
    /// Layout: {i64, i8*} where tag=1(data_ptr) for Some, tag=0(null) for None
    pub(super) fn build_option_from_fat_struct(&mut self, fat_alloca: PointerValue<'ctx>, found_flag_a: PointerValue<'ctx>, inner_type: InnerType) -> Result<TypedValue<'ctx>, String> {
        let is_found = self.builder.build_load(self.bool_ty(), found_flag_a, "is_found").map_err(llvm_err)?.into_int_value();
        let i64_ty = self.i64_ty();
        let ptr_ty = self.ptr_ty();
        let enum_ty = self.context.struct_type(&[i64_ty.into(), ptr_ty.into()], false);
        // Clone fat struct into heap for Some variant
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let some_bb = self.context.append_basic_block(current_fn, "opt_some");
        let none_bb = self.context.append_basic_block(current_fn, "opt_none");
        let merge_bb = self.context.append_basic_block(current_fn, "opt_merge");
        let is_found_cond = self.builder.build_int_compare(IntPredicate::NE, is_found, self.bool_ty().const_zero(), "is_found_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_found_cond, some_bb, none_bb);
        // Some: malloc_rc(16), store fat struct, build {1, ptr}
        self.builder.position_at_end(some_bb);
        let fat_val = self.builder.build_load(self.string_type, fat_alloca, "fat_val").map_err(llvm_err)?;
        let buf = self.malloc_rc(i64_ty.const_int(16, false))?;
        self.builder.build_store(buf, fat_val).map_err(llvm_err)?;
        self.rc_inc(buf)?;
        let some_undef = enum_ty.get_undef();
        let s1 = self.builder.build_insert_value(some_undef, i64_ty.const_int(0, false), 0, "s_tag").map_err(llvm_err)?;
        let some_val = self.builder.build_insert_value(s1, buf, 1, "s_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // None: build {0, null}
        self.builder.position_at_end(none_bb);
        let none_undef = enum_ty.get_undef();
        let n1 = self.builder.build_insert_value(none_undef, i64_ty.const_int(1, false), 0, "n_tag").map_err(llvm_err)?;
        let none_val = self.builder.build_insert_value(n1, ptr_ty.const_zero(), 1, "n_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // Merge
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(enum_ty, "opt_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&some_val, some_bb), (&none_val, none_bb)]);
        let alloca = self.builder.build_alloca(enum_ty, "opt_alloca").map_err(llvm_err)?;
        self.builder.build_store(alloca, phi.as_basic_value()).map_err(llvm_err)?;
        Ok(TypedValue::Enum(alloca, enum_ty, inner_type, true))
    }

    /// Build Option<Int>: Some(idx) or None
    /// Layout: {i64, i8*} where tag=1(data_ptr) for Some, tag=0(null) for None
    pub(super) fn build_option_int(&mut self, val: IntValue<'ctx>, is_some: IntValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        let i64_ty = self.i64_ty();
        let ptr_ty = self.ptr_ty();
        let enum_ty = self.context.struct_type(&[i64_ty.into(), ptr_ty.into()], false);
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let some_bb = self.context.append_basic_block(current_fn, "opti_some");
        let none_bb = self.context.append_basic_block(current_fn, "opti_none");
        let merge_bb = self.context.append_basic_block(current_fn, "opti_merge");
        let is_some_cond = self.builder.build_int_compare(IntPredicate::NE, is_some, self.bool_ty().const_zero(), "is_some_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_some_cond, some_bb, none_bb);
        // Some: malloc_rc(8), store i64, build {1, ptr}
        self.builder.position_at_end(some_bb);
        let buf = self.malloc_rc(i64_ty.const_int(8, false))?;
        let i8_ptr = self.builder.build_pointer_cast(buf, ptr_ty, "i8p").map_err(llvm_err)?;
        let val_ptr = self.builder.build_pointer_cast(i8_ptr, self.context.ptr_type(Default::default()), "val_ptr").map_err(llvm_err)?;
        self.builder.build_store(val_ptr, val).map_err(llvm_err)?;
        self.rc_inc(buf)?;
        let some_undef = enum_ty.get_undef();
        let s1 = self.builder.build_insert_value(some_undef, i64_ty.const_int(0, false), 0, "s_tag").map_err(llvm_err)?;
        let some_val = self.builder.build_insert_value(s1, buf, 1, "s_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // None: build {0, null}
        self.builder.position_at_end(none_bb);
        let none_undef = enum_ty.get_undef();
        let n1 = self.builder.build_insert_value(none_undef, i64_ty.const_int(1, false), 0, "n_tag").map_err(llvm_err)?;
        let none_val = self.builder.build_insert_value(n1, ptr_ty.const_zero(), 1, "n_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // Merge
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(enum_ty, "opti_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&some_val, some_bb), (&none_val, none_bb)]);
        let alloca = self.builder.build_alloca(enum_ty, "opti_alloca").map_err(llvm_err)?;
        self.builder.build_store(alloca, phi.as_basic_value()).map_err(llvm_err)?;
        Ok(TypedValue::Enum(alloca, enum_ty, InnerType::Int, true))
    }

    /// Build Option<Float>: Some(val) or None
    pub(super) fn build_option_float(&mut self, val: FloatValue<'ctx>, is_some: IntValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        let i64_ty = self.i64_ty();
        let ptr_ty = self.ptr_ty();
        let enum_ty = self.context.struct_type(&[i64_ty.into(), ptr_ty.into()], false);
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let some_bb = self.context.append_basic_block(current_fn, "optf_some");
        let none_bb = self.context.append_basic_block(current_fn, "optf_none");
        let merge_bb = self.context.append_basic_block(current_fn, "optf_merge");
        let is_some_cond = self.builder.build_int_compare(IntPredicate::NE, is_some, self.bool_ty().const_zero(), "is_some_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_some_cond, some_bb, none_bb);
        // Some: malloc_rc(8), store f64, build {tag:0, ptr}
        self.builder.position_at_end(some_bb);
        let buf = self.malloc_rc(i64_ty.const_int(8, false))?;
        let f64_ptr = self.builder.build_pointer_cast(buf, self.context.ptr_type(Default::default()), "f64_ptr").map_err(llvm_err)?;
        self.builder.build_store(f64_ptr, val).map_err(llvm_err)?;
        self.rc_inc(buf)?;
        let some_undef = enum_ty.get_undef();
        let s1 = self.builder.build_insert_value(some_undef, i64_ty.const_int(0, false), 0, "s_tag").map_err(llvm_err)?;
        let some_val = self.builder.build_insert_value(s1, buf, 1, "s_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // None: build {tag:1, null}
        self.builder.position_at_end(none_bb);
        let none_undef = enum_ty.get_undef();
        let n1 = self.builder.build_insert_value(none_undef, i64_ty.const_int(1, false), 0, "n_tag").map_err(llvm_err)?;
        let none_val = self.builder.build_insert_value(n1, ptr_ty.const_zero(), 1, "n_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // Merge
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(enum_ty, "optf_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&some_val, some_bb), (&none_val, none_bb)]);
        let alloca = self.builder.build_alloca(enum_ty, "optf_alloca").map_err(llvm_err)?;
        self.builder.build_store(alloca, phi.as_basic_value()).map_err(llvm_err)?;
        Ok(TypedValue::Enum(alloca, enum_ty, InnerType::Float, true))
    }

    /// Build Option<List<T>>: Some(list) or None based on is_empty condition
    pub(super) fn build_option_list(&mut self, list_val: StructValue<'ctx>, is_empty: IntValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        let current_fn = self.builder.get_insert_block().and_then(|b| b.get_parent()).ok_or("no function")?;
        let enum_ty = self.context.struct_type(&[self.i64_ty().into(), self.ptr_ty().into()], false);
        let some_bb = self.context.append_basic_block(current_fn, "optl_some");
        let none_bb = self.context.append_basic_block(current_fn, "optl_none");
        let merge_bb = self.context.append_basic_block(current_fn, "optl_merge");
        let is_empty_cond = self.builder.build_int_compare(IntPredicate::NE, is_empty, self.bool_ty().const_zero(), "is_empty_cond").map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_empty_cond, none_bb, some_bb);
        // Some: malloc_rc(24), copy list, build {tag:0, ptr}
        self.builder.position_at_end(some_bb);
        let buf = self.malloc_rc(self.i64_ty().const_int(24, false))?;
        self.builder.build_store(buf, list_val).map_err(llvm_err)?;
        self.rc_inc(buf)?;
        let some_undef = enum_ty.get_undef();
        let s1 = self.builder.build_insert_value(some_undef, self.i64_ty().const_int(0, false), 0, "s_tag").map_err(llvm_err)?;
        let some_val = self.builder.build_insert_value(s1, buf, 1, "s_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // None: build {tag:1, null}
        self.builder.position_at_end(none_bb);
        let none_undef = enum_ty.get_undef();
        let n1 = self.builder.build_insert_value(none_undef, self.i64_ty().const_int(1, false), 0, "n_tag").map_err(llvm_err)?;
        let none_val = self.builder.build_insert_value(n1, self.ptr_ty().const_zero(), 1, "n_ptr").map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);
        // Merge
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(enum_ty, "optl_phi").map_err(llvm_err)?;
        phi.add_incoming(&[(&some_val, some_bb), (&none_val, none_bb)]);
        let alloca = self.builder.build_alloca(enum_ty, "optl_alloca").map_err(llvm_err)?;
        self.builder.build_store(alloca, phi.as_basic_value()).map_err(llvm_err)?;
        Ok(TypedValue::Enum(alloca, enum_ty, InnerType::Int, true))
    }


    /// Inline flatMap for Option: pattern match on opt, call callback with unwrapped value,
    /// return the callback's result directly. This avoids the untyped callback i64 round-trip.
    pub(super) fn builtin_flat_map(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        self.builtin_flat_map_impl(args, trailing, "Option")
    }

    /// Inline flatMapResult for Result: pattern match on res, call callback with unwrapped value,
    /// return the callback's result directly.
    pub(super) fn builtin_flat_map_result(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        self.builtin_flat_map_impl(args, trailing, "Result")
    }

    pub(super) fn builtin_flat_map_impl(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>, enum_name: &str) -> Result<TypedValue<'ctx>, String> {
        // flatMap(enum_val, fn) or flatMap(enum_val) { lambda }
        let (enum_val, callback) = if let Some(lam) = trailing {
            if args.len() != 1 {
                return Err(format!("{} with trailing lambda expects 1 argument", enum_name));
            }
            let ev = self.compile_expr(&args[0])?;
            let cb = self.compile_expr(lam)?;
            (ev, cb)
        } else if args.len() == 2 {
            let ev = self.compile_expr(&args[0])?;
            let cb = self.compile_expr(&args[1])?;
            (ev, cb)
        } else {
            return Err(format!("flatMap expects 2 arguments (enum, fn)"));
        };

        // Extract the callback's function pointer and type
        let (fn_ptr, fn_type) = match callback {
            TypedValue::Fn(p, ft) => (p, ft),
            _ => return Err(format!("{}: second argument must be a function", enum_name)),
        };

        // Get the enum value (as an alloca pointer to {i64, ptr})
        let (enum_ptr, enum_ty) = match enum_val {
            TypedValue::Enum(p, t, ..) => (p, t),
            _ => return Err(format!("{}: first argument must be an {}", enum_name, enum_name)),
        };

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile flatMap outside function")?;

        let i64 = self.i64_ty();

        // Allocate result at entry
        let result_bt: BasicTypeEnum = enum_ty.into();
        let entry = current_fn.get_first_basic_block().unwrap();
        let saved_pos = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(instr) => { let _ = self.builder.position_before(&instr); }
            None => self.builder.position_at_end(entry),
        }
        let result_alloca = self.builder.build_alloca(result_bt, "fm_result").map_err(llvm_err)?;
        let zero = result_bt.const_zero();
        self.builder.build_store(result_alloca, zero).map_err(llvm_err)?;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        // Build match: check tag (0 = Some/Ok, 1 = None/Err)
        let merge_block = self.context.append_basic_block(current_fn, "fm_merge");
        let some_block = self.context.append_basic_block(current_fn, "fm_some");
        let none_block = self.context.append_basic_block(current_fn, "fm_none");

        let enum_bt: BasicTypeEnum = enum_ty.into();
        let enum_raw = self.builder.build_load(enum_bt, enum_ptr, "fm_enum").map_err(llvm_err)?;
        let enum_loaded = enum_raw.into_struct_value();
        let tag = self.builder.build_extract_value(enum_loaded, 0, "fm_tag").map_err(llvm_err)?
            .into_int_value();
        let is_some = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "fm_is_some")
            .map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_some, some_block, none_block);

        // Some/Ok branch: extract inner value, call callback, store result
        self.builder.position_at_end(some_block);
        let data_ptr = self.builder.build_extract_value(enum_loaded, 1, "fm_data").map_err(llvm_err)?
            .into_pointer_value();
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, self.ptr_ty(), "fm_inner")
            .map_err(llvm_err)?;
        let inner_val = self.builder.build_load(i64, inner_ptr, "fm_v")
            .map_err(llvm_err)?;

        // Call the callback with its actual function type (not i64->i64!)
        let cc = self.builder.build_indirect_call(fn_type, fn_ptr, &[inner_val.into()], "fm_call")
            .map_err(llvm_err)?;
        match cc.try_as_basic_value().basic() {
            Some(bv) => {
                self.builder.build_store(result_alloca, bv).map_err(llvm_err)?;
            }
            None => {} // void return, leave result as zero-init
        };
        let _ = self.builder.build_unconditional_branch(merge_block);

        // None/Err branch: store the original enum value
        self.builder.position_at_end(none_block);
        self.builder.build_store(result_alloca, enum_loaded).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge
        self.builder.position_at_end(merge_block);
        let result = self.builder.build_load(result_bt, result_alloca, "fm_result_ld").map_err(llvm_err)?;
        self.bv_to_typed(result)
    }

    /// Inline map for Option/Result: pattern match on enum, call callback with unwrapped value,
    /// wrap the result back in Some/Ok.
    pub(super) fn builtin_enum_map(&mut self, args: &[Expr], trailing: &Option<Box<Expr>>) -> Result<TypedValue<'ctx>, String> {
        // map(enum_val, fn) or map(enum_val) { lambda }
        let (enum_val, callback) = if let Some(lam) = trailing {
            if args.len() != 1 {
                return Err("map on enum with trailing lambda expects 1 argument".to_string());
            }
            let ev = self.compile_expr(&args[0])?;
            let cb = self.compile_expr(lam)?;
            (ev, cb)
        } else if args.len() == 2 {
            let ev = self.compile_expr(&args[0])?;
            let cb = self.compile_expr(&args[1])?;
            (ev, cb)
        } else {
            return Err("map expects 2 arguments (enum, fn)".to_string());
        };

        let (fn_ptr, fn_type) = match callback {
            TypedValue::Fn(p, ft) => (p, ft),
            _ => return Err("map: second argument must be a function".to_string()),
        };

        let (enum_ptr, enum_ty) = match enum_val {
            TypedValue::Enum(p, t, ..) => (p, t),
            _ => return Err("map: first argument must be an Option or Result".to_string()),
        };

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile map outside function")?;

        let i64 = self.i64_ty();
        let ptr = self.ptr_ty();

        // Allocate result at entry
        let result_bt: BasicTypeEnum = enum_ty.into();
        let entry = current_fn.get_first_basic_block().unwrap();
        let saved_pos = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(instr) => { let _ = self.builder.position_before(&instr); }
            None => self.builder.position_at_end(entry),
        }
        let result_alloca = self.builder.build_alloca(result_bt, "em_result").map_err(llvm_err)?;
        let zero = result_bt.const_zero();
        self.builder.build_store(result_alloca, zero).map_err(llvm_err)?;
        let heap_alloca = self.builder.build_alloca(result_bt, "em_heap").map_err(llvm_err)?;
        self.builder.build_store(heap_alloca, zero).map_err(llvm_err)?;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        let merge_block = self.context.append_basic_block(current_fn, "em_merge");
        let some_block = self.context.append_basic_block(current_fn, "em_some");
        let none_block = self.context.append_basic_block(current_fn, "em_none");

        let enum_bt: BasicTypeEnum = enum_ty.into();
        let enum_raw = self.builder.build_load(enum_bt, enum_ptr, "em_enum").map_err(llvm_err)?;
        let enum_loaded = enum_raw.into_struct_value();
        let tag = self.builder.build_extract_value(enum_loaded, 0, "em_tag").map_err(llvm_err)?
            .into_int_value();
        let is_some = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "em_is_some")
            .map_err(llvm_err)?;
        let _ = self.builder.build_conditional_branch(is_some, some_block, none_block);

        // Some/Ok branch: extract inner value, call callback, wrap result in Some/Ok
        self.builder.position_at_end(some_block);
        let data_ptr = self.builder.build_extract_value(enum_loaded, 1, "em_data").map_err(llvm_err)?
            .into_pointer_value();
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, ptr, "em_inner").map_err(llvm_err)?;
        let inner_val = self.builder.build_load(i64, inner_ptr, "em_v").map_err(llvm_err)?;

        // Call the callback with the inner value
        let cc = self.builder.build_indirect_call(fn_type, fn_ptr, &[inner_val.into()], "em_call")
            .map_err(llvm_err)?;
        let cb_result = cc.try_as_basic_value().basic().ok_or("em call failed")?;

        // Wrap the callback result in Some/Ok (tag = 0) on the heap
        let buf = self.malloc_rc(i64.const_int(8, false))?;
        let buf_ptr = self.builder.build_pointer_cast(buf, ptr, "em_bp").map_err(llvm_err)?;
        self.builder.build_store(buf_ptr, cb_result).map_err(llvm_err)?;
        self.rc_inc(buf)?;

        let undef = enum_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, i64.const_int(0, false), 0, "em_ok_tag").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, buf, 1, "em_ok_data").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, r2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // None/Err branch: store original enum unchanged
        self.builder.position_at_end(none_block);
        self.builder.build_store(result_alloca, enum_loaded).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_block);

        // Merge
        self.builder.position_at_end(merge_block);
        let result = self.builder.build_load(result_bt, result_alloca, "em_result_ld").map_err(llvm_err)?;
        self.bv_to_typed(result)
    }
}
