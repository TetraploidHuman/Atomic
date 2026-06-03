// Submodule: stmt

use atomic::ast::*;
use inkwell::values::PointerValue;
use inkwell::types::{BasicType, BasicTypeEnum, BasicMetadataTypeEnum};
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, ValKind, Scope, TcoState, llvm_err};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Let { name, type_ann, value, propagate, mutable, lazy_init, .. } => {
                if *propagate {
                    return self.compile_propagate_let(name, type_ann.as_ref(), value);
                }
                if *lazy_init {
                    // Lazy val: defer evaluation to first access
                    let (ty, kind) = if let Some(ann) = type_ann {
                        (self.ast_type_to_basic_type(ann), self.param_val_kind(Some(ann)))
                    } else {
                        // For lazy val without type annotation, infer from expression
                        // We need to peek at the expression type without evaluating it
                        (self.i64_ty().into(), ValKind::Int) // default, will be refined on access
                    };
                    let alloca = self.builder.build_alloca(ty, name).map_err(llvm_err)?;
                    let flag = self.builder.build_alloca(self.bool_ty(), &format!("{}_lazy_flag", name)).map_err(llvm_err)?;
                    self.builder.build_store(flag, self.bool_ty().const_int(0, false)).map_err(llvm_err)?;
                    self.scope.set_lazy(name.clone(), alloca, ty, kind, flag, value.clone());
                } else {
                    let val = self.compile_expr(value)?;
                    let (ty, kind) = if let Some(ann) = type_ann {
                        (self.ast_type_to_basic_type(ann), self.param_val_kind(Some(ann)))
                    } else {
                        (val.get_type_for_alloca(self), val.val_kind())
                    };
                    let alloca = self.builder.build_alloca(ty, name).map_err(llvm_err)?;
                    self.store_typed_value(&val, alloca, ty)?;
                    // RC: increment refcount for heap-typed values being bound to a variable
                    self.rc_inc_typed_value(&val)?;
                    let fn_type = match &val {
                        TypedValue::Fn(_, ft) => Some(*ft),
                        _ => None,
                    };
                    // Infer AST type for enum values to support pattern match resolution
                    let ast_type = (*type_ann).clone().or_else(|| {
                        if matches!(kind, ValKind::Enum) {
                            let inferred = self.infer_expr_type(value);
                            if matches!(inferred, Type::Named(_) | Type::Generic(_, _)) {
                                Some(inferred)
                            } else { None }
                        } else { None }
                    });
                    if *mutable {
                        self.scope.set_mutable(name.clone(), alloca, ty, kind, fn_type);
                    } else if let Some(at) = ast_type {
                        self.scope.set_with_ast_type(name.clone(), alloca, ty, kind, fn_type, at);
                    } else {
                        self.scope.set_with_fn_type(name.clone(), alloca, ty, kind, fn_type);
                    }
                    // Preserve enum inner type (Int/Float/Str) for unwrap etc.
                    if let TypedValue::Enum(_, _, inner_type, rc_managed) = &val {
                        self.scope.set_enum_inner_type(name, *inner_type);
                        self.scope.set_enum_data_rc_managed(name, *rc_managed);
                    }
                }
            }
            Stmt::Destructure { names, renames, rest, is_list, is_struct, value, mutable, .. } => {
                let val = self.compile_expr(value)?;
                if *is_list {
                    // List destructuring: val [a, b, c] = list or val [head, ...tail] = list
                    let list_ptr = match val {
                        TypedValue::List(ptr) => ptr,
                        _ => return Err("List destructuring requires a list value".to_string()),
                    };
                    let list_val = self.load_list(list_ptr)?;
                    let data = self.builder.build_extract_value(list_val, 0, "data").map_err(llvm_err)?
                        .into_pointer_value();
                    let len = self.builder.build_extract_value(list_val, 1, "len").map_err(llvm_err)?
                        .into_int_value();
                    let data_str = self.builder.build_pointer_cast(data, self.ptr_ty(), "data_str").map_err(llvm_err)?;
                    // Bind named elements
                    for (i, name) in names.iter().enumerate() {
                        let idx = self.i64_ty().const_int(i as u64, false);
                        let elem_ptr = unsafe { self.builder.build_gep(self.string_type, data_str, &[idx], "delem_ptr") }.map_err(llvm_err)?;
                        let loaded = self.builder.build_load(self.string_type, elem_ptr, "delem").map_err(llvm_err)?;
                        let ss = loaded.into_struct_value();
                        let tag = self.builder.build_extract_value(ss, 0, "tag").map_err(llvm_err)?.into_int_value();
                        let tag_ty = tag.get_type();
                        let alloca = self.builder.build_alloca(tag_ty, name).map_err(llvm_err)?;
                        self.builder.build_store(alloca, tag).map_err(llvm_err)?;
                        if *mutable {
                            self.scope.set_mutable(name.clone(), alloca, tag_ty.into(), ValKind::Int, None);
                        } else {
                            self.scope.set(name.clone(), alloca, tag_ty.into(), ValKind::Int);
                        }
                    }
                    // Bind rest (tail): create a new list from the remaining elements
                    if let Some(rest_name) = rest {
                        let start_idx = names.len() as u64;
                        let _new_len = self.builder.build_int_sub(
                            len, self.i64_ty().const_int(start_idx, false), "rest_len"
                        ).map_err(llvm_err)?;
                        // Create new list
                        let cap = self.i64_ty().const_int(4, false);
                        let new_list_cc = self.call_rt("atomic_list_create", &[cap.into()])?;
                        let new_list_bv = new_list_cc.try_as_basic_value().basic().ok_or("rest list create fail")?;
                        let rest_alloca = self.builder.build_alloca(self.list_type, rest_name).map_err(llvm_err)?;
                        self.builder.build_store(rest_alloca, new_list_bv).map_err(llvm_err)?;
                        // Copy remaining elements via loop
                        let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                        let loop_header_block = self.context.append_basic_block(current_fn, "rest_hdr");
                        let loop_body_block = self.context.append_basic_block(current_fn, "rest_bdy");
                        let done_block = self.context.append_basic_block(current_fn, "rest_done");
                        let cur_block = self.builder.get_insert_block().unwrap();
                        let _ = self.builder.build_unconditional_branch(loop_header_block);
                        // Loop header: phi + condition
                        self.builder.position_at_end(loop_header_block);
                        let i_phi = self.builder.build_phi(self.i64_ty(), "rest_i").map_err(llvm_err)?;
                        let start_idx_val = self.i64_ty().const_int(start_idx, false);
                        i_phi.add_incoming(&[(&start_idx_val, cur_block)]);
                        let done_cond = self.builder.build_int_compare(
                            IntPredicate::SGE, i_phi.as_basic_value().into_int_value(), len, "rest_done"
                        ).map_err(llvm_err)?;
                        let _ = self.builder.build_conditional_branch(done_cond, done_block, loop_body_block);
                        // Loop body: copy element at i_phi to rest list
                        self.builder.position_at_end(loop_body_block);
                        let src_ptr = unsafe { self.builder.build_gep(self.string_type, data_str, &[i_phi.as_basic_value().into_int_value()], "rest_src") }.map_err(llvm_err)?;
                        let elem = self.builder.build_load(self.string_type, src_ptr, "rest_elem").map_err(llvm_err)?;
                        let rest_loaded = self.load_list(rest_alloca)?;
                        let pushed = self.call_rt("atomic_list_push", &[rest_loaded.into(), elem.into()])?;
                        let new_rest = pushed.try_as_basic_value().basic().ok_or("rest push fail")?;
                        self.builder.build_store(rest_alloca, new_rest).map_err(llvm_err)?;
                        let next_i = self.builder.build_int_add(
                            i_phi.as_basic_value().into_int_value(),
                            self.i64_ty().const_int(1, false), "rest_next"
                        ).map_err(llvm_err)?;
                        i_phi.add_incoming(&[(&next_i, loop_body_block)]);
                        let _ = self.builder.build_unconditional_branch(loop_header_block);
                        self.builder.position_at_end(done_block);
                        if *mutable {
                            self.scope.set_mutable(rest_name.clone(), rest_alloca, self.list_type.into(), ValKind::List, None);
                        } else {
                            self.scope.set(rest_name.clone(), rest_alloca, self.list_type.into(), ValKind::List);
                        }
                    }
                } else if *is_struct {
                    // Struct destructuring: val {x, y} = struct_val
                    match val {
                        TypedValue::Struct(alloca, struct_ty) => {
                            let bt: BasicTypeEnum = struct_ty.into();
                            let loaded = self.builder.build_load(bt, alloca, "destr_struct").map_err(llvm_err)?
                                .into_struct_value();
                            // Find field name → index mapping from anon_structs
                            let field_names: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                            let field_indices: Vec<usize> = if let Some((key, _)) = self.anon_structs.iter()
                                .find(|(k, _)| k.as_slice() == field_names)
                            {
                                (0..key.len()).collect()
                            } else {
                                // Fallback: use declaration order
                                (0..names.len()).collect()
                            };
                            for (i, name) in names.iter().enumerate() {
                                let field_idx = field_indices[i] as u32;
                                let field = self.builder.build_extract_value(loaded, field_idx, &format!("f{}", i))
                                    .map_err(llvm_err)?;
                                let field_ty = field.get_type();
                                // Determine the local variable name (with rename support)
                                let local_name = renames.iter()
                                    .find(|(fld, _)| fld == name)
                                    .map(|(_, local)| local.clone())
                                    .unwrap_or_else(|| name.clone());
                                let field_alloca = self.builder.build_alloca(field_ty, &local_name).map_err(llvm_err)?;
                                self.builder.build_store(field_alloca, field).map_err(llvm_err)?;
                                let kind = self.bv_kind(&field);
                                if *mutable {
                                    self.scope.set_mutable(local_name, field_alloca, field_ty, kind, None);
                                } else {
                                    self.scope.set(local_name, field_alloca, field_ty, kind);
                                }
                            }
                        }
                        _ => return Err("Struct destructuring requires a struct value".to_string()),
                    }
                } else {
                    // Tuple destructuring: val (x, y) = tuple
                    match val {
                        TypedValue::Struct(alloca, struct_ty) => {
                            let bt: BasicTypeEnum = struct_ty.into();
                            let loaded = self.builder.build_load(bt, alloca, "destr_tuple").map_err(llvm_err)?
                                .into_struct_value();
                            for (i, name) in names.iter().enumerate() {
                                let field = self.builder.build_extract_value(loaded, i as u32, &format!("f{}", i))
                                    .map_err(llvm_err)?;
                                let field_ty = field.get_type();
                                let field_alloca = self.builder.build_alloca(field_ty, name).map_err(llvm_err)?;
                                self.builder.build_store(field_alloca, field).map_err(llvm_err)?;
                                let kind = self.bv_kind(&field);
                                if *mutable {
                                    self.scope.set_mutable(name.clone(), field_alloca, field_ty, kind, None);
                                } else {
                                    self.scope.set(name.clone(), field_alloca, field_ty, kind);
                                }
                            }
                        }
                        _ => return Err("Destructuring requires a tuple value".to_string()),
                    }
                }
            }
            Stmt::Const { name, value, .. } => {
                match value {
                    Expr::Literal(lit) => {
                        let (global_ptr, ty, kind): (PointerValue, BasicTypeEnum, ValKind) = match lit {
                            Literal::Int(n) => {
                                let g = self.module.add_global(self.i64_ty(), None, name);
                                g.set_initializer(&self.i64_ty().const_int(*n as u64, true));
                                (g.as_pointer_value(), self.i64_ty().into(), ValKind::Int)
                            }
                            Literal::Float(n) => {
                                let g = self.module.add_global(self.f64_ty(), None, name);
                                g.set_initializer(&self.f64_ty().const_float(*n));
                                (g.as_pointer_value(), self.f64_ty().into(), ValKind::Float)
                            }
                            Literal::Bool(b) => {
                                let g = self.module.add_global(self.bool_ty(), None, name);
                                g.set_initializer(&self.bool_ty().const_int(if *b { 1 } else { 0 }, false));
                                (g.as_pointer_value(), self.bool_ty().into(), ValKind::Bool)
                            }
                            Literal::Char(c) => {
                                let g = self.module.add_global(self.i64_ty(), None, name);
                                g.set_initializer(&self.i64_ty().const_int(*c as u64, false));
                                (g.as_pointer_value(), self.i64_ty().into(), ValKind::Int)
                            }
                            Literal::Unit => {
                                let g = self.module.add_global(self.i64_ty(), None, name);
                                g.set_initializer(&self.i64_ty().const_int(0, false));
                                (g.as_pointer_value(), self.i64_ty().into(), ValKind::Unit)
                            }
                            Literal::String(s) => {
                                // Create a global string constant: {i64, ptr} fat struct
                                // First, create a global byte array for the string data
                                let content_bytes: Vec<u8> = s.bytes().chain(std::iter::once(0)).collect();
                                let arr_ty = self.context.i8_type().array_type(content_bytes.len() as u32);
                                let str_data_g = self.module.add_global(arr_ty, None, &format!("__const_str_data_{}", name));
                                let arr_val = self.context.const_string(&content_bytes, false);
                                str_data_g.set_initializer(&arr_val);
                                // Create constant fat struct {i64, ptr}
                                let len_val = self.i64_ty().const_int(s.len() as u64, false);
                                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                                let data_ptr = str_data_g.as_pointer_value();
                                let data_ptr_i8 = data_ptr.const_cast(i8_ptr_ty);
                                let fat_struct = self.context.const_struct(
                                    &[len_val.into(), data_ptr_i8.into()], false,
                                );
                                let g = self.module.add_global(self.string_type, None, name);
                                g.set_initializer(&fat_struct);
                                (g.as_pointer_value(), self.string_type.into(), ValKind::Str)
                            }
                        };
                        self.consts.insert(name.clone(), (global_ptr, ty, kind));
                    }
                    _ => {
                        let val = self.compile_expr(value)?;
                        if let Some(bv) = val.to_bv() {
                            let ty = bv.get_type();
                            let g = self.module.add_global(ty, None, name);
                            g.set_initializer(&bv);
                            self.consts.insert(name.clone(), (g.as_pointer_value(), ty, val.val_kind()));
                        } else {
                            return Err(format!("Non-basic-value const '{}' is not supported", name));
                        }
                    }
                }
            }
            Stmt::Fun { name, params, return_type, body, .. } => {
                let all_typed = params.iter().all(|p| p.ty.is_some());
                let fn_name = if all_typed && self.overloaded_functions.contains_key(name.as_str()) {
                    let param_types: Vec<Type> = params.iter()
                        .map(|p| p.ty.clone().unwrap_or(Type::Named("Int".into())))
                        .collect();
                    Self::mangle_name(name, &param_types)
                } else {
                    name.clone()
                };
                self.compile_fun_def(&fn_name, name, params, return_type.as_ref(), body)?;
            }
            Stmt::Continue { .. } => { self.compile_expr(&Expr::Continue)?; }
            Stmt::Break { .. } => { self.compile_expr(&Expr::Break)?; }
            Stmt::Expr { expr, .. } => { self.compile_expr(expr)?; }
            Stmt::Return { value: expr, .. } => {
                if let Some(e) = expr {
                    // TCO: detect tail-recursive calls in return position.
                    // Two patterns: return call(...) and return when cond then val else call(...)
                    let tco_info: Option<(
                        Vec<(inkwell::values::PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>, ValKind)>,
                        inkwell::basic_block::BasicBlock<'ctx>,
                    )> = self.extract_tco_info(e);

                    // Pattern 1: return call(...) — direct tail call
                    if let Some((param_slots, tail_entry)) = tco_info {
                        if let Expr::Call { args, .. } = e {
                            // Compile all args first before storing — storing to param
                            // allocas would corrupt scope variables later args depend on.
                            let arg_vals: Vec<TypedValue<'ctx>> = args.iter()
                                .map(|a| self.compile_expr(a))
                                .collect::<Result<_, _>>()?;
                            for (i, arg_val) in arg_vals.iter().enumerate() {
                                let (alloca, ty, _kind) = &param_slots[i];
                                self.store_typed_value(arg_val, *alloca, *ty)?;
                            }
                            self.builder.build_unconditional_branch(tail_entry).map_err(llvm_err)?;
                            return Ok(());
                        }
                    }

                    // Pattern 2: return when cond then val else call(...)
                    if let Expr::When(when_expr) = e {
                        if let WhenKind::OneLine { condition, then_expr, else_expr } = &when_expr.kind {
                            let then_tco = self.extract_tco_info(then_expr);
                            let else_tco = self.extract_tco_info(else_expr);
                            if then_tco.is_some() || else_tco.is_some() {
                                self.compile_tco_when(condition, then_expr, else_expr, &then_tco, &else_tco)?;
                                return Ok(());
                            }
                        }
                    }

                    let v = self.compile_expr(e)?;
                    if let Some(bv) = v.to_bv() {
                        let _ = self.builder.build_return(Some(&bv));
                        return Ok(());
                    }
                    // Handle complex types that return by struct value
                    match &v {
                        TypedValue::Str(ptr) => {
                            let sv = self.load_string(*ptr)?;
                            let _ = self.builder.build_return(Some(&sv));
                            return Ok(());
                        }
                        TypedValue::Enum(ptr, ty, ..) => {
                            let bt: BasicTypeEnum = (*ty).into();
                            let loaded = self.builder.build_load(bt, *ptr, "ret_enum").map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&loaded));
                            return Ok(());
                        }
                        TypedValue::Struct(ptr, ty) => {
                            let bt: BasicTypeEnum = (*ty).into();
                            let loaded = self.builder.build_load(bt, *ptr, "ret_struct").map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&loaded));
                            return Ok(());
                        }
                        TypedValue::Stream(ptr) => {
                            let list_field = self.builder.build_struct_gep(self.stream_type, *ptr, 1, "ret_sl2").map_err(llvm_err)?;
                            let sv = self.builder.build_load(self.list_type, list_field, "ret_sv2").map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&sv));
                            return Ok(());
                        }
                        TypedValue::Task(ptr) => {
                            let sv = self.builder.build_load(self.task_type, *ptr, "ret_task").map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&sv));
                            return Ok(());
                        }
                        TypedValue::List(ptr) | TypedValue::Map(ptr) | TypedValue::Set(ptr) => {
                            let sv = self.load_list(*ptr)?;
                            let _ = self.builder.build_return(Some(&sv));
                            return Ok(());
                        }
                        TypedValue::LazyList(ptr) => {
                            let ll_val = self.builder.build_load(self.lazylist_type, *ptr, "ret_ll").map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&ll_val));
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                let _ = self.builder.build_return(None);
            }
            Stmt::Extension { type_name, methods, .. } => {
                for m in methods {
                    if let Stmt::Fun { name, params, return_type, body, .. } = m {
                        let fn_name = format!("{}_{}", type_name, name);
                        self.compile_fun_def(&fn_name, name, params, return_type.as_ref(), body)?;
                    }
                }
            }
            Stmt::External { name, params, return_type, .. } => {
                // Declare external C function in LLVM module
                let param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = params.iter()
                    .map(|p| {
                        let bt = self.ast_type_to_basic_type(p.ty.as_ref().unwrap_or(&Type::Named("Int".to_string())));
                        bt.into()
                    })
                    .collect();
                let fn_type = match return_type {
                    Some(rt) => {
                        let ret_bt = self.ast_type_to_basic_type(rt);
                        ret_bt.fn_type(&param_types, false)
                    }
                    None => self.void_ty().fn_type(&param_types, false),
                };
                self.module.add_function(name, fn_type, None);
            }
            Stmt::ExternalType { name, .. } => {
                // Create opaque LLVM struct type for external C type
                let opaque_ty = self.context.opaque_struct_type(name);
                self.named_structs.insert(name.clone(), opaque_ty);
            }
            Stmt::Module { name, body, .. } => {
                // Compile module body with prefixed function names
                let prefix = format!("{}_", name);
                let mut saved_scope = Scope::new();
                std::mem::swap(&mut self.scope, &mut saved_scope);
                self.scope = Scope::with_parent(saved_scope);
                for inner_stmt in body {
                    // Transform function names in the module body to include the prefix
                    let renamed = self.rename_module_stmt(inner_stmt, &prefix);
                    self.compile_stmt(&renamed)?;
                }
                // Restore scope
                let mut parent = Scope::new();
                std::mem::swap(&mut self.scope, &mut parent);
                if let Some(p) = parent.parent { self.scope = *p; }
            }
            _ => {}
        }
        Ok(())
    }

    fn rename_module_stmt(&self, stmt: &Stmt, prefix: &str) -> Stmt {
        match stmt {
            Stmt::Fun { name, params, return_type, body, is_single_expr, span, type_params } => {
                Stmt::Fun {
                    name: format!("{}{}", prefix, name),
                    params: params.clone(),
                    return_type: return_type.clone(),
                    body: body.clone(),
                    type_params: type_params.clone(),
                    is_single_expr: *is_single_expr,
                    span: *span,
                }
            }
            Stmt::Const { name, type_ann, value, span } => {
                Stmt::Const {
                    name: format!("{}{}", prefix, name),
                    type_ann: type_ann.clone(),
                    value: value.clone(),
                    span: *span,
                }
            }
            other => other.clone(),
        }
    }

    /// Extract TCO state if `expr` is a tail-recursive self-call.
    /// Returns (param_slots clone, tail_entry block).
    pub(super) fn extract_tco_info(
        &self,
        expr: &Expr,
    ) -> Option<(
        Vec<(inkwell::values::PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>, ValKind)>,
        inkwell::basic_block::BasicBlock<'ctx>,
    )> {
        if let Expr::Call { func, args, trailing_lambda: None } = expr {
            if let Expr::Ident(fn_name) = func.as_ref() {
                if let Some(ref tco) = self.tco_state {
                    if *fn_name == tco.fn_name && args.len() <= tco.param_slots.len() {
                        return Some((tco.param_slots.clone(), tco.tail_entry));
                    }
                }
            }
        }
        None
    }

    /// Compile `when cond then then_expr else else_expr` where at least one branch is a TCO call.
    /// The non-TCO branch is compiled normally and returned; the TCO branch stores args
    /// and branches to tail_entry.
    pub(super) fn compile_tco_when(
        &mut self,
        condition: &Expr,
        then_expr: &Expr,
        else_expr: &Expr,
        then_tco: &Option<(
            Vec<(inkwell::values::PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>, ValKind)>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )>,
        else_tco: &Option<(
            Vec<(inkwell::values::PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>, ValKind)>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )>,
    ) -> Result<(), String> {
        let cond_val = self.compile_expr(condition)?;
        let cond_as_bool = match cond_val {
            TypedValue::Bool(b) => b,
            _ => {
                let bv = cond_val.to_bv().ok_or("When condition must be a basic value")?;
                self.builder.build_int_compare(
                    IntPredicate::NE, bv.into_int_value(), self.i64_ty().const_int(0, false), "when_cond"
                ).map_err(llvm_err)?
            }
        };

        let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let then_block = self.context.append_basic_block(current_fn, "tco_when_then");
        let else_block = self.context.append_basic_block(current_fn, "tco_when_else");

        self.builder.build_conditional_branch(cond_as_bool, then_block, else_block).map_err(llvm_err)?;

        // --- else block (may be TCO or normal) ---
        self.builder.position_at_end(else_block);
        if let Some((ref param_slots, tail_entry)) = else_tco {
            if let Expr::Call { args, .. } = else_expr {
                // Compile all args first before storing — storing to param allocas
                // would corrupt scope variables that later args depend on.
                let arg_vals: Vec<TypedValue<'ctx>> = args.iter()
                    .map(|a| self.compile_expr(a))
                    .collect::<Result<_, _>>()?;
                for (i, arg_val) in arg_vals.iter().enumerate() {
                    let (alloca, ty, _kind) = &param_slots[i];
                    self.store_typed_value(arg_val, *alloca, *ty)?;
                }
                self.builder.build_unconditional_branch(*tail_entry).map_err(llvm_err)?;
            }
        } else {
            let v = self.compile_expr(else_expr)?;
            self.build_return_for_value(&v)?;
        }

        // --- then block (may be TCO or normal) ---
        self.builder.position_at_end(then_block);
        if let Some((ref param_slots, tail_entry)) = then_tco {
            if let Expr::Call { args, .. } = then_expr {
                let arg_vals: Vec<TypedValue<'ctx>> = args.iter()
                    .map(|a| self.compile_expr(a))
                    .collect::<Result<_, _>>()?;
                for (i, arg_val) in arg_vals.iter().enumerate() {
                    let (alloca, ty, _kind) = &param_slots[i];
                    self.store_typed_value(arg_val, *alloca, *ty)?;
                }
                self.builder.build_unconditional_branch(*tail_entry).map_err(llvm_err)?;
            }
        } else {
            let v = self.compile_expr(then_expr)?;
            self.build_return_for_value(&v)?;
        }

        Ok(())
    }

    /// Emit a return instruction for a TypedValue, handling all types.
    pub(super) fn build_return_for_value(&self, v: &TypedValue<'ctx>) -> Result<(), String> {
        if let Some(bv) = v.to_bv() {
            let _ = self.builder.build_return(Some(&bv));
            return Ok(());
        }
        match v {
            TypedValue::Str(ptr) => {
                let sv = self.load_string(*ptr)?;
                let _ = self.builder.build_return(Some(&sv));
            }
            TypedValue::Enum(ptr, ty, ..) => {
                let bt: BasicTypeEnum = (*ty).into();
                let loaded = self.builder.build_load(bt, *ptr, "ret_enum").map_err(llvm_err)?;
                let _ = self.builder.build_return(Some(&loaded));
            }
            TypedValue::Struct(ptr, ty) => {
                let bt: BasicTypeEnum = (*ty).into();
                let loaded = self.builder.build_load(bt, *ptr, "ret_struct").map_err(llvm_err)?;
                let _ = self.builder.build_return(Some(&loaded));
            }
            TypedValue::Stream(ptr) => {
                let list_field = self.builder.build_struct_gep(self.stream_type, *ptr, 1, "ret_sl").map_err(llvm_err)?;
                let sv = self.builder.build_load(self.list_type, list_field, "ret_sv").map_err(llvm_err)?;
                let _ = self.builder.build_return(Some(&sv));
            }
            TypedValue::Task(ptr) => {
                let sv = self.builder.build_load(self.task_type, *ptr, "ret_task2").map_err(llvm_err)?;
                let _ = self.builder.build_return(Some(&sv));
            }
            TypedValue::List(ptr) | TypedValue::Map(ptr) | TypedValue::Set(ptr) => {
                let sv = self.load_list(*ptr)?;
                let _ = self.builder.build_return(Some(&sv));
            }
            TypedValue::LazyList(ptr) => {
                let ll_val = self.builder.build_load(self.lazylist_type, *ptr, "ret_ll2").map_err(llvm_err)?;
                let _ = self.builder.build_return(Some(&ll_val));
            }
            _ => {
                let _ = self.builder.build_return(None);
            }
        }
        Ok(())
    }

    pub(super) fn compile_fun_def(
        &mut self, name: &str, original_name: &str, params: &[Param], _return_type: Option<&Type>, body: &Expr,
    ) -> Result<(), String> {
        // Function was already declared in Pass 1; just look it up
        let function = self.module.get_function(name)
            .ok_or_else(|| format!("Function '{}' not found in module (should have been declared in Pass 1)", name))?;
        let entry = self.context.append_basic_block(function, "entry");

        // Save builder position
        let saved_pos = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

        let mut saved_scope = Scope::new();
        std::mem::swap(&mut self.scope, &mut saved_scope);
        self.scope = Scope::new();

        let mut param_slots: Vec<(PointerValue<'ctx>, BasicTypeEnum<'ctx>, ValKind)> = Vec::new();
        for (i, param) in params.iter().enumerate() {
            if let Some(pv) = function.get_nth_param(i as u32) {
                let alloca = self.builder.build_alloca(pv.get_type(), &param.name).map_err(llvm_err)?;
                self.builder.build_store(alloca, pv).map_err(llvm_err)?;
                let kind = self.param_val_kind(param.ty.as_ref());
                param_slots.push((alloca, pv.get_type(), kind));
                if let Some(Type::Function(param_tys_ast, ret_ast)) = param.ty.as_ref() {
                    let ret = Some(ret_ast.as_ref());
                    let param_llvm_tys: Vec<BasicMetadataTypeEnum> = param_tys_ast.iter()
                        .map(|t| self.ast_type_to_llvm(Some(t)))
                        .collect();
                    let fn_type = self.build_fn_type(ret, name, &param_llvm_tys);
                    self.scope.set_with_fn_type(param.name.clone(), alloca, pv.get_type(), kind, Some(fn_type));
                } else {
                    self.scope.set(param.name.clone(), alloca, pv.get_type(), kind);
                }
            }
        }

        // Set up TCO: create a tail_entry block that reloads params from allocas
        let tail_entry = self.context.append_basic_block(function, "tail_entry");
        let _ = self.builder.build_unconditional_branch(tail_entry);
        self.builder.position_at_end(tail_entry);
        self.tco_state = Some(TcoState {
            tail_entry,
            param_slots,
            fn_name: original_name.to_string(),
        });

        let result = self.compile_expr(body)?;

        // If the body already ended with a return/break/continue, the current block
        // already has a terminator — skip the fallback ret.
        let current_block = self.builder.get_insert_block().unwrap();
        if current_block.get_terminator().is_none() {
            let llvm_void: bool = function.get_type().get_return_type().is_none();

            if llvm_void {
                let _ = self.builder.build_return(None);
            } else {
                match &result {
                    TypedValue::Str(ptr) => {
                        let str_val = self.load_string(*ptr)?;
                        // If the function returns fat_return_type, convert
                        if function.get_type().get_return_type()
                            .map_or(false, |rt| rt == self.fat_return_type.into())
                        {
                            let sv = str_val;
                            let len = self.builder.build_extract_value(sv, 0, "slen").map_err(llvm_err)?;
                            let data = self.builder.build_extract_value(sv, 1, "sdata").map_err(llvm_err)?;
                            let undef_fat = self.fat_return_type.get_undef();
                            let f1 = self.builder.build_insert_value(undef_fat, len, 0, "ftag").map_err(llvm_err)?;
                            let f2 = self.builder.build_insert_value(f1, data, 1, "fdata").map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&f2));
                        } else {
                            let _ = self.builder.build_return(Some(&str_val));
                        }
                    }
                    TypedValue::Enum(ptr, ty, ..) => {
                        let bt: BasicTypeEnum = (*ty).into();
                        let loaded = self.builder.build_load(bt, *ptr, "ret_enum").map_err(llvm_err)?;
                        // If the function returns fat_return_type, convert
                        if function.get_type().get_return_type()
                            .map_or(false, |rt| rt == self.fat_return_type.into())
                        {
                            let sv = loaded.into_struct_value();
                            let tag = self.builder.build_extract_value(sv, 0, "etag").map_err(llvm_err)?;
                            let data = self.builder.build_extract_value(sv, 1, "edata").map_err(llvm_err)?;
                            let undef_fat = self.fat_return_type.get_undef();
                            let f1 = self.builder.build_insert_value(undef_fat, tag, 0, "ftag").map_err(llvm_err)?;
                            let f2 = self.builder.build_insert_value(f1, data, 1, "fdata").map_err(llvm_err)?;
                            let _ = self.builder.build_return(Some(&f2));
                        } else {
                            let _ = self.builder.build_return(Some(&loaded));
                        }
                    }
                    TypedValue::Struct(ptr, ty) => {
                        let bt: BasicTypeEnum = (*ty).into();
                        let loaded = self.builder.build_load(bt, *ptr, "ret_struct").map_err(llvm_err)?;
                        let _ = self.builder.build_return(Some(&loaded));
                    }
                    _ => {
                        if let Some(bv) = result.to_bv() {
                            // If the function returns a fat {i64,ptr} struct but
                            // the body produced a scalar, pack it into the struct.
                            let need_pack = function.get_type().get_return_type()
                                .map_or(false, |rt| rt.is_struct_type());
                            if need_pack {
                                let struct_ty = function.get_type().get_return_type()
                                    .unwrap().into_struct_type();
                                // If we have a pending fat_ret from an untyped call and
                                // the target is NOT fat_return_type (i.e., it's an enum type),
                                // bitcast to preserve both struct fields.
                                if let Some((fat_alloca, _fat_ty)) = self.last_fat_ret.take() {
                                    if struct_ty != self.fat_return_type {
                                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                                        let cast_ptr = self.builder.build_bit_cast(fat_alloca, ptr_ty, "ret_bc").map_err(llvm_err)?;
                                        let val = self.builder.build_load(struct_ty, cast_ptr.into_pointer_value(), "ret_cast").map_err(llvm_err)?;
                                        let _ = self.builder.build_return(Some(&val));
                                    } else {
                                        // Pack scalar into fat_return_type as usual
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
                                    }
                                } else {
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
                                }
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

        // Note: don't call add_function here — it was already declared in Pass 1

        self.tco_state = None;
        self.scope = saved_scope;

        // Restore builder position
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        Ok(())
    }

    pub(super) fn ast_type_to_llvm(&self, ty: Option<&Type>) -> inkwell::types::BasicMetadataTypeEnum<'ctx> {
        match ty {
            None | Some(Type::Unit) => self.i64_ty().into(),
            Some(Type::Named(n)) => match n.as_str() {
                "Float" | "Double" => self.f64_ty().into(),
                "Bool" => self.bool_ty().into(),
                "String" | "Str" => self.string_type.into(),
                "Unit" => self.i64_ty().into(),
                name => {
                    if let Some(st) = self.named_structs.get(name) {
                        (*st).into()
                    } else if let Some(et) = self.enum_types.get(name) {
                        (*et).into()
                    } else {
                        self.i64_ty().into()
                    }
                }
            },
            Some(Type::Function(_, _)) => self.ptr_ty().into(),
            _ => self.i64_ty().into(),
        }
    }

    pub(super) fn ast_type_to_basic_type(&self, ty: &Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Named(n) => match n.as_str() {
                "Int" => self.i64_ty().into(),
                "Float" | "Double" => self.f64_ty().into(),
                "Bool" => self.bool_ty().into(),
                "String" | "Str" => self.string_type.into(),
                "Unit" => self.i64_ty().into(),
                name => {
                    if let Some(st) = self.named_structs.get(name) {
                        (*st).into()
                    } else if let Some(et) = self.enum_types.get(name) {
                        (*et).into()
                    } else {
                        self.i64_ty().into()
                    }
                }
            },
            Type::Struct(fields) => {
                let field_tys: Vec<BasicTypeEnum> = fields.iter()
                    .map(|(_, fty)| self.ast_type_to_basic_type(fty))
                    .collect();
                self.context.struct_type(&field_tys, false).into()
            }
            Type::Function(_, _) => self.ptr_ty().into(),
            Type::Map(_, _) => self.list_type.into(),
            Type::Set(_) => self.list_type.into(),
            Type::Task(_) => self.task_type.into(),
            Type::Stream(_) => self.ptr_ty().into(),
            Type::LazyList(_) => self.lazylist_type.into(),
            Type::CString | Type::Ptr(_) | Type::FileHandle => self.ptr_ty().into(),
            _ => self.i64_ty().into(),
        }
    }

    pub(super) fn param_val_kind(&self, ty: Option<&Type>) -> ValKind {
        match ty {
            Some(Type::Named(n)) => match n.as_str() {
                "Float" => ValKind::Float,
                "Bool" => ValKind::Bool,
                "String" | "Str" => ValKind::Str,
                name => {
                    if self.named_structs.contains_key(name) {
                        ValKind::Struct
                    } else if self.enum_types.contains_key(name) {
                        ValKind::Enum
                    } else {
                        ValKind::Int
                    }
                }
            },
            Some(Type::Function(_, _)) => ValKind::Fn,
            Some(Type::Map(_, _)) => ValKind::Map,
            Some(Type::Set(_)) => ValKind::Set,
            Some(Type::Task(_)) => ValKind::Task,
            Some(Type::Stream(_)) => ValKind::Stream,
            Some(Type::LazyList(_)) => ValKind::LazyList,
            _ => ValKind::Int,
        }
    }

    // ---- Expressions (all &mut self since they may assign) ----

}
