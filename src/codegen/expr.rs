// Submodule: expr

use atomic::ast::*;
use inkwell::builder::BuilderError;
use inkwell::values::{BasicValue, BasicValueEnum, IntValue, PointerValue};
use inkwell::types::{BasicTypeEnum, BasicMetadataTypeEnum, FunctionType};
use inkwell::{IntPredicate, FloatPredicate};

use super::{CodeGen, TypedValue, ValKind, Scope, llvm_err, InnerType};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn compile_expr(&mut self, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        match expr {
            Expr::Literal(lit) => self.compile_literal(lit),
            Expr::Ident(name) => self.compile_ident(name),
            Expr::Binary(lhs, op, rhs) => self.compile_binary(lhs, *op, rhs),
            Expr::Unary(op, e) => self.compile_unary(*op, e),
            Expr::Call { func, args, trailing_lambda } => self.compile_call(func, args, trailing_lambda),
            Expr::When(w) => self.compile_when(w),
            Expr::Block(stmts) => self.compile_block(stmts),
            Expr::Assign { target, value, propagate } => {
                if *propagate {
                    self.compile_propagate_assign(target, value)
                } else {
                    self.compile_assign(target, value)
                }
            }
            Expr::For(f) => self.compile_for(f),
            Expr::StringInterpolate(parts) => self.compile_string_interp(parts),
            Expr::FieldAccess(obj, field) => self.compile_field_access(obj, field),
            Expr::StructLiteral(fields) => self.compile_struct_lit(fields),
            Expr::MapLiteral(entries) => self.compile_map_lit(entries),
            Expr::SetLiteral(elements) => self.compile_set_lit(elements),
            Expr::Lambda { params, body, .. } => self.compile_lambda(params, body),
            Expr::Index(obj, idx) => self.compile_index(obj, idx),
            Expr::Range(start, end) => self.compile_range(start, end),
            Expr::Tuple(exprs) => self.compile_tuple(exprs),
            Expr::SafeFieldAccess(obj, field) => self.compile_safe_field_access(obj, field),
            Expr::SafeCall { receiver, args } => self.compile_safe_call(receiver, args),
            Expr::Try(inner) => {
                let val = self.compile_expr(inner)?;
                self.propagate_unwrap(&val)
            }
            Expr::Continue => {
                if let Some(target) = self.continue_target {
                    self.builder.build_unconditional_branch(target).map_err(llvm_err)?;
                    Ok(TypedValue::Unit)
                } else {
                    Err("continue outside loop".to_string())
                }
            }
            Expr::Break => {
                if let Some(target) = self.break_target {
                    self.builder.build_unconditional_branch(target).map_err(llvm_err)?;
                    Ok(TypedValue::Unit)
                } else {
                    Err("break outside loop".to_string())
                }
            }
            Expr::FunctionRef(name) => {
                // Resolve function reference: ::name, ::Type.method, ::module::func
                self.compile_function_ref(name)
            }
            Expr::Copy(inner) => {
                let val = self.compile_expr(inner)?;
                match &val {
                    TypedValue::Int(_) | TypedValue::Float(_) | TypedValue::Bool(_) | TypedValue::Unit | TypedValue::Fn(_, _)
                    | TypedValue::CString(_) | TypedValue::Ptr(_) | TypedValue::FileHandle(_) => {
                        // Value types and pointers are automatically copied
                        Ok(val)
                    }
                    TypedValue::Str(ptr) => {
                        // Copy string: load struct, allocate new, store back
                        // RC is handled by rc_inc_typed_value when the result is bound to a variable
                        let loaded = self.load_string(*ptr)?;
                        let new_alloca = self.builder.build_alloca(self.string_type, "str_copy").map_err(llvm_err)?;
                        self.builder.build_store(new_alloca, loaded).map_err(llvm_err)?;
                        Ok(TypedValue::Str(new_alloca))
                    }
                    TypedValue::Struct(ptr, st) => {
                        let bt: BasicTypeEnum = (*st).into();
                        let loaded = self.builder.build_load(bt, *ptr, "struct_copy_ld").map_err(llvm_err)?;
                        let new_alloca = self.builder.build_alloca(bt, "struct_copy").map_err(llvm_err)?;
                        self.builder.build_store(new_alloca, loaded).map_err(llvm_err)?;
                        Ok(TypedValue::Struct(new_alloca, *st))
                    }
                    TypedValue::Enum(ptr, et, ..) => {
                        let bt: BasicTypeEnum = (*et).into();
                        let loaded = self.builder.build_load(bt, *ptr, "enum_copy_ld").map_err(llvm_err)?;
                        let new_alloca = self.builder.build_alloca(bt, "enum_copy").map_err(llvm_err)?;
                        self.builder.build_store(new_alloca, loaded).map_err(llvm_err)?;
                        Ok(TypedValue::Enum(new_alloca, *et, InnerType::Int, false))
                    }
                    TypedValue::List(ptr) => {
                        let loaded = self.load_list(*ptr)?;
                        let new_alloca = self.builder.build_alloca(self.list_type, "list_copy").map_err(llvm_err)?;
                        self.builder.build_store(new_alloca, loaded).map_err(llvm_err)?;
                        Ok(TypedValue::List(new_alloca))
                    }
                    TypedValue::Map(ptr) => {
                        let loaded = self.load_list(*ptr)?;
                        let new_alloca = self.builder.build_alloca(self.list_type, "map_copy").map_err(llvm_err)?;
                        self.builder.build_store(new_alloca, loaded).map_err(llvm_err)?;
                        Ok(TypedValue::Map(new_alloca))
                    }
                    TypedValue::Set(ptr) => {
                        let loaded = self.load_list(*ptr)?;
                        let new_alloca = self.builder.build_alloca(self.list_type, "set_copy").map_err(llvm_err)?;
                        self.builder.build_store(new_alloca, loaded).map_err(llvm_err)?;
                        Ok(TypedValue::Set(new_alloca))
                    }
                    TypedValue::Task(ptr) => {
                        // Task is a heap pointer; just copy the reference
                        Ok(TypedValue::Task(*ptr))
                    }
                    TypedValue::Stream(ptr) => {
                        // Stream is a heap pointer; just copy the reference
                        Ok(TypedValue::Stream(*ptr))
                    }
                    TypedValue::LazyList(ptr) => {
                        let loaded = self.load_list(*ptr)?;
                        let new_alloca = self.builder.build_alloca(self.list_type, "lazylist_copy").map_err(llvm_err)?;
                        self.builder.build_store(new_alloca, loaded).map_err(llvm_err)?;
                        Ok(TypedValue::LazyList(new_alloca))
                    }
                }
            }
            Expr::Unsafe(inner) => {
                let prev = self.in_unsafe;
                self.in_unsafe = true;
                let result = self.compile_expr(inner);
                self.in_unsafe = prev;
                result
            }
        }
    }

    pub(super) fn compile_lambda(&mut self, params: &[String], body: &Expr) -> Result<TypedValue<'ctx>, String> {
        self.lambda_count += 1;
        let lambda_name = format!(".lambda_{}", self.lambda_count);

        // Lambdas use the fat {i64,ptr} return type so callers through untyped
        // parameters (Int fallback) get the correct struct return. The body's
        // scalar result is packed into the struct by compile_fun_def.
        let i64 = self.i64_ty();
        let param_tys: Vec<BasicMetadataTypeEnum> = params.iter().map(|_| BasicMetadataTypeEnum::from(i64)).collect();
        let fn_type = self.build_fn_type(None, &lambda_name, &param_tys);

        let function = self.module.add_function(&lambda_name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");

        let saved_pos = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

        let mut saved_scope = Scope::new();
        std::mem::swap(&mut self.scope, &mut saved_scope);
        self.scope = Scope::new();

        for (i, param) in params.iter().enumerate() {
            if let Some(pv) = function.get_nth_param(i as u32) {
                let alloca = self.builder.build_alloca(pv.get_type(), param).map_err(llvm_err)?;
                self.builder.build_store(alloca, pv).map_err(llvm_err)?;
                self.scope.set(param.clone(), alloca, pv.get_type(), ValKind::Int);
            }
        }

        let result = self.compile_expr(body)?;

        let current_block = self.builder.get_insert_block().unwrap();
        if current_block.get_terminator().is_none() {
            let llvm_void: bool = function.get_type().get_return_type().is_none();

            if llvm_void {
                let _ = self.builder.build_return(None);
            } else {
                match &result {
                    TypedValue::Enum(ptr, ty, ..) => {
                        let bt: BasicTypeEnum = (*ty).into();
                        let loaded = self.builder.build_load(bt, *ptr, "ret_enum").map_err(llvm_err)?;
                        // Convert from the specific enum type to fat_return_type
                        // Both have {i64, ptr} layout but are different LLVM named types
                        let sv = loaded.into_struct_value();
                        let tag = self.builder.build_extract_value(sv, 0, "etag").map_err(llvm_err)?;
                        let data = self.builder.build_extract_value(sv, 1, "edata").map_err(llvm_err)?;
                        let undef_fat = self.fat_return_type.get_undef();
                        let f1 = self.builder.build_insert_value(undef_fat, tag, 0, "ftag").map_err(llvm_err)?;
                        let f2 = self.builder.build_insert_value(f1, data, 1, "fdata").map_err(llvm_err)?;
                        let _ = self.builder.build_return(Some(&f2));
                    }
                    TypedValue::Struct(ptr, ty) => {
                        let bt: BasicTypeEnum = (*ty).into();
                        let loaded = self.builder.build_load(bt, *ptr, "ret_struct").map_err(llvm_err)?;
                        let _ = self.builder.build_return(Some(&loaded));
                    }
                    TypedValue::Bool(v) => {
                        let extended = self.builder.build_int_z_extend(*v, i64, "ext").map_err(llvm_err)?;
                        // Pack into fat {i64,ptr} if needed
                        if function.get_type().get_return_type()
                            .map_or(false, |rt| rt.is_struct_type())
                        {
                            let struct_ty = function.get_type().get_return_type()
                                .unwrap().into_struct_type();
                            let alloca = self.builder.build_alloca(struct_ty, "ret_pack")
                                .map_err(llvm_err)?;
                            let zero = struct_ty.const_zero();
                            self.builder.build_store(alloca, zero).map_err(llvm_err)?;
                            let gep0 = self.builder.build_struct_gep(struct_ty, alloca, 0, "ret_pack0")
                                .map_err(llvm_err)?;
                            self.builder.build_store(gep0, extended).map_err(llvm_err)?;
                            let loaded = self.builder.build_load(struct_ty, alloca, "ret_packed")
                                .map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&loaded));
                        } else {
                            let _ = self.builder.build_return(Some(&extended));
                        }
                    }
                    _ => {
                        if let Some(bv) = result.to_bv() {
                            // If the lambda returns a fat {i64,ptr} struct but
                            // the body produced a scalar, pack it.
                            let need_pack = function.get_type().get_return_type()
                                .map_or(false, |rt| rt.is_struct_type());
                            if need_pack {
                                let struct_ty = function.get_type().get_return_type()
                                    .unwrap().into_struct_type();
                                let alloca = self.builder.build_alloca(struct_ty, "ret_pack")
                                    .map_err(llvm_err)?;
                                let zero = struct_ty.const_zero();
                                self.builder.build_store(alloca, zero).map_err(llvm_err)?;
                                let gep0 = self.builder.build_struct_gep(struct_ty, alloca, 0, "ret_pack0")
                                    .map_err(llvm_err)?;
                                self.builder.build_store(gep0, bv).map_err(llvm_err)?;
                                let loaded = self.builder.build_load(struct_ty, alloca, "ret_packed")
                                    .map_err(llvm_err)?;
                                let _ = self.builder.build_return(Some(&loaded));
                            } else {
                                let _ = self.builder.build_return(Some(&bv));
                            }
                        } else {
                            // Unit, Str, List, etc. — return zero fat struct if needed
                            if let Some(ret_ty) = function.get_type().get_return_type() {
                                if ret_ty.is_struct_type() {
                                    let zero = ret_ty.into_struct_type().const_zero();
                                    let _ = self.builder.build_return(Some(&zero));
                                } else {
                                    let _ = self.builder.build_return(None);
                                }
                            } else {
                                let _ = self.builder.build_return(None);
                            }
                        }
                    }
                }
            }
        }

        self.scope = saved_scope;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        Ok(TypedValue::Fn(function.as_global_value().as_pointer_value(), fn_type))
    }

    pub(super) fn compile_literal(&mut self, lit: &Literal) -> Result<TypedValue<'ctx>, String> {
        match lit {
            Literal::Int(n) => Ok(TypedValue::Int(self.i64_ty().const_int(*n as u64, true))),
            Literal::Float(n) => Ok(TypedValue::Float(self.f64_ty().const_float(*n))),
            Literal::Bool(b) => Ok(TypedValue::Bool(self.bool_ty().const_int(if *b { 1 } else { 0 }, false))),
            Literal::String(s) => self.compile_string_literal(s),
            Literal::Char(c) => Ok(TypedValue::Int(self.i64_ty().const_int(*c as u64, false))),
            Literal::Unit => Ok(TypedValue::Unit),
        }
    }

    pub(super) fn compile_string_literal(&mut self, s: &str) -> Result<TypedValue<'ctx>, String> {
        let g = self.builder.build_global_string_ptr(s, ".str").map_err(llvm_err)?;
        let len = self.i64_ty().const_int(s.len() as u64, false);
        let cc = self.call_rt("atomic_string_create", &[g.as_pointer_value().into(), len.into()])?;
        match cc.try_as_basic_value().basic() {
            Some(bv) => {
                // String is returned as {i64, i8*} struct by value.
                // Store it on the stack and use the pointer.
                let alloca = self.builder.build_alloca(self.string_type, "str_val").map_err(llvm_err)?;
                self.builder.build_store(alloca, bv).map_err(llvm_err)?;
                Ok(TypedValue::Str(alloca))
            }
            None => Ok(TypedValue::Str(g.as_pointer_value())),
        }
    }

    pub(super) fn compile_ident(&mut self, name: &str) -> Result<TypedValue<'ctx>, String> {
        // Check compile-time constants first
        if let Some(&(global_ptr, ty, kind)) = self.consts.get(name) {
            let loaded = self.builder.build_load(ty, global_ptr, name).map_err(llvm_err)?;
            match kind {
                ValKind::Str => {
                    let alloca = self.builder.build_alloca(ty, "const_str").map_err(llvm_err)?;
                    self.builder.build_store(alloca, loaded).map_err(llvm_err)?;
                    return Ok(TypedValue::Str(alloca));
                }
                ValKind::List => {
                    let alloca = self.builder.build_alloca(ty, "const_list").map_err(llvm_err)?;
                    self.builder.build_store(alloca, loaded).map_err(llvm_err)?;
                    return Ok(TypedValue::List(alloca));
                }
                ValKind::Map => {
                    let alloca = self.builder.build_alloca(ty, "const_map").map_err(llvm_err)?;
                    self.builder.build_store(alloca, loaded).map_err(llvm_err)?;
                    return Ok(TypedValue::Map(alloca));
                }
                ValKind::Set => {
                    let alloca = self.builder.build_alloca(ty, "const_set").map_err(llvm_err)?;
                    self.builder.build_store(alloca, loaded).map_err(llvm_err)?;
                    return Ok(TypedValue::Set(alloca));
                }
                ValKind::CString => {
                    if let BasicValueEnum::PointerValue(p) = loaded {
                        return Ok(TypedValue::CString(p));
                    }
                    return self.bv_to_typed(loaded);
                }
                ValKind::Ptr => {
                    if let BasicValueEnum::PointerValue(p) = loaded {
                        return Ok(TypedValue::Ptr(p));
                    }
                    return self.bv_to_typed(loaded);
                }
                ValKind::FileHandle => {
                    if let BasicValueEnum::PointerValue(p) = loaded {
                        return Ok(TypedValue::FileHandle(p));
                    }
                    return self.bv_to_typed(loaded);
                }
                _ => return self.bv_to_typed(loaded),
            }
        }
        // Check for lazy val first — extract data before borrowing self mutably
        let lazy_info: Option<(PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>, ValKind, PointerValue<'ctx>, Expr, Option<FunctionType<'ctx>>)> =
            if let Some(var) = self.scope.get(name) {
                if let (Some(flag_ptr), Some(init_expr)) = (var.lazy_flag, var.lazy_init_expr.clone()) {
                    Some((var.ptr, var.ty, var.kind, flag_ptr, init_expr, var.fn_type))
                } else {
                    None
                }
            } else {
                None
            };

        if let Some((lazy_ptr, lazy_ty, lazy_kind, flag_ptr, init_expr, lazy_fn_type)) = lazy_info {
            let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let init_block = self.context.append_basic_block(current_fn, &format!("lazy_init_{}", name));
            let merge_block = self.context.append_basic_block(current_fn, &format!("lazy_merge_{}", name));

            let flag_val = self.builder.build_load(self.bool_ty(), flag_ptr, "lazy_flag").map_err(llvm_err)?;
            let is_init = self.builder.build_int_compare(
                IntPredicate::NE, flag_val.into_int_value(), self.bool_ty().const_int(0, false), "is_init"
            ).map_err(llvm_err)?;
            self.builder.build_conditional_branch(is_init, merge_block, init_block).map_err(llvm_err)?;

            // Init block: evaluate initializer and store
            self.builder.position_at_end(init_block);
            let init_val = self.compile_expr(&init_expr)?;
            self.store_typed_value(&init_val, lazy_ptr, lazy_ty)?;
            self.builder.build_store(flag_ptr, self.bool_ty().const_int(1, false)).map_err(llvm_err)?;
            self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;

            // Merge block: load and return the value
            self.builder.position_at_end(merge_block);
            let val = self.builder.build_load(lazy_ty, lazy_ptr, name).map_err(llvm_err)?;

            return match lazy_kind {
                ValKind::Str => {
                    let alloca = self.builder.build_alloca(lazy_ty, "str_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    Ok(TypedValue::Str(alloca))
                }
                ValKind::Fn => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        if let Some(ft) = lazy_fn_type {
                            return Ok(TypedValue::Fn(p, ft));
                        }
                    }
                    self.bv_to_typed(val)
                }
                ValKind::List => {
                    let alloca = self.builder.build_alloca(lazy_ty, "list_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    Ok(TypedValue::List(alloca))
                }
                ValKind::Map => {
                    let alloca = self.builder.build_alloca(lazy_ty, "map_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    Ok(TypedValue::Map(alloca))
                }
                ValKind::Set => {
                    let alloca = self.builder.build_alloca(lazy_ty, "set_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    Ok(TypedValue::Set(alloca))
                }
                ValKind::Task => {
                    let task_ptr = self.builder.build_load(self.ptr_ty(), lazy_ptr, "task_ld").map_err(llvm_err)?;
                    Ok(TypedValue::Task(task_ptr.into_pointer_value()))
                }
                ValKind::Stream => {
                    let stream_ptr = self.builder.build_load(self.ptr_ty(), lazy_ptr, "stream_ld").map_err(llvm_err)?;
                    Ok(TypedValue::Stream(stream_ptr.into_pointer_value()))
                }
                ValKind::LazyList => Ok(TypedValue::LazyList(lazy_ptr)),
                ValKind::CString => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        Ok(TypedValue::CString(p))
                    } else {
                        self.bv_to_typed(val)
                    }
                }
                ValKind::Ptr => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        Ok(TypedValue::Ptr(p))
                    } else {
                        self.bv_to_typed(val)
                    }
                }
                ValKind::FileHandle => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        Ok(TypedValue::FileHandle(p))
                    } else {
                        self.bv_to_typed(val)
                    }
                }
                ValKind::Struct => {
                    let alloca = self.builder.build_alloca(lazy_ty, "struct_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    Ok(TypedValue::Struct(alloca, val.into_struct_value().get_type()))
                }
                ValKind::Enum => {
                    let alloca = self.builder.build_alloca(lazy_ty, "enum_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    Ok(TypedValue::Enum(alloca, val.into_struct_value().get_type(), InnerType::Int, false))
                }
                _ => self.bv_to_typed(val),
            };
        }

        if let Some(var) = self.scope.get(name) {
            let val = self.builder.build_load(var.ty, var.ptr, name).map_err(llvm_err)?;
            let kind = var.kind;

            match kind {
                ValKind::Str => {
                    let alloca = self.builder.build_alloca(var.ty, "str_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    return Ok(TypedValue::Str(alloca));
                }
                ValKind::Fn => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        if let Some(ft) = var.fn_type {
                            return Ok(TypedValue::Fn(p, ft));
                        }
                        return Err(format!(
                            "Function variable '{}' has no type information (internal error: fn_type not preserved)",
                            name
                        ));
                    }
                    return Err(format!("Expected function pointer for '{}', got: {:?}", name, val));
                }
                ValKind::List => {
                    let alloca = self.builder.build_alloca(var.ty, "list_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    return Ok(TypedValue::List(alloca));
                }
                ValKind::Map => {
                    let alloca = self.builder.build_alloca(var.ty, "map_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    return Ok(TypedValue::Map(alloca));
                }
                ValKind::Set => {
                    let alloca = self.builder.build_alloca(var.ty, "set_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    return Ok(TypedValue::Set(alloca));
                }
                ValKind::Task => {
                    let task_ptr = self.builder.build_load(self.ptr_ty(), var.ptr, "task_ld2").map_err(llvm_err)?;
                    return Ok(TypedValue::Task(task_ptr.into_pointer_value()));
                }
                ValKind::Stream => {
                    let stream_ptr = self.builder.build_load(self.ptr_ty(), var.ptr, "stream_ld2").map_err(llvm_err)?;
                    return Ok(TypedValue::Stream(stream_ptr.into_pointer_value()));
                }
                ValKind::LazyList => {
                    return Ok(TypedValue::LazyList(var.ptr));
                }
                ValKind::CString => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        return Ok(TypedValue::CString(p));
                    }
                    return self.bv_to_typed(val);
                }
                ValKind::Ptr => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        return Ok(TypedValue::Ptr(p));
                    }
                    return self.bv_to_typed(val);
                }
                ValKind::FileHandle => {
                    if let BasicValueEnum::PointerValue(p) = val {
                        return Ok(TypedValue::FileHandle(p));
                    }
                    return self.bv_to_typed(val);
                }
                ValKind::Struct => {
                    let alloca = self.builder.build_alloca(var.ty, "struct_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    let st = var.ty.into_struct_type();
                    return Ok(TypedValue::Struct(alloca, st));
                }
                ValKind::Enum => {
                    let alloca = self.builder.build_alloca(var.ty, "enum_tmp").map_err(llvm_err)?;
                    self.builder.build_store(alloca, val).map_err(llvm_err)?;
                    let et = var.ty.into_struct_type();
                    let inner_type = var.enum_inner_type.unwrap_or(InnerType::Int);
                    return Ok(TypedValue::Enum(alloca, et, inner_type, false));
                }
                _ => {
                    if val.is_struct_value() {
                        let alloca = self.builder.build_alloca(var.ty, "tmp_struct").map_err(llvm_err)?;
                        self.builder.build_store(alloca, val).map_err(llvm_err)?;
                        return Ok(TypedValue::Str(alloca));
                    }
                    self.bv_to_typed(val)
                }
            }
        } else if let Some(fn_val) = self.module.get_function(name) {
            let fn_ptr = fn_val.as_global_value().as_pointer_value();
            let fn_type = fn_val.get_type();
            return Ok(TypedValue::Fn(fn_ptr, fn_type));
        } else if let Some((enum_info, variant)) = self.registry.lookup_variant(name).map(|(ei, vi)| (ei.clone(), vi.clone())) {
            if variant.params.is_empty() {
                // Unit variant: construct the enum value directly
                self.compile_enum_construct(&enum_info, &variant, &[])
            } else {
                Err(format!("Enum variant '{}' requires arguments (use the variant as a function call)", name))
            }
        } else if name == "pi" {
            let pi_val = self.f64_ty().const_float(std::f64::consts::PI);
            return Ok(TypedValue::Float(pi_val));
        } else if name == "e" {
            let e_val = self.f64_ty().const_float(std::f64::consts::E);
            return Ok(TypedValue::Float(e_val));
        } else {
            Err(format!("Undefined variable: {}", name))
        }
    }

    /// Unpack a call result: if it's a fat_return struct, extract field 0 as Int.
    /// Otherwise fall through to bv_to_typed.
    /// Also stores the full fat_ret alloca in last_fat_ret for possible later use
    /// (e.g., when the result is returned from a function that declares an enum return type).
    pub(super) fn unpack_fat_return(&mut self, bv: BasicValueEnum<'ctx>, ret_ty: Option<BasicTypeEnum<'ctx>>) -> Result<TypedValue<'ctx>, String> {
        if let Some(rt) = ret_ty {
            if let BasicTypeEnum::StructType(fat_ty) = rt {
                if fat_ty == self.fat_return_type {
                    if let BasicValueEnum::StructValue(sv) = bv {
                        let alloca = self.builder.build_alloca(fat_ty, "fat_unpack").map_err(llvm_err)?;
                        self.builder.build_store(alloca, sv).map_err(llvm_err)?;
                        // Save the full fat_ret alloca for potential bitcast later
                        self.last_fat_ret = Some((alloca, fat_ty));
                        let gep0 = self.builder.build_struct_gep(fat_ty, alloca, 0, "val_gep").map_err(llvm_err)?;
                        let val = self.builder.build_load(self.i64_ty(), gep0, "val").map_err(llvm_err)?
                            .into_int_value();
                        return Ok(TypedValue::Int(val));
                    }
                }
            }
        }
        self.bv_to_typed(bv)
    }

    pub(super) fn bv_to_typed(&mut self, val: BasicValueEnum<'ctx>) -> Result<TypedValue<'ctx>, String> {
        match val {
            BasicValueEnum::IntValue(v) if v.get_type().get_bit_width() == 1 => Ok(TypedValue::Bool(v)),
            BasicValueEnum::IntValue(v) => Ok(TypedValue::Int(v)),
            BasicValueEnum::FloatValue(v) => Ok(TypedValue::Float(v)),
            BasicValueEnum::PointerValue(_v) => {
                // Pointers might be string alloca pointers; handle carefully
                Ok(TypedValue::Unit)
            }
            BasicValueEnum::StructValue(v) => {
                let st = v.get_type();
                let alloca = self.builder.build_alloca(st, "struct_tmp2")
                    .map_err(llvm_err)?;
                self.builder.build_store(alloca, v).map_err(llvm_err)?;
                if st == self.fat_return_type {
                    // Fat return from untyped lambda/function: extract field 0 as Int.
                    // Also save the full alloca for possible enum bitcast later.
                    self.last_fat_ret = Some((alloca, st));
                    let gep0 = self.builder.build_struct_gep(st, alloca, 0, "fv_gep").map_err(llvm_err)?;
                    let val = self.builder.build_load(self.i64_ty(), gep0, "fv").map_err(llvm_err)?
                        .into_int_value();
                    Ok(TypedValue::Int(val))
                } else if st == self.string_type {
                    // Named __atomic_str type — must check before enum_types since
                    // enum types are anonymous {i64, ptr} which used to collide.
                    Ok(TypedValue::Str(alloca))
                } else if self.enum_types.values().any(|et| *et == st) {
                    // Matches a registered enum type (anonymous {i64,ptr})
                    Ok(TypedValue::Enum(alloca, st, InnerType::Int, false))
                } else {
                    Ok(TypedValue::Struct(alloca, st))
                }
            }
            _ => Ok(TypedValue::Unit),
        }
    }

    /// Infer ValKind from a BasicValueEnum (used for destructuring, where types are not annotated)
    pub(super) fn bv_kind(&self, val: &BasicValueEnum<'ctx>) -> ValKind {
        match val {
            BasicValueEnum::IntValue(v) if v.get_type().get_bit_width() == 1 => ValKind::Bool,
            BasicValueEnum::IntValue(_) => ValKind::Int,
            BasicValueEnum::FloatValue(_) => ValKind::Float,
            BasicValueEnum::StructValue(v) => {
                let st = v.get_type();
                if st == self.string_type { ValKind::Str }
                else if st == self.list_type { ValKind::List }
                else if self.enum_types.values().any(|et| *et == st) { ValKind::Enum }
                else { ValKind::Struct }
            }
            _ => ValKind::Int,
        }
    }

    pub(super) fn compile_binary(&mut self, lhs: &Expr, op: BinaryOp, rhs: &Expr) -> Result<TypedValue<'ctx>, String> {
        // Short-circuit evaluation for && and ||
        // Also handle Is/In before compiling RHS — the RHS may be a type/variant name
        match op {
            BinaryOp::And => return self.compile_and(lhs, rhs),
            BinaryOp::Or => return self.compile_or(lhs, rhs),
            BinaryOp::Is => return self.bin_is(lhs, rhs),
            BinaryOp::In => return self.bin_in(lhs, rhs),
            _ => {}
        }

        let left = self.compile_expr(lhs)?;
        let right = self.compile_expr(rhs)?;
        match op {
            BinaryOp::Add => self.bin_add(&left, &right),
            BinaryOp::Sub => self.bin_arith(&left, &right, "sub",
                |b, l, r| b.build_int_sub(l, r, "sub"),
                |b, l, r| b.build_float_sub(l, r, "sub")),
            BinaryOp::Mul => self.bin_arith(&left, &right, "mul",
                |b, l, r| b.build_int_mul(l, r, "mul"),
                |b, l, r| b.build_float_mul(l, r, "mul")),
            BinaryOp::Div => self.bin_arith(&left, &right, "div",
                |b, l, r| b.build_int_signed_div(l, r, "div"),
                |b, l, r| b.build_float_div(l, r, "div")),
            BinaryOp::Mod => self.bin_mod(&left, &right),
            BinaryOp::Pow => self.bin_pow(&left, &right),
            BinaryOp::Eq => self.compare_eq(&left, &right),
            BinaryOp::Neq => self.compare_neq(&left, &right),
            BinaryOp::Lt => self.compare(IntPredicate::SLT, FloatPredicate::OLT, &left, &right),
            BinaryOp::Gt => self.compare(IntPredicate::SGT, FloatPredicate::OGT, &left, &right),
            BinaryOp::Lte => self.compare(IntPredicate::SLE, FloatPredicate::OLE, &left, &right),
            BinaryOp::Gte => self.compare(IntPredicate::SGE, FloatPredicate::OGE, &left, &right),
            BinaryOp::BitAnd => self.bin_bitwise(&left, &right, "and",
                |b, l, r| b.build_and(l, r, "and")),
            BinaryOp::BitOr => self.bin_bitwise(&left, &right, "or",
                |b, l, r| b.build_or(l, r, "or")),
            BinaryOp::BitXor => self.bin_bitwise(&left, &right, "xor",
                |b, l, r| b.build_xor(l, r, "xor")),
            BinaryOp::Shl => self.bin_bitwise(&left, &right, "shl",
                |b, l, r| b.build_left_shift(l, r, "shl")),
            BinaryOp::Shr => self.bin_bitwise(&left, &right, "shr",
                |b, l, r| b.build_right_shift(l, r, false, "shr")),
            BinaryOp::Is => self.bin_is(lhs, rhs),
            BinaryOp::In => self.bin_in(lhs, rhs),
            BinaryOp::Range | BinaryOp::RangeExclusive => {
                let inclusive = matches!(op, BinaryOp::Range);
                let start_int = match &left { TypedValue::Int(v) => *v, _ => return Err("Range start must be integer".into()) };
                let end_int = match &right { TypedValue::Int(v) => *v, _ => return Err("Range end must be integer".into()) };
                let range_ty = self.context.struct_type(
                    &[self.i64_ty().into(), self.i64_ty().into(), self.i64_ty().into()],
                    false,
                );
                let alloca = self.builder.build_alloca(range_ty, "range").map_err(llvm_err)?;
                let sptr = self.builder.build_struct_gep(range_ty, alloca, 0, "r_start").map_err(llvm_err)?;
                self.builder.build_store(sptr, start_int).map_err(llvm_err)?;
                let eptr = self.builder.build_struct_gep(range_ty, alloca, 1, "r_end").map_err(llvm_err)?;
                self.builder.build_store(eptr, end_int).map_err(llvm_err)?;
                let iptr = self.builder.build_struct_gep(range_ty, alloca, 2, "r_inc").map_err(llvm_err)?;
                self.builder.build_store(iptr, self.i64_ty().const_int(if inclusive { 1 } else { 0 }, false)).map_err(llvm_err)?;
                Ok(TypedValue::Struct(alloca, range_ty))
            }
            _ => Err("Operator not supported".to_string()),
        }
    }

    /// Short-circuit AND: evaluate left; if false, result is false; else evaluate right
    /// Short-circuit AND: evaluate left; if false, result is false; else evaluate right
    pub(super) fn compile_and(&mut self, lhs: &Expr, rhs: &Expr) -> Result<TypedValue<'ctx>, String> {
        let left = self.compile_expr(lhs)?;
        let left_bool = match left {
            TypedValue::Bool(b) => b,
            _ => return Err("&& requires boolean operands".to_string()),
        };

        let entry_block = self.builder.get_insert_block().unwrap();
        let current_fn = entry_block.get_parent().unwrap();
        let rhs_block = self.context.append_basic_block(current_fn, "and_rhs");
        let merge_block = self.context.append_basic_block(current_fn, "and_merge");
        let b1 = self.bool_ty();
        let false_val = b1.const_int(0, false);

        self.builder.build_conditional_branch(left_bool, rhs_block, merge_block).map_err(llvm_err)?;

        self.builder.position_at_end(rhs_block);
        let right = self.compile_expr(rhs)?;
        let right_bool = match right {
            TypedValue::Bool(b) => b,
            _ => return Err("&& requires boolean operands".to_string()),
        };
        self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;

        self.builder.position_at_end(merge_block);
        let phi = self.builder.build_phi(b1, "and_res").map_err(llvm_err)?;
        phi.add_incoming(&[
            (&false_val as &dyn inkwell::values::BasicValue, entry_block),
            (&right_bool, rhs_block),
        ]);

        Ok(TypedValue::Bool(phi.as_basic_value().into_int_value()))
    }

    /// Short-circuit OR: evaluate left; if true, result is true; else evaluate right
    pub(super) fn compile_or(&mut self, lhs: &Expr, rhs: &Expr) -> Result<TypedValue<'ctx>, String> {
        let left = self.compile_expr(lhs)?;
        let left_bool = match left {
            TypedValue::Bool(b) => b,
            _ => return Err("|| requires boolean operands".to_string()),
        };

        let entry_block = self.builder.get_insert_block().unwrap();
        let current_fn = entry_block.get_parent().unwrap();
        let rhs_block = self.context.append_basic_block(current_fn, "or_rhs");
        let merge_block = self.context.append_basic_block(current_fn, "or_merge");
        let b1 = self.bool_ty();
        let true_val = b1.const_int(1, false);

        // If left is true, short-circuit to merge with true; else evaluate right
        self.builder.build_conditional_branch(left_bool, merge_block, rhs_block).map_err(llvm_err)?;

        self.builder.position_at_end(rhs_block);
        let right = self.compile_expr(rhs)?;
        let right_bool = match right {
            TypedValue::Bool(b) => b,
            _ => return Err("|| requires boolean operands".to_string()),
        };
        self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;

        self.builder.position_at_end(merge_block);
        let phi = self.builder.build_phi(b1, "or_res").map_err(llvm_err)?;
        phi.add_incoming(&[
            (&true_val as &dyn inkwell::values::BasicValue, entry_block),
            (&right_bool, rhs_block),
        ]);

        Ok(TypedValue::Bool(phi.as_basic_value().into_int_value()))
    }

    pub(super) fn bin_add(&mut self, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) =>
                Ok(TypedValue::Int(self.builder.build_int_add(*a, *b, "add").map_err(llvm_err)?)),
            (TypedValue::Float(a), TypedValue::Float(b)) =>
                Ok(TypedValue::Float(self.builder.build_float_add(*a, *b, "add").map_err(llvm_err)?)),
            (TypedValue::Str(a), TypedValue::Str(b)) => {
                let cc = self.call_rt_with_2str("atomic_string_concat", *a, *b)?;
                match cc.try_as_basic_value().basic() {
                    Some(bv) => {
                        let alloca = self.builder.build_alloca(self.string_type, "concat").map_err(llvm_err)?;
                        self.builder.build_store(alloca, bv).map_err(llvm_err)?;
                        Ok(TypedValue::Str(alloca))
                    }
                    None => Err("String concat failed".to_string()),
                }
            }
            // Int + Float / Float + Int → promote to Float
            (TypedValue::Float(_), _) | (_, TypedValue::Float(_)) => {
                let fa = self.promote_to_float(l)?;
                let fb = self.promote_to_float(r)?;
                Ok(TypedValue::Float(self.builder.build_float_add(fa, fb, "add").map_err(llvm_err)?))
            }
            _ => Err("Cannot add these types".to_string()),
        }
    }

    /// Promote Int to Float for mixed-type arithmetic
    fn promote_to_float(&self, v: &TypedValue<'ctx>) -> Result<inkwell::values::FloatValue<'ctx>, String> {
        match v {
            TypedValue::Int(i) => Ok(self.builder.build_signed_int_to_float(*i, self.f64_ty(), "promote").map_err(llvm_err)?),
            TypedValue::Float(f) => Ok(*f),
            _ => Err("Cannot promote to Float".to_string()),
        }
    }

    pub(super) fn bin_arith(
        &mut self, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>, _n: &str,
        int_op: fn(&inkwell::builder::Builder<'ctx>, IntValue<'ctx>, IntValue<'ctx>) -> Result<IntValue<'ctx>, BuilderError>,
        float_op: fn(&inkwell::builder::Builder<'ctx>, inkwell::values::FloatValue<'ctx>, inkwell::values::FloatValue<'ctx>) -> Result<inkwell::values::FloatValue<'ctx>, BuilderError>,
    ) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) => Ok(TypedValue::Int(int_op(&self.builder, *a, *b).map_err(llvm_err)?)),
            (TypedValue::Float(a), TypedValue::Float(b)) => Ok(TypedValue::Float(float_op(&self.builder, *a, *b).map_err(llvm_err)?)),
            // Int + Float → promote Int to Float
            (TypedValue::Float(_), _) | (_, TypedValue::Float(_)) => {
                let fa = self.promote_to_float(l)?;
                let fb = self.promote_to_float(r)?;
                Ok(TypedValue::Float(float_op(&self.builder, fa, fb).map_err(llvm_err)?))
            }
            _ => Err("Cannot perform arithmetic on these types".to_string()),
        }
    }

    pub(super) fn bin_mod(&mut self, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) =>
                Ok(TypedValue::Int(self.builder.build_int_signed_rem(*a, *b, "mod").map_err(llvm_err)?)),
            _ => Err("Modulo requires integer operands".to_string()),
        }
    }

    pub(super) fn bin_pow(&mut self, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) => {
                let pow_fn = self.module.get_function("atomic_int_pow").unwrap();
                let result = self.builder.build_call(pow_fn, &[(*a).into(), (*b).into()], "pow")
                    .map_err(llvm_err)?
                    .try_as_basic_value().basic().unwrap().into_int_value();
                Ok(TypedValue::Int(result))
            }
            (TypedValue::Float(a), TypedValue::Float(b)) => {
                let pow_fn = self.module.get_function("pow").unwrap();
                let result = self.builder.build_call(pow_fn, &[(*a).into(), (*b).into()], "pow")
                    .map_err(llvm_err)?
                    .try_as_basic_value().basic().unwrap().into_float_value();
                Ok(TypedValue::Float(result))
            }
            // Mixed Int/Float → promote to Float
            (TypedValue::Float(_), _) | (_, TypedValue::Float(_)) => {
                let fa = self.promote_to_float(l)?;
                let fb = self.promote_to_float(r)?;
                let pow_fn = self.module.get_function("pow").unwrap();
                let result = self.builder.build_call(pow_fn, &[fa.into(), fb.into()], "pow")
                    .map_err(llvm_err)?
                    .try_as_basic_value().basic().unwrap().into_float_value();
                Ok(TypedValue::Float(result))
            }
            _ => Err("** requires numeric operands".to_string()),
        }
    }

    pub(super) fn bin_bitwise(
        &mut self, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>, _n: &str,
        op: fn(&inkwell::builder::Builder<'ctx>, IntValue<'ctx>, IntValue<'ctx>) -> Result<IntValue<'ctx>, BuilderError>,
    ) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) => Ok(TypedValue::Int(op(&self.builder, *a, *b).map_err(llvm_err)?)),
            _ => Err("Bitwise operations require integer operands".to_string()),
        }
    }

    pub(super) fn compare_eq(&mut self, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) =>
                Ok(TypedValue::Bool(self.builder.build_int_compare(IntPredicate::EQ, *a, *b, "eq").map_err(llvm_err)?)),
            (TypedValue::Bool(a), TypedValue::Bool(b)) =>
                Ok(TypedValue::Bool(self.builder.build_int_compare(IntPredicate::EQ, *a, *b, "eq").map_err(llvm_err)?)),
            (TypedValue::Float(a), TypedValue::Float(b)) =>
                Ok(TypedValue::Bool(self.builder.build_float_compare(FloatPredicate::OEQ, *a, *b, "eq").map_err(llvm_err)?)),
            // Int + Float comparison → promote Int to Float
            (TypedValue::Float(_), _) | (_, TypedValue::Float(_)) => {
                let fa = self.promote_to_float(l)?;
                let fb = self.promote_to_float(r)?;
                Ok(TypedValue::Bool(self.builder.build_float_compare(FloatPredicate::OEQ, fa, fb, "eq").map_err(llvm_err)?))
            }
            (TypedValue::Str(a), TypedValue::Str(b)) => {
                let sa = self.load_string(*a)?;
                let sb = self.load_string(*b)?;
                let cc = self.call_rt("atomic_string_eq", &[sa.into(), sb.into()])?;
                Ok(TypedValue::Bool(cc.try_as_basic_value().basic().ok_or("streq failed")?.into_int_value()))
            }
            _ => Err("Cannot compare these types".to_string()),
        }
    }

    pub(super) fn compare_neq(&mut self, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) =>
                Ok(TypedValue::Bool(self.builder.build_int_compare(IntPredicate::NE, *a, *b, "neq").map_err(llvm_err)?)),
            (TypedValue::Bool(a), TypedValue::Bool(b)) =>
                Ok(TypedValue::Bool(self.builder.build_int_compare(IntPredicate::NE, *a, *b, "neq").map_err(llvm_err)?)),
            (TypedValue::Float(a), TypedValue::Float(b)) =>
                Ok(TypedValue::Bool(self.builder.build_float_compare(FloatPredicate::ONE, *a, *b, "neq").map_err(llvm_err)?)),
            // Int + Float comparison → promote Int to Float
            (TypedValue::Float(_), _) | (_, TypedValue::Float(_)) => {
                let fa = self.promote_to_float(l)?;
                let fb = self.promote_to_float(r)?;
                Ok(TypedValue::Bool(self.builder.build_float_compare(FloatPredicate::ONE, fa, fb, "neq").map_err(llvm_err)?))
            }
            (TypedValue::Str(a), TypedValue::Str(b)) => {
                let sa = self.load_string(*a)?;
                let sb = self.load_string(*b)?;
                let cc = self.call_rt("atomic_string_eq", &[sa.into(), sb.into()])?;
                let eq = cc.try_as_basic_value().basic().ok_or("strneq failed")?.into_int_value();
                let one = self.bool_ty().const_int(1, false);
                Ok(TypedValue::Bool(self.builder.build_xor(eq, one, "strneq").map_err(llvm_err)?))
            }
            _ => Err("Cannot compare these types".to_string()),
        }
    }

    pub(super) fn compare(
        &mut self, ip: IntPredicate, fp: FloatPredicate, l: &TypedValue<'ctx>, r: &TypedValue<'ctx>,
    ) -> Result<TypedValue<'ctx>, String> {
        match (l, r) {
            (TypedValue::Int(a), TypedValue::Int(b)) =>
                Ok(TypedValue::Bool(self.builder.build_int_compare(ip, *a, *b, "cmp").map_err(llvm_err)?)),
            (TypedValue::Bool(a), TypedValue::Bool(b)) =>
                Ok(TypedValue::Bool(self.builder.build_int_compare(ip, *a, *b, "cmp").map_err(llvm_err)?)),
            (TypedValue::Float(a), TypedValue::Float(b)) =>
                Ok(TypedValue::Bool(self.builder.build_float_compare(fp, *a, *b, "cmp").map_err(llvm_err)?)),
            // Int + Float comparison → promote Int to Float
            (TypedValue::Float(_), _) | (_, TypedValue::Float(_)) => {
                let fa = self.promote_to_float(l)?;
                let fb = self.promote_to_float(r)?;
                Ok(TypedValue::Bool(self.builder.build_float_compare(fp, fa, fb, "cmp").map_err(llvm_err)?))
            }
            (TypedValue::Str(a), TypedValue::Str(b)) => {
                let sa = self.load_string(*a)?;
                let sb = self.load_string(*b)?;
                let cc = self.call_rt("atomic_string_compare", &[sa.into(), sb.into()])?;
                let cmp = cc.try_as_basic_value().basic().ok_or("strcmp failed")?.into_int_value();
                Ok(TypedValue::Bool(self.builder.build_int_compare(ip, cmp, self.i64_ty().const_int(0, false), "strcmp").map_err(llvm_err)?))
            }
            _ => Err("Cannot compare these types".to_string()),
        }
    }

    /// `in` operator: value in range, value in list, value in set, key in map
    pub(super) fn bin_in(&mut self, lhs: &Expr, rhs: &Expr) -> Result<TypedValue<'ctx>, String> {
        let value = self.compile_expr(lhs)?;
        // Check if rhs is a range expression
        match rhs {
            Expr::Range(start_expr, end_expr) => {
                let start = self.compile_expr(start_expr)?;
                let end = self.compile_expr(end_expr)?;
                let start_int = match start { TypedValue::Int(v) => v, _ => return Err("Range start must be integer".into()) };
                let end_int = match end { TypedValue::Int(v) => v, _ => return Err("Range end must be integer".into()) };
                let val_int = match value { TypedValue::Int(v) => v, _ => return Err("Value must be integer for range check".into()) };
                let ge_start = self.builder.build_int_compare(IntPredicate::SGE, val_int, start_int, "in_ge").map_err(llvm_err)?;
                let lt_end = self.builder.build_int_compare(IntPredicate::SLT, val_int, end_int, "in_lt").map_err(llvm_err)?;
                let result = self.builder.build_and(ge_start, lt_end, "in_range").map_err(llvm_err)?;
                Ok(TypedValue::Bool(result))
            }
            Expr::Binary(start_expr, BinaryOp::RangeExclusive, end_expr) => {
                let start = self.compile_expr(start_expr)?;
                let end = self.compile_expr(end_expr)?;
                let start_int = match start { TypedValue::Int(v) => v, _ => return Err("Range start must be integer".into()) };
                let end_int = match end { TypedValue::Int(v) => v, _ => return Err("Range end must be integer".into()) };
                let val_int = match value { TypedValue::Int(v) => v, _ => return Err("Value must be integer for range check".into()) };
                let ge_start = self.builder.build_int_compare(IntPredicate::SGE, val_int, start_int, "in_ge").map_err(llvm_err)?;
                // Exclusive: value < end (not <=)
                let lt_end = self.builder.build_int_compare(IntPredicate::SLT, val_int, end_int, "in_lt").map_err(llvm_err)?;
                let result = self.builder.build_and(ge_start, lt_end, "in_range_excl").map_err(llvm_err)?;
                Ok(TypedValue::Bool(result))
            }
            _ => {
                // Collection containment: list, set, map
                let collection = self.compile_expr(rhs)?;
                match collection {
                    TypedValue::List(ptr) | TypedValue::Set(ptr) | TypedValue::LazyList(ptr) => {
                        let elem_fat = self.to_fat_struct(&value)?;
                        let list_val = self.load_list(ptr)?;
                        let cc = self.call_rt("atomic_list_contains", &[list_val.into(), elem_fat.into()])?;
                        let result_bv = cc.try_as_basic_value().basic().ok_or("list_contains failed")?;
                        let result = result_bv.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    TypedValue::Stream(ptr) => {
                        let elem_fat = self.to_fat_struct(&value)?;
                        let list_field = self.builder.build_struct_gep(self.stream_type, ptr, 1, "in_strm_lf").map_err(llvm_err)?;
                        let list_val = self.builder.build_load(self.list_type, list_field, "in_strm_lv").map_err(llvm_err)?;
                        let cc = self.call_rt("atomic_list_contains", &[list_val.into(), elem_fat.into()])?;
                        let result_bv = cc.try_as_basic_value().basic().ok_or("list_contains failed")?;
                        let result = result_bv.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    TypedValue::Map(ptr) => {
                        let key_fat = self.to_fat_struct(&value)?;
                        let map_val = self.load_list(ptr)?;
                        let cc = self.call_rt("atomic_map_contains", &[map_val.into(), key_fat.into()])?;
                        let result_bv = cc.try_as_basic_value().basic().ok_or("map_contains failed")?;
                        let result = result_bv.into_int_value();
                        Ok(TypedValue::Bool(result))
                    }
                    _ => Err("'in' operator requires a range or collection on the right".into()),
                }
            }
        }
    }

    /// `is` operator: expr is Type — runtime type check
    pub(super) fn bin_is(&mut self, lhs: &Expr, rhs: &Expr) -> Result<TypedValue<'ctx>, String> {
        let type_name = match rhs {
            Expr::Ident(name) => name.clone(),
            _ => return Err("'is' operator requires a type name on the right".into()),
        };

        // Check if rhs is a known enum variant (e.g., x is Some)
        if let Some((enum_info, variant_info)) = self.registry.lookup_variant(&type_name) {
            let variant_idx = enum_info.variants.iter()
                .position(|v| v.name == variant_info.name)
                .unwrap_or(0) as u64;

            let val = self.compile_expr(lhs)?;
            match val {
                TypedValue::Enum(ptr, enum_ty, ..) => {
                    let loaded = self.builder.build_load(enum_ty, ptr, "is_ld").map_err(llvm_err)?;
                    let tag = self.builder.build_extract_value(loaded.into_struct_value(), 0, "tag").map_err(llvm_err)?;
                    let cmp = self.builder.build_int_compare(
                        IntPredicate::EQ,
                        tag.into_int_value(),
                        self.i64_ty().const_int(variant_idx, false),
                        "is_match"
                    ).map_err(llvm_err)?;
                    Ok(TypedValue::Bool(cmp))
                }
                _ => {
                    // Non-enum value can't match an enum variant
                    Ok(TypedValue::Bool(self.bool_ty().const_int(0, false)))
                }
            }
        } else {
            // For non-enum type names (Int, String, etc.), type safety is static
            // Compile lhs for side effects, return true
            let _ = self.compile_expr(lhs)?;
            Ok(TypedValue::Bool(self.bool_ty().const_int(1, false)))
        }
    }

    pub(super) fn compile_unary(&mut self, op: UnaryOp, expr: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(expr)?;
        match op {
            UnaryOp::Neg => match val {
                TypedValue::Int(v) => Ok(TypedValue::Int(self.builder.build_int_neg(v, "neg").map_err(llvm_err)?)),
                TypedValue::Float(v) => Ok(TypedValue::Float(self.builder.build_float_neg(v, "neg").map_err(llvm_err)?)),
                _ => Err("Cannot negate this type".to_string()),
            },
            UnaryOp::Not => match val {
                TypedValue::Bool(v) => Ok(TypedValue::Bool(self.builder.build_not(v, "not").map_err(llvm_err)?)),
                _ => Err("'not' requires boolean operand".to_string()),
            },
            UnaryOp::BitNot => match val {
                TypedValue::Int(v) => Ok(TypedValue::Int(self.builder.build_not(v, "bitnot").map_err(llvm_err)?)),
                _ => Err("'~' requires integer operand".to_string()),
            },
        }
    }

    /// Map a TypedValue to its type name string for UFCS lookup
    pub(super) fn type_name_from_typed_value(&self, v: &TypedValue<'ctx>) -> String {
        match v {
            TypedValue::Int(_) => "Int".to_string(),
            TypedValue::Float(_) => "Float".to_string(),
            TypedValue::Bool(_) => "Bool".to_string(),
            TypedValue::Str(_) => "String".to_string(),
            TypedValue::Struct(_, st) => {
                for (name, ty) in &self.named_structs {
                    if *ty == *st {
                        return name.clone();
                    }
                }
                "Struct".to_string()
            }
            TypedValue::Enum(..) => "Enum".to_string(),
            TypedValue::Unit => "Unit".to_string(),
            TypedValue::Fn(_, _) => "Fn".to_string(),
            TypedValue::List(_) => "List".to_string(),
            TypedValue::Map(_) => "Map".to_string(),
            TypedValue::Set(_) => "Set".to_string(),
            TypedValue::Task(_) => "Task".to_string(),
            TypedValue::Stream(_) => "Stream".to_string(),
            TypedValue::LazyList(_) => "LazyList".to_string(),
            TypedValue::CString(_) => "CString".to_string(),
            TypedValue::Ptr(_) => "Ptr".to_string(),
            TypedValue::FileHandle(_) => "FileHandle".to_string(),
        }
    }

    /// Compile an expression and load the result as a BasicValueEnum for passing as a call argument.
    /// Handles loading from alloca pointers for enum, struct, and string types.
    pub(super) fn compile_and_load(&mut self, expr: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let v = self.compile_expr(expr)?;
        match &v {
            TypedValue::Enum(ptr, ty, ..) => {
                let bt: BasicTypeEnum = (*ty).into();
                Ok(self.builder.build_load(bt, *ptr, "arg_enum").map_err(llvm_err)?)
            }
            TypedValue::Struct(ptr, ty) => {
                let bt: BasicTypeEnum = (*ty).into();
                Ok(self.builder.build_load(bt, *ptr, "arg_struct").map_err(llvm_err)?)
            }
            TypedValue::Str(ptr) => {
                Ok(self.load_string(*ptr)?.into())
            }
            TypedValue::List(ptr) => {
                Ok(self.load_list(*ptr)?.into())
            }
            TypedValue::Map(ptr) => {
                Ok(self.load_list(*ptr)?.into())
            }
            TypedValue::Set(ptr) => {
                Ok(self.load_list(*ptr)?.into())
            }
            TypedValue::CString(p) | TypedValue::Ptr(p) | TypedValue::FileHandle(p) => {
                Ok((*p).into())
            }
            _ => {
                v.to_bv().ok_or_else(|| format!("Cannot pass value as argument"))
            }
        }
    }

    /// Coerce an argument value to match the expected parameter type
    pub(super) fn coerce_arg(&mut self, val: BasicValueEnum<'ctx>, expected_ty: Option<&BasicMetadataTypeEnum<'ctx>>) -> Result<BasicValueEnum<'ctx>, String> {
        let expected_ty: BasicTypeEnum = match expected_ty {
            Some(t) => match BasicTypeEnum::try_from(*t) {
                Ok(bt) => bt,
                Err(_) => return Ok(val),
            },
            None => return Ok(val),
        };
        let actual_is_ptr = matches!(val, BasicValueEnum::PointerValue(_));
        let expected_is_i64 = matches!(expected_ty, BasicTypeEnum::IntType(t) if t.get_bit_width() == 64);
        let expected_is_f64 = matches!(expected_ty, BasicTypeEnum::FloatType(_));
        let expected_is_ptr = matches!(expected_ty, BasicTypeEnum::PointerType(_));
        let actual_is_i64 = matches!(val, BasicValueEnum::IntValue(i) if i.get_type().get_bit_width() == 64);
        let actual_is_f64 = matches!(val, BasicValueEnum::FloatValue(_));

        if actual_is_ptr && expected_is_i64 {
            // ptr → i64: function pointer passed to untyped parameter
            let ptr_val = val.into_pointer_value();
            let i64_val = self.builder.build_ptr_to_int(ptr_val, self.i64_ty(), "ptr2int")
                .map_err(llvm_err)?;
            Ok(i64_val.as_basic_value_enum())
        } else if !actual_is_ptr && expected_is_ptr {
            // i64 → ptr: int passed to function pointer parameter
            let int_val = val.into_int_value();
            let ptr_val = self.builder.build_int_to_ptr(int_val, self.ptr_ty(), "int2ptr")
                .map_err(llvm_err)?;
            Ok(ptr_val.as_basic_value_enum())
        } else if actual_is_i64 && expected_is_f64 {
            // Int → Float promotion
            let int_val = val.into_int_value();
            let float_val = self.builder.build_signed_int_to_float(int_val, self.f64_ty(), "int2float")
                .map_err(llvm_err)?;
            Ok(float_val.as_basic_value_enum())
        } else if actual_is_f64 && expected_is_i64 {
            // Float → Int truncation
            let float_val = val.into_float_value();
            let int_val = self.builder.build_float_to_signed_int(float_val, self.i64_ty(), "float2int")
                .map_err(llvm_err)?;
            Ok(int_val.as_basic_value_enum())
        } else {
            Ok(val)
        }
    }

    /// Compile function reference: ::function_name, ::Type.method, ::module::func
    pub(super) fn compile_function_ref(&mut self, name: &str) -> Result<TypedValue<'ctx>, String> {
        // Resolve :: separators in path (e.g., "math::add" or "Type::method")
        let resolved = name.replace("::", "_").replace('.', "_");

        // Try the resolved name first (handles module::function -> module_function)
        if let Some(fn_val) = self.module.get_function(&resolved) {
            let fn_ptr = fn_val.as_global_value().as_pointer_value();
            let fn_type = fn_val.get_type();
            return Ok(TypedValue::Fn(fn_ptr, fn_type));
        }

        // Try the original name (handles simple ::function_name)
        if let Some(fn_val) = self.module.get_function(name) {
            let fn_ptr = fn_val.as_global_value().as_pointer_value();
            let fn_type = fn_val.get_type();
            return Ok(TypedValue::Fn(fn_ptr, fn_type));
        }

        // Handle Type.method pattern: ::Int.toString -> atomic_int_to_string
        if let Some((type_part, method)) = name.rsplit_once('.') {
            let type_name = type_part;
            // Map type-method to runtime function name
            // Many builtins have corresponding atomic_* runtime functions
            let rt_name = match (type_name, method) {
                // Int/Float/Bool -> String conversions
                ("Int", "toString") | ("Bool", "toString") => "atomic_int_to_string",
                ("Float", "toString") => "atomic_float_to_string",
                // String methods
                ("String", "len") | ("String", "length") => "atomic_string_len",
                ("String", "to_upper") => "atomic_string_to_upper",
                ("String", "to_lower") => "atomic_string_to_lower",
                ("String", "trim") => "atomic_string_trim",
                ("String", "substring") => "atomic_string_substring",
                ("String", "starts_with") => "atomic_string_starts_with",
                ("String", "ends_with") => "atomic_string_ends_with",
                ("String", "split") => "atomic_string_split",
                ("String", "contains") => "atomic_string_contains",
                ("String", "to_int") | ("String", "to_float") => {
                    return Err(format!("::{}::{} cannot be used as a function reference (requires parsing)", type_name, method));
                }
                // List methods
                ("List", "len") | ("Map", "len") | ("Set", "len") => "atomic_list_len",
                ("List", "head") => "atomic_list_head",
                ("List", "last") => "atomic_list_last",
                ("List", "tail") => "atomic_list_tail",
                ("List", "init") => "atomic_list_init",
                ("List", "reverse") => "atomic_list_reverse",
                ("List", "take") => "atomic_list_take",
                ("List", "drop") => "atomic_list_drop",
                ("List", "contains") => "atomic_list_contains",
                ("List", "zip") => "atomic_list_zip",
                ("List", "get") => "atomic_list_get",
                ("List", "append") | ("List", "push") => "atomic_list_push",
                ("List", "range") => "atomic_list_range",
                // Map methods
                ("Map", "contains") => "atomic_map_contains",
                ("Map", "get") => "atomic_map_get",
                ("Map", "insert") => "atomic_map_insert",
                ("Map", "remove") => "atomic_map_remove",
                // Other String methods with runtime functions
                ("String", "chars") => "atomic_string_chars",
                ("String", "join") => "atomic_string_join",
                ("String", "replace") => "atomic_string_replace",
                ("String", "repeat") => "atomic_string_repeat",
                ("String", "trim_start") => "atomic_string_trim_start",
                ("String", "trim_end") => "atomic_string_trim_end",
                // Methods without simple runtime function counterparts
                ("List", "map") | ("List", "filter") | ("List", "fold") | ("List", "flat_map")
                | ("List", "flatten") | ("List", "unique") | ("List", "with_index")
                | ("List", "sorted") | ("List", "sum") | ("List", "product")
                | ("List", "prepend") | ("List", "is_empty") | ("List", "any") | ("List", "all")
                | ("List", "find") | ("List", "reduce") | ("List", "split_lines")
                | ("Option", "map") | ("Option", "flatMap") | ("Option", "unwrap")
                | ("Option", "unwrap_or") | ("Option", "is_some") | ("Option", "is_none")
                | ("LazyList", _) | ("Task", _) | ("Stream", _) | ("Ptr", _) | ("CString", _) => {
                    // These either take function arguments or operate on complex types —
                    // register as wrapper-needed and create a placeholder
                    self.builtin_wrappers_needed.insert(method.to_string());
                    return Err(format!(
                        "::{}::{} requires runtime support not yet available as function reference",
                        type_name, method
                    ));
                }
                _ => {
                    // Try Type_method mangling for extension methods
                    let mangled = format!("{}_{}", type_name, method);
                    if let Some(fn_val) = self.module.get_function(&mangled) {
                        let fn_ptr = fn_val.as_global_value().as_pointer_value();
                        let fn_type = fn_val.get_type();
                        return Ok(TypedValue::Fn(fn_ptr, fn_type));
                    }
                    let alt_mangled = format!("{}_{}", type_part.replace("::", "_"), method);
                    if mangled != alt_mangled {
                        if let Some(fn_val) = self.module.get_function(&alt_mangled) {
                            let fn_ptr = fn_val.as_global_value().as_pointer_value();
                            let fn_type = fn_val.get_type();
                            return Ok(TypedValue::Fn(fn_ptr, fn_type));
                        }
                    }
                    return Err(format!("Function reference '::{}' could not be resolved", name));
                }
            };
            // Look up the runtime function
            if let Some(fn_val) = self.module.get_function(rt_name) {
                let fn_ptr = fn_val.as_global_value().as_pointer_value();
                let fn_type = fn_val.get_type();
                return Ok(TypedValue::Fn(fn_ptr, fn_type));
            }
            Err(format!("Runtime function '{}' not found for ::{}", rt_name, name))
        } else {
            Err(format!("Undefined function reference: ::{}", name))
        }
    }

}
