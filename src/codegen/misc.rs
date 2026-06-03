// Submodule: misc

use atomic::ast::*;
use inkwell::values::{BasicValue, BasicValueEnum, BasicMetadataValueEnum, PointerValue};
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::IntPredicate;

use super::{CodeGen, TypedValue, ValKind, Scope, llvm_err, InnerType};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn compile_index(&mut self, obj: &Expr, idx: &Expr) -> Result<TypedValue<'ctx>, String> {
        let o = self.compile_expr(obj)?;

        // Tuple/struct indexing: requires compile-time constant integer index
        if let TypedValue::Struct(ptr, struct_ty) = &o {
            let index = match idx {
                Expr::Literal(Literal::Int(n)) => *n as u32,
                _ => return Err("Tuple/struct index must be an integer literal".to_string()),
            };
            let bt: BasicTypeEnum = (*struct_ty).into();
            let loaded = self.builder.build_load(bt, *ptr, "tuple_ld").map_err(llvm_err)?;
            let struct_val = loaded.into_struct_value();
            let field_val = self.builder.build_extract_value(struct_val, index, "tuple_idx")
                .map_err(llvm_err)?;
            return self.bv_to_typed(field_val);
        }

        // Map indexing: map[key] -> Option<V>
        if let TypedValue::Map(map_ptr) = &o {
            return self.compile_map_index(*map_ptr, idx);
        }

        // Set indexing: set[elem] -> Option<T>
        if let TypedValue::Set(set_ptr) = &o {
            return self.compile_set_index(*set_ptr, idx);
        }

        let i = self.compile_expr(idx)?;
        let index_val = match i {
            TypedValue::Int(v) => v,
            _ => return Err("Index must be an integer".to_string()),
        };

        match o {
            TypedValue::List(list_ptr) | TypedValue::LazyList(list_ptr) => {
                let list_val = self.load_list(list_ptr)?;
                let cc = self.call_rt("atomic_list_get", &[list_val.into(), index_val.into()])?;
                match cc.try_as_basic_value().basic() {
                    Some(bv) => {
                        // list_get returns {i64, ptr} fat struct — the universal value repr.
                        // Store in string alloca; callers extract fields as needed.
                        let fat = bv.into_struct_value();
                        let alloca = self.builder.build_alloca(self.string_type, "list_elem").map_err(llvm_err)?;
                        self.builder.build_store(alloca, fat).map_err(llvm_err)?;
                        Ok(TypedValue::Str(alloca))
                    }
                    None => Err("list_get failed".to_string()),
                }
            }
            TypedValue::Str(str_ptr) => {
                let str_val = self.load_string(str_ptr)?;
                let data = self.builder.build_extract_value(str_val, 1, "data").map_err(llvm_err)?.into_pointer_value();
                let i8 = self.context.i8_type();
                let char_ptr = unsafe { self.builder.build_gep(i8, data, &[index_val], "char_ptr").map_err(llvm_err) }?;
                let char_val = self.builder.build_load(i8, char_ptr, "char").map_err(llvm_err)?.into_int_value();
                let extended = self.builder.build_int_z_extend(char_val, self.i64_ty(), "char_ext").map_err(llvm_err)?;
                Ok(TypedValue::Int(extended))
            }
            _ => Err("Index access not supported for this type".to_string()),
        }
    }

    pub(super) fn compile_map_index(&mut self, map_ptr: PointerValue<'ctx>, idx: &Expr) -> Result<TypedValue<'ctx>, String> {
        let key_val = self.compile_expr(idx)?;
        let key_fat = self.to_fat_struct(&key_val)?;

        let option_ty = *self.enum_types.get("Option")
            .ok_or("Option type not found; ensure enum Option<T> { Some(T), None } is defined")?;
        let option_bt: BasicTypeEnum = option_ty.into();
        let option_alloca = self.builder.build_alloca(option_bt, "map_idx_opt").map_err(llvm_err)?;

        let (_, some_variant) = self.registry.lookup_variant("Some")
            .ok_or("Some variant not found")?;
        let some_tag = some_variant.tag as u64;
        let none_tag = 1u64 - some_tag; // if Some=0, None=1; if Some=1, None=0

        let map_loaded = self.load_list(map_ptr)?;
        let contains_fn = self.module.get_function("atomic_map_contains")
            .ok_or("atomic_map_contains not found")?;
        let cc = self.builder.build_call(contains_fn, &[map_loaded.into(), key_fat.into()], "contains")
            .map_err(llvm_err)?;
        let contains = cc.try_as_basic_value().basic().ok_or("contains failed")?.into_int_value();

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile map index outside function")?;
        let some_bb = self.context.append_basic_block(current_fn, "map_idx_some");
        let none_bb = self.context.append_basic_block(current_fn, "map_idx_none");
        let merge_bb = self.context.append_basic_block(current_fn, "map_idx_merge");

        let _ = self.builder.build_conditional_branch(contains, some_bb, none_bb);

        // Some path: get value from map, store on heap, build Some enum
        self.builder.position_at_end(some_bb);
        let map_loaded2 = self.load_list(map_ptr)?;
        let get_fn = self.module.get_function("atomic_map_get")
            .ok_or("atomic_map_get not found")?;
        // Need to re-compile key since we're in a different basic block
        let key_val2 = self.compile_expr(idx)?;
        let key_fat2 = self.to_fat_struct(&key_val2)?;
        let gc = self.builder.build_call(get_fn, &[map_loaded2.into(), key_fat2.into()], "get")
            .map_err(llvm_err)?;
        let val_fat = gc.try_as_basic_value().basic().ok_or("map_get failed")?.into_struct_value();
        // Allocate heap space for the value (16 bytes for fat struct)
        let sixteen = self.i64_ty().const_int(16, false);
        let malloc_fn = self.module.get_function("malloc")
            .ok_or("malloc not found")?;
        let heap_ptr = self.builder.build_call(malloc_fn, &[sixteen.into()], "heap_val")
            .map_err(llvm_err)?
            .try_as_basic_value().basic().ok_or("malloc failed")?.into_pointer_value();
        self.builder.build_store(heap_ptr, val_fat).map_err(llvm_err)?;
        let undef = option_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(some_tag, false), 0, "some_tag")
            .map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, heap_ptr, 1, "some_ptr")
            .map_err(llvm_err)?;
        self.builder.build_store(option_alloca, r2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);

        // None path: build None enum: {none_tag, null}
        self.builder.position_at_end(none_bb);
        let undef2 = option_ty.get_undef();
        let rn1 = self.builder.build_insert_value(undef2, self.i64_ty().const_int(none_tag, false), 0, "none_tag")
            .map_err(llvm_err)?;
        let rn2 = self.builder.build_insert_value(rn1, self.ptr_ty().const_zero(), 1, "none_ptr")
            .map_err(llvm_err)?;
        self.builder.build_store(option_alloca, rn2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);

        self.builder.position_at_end(merge_bb);
        Ok(TypedValue::Enum(option_alloca, option_ty, InnerType::Int, false))
    }


    /// Set indexing: set[elem] -> Option<T>
    pub(super) fn compile_set_index(&mut self, set_ptr: PointerValue<'ctx>, idx: &Expr) -> Result<TypedValue<'ctx>, String> {
        let elem_val = self.compile_expr(idx)?;
        let elem_fat = self.to_fat_struct(&elem_val)?;

        let option_ty = *self.enum_types.get("Option")
            .ok_or("Option type not found; ensure enum Option<T> { Some(T), None } is defined")?;
        let option_bt: BasicTypeEnum = option_ty.into();
        let option_alloca = self.builder.build_alloca(option_bt, "set_idx_opt").map_err(llvm_err)?;

        let (_, some_variant) = self.registry.lookup_variant("Some")
            .ok_or("Some variant not found")?;
        let some_tag = some_variant.tag as u64;
        let none_tag = 1u64 - some_tag;

        let set_loaded = self.load_list(set_ptr)?;
        let contains_fn = self.module.get_function("atomic_map_contains")
            .ok_or("atomic_map_contains not found")?;
        let cc = self.builder.build_call(contains_fn, &[set_loaded.into(), elem_fat.into()], "contains")
            .map_err(llvm_err)?;
        let contains = cc.try_as_basic_value().basic().ok_or("contains failed")?.into_int_value();

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile set index outside function")?;
        let some_bb = self.context.append_basic_block(current_fn, "set_idx_some");
        let none_bb = self.context.append_basic_block(current_fn, "set_idx_none");
        let merge_bb = self.context.append_basic_block(current_fn, "set_idx_merge");

        let _ = self.builder.build_conditional_branch(contains, some_bb, none_bb);

        // Some path: wrap element in Some(elem)
        self.builder.position_at_end(some_bb);
        // Re-compile elem since we're in a different basic block
        let elem_val2 = self.compile_expr(idx)?;
        let elem_fat2 = self.to_fat_struct(&elem_val2)?;
        let sixteen = self.i64_ty().const_int(16, false);
        let malloc_fn = self.module.get_function("malloc").ok_or("malloc not found")?;
        let heap_ptr = self.builder.build_call(malloc_fn, &[sixteen.into()], "heap_elem")
            .map_err(llvm_err)?
            .try_as_basic_value().basic().ok_or("malloc failed")?.into_pointer_value();
        self.builder.build_store(heap_ptr, elem_fat2).map_err(llvm_err)?;
        let undef = option_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(some_tag, false), 0, "some_tag")
            .map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, heap_ptr, 1, "some_ptr")
            .map_err(llvm_err)?;
        self.builder.build_store(option_alloca, r2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);

        // None path
        self.builder.position_at_end(none_bb);
        let undef2 = option_ty.get_undef();
        let rn1 = self.builder.build_insert_value(undef2, self.i64_ty().const_int(none_tag, false), 0, "none_tag")
            .map_err(llvm_err)?;
        let rn2 = self.builder.build_insert_value(rn1, self.ptr_ty().const_zero(), 1, "none_ptr")
            .map_err(llvm_err)?;
        self.builder.build_store(option_alloca, rn2).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(merge_bb);

        self.builder.position_at_end(merge_bb);
        Ok(TypedValue::Enum(option_alloca, option_ty, InnerType::Int, false))
    }

    pub(super) fn compile_range(&mut self, start: &Expr, end: &Expr) -> Result<TypedValue<'ctx>, String> {
        // Create a range struct {start: i64, end: i64, inclusive: i64}
        let start_v = self.compile_expr(start)?;
        let end_v = self.compile_expr(end)?;
        let start_int = match start_v { TypedValue::Int(v) => v, _ => return Err("Range start must be integer".into()) };
        let end_int = match end_v { TypedValue::Int(v) => v, _ => return Err("Range end must be integer".into()) };
        let range_ty = self.range_type;
        let alloca = self.builder.build_alloca(range_ty, "range").map_err(llvm_err)?;
        let sptr = self.builder.build_struct_gep(range_ty, alloca, 0, "r_start").map_err(llvm_err)?;
        self.builder.build_store(sptr, start_int).map_err(llvm_err)?;
        let eptr = self.builder.build_struct_gep(range_ty, alloca, 1, "r_end").map_err(llvm_err)?;
        self.builder.build_store(eptr, end_int).map_err(llvm_err)?;
        let iptr = self.builder.build_struct_gep(range_ty, alloca, 2, "r_inc").map_err(llvm_err)?;
        self.builder.build_store(iptr, self.i64_ty().const_int(1, false)).map_err(llvm_err)?;
        Ok(TypedValue::Struct(alloca, range_ty))
    }

    pub(super) fn compile_block(&mut self, stmts: &[Stmt]) -> Result<TypedValue<'ctx>, String> {
        let mut saved = Scope::new();
        std::mem::swap(&mut self.scope, &mut saved);
        self.scope = Scope::with_parent(saved);

        let mut last = TypedValue::Unit;
        for (_i, s) in stmts.iter().enumerate() {
            match s {
                Stmt::Expr { expr: e, .. } => last = self.compile_expr(e)?,
                _ => self.compile_stmt(s)?,
            }
        }

        // RC cleanup: decrement refcounts on heap-typed variables in this scope
        self.emit_scope_cleanup()?;

        let mut parent = Scope::new();
        std::mem::swap(&mut self.scope, &mut parent);
        if let Some(p) = parent.parent { self.scope = *p; }
        Ok(last)
    }

    pub(super) fn compile_assign(&mut self, target: &Expr, value: &Expr) -> Result<TypedValue<'ctx>, String> {
        let v = self.compile_expr(value)?;
        match target {
            Expr::Ident(name) => {
                let var = self.scope.get(name).ok_or_else(|| format!("Undefined variable: {}", name))?;
                if !var.mutable {
                    return Err(format!("Cannot assign to immutable variable '{}' (use 'var' instead of 'val')", name));
                }
                match &v {
                    TypedValue::Str(ptr) => {
                        let str_struct = self.load_string(*ptr)?;
                        self.builder.build_store(var.ptr, str_struct).map_err(llvm_err)?;
                    }
                    TypedValue::List(ptr) | TypedValue::Map(ptr) | TypedValue::Set(ptr) | TypedValue::Task(ptr) | TypedValue::Stream(ptr) => {
                        let list_struct = self.load_list(*ptr)?;
                        self.builder.build_store(var.ptr, list_struct).map_err(llvm_err)?;
                    }
                    TypedValue::Struct(ptr, ty) => {
                        let bt: BasicTypeEnum = (*ty).into();
                        let loaded = self.builder.build_load(bt, *ptr, "assign_ld").map_err(llvm_err)?;
                        self.builder.build_store(var.ptr, loaded).map_err(llvm_err)?;
                    }
                    TypedValue::Enum(ptr, ty, ..) => {
                        let bt: BasicTypeEnum = (*ty).into();
                        let loaded = self.builder.build_load(bt, *ptr, "assign_ld").map_err(llvm_err)?;
                        self.builder.build_store(var.ptr, loaded).map_err(llvm_err)?;
                    }
                    TypedValue::LazyList(ptr) | TypedValue::CString(ptr) | TypedValue::Ptr(ptr) | TypedValue::FileHandle(ptr) => {
                        self.builder.build_store(var.ptr, *ptr).map_err(llvm_err)?;
                    }
                    _ => {
                        if let Some(bv) = v.to_bv() {
                            self.builder.build_store(var.ptr, bv).map_err(llvm_err)?;
                        }
                    }
                }
                Ok(v)
            }
            Expr::FieldAccess(obj, field) => {
                let obj_val = self.compile_expr(obj)?;
                match obj_val {
                    TypedValue::Struct(ptr, st) => {
                        let idx = self.struct_field_index(&st, field)?;
                        let field_ptr = self.builder.build_struct_gep(st, ptr, idx, "field_gep").map_err(llvm_err)?;
                        if let Some(bv) = v.to_bv() { self.builder.build_store(field_ptr, bv).map_err(llvm_err)?; }
                        Ok(v)
                    }
                    _ => Err(format!("Cannot assign to field '{}' of non-struct", field)),
                }
            }
            Expr::Tuple(names) => {
                for (i, (_, name_expr)) in names.iter().enumerate() {
                    let name = match name_expr {
                        Expr::Ident(n) => n,
                        _ => return Err("Destructuring target must be an identifier".to_string()),
                    };
                    // Collect var info before mutable self call
                    let var_ptr = {
                        let var = self.scope.get(name).ok_or_else(|| format!("Undefined variable: {}", name))?;
                        if !var.mutable {
                            return Err(format!("Cannot assign to immutable variable '{}'", name));
                        }
                        var.ptr
                    };
                    let field_val = self.extract_field_from_struct(&v, i)?;
                    if let Some(bv) = field_val.to_bv() { self.builder.build_store(var_ptr, bv).map_err(llvm_err)?; }
                }
                Ok(v)
            }
            _ => Err("Complex assignment not yet supported".to_string()),
        }
    }

    /// Get the field index within a struct type by field name
    pub(super) fn struct_field_index(&self, st: &StructType<'ctx>, field: &str) -> Result<u32, String> {
        // Find the named struct whose LLVM type matches st
        for (name, named_st) in &self.named_structs {
            if *named_st == *st {
                if let Some(si) = self.registry.structs.values().find(|si| si.name == *name) {
                    return si.fields.iter().position(|(n, _)| n == field)
                        .map(|i| i as u32)
                        .ok_or_else(|| format!("Field '{}' not found in struct '{}'", field, name));
                }
            }
        }
        Err(format!("Field '{}' not found in struct", field))
    }

    /// Extract a field value from a TypedValue::Struct at the given index
    pub(super) fn extract_field_from_struct(&mut self, struct_val: &TypedValue<'ctx>, idx: usize) -> Result<TypedValue<'ctx>, String> {
        match struct_val {
            TypedValue::Struct(ptr, st) => {
                let bt: BasicTypeEnum = (*st).into();
                let loaded = self.builder.build_load(bt, *ptr, "field_load").map_err(llvm_err)?
                    .into_struct_value();
                let field = self.builder.build_extract_value(loaded, idx as u32, &format!("f{}", idx))
                    .map_err(llvm_err)?;
                let field_ty = field.get_type();
                let alloca = self.builder.build_alloca(field_ty, "field_tmp").map_err(llvm_err)?;
                self.builder.build_store(alloca, field).map_err(llvm_err)?;
                let kind = self.bv_kind(&field);
                match kind {
                    ValKind::Str => Ok(TypedValue::Str(alloca)),
                    ValKind::List => Ok(TypedValue::List(alloca)),
                    ValKind::Map => Ok(TypedValue::Map(alloca)),
                    ValKind::Set => Ok(TypedValue::Set(alloca)),
                    ValKind::Struct => Ok(TypedValue::Struct(alloca, *st)),
                    ValKind::Enum => Ok(TypedValue::Enum(alloca, *st, InnerType::Int, false)),
                    ValKind::Bool => Ok(TypedValue::Bool(field.into_int_value())),
                    ValKind::Int => Ok(TypedValue::Int(field.into_int_value())),
                    ValKind::Float => Ok(TypedValue::Float(field.into_float_value())),
                    _ => Ok(TypedValue::Unit),
                }
            }
            _ => Err("Cannot extract field from non-struct value".to_string()),
        }
    }

    /// Compile error propagation binding: val x? = Some(expr)
    /// On Some/Ok: extract inner value, bind to variable
    /// On None/Err: return early from the current function with the error
    pub(super) fn compile_propagate_let(&mut self, name: &str, type_ann: Option<&Type>, value: &Expr) -> Result<(), String> {
        let val = self.compile_expr(value)?;
        let inner = self.propagate_unwrap(&val)?;
        let (ty, kind) = if let Some(ann) = type_ann {
            (self.ast_type_to_basic_type(ann), self.param_val_kind(Some(ann)))
        } else {
            (inner.get_type_for_alloca(self), inner.val_kind())
        };
        let alloca = self.builder.build_alloca(ty, name).map_err(llvm_err)?;
        self.store_typed_value(&inner, alloca, ty)?;
        self.scope.set(name.to_string(), alloca, ty, kind);
        Ok(())
    }

    /// Compile error propagation assignment: x? = Some(expr)
    pub(super) fn compile_propagate_assign(&mut self, target: &Expr, value: &Expr) -> Result<TypedValue<'ctx>, String> {
        let val = self.compile_expr(value)?;
        let inner = self.propagate_unwrap(&val)?;
        match target {
            Expr::Ident(name) => {
                if let Some(var) = self.scope.get(name) {
                    self.store_typed_value(&inner, var.ptr, var.ty)?;
                    return Ok(inner);
                }
                Err(format!("Undefined variable: {}", name))
            }
            Expr::FieldAccess(obj, field) => {
                let obj_val = self.compile_expr(obj)?;
                match obj_val {
                    TypedValue::Struct(ptr, st) => {
                        let field_idx = self.registry.structs.values()
                            .find(|si| si.fields.iter().any(|(n, _)| n == field))
                            .and_then(|si| si.fields.iter().position(|(n, _)| n == field))
                            .ok_or_else(|| format!("Field '{}' not found in struct", field))?;
                        let field_ptr = self.builder.build_struct_gep(st, ptr, field_idx as u32, "field_gep")
                            .map_err(llvm_err)?;
                        let field_ty = self.ast_type_to_basic_type(&Type::Named("Int".into()));
                        self.store_typed_value(&inner, field_ptr, field_ty)?;
                        Ok(inner)
                    }
                    _ => Err("Propagation assignment target must be a struct field".to_string()),
                }
            }
            _ => Err("Complex assignment not yet supported".to_string()),
        }
    }

    /// Check if a TypedValue is an enum with tag 0 (Some/Ok).
    /// If yes: extract inner data and continue in the current block.
    /// If no: branch to an early return of the original enum from the current function.
    pub(super) fn propagate_unwrap(&mut self, enum_val: &TypedValue<'ctx>) -> Result<TypedValue<'ctx>, String> {
        let (enum_ptr, enum_ty) = match enum_val {
            TypedValue::Enum(p, t, ..) => (*p, *t),
            _ => return Err("Error propagation (?) requires an Option or Result enum".to_string()),
        };

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot propagate error outside function")?;

        let i64 = self.i64_ty();
        let ptr_ty = self.ptr_ty();
        let bt: BasicTypeEnum = enum_ty.into();

        let loaded = self.builder.build_load(bt, enum_ptr, "prop_enum").map_err(llvm_err)?;
        let enum_struct = loaded.into_struct_value();
        let tag = self.builder.build_extract_value(enum_struct, 0, "prop_tag")
            .map_err(llvm_err)?.into_int_value();
        let data_ptr = self.builder.build_extract_value(enum_struct, 1, "prop_data")
            .map_err(llvm_err)?.into_pointer_value();

        let is_ok = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "prop_is_ok")
            .map_err(llvm_err)?;

        let ok_block = self.context.append_basic_block(current_fn, "prop_ok");
        let fail_block = self.context.append_basic_block(current_fn, "prop_fail");

        self.builder.build_conditional_branch(is_ok, ok_block, fail_block).map_err(llvm_err)?;

        // On failure: return the original None/Err enum from the function
        self.builder.position_at_end(fail_block);
        let _ = self.builder.build_return(Some(&loaded));

        // On success: extract the inner value
        self.builder.position_at_end(ok_block);
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, ptr_ty, "prop_inner")
            .map_err(llvm_err)?;
        let inner_val = self.builder.build_load(i64, inner_ptr, "prop_inner_val")
            .map_err(llvm_err)?;
        self.bv_to_typed(inner_val)
    }

    pub(super) fn compile_string_interp(&mut self, parts: &[StringPart]) -> Result<TypedValue<'ctx>, String> {
        let mut result: Option<PointerValue<'ctx>> = None;
        for p in parts {
            let str_ptr = match p {
                StringPart::Literal(s) => {
                    let tv = self.compile_string_literal(s)?;
                    match tv {
                        TypedValue::Str(ptr) => Some(ptr),
                        _ => None,
                    }
                }
                StringPart::Expr(expr) => {
                    let val = self.compile_expr(expr)?;
                    self.value_to_string_ptr(&val)?
                }
            };

            if let Some(ptr) = str_ptr {
                result = match result {
                    None => Some(ptr),
                    Some(acc) => {
                        let cc = self.call_rt_with_2str("atomic_string_concat", acc, ptr)?;
                        match cc.try_as_basic_value().basic() {
                            Some(bv) => {
                                let alloca = self.builder.build_alloca(self.string_type, "interp").map_err(llvm_err)?;
                                self.builder.build_store(alloca, bv).map_err(llvm_err)?;
                                Some(alloca)
                            }
                            None => Some(acc),
                        }
                    }
                };
            }
        }
        match result {
            Some(ptr) => Ok(TypedValue::Str(ptr)),
            None => {
                let g = self.builder.build_global_string_ptr("", "empty").map_err(llvm_err)?;
                Ok(TypedValue::Str(g.as_pointer_value()))
            }
        }
    }

    /// Convert a typed value to a string pointer (for string interpolation)
    pub(super) fn value_to_string_ptr(&mut self, val: &TypedValue<'ctx>) -> Result<Option<PointerValue<'ctx>>, String> {
        match val {
            TypedValue::Int(iv) => {
                let cc = self.call_rt("atomic_int_to_string", &[(*iv).into()])?;
                match cc.try_as_basic_value().basic() {
                    Some(bv) => {
                        let alloca = self.builder.build_alloca(self.string_type, "int_str").map_err(llvm_err)?;
                        self.builder.build_store(alloca, bv).map_err(llvm_err)?;
                        Ok(Some(alloca))
                    }
                    None => Ok(None),
                }
            }
            TypedValue::Float(fv) => {
                let cc = self.call_rt("atomic_float_to_string", &[(*fv).into()])?;
                match cc.try_as_basic_value().basic() {
                    Some(bv) => {
                        let alloca = self.builder.build_alloca(self.string_type, "float_str").map_err(llvm_err)?;
                        self.builder.build_store(alloca, bv).map_err(llvm_err)?;
                        Ok(Some(alloca))
                    }
                    None => Ok(None),
                }
            }
            TypedValue::Str(ptr) => Ok(Some(*ptr)),
            TypedValue::Bool(bv) => {
                // Convert bool to string "true" or "false"
                let true_str = self.compile_string_literal("true")?;
                let false_str = self.compile_string_literal("false")?;
                if let (TypedValue::Str(tp), TypedValue::Str(fp)) = (&true_str, &false_str) {
                    let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let true_block = self.context.append_basic_block(current_fn, "bool_true");
                    let false_block = self.context.append_basic_block(current_fn, "bool_false");
                    let merge_block = self.context.append_basic_block(current_fn, "bool_merge");

                    self.builder.build_conditional_branch(*bv, true_block, false_block).map_err(llvm_err)?;

                    self.builder.position_at_end(true_block);
                    self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;

                    self.builder.position_at_end(false_block);
                    self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;

                    self.builder.position_at_end(merge_block);
                    let phi = self.builder.build_phi(self.ptr_ty(), "bool_str").map_err(llvm_err)?;
                    let tp_bv: BasicValueEnum = (*tp).into();
                    let fp_bv: BasicValueEnum = (*fp).into();
                    phi.add_incoming(&[(&tp_bv, true_block), (&fp_bv, false_block)]);
                    Ok(Some(phi.as_basic_value().into_pointer_value()))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None), // Floats and other types not yet supported in interpolation
        }
    }

    pub(super) fn compile_field_access(&mut self, obj: &Expr, field: &str) -> Result<TypedValue<'ctx>, String> {
        // Handle enum variant access: EnumName.Variant
        if let Expr::Ident(enum_name) = obj {
            if self.enum_types.contains_key(enum_name) {
                // Look up the variant in this specific enum
                let variant_info = self.registry.lookup_variant(field)
                    .map(|(ei, vi)| (ei.clone(), vi.clone()));
                if let Some((enum_info, variant)) = variant_info {
                    if enum_info.name == *enum_name {
                        if variant.params.is_empty() {
                            return self.compile_enum_construct(&enum_info, &variant, &[]);
                        }
                        return Err(format!("Enum variant '{}.{}' requires arguments", enum_name, field));
                    }
                }
                return Err(format!("Variant '{}' not found in enum '{}'", field, enum_name));
            }
            // Check if it's a module-qualified function call handled elsewhere (e.g., math.add)
        }
        let o = self.compile_expr(obj)?;
        if let TypedValue::Str(ptr) = &o {
            if field == "length" {
                let gep = self.builder.build_struct_gep(self.string_type, *ptr, 0, "lenp").map_err(llvm_err)?;
                let len = self.builder.build_load(self.i64_ty(), gep, "len").map_err(llvm_err)?.into_int_value();
                return Ok(TypedValue::Int(len));
            }
        }
        if let TypedValue::Struct(ptr, struct_ty) = &o {
            let bt: BasicTypeEnum = (*struct_ty).into();
            let loaded = self.builder.build_load(bt, *ptr, "struct_ld").map_err(llvm_err)?;
            let struct_val = loaded.into_struct_value();

            // Check if field is a numeric index for tuple access: .0, .1, etc.
            if let Ok(idx) = field.parse::<usize>() {
                let field_val = self.builder.build_extract_value(struct_val, idx as u32, field)
                    .map_err(llvm_err)?;
                return self.bv_to_typed(field_val);
            }

            let field_names = self.lookup_struct_field_names(*struct_ty);
            let idx = field_names.iter().position(|n| n == field)
                .ok_or_else(|| format!("Field '{}' not found on struct", field))?;
            let field_val = self.builder.build_extract_value(struct_val, idx as u32, field)
                .map_err(llvm_err)?;
            return self.bv_to_typed(field_val);
        }
        Err(format!("Field '{}' not supported on this type", field))
    }

    /// Compile safe field access: expr?.field
    /// On Some/Ok: extract inner value, access field, wrap result in Some
    /// On None/Err: return the original enum as-is
    /// Safe field access with early return: obj?.field
    /// If obj is None/Err, return early from the function (propagate the error).
    /// If obj is Some(v)/Ok(v), unwrap v, access .field, and wrap the result back in Some/Ok.
    pub(super) fn compile_safe_field_access(&mut self, obj: &Expr, field: &str) -> Result<TypedValue<'ctx>, String> {
        let receiver = self.compile_expr(obj)?;
        let (enum_ptr, enum_ty) = match &receiver {
            TypedValue::Enum(p, t, ..) => (*p, *t),
            _ => return Err("Safe field access (?.field) requires an Option or Result enum receiver".to_string()),
        };

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile safe field access outside function")?;

        let i64 = self.i64_ty();
        let ptr_ty = self.ptr_ty();
        let bt: BasicTypeEnum = enum_ty.into();

        let loaded = self.builder.build_load(bt, enum_ptr, "safe_enum_ld").map_err(llvm_err)?;
        let enum_struct = loaded.into_struct_value();
        let tag = self.builder.build_extract_value(enum_struct, 0, "safe_tag")
            .map_err(llvm_err)?.into_int_value();
        let data_ptr = self.builder.build_extract_value(enum_struct, 1, "safe_data")
            .map_err(llvm_err)?.into_pointer_value();

        let is_ok = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "is_ok")
            .map_err(llvm_err)?;

        let ok_block = self.context.append_basic_block(current_fn, "safe_ok");
        let fail_block = self.context.append_basic_block(current_fn, "safe_fail");
        let merge_block = self.context.append_basic_block(current_fn, "safe_merge");

        // Allocate result at entry
        let entry = current_fn.get_first_basic_block().unwrap();
        let saved_pos = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(instr) => { let _ = self.builder.position_before(&instr); }
            None => self.builder.position_at_end(entry),
        }
        let result_alloca = self.builder.build_alloca(bt, "safe_result").map_err(llvm_err)?;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        self.builder.build_conditional_branch(is_ok, ok_block, fail_block).map_err(llvm_err)?;

        // On failure: if the function returns an enum type, early-return with the error.
        // If the function returns void (e.g. main), fall back to producing the enum value.
        let fn_returns_void = current_fn.get_type().get_return_type().is_none();
        self.builder.position_at_end(fail_block);
        if fn_returns_void {
            self.builder.build_store(result_alloca, loaded).map_err(llvm_err)?;
            self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;
        } else {
            let _ = self.builder.build_return(Some(&loaded));
        }

        // On success: unwrap inner value, access field, wrap in Some/Ok
        self.builder.position_at_end(ok_block);
        let field_val = self.try_struct_field_load(data_ptr, field)?;
        let field_ty = field_val.get_type_for_alloca(self);
        let heap_ptr = self.builder.build_alloca(field_ty, "wrap_data").map_err(llvm_err)?;
        self.store_typed_value(&field_val, heap_ptr, field_ty)?;
        let heap_ptr_i8 = self.builder.build_pointer_cast(heap_ptr, ptr_ty, "heap_i8").map_err(llvm_err)?;
        let undef = enum_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, i64.const_int(0, false), 0, "ok_tag").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, heap_ptr_i8, 1, "ok_data").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, r2).map_err(llvm_err)?;
        self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;

        self.builder.position_at_end(merge_block);
        Ok(TypedValue::Enum(result_alloca, enum_ty, InnerType::Int, false))
    }

    /// Try to load a field from a struct-typed data pointer.
    pub(super) fn try_struct_field_load(&mut self, data_ptr: PointerValue<'ctx>, field: &str) -> Result<TypedValue<'ctx>, String> {
        // Search through named structs for one containing the field
        for (struct_name, struct_ty) in &self.named_structs.clone() {
            if let Some(info) = self.registry.get_struct(struct_name) {
                if let Some((idx, (_, _field_ty))) = info.fields.iter().enumerate()
                    .find(|(_, (n, _))| n == field) {
                    // Cast data_ptr to struct pointer, load struct, extract field
                    let struct_bt: BasicTypeEnum = (*struct_ty).into();
                    let struct_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let casted = self.builder.build_pointer_cast(data_ptr, struct_ptr_ty, "struct_ptr")
                        .map_err(llvm_err)?;
                    let loaded = self.builder.build_load(struct_bt, casted, "struct_val")
                        .map_err(llvm_err)?;
                    let struct_val = loaded.into_struct_value();
                    let fv = self.builder.build_extract_value(struct_val, idx as u32, field)
                        .map_err(llvm_err)?;
                    return self.bv_to_typed(fv);
                }
            }
        }
        // Fallback: load as i64 (for simple types like Int, Bool)
        let i64 = self.i64_ty();
        let i64_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let casted = self.builder.build_pointer_cast(data_ptr, i64_ptr_ty, "i64_ptr")
            .map_err(llvm_err)?;
        let loaded = self.builder.build_load(i64, casted, field).map_err(llvm_err)?.into_int_value();
        Ok(TypedValue::Int(loaded))
    }

    /// Compile safe call: expr?.method(args)
    /// On Some/Ok: extract inner value, call method(inner_value, ...args), wrap in Some
    /// Safe call with early return: obj?.method(args)
    /// If obj is None/Err, return early from the function (propagate the error).
    /// If obj is Some(v)/Ok(v), unwrap v, call method(v, args...), and wrap result in Some/Ok.
    pub(super) fn compile_safe_call(&mut self, receiver: &Expr, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        // receiver is FieldAccess(original_expr, "method_name")
        let (original, method_name) = match receiver {
            Expr::FieldAccess(obj, name) => (obj.as_ref(), name.as_str()),
            _ => return Err("Safe call (?.method()) requires a method name".to_string()),
        };

        let enum_val = self.compile_expr(original)?;
        let (enum_ptr, enum_ty) = match &enum_val {
            TypedValue::Enum(p, t, ..) => (*p, *t),
            _ => return Err("Safe call (?.method()) requires an Option or Result enum receiver".to_string()),
        };

        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("Cannot compile safe call outside function")?;

        let i64 = self.i64_ty();
        let ptr_ty = self.ptr_ty();
        let bt: BasicTypeEnum = enum_ty.into();

        let loaded = self.builder.build_load(bt, enum_ptr, "safe_call_ld").map_err(llvm_err)?;
        let enum_struct = loaded.into_struct_value();
        let tag = self.builder.build_extract_value(enum_struct, 0, "sc_tag")
            .map_err(llvm_err)?.into_int_value();
        let data_ptr = self.builder.build_extract_value(enum_struct, 1, "sc_data")
            .map_err(llvm_err)?.into_pointer_value();

        let is_ok = self.builder.build_int_compare(IntPredicate::EQ, tag, i64.const_int(0, false), "sc_is_ok")
            .map_err(llvm_err)?;

        let ok_block = self.context.append_basic_block(current_fn, "sc_ok");
        let fail_block = self.context.append_basic_block(current_fn, "sc_fail");
        let merge_block = self.context.append_basic_block(current_fn, "sc_merge");

        // Allocate result at entry
        let entry = current_fn.get_first_basic_block().unwrap();
        let saved_pos = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(instr) => { let _ = self.builder.position_before(&instr); }
            None => self.builder.position_at_end(entry),
        }
        let result_alloca = self.builder.build_alloca(bt, "sc_result").map_err(llvm_err)?;
        if let Some(block) = saved_pos {
            self.builder.position_at_end(block);
        }

        self.builder.build_conditional_branch(is_ok, ok_block, fail_block).map_err(llvm_err)?;

        // On failure: if the function returns an enum type, early-return with the error.
        // If the function returns void (e.g. main), fall back to producing the enum value.
        let fn_returns_void = current_fn.get_type().get_return_type().is_none();
        self.builder.position_at_end(fail_block);
        if fn_returns_void {
            self.builder.build_store(result_alloca, loaded).map_err(llvm_err)?;
            self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;
        } else {
            let _ = self.builder.build_return(Some(&loaded));
        }

        // On success: unwrap inner value, call method, wrap result in Some/Ok
        self.builder.position_at_end(ok_block);
        let inner_ptr = self.builder.build_pointer_cast(data_ptr, ptr_ty, "sc_inner").map_err(llvm_err)?;
        let inner_val = self.builder.build_load(i64, inner_ptr, "sc_inner_val").map_err(llvm_err)?;
        let inner_typed = self.bv_to_typed(inner_val)?;

        let mut all_args = vec![inner_typed];
        for a in args {
            all_args.push(self.compile_expr(a)?);
        }
        let call_result = self.compile_ufcs_call(method_name, &all_args)?;

        let call_ty = call_result.get_type_for_alloca(self);
        let heap_ptr = self.builder.build_alloca(call_ty, "sc_wrap").map_err(llvm_err)?;
        self.store_typed_value(&call_result, heap_ptr, call_ty)?;
        let heap_ptr_i8 = self.builder.build_pointer_cast(heap_ptr, ptr_ty, "sc_heap_i8").map_err(llvm_err)?;
        let undef = enum_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, i64.const_int(0, false), 0, "sc_ok_tag").map_err(llvm_err)?;
        let r2 = self.builder.build_insert_value(r1, heap_ptr_i8, 1, "sc_ok_data").map_err(llvm_err)?;
        self.builder.build_store(result_alloca, r2).map_err(llvm_err)?;
        self.builder.build_unconditional_branch(merge_block).map_err(llvm_err)?;

        self.builder.position_at_end(merge_block);
        Ok(TypedValue::Enum(result_alloca, enum_ty, InnerType::Int, false))
    }

    /// Compile a UFCS call: method_name(value, args...)
    pub(super) fn compile_ufcs_call(&mut self, method_name: &str, args: &[TypedValue<'ctx>]) -> Result<TypedValue<'ctx>, String> {
        if let Some(fn_val) = self.module.get_function(method_name) {
            let mut ca: Vec<BasicMetadataValueEnum> = Vec::new();
            for a in args {
                if let Some(bv) = a.to_bv() {
                    ca.push(bv.into());
                } else {
                    ca.push(self.i64_ty().const_int(0, false).into());
                }
            }
            let cc = self.builder.build_call(fn_val, &ca, "ufcs").map_err(llvm_err)?;
            match cc.try_as_basic_value().basic() {
                Some(bv) => self.bv_to_typed(bv),
                None => Ok(TypedValue::Unit),
            }
        } else {
            Err(format!("Method '{}' not found", method_name))
        }
    }

    pub(super) fn lookup_struct_field_names(&self, struct_ty: StructType<'ctx>) -> Vec<String> {
        for (name, st) in &self.named_structs {
            if *st == struct_ty {
                if let Some(info) = self.registry.get_struct(name) {
                    return info.fields.iter().map(|(n, _)| n.clone()).collect();
                }
            }
        }
        for (names, st) in &self.anon_structs {
            if *st == struct_ty {
                return names.clone();
            }
        }
        vec![]
    }

    pub(super) fn compile_struct_lit(&mut self, fields: &[(String, Expr)]) -> Result<TypedValue<'ctx>, String> {
        let field_names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();

        // Try to find a matching named struct type
        let struct_ty = if let Some(info) = self.registry.find_struct_by_fields(&field_names) {
            *self.named_structs.get(&info.name)
                .ok_or_else(|| format!("Struct '{}' not in LLVM type map", info.name))?
        } else {
            // Create an anonymous struct type
            if let Some(ct) = self.anon_structs.get(&field_names) {
                *ct
            } else {
                let field_tys: Vec<BasicTypeEnum> = fields.iter()
                    .map(|_| Ok::<BasicTypeEnum, String>(self.i64_ty().into()))
                    .collect::<Result<Vec<_>, _>>()?;
                let anon_ty = self.context.struct_type(&field_tys, false);
                self.anon_structs.insert(field_names, anon_ty);
                anon_ty
            }
        };

        let bt: BasicTypeEnum = struct_ty.into();
        let alloca = self.builder.build_alloca(bt, "struct_lit").map_err(llvm_err)?;

        let undef = struct_ty.get_undef();
        let mut result = undef;

        for (i, (_, expr)) in fields.iter().enumerate() {
            let val = self.compile_expr(expr)?;
            let bv = val.to_bv().unwrap_or_else(|| {
                self.i64_ty().const_int(0, false).as_basic_value_enum()
            });
            result = self.builder.build_insert_value(result, bv, i as u32, "field")
                .map_err(llvm_err)?.into_struct_value();
        }

        self.builder.build_store(alloca, result).map_err(llvm_err)?;
        Ok(TypedValue::Struct(alloca, struct_ty))
    }

    pub(super) fn compile_tuple(&mut self, exprs: &[(Option<String>, Expr)]) -> Result<TypedValue<'ctx>, String> {
        if exprs.is_empty() {
            return Ok(TypedValue::Unit);
        }
        let mut field_tys: Vec<BasicTypeEnum> = Vec::new();
        let mut values: Vec<TypedValue<'ctx>> = Vec::new();
        let mut field_names: Vec<String> = Vec::new();
        for (name_opt, expr) in exprs {
            let val = self.compile_expr(expr)?;
            field_tys.push(val.get_type_for_alloca(self));
            values.push(val);
            if let Some(name) = name_opt {
                field_names.push(name.clone());
            } else {
                field_names.push(format!("_{}", field_names.len()));
            }
        }
        let struct_ty = self.context.struct_type(&field_tys, false);
        // Register in anon_structs so field access by name works
        self.anon_structs.entry(field_names).or_insert(struct_ty);
        let bt: BasicTypeEnum = struct_ty.into();
        let alloca = self.builder.build_alloca(bt, "tuple").map_err(llvm_err)?;

        let undef = struct_ty.get_undef();
        let mut result = undef;
        for (i, val) in values.iter().enumerate() {
            let bv: BasicValueEnum = match val {
                TypedValue::Str(ptr) => {
                    let loaded = self.load_string(*ptr)?;
                    loaded.as_basic_value_enum()
                }
                TypedValue::List(ptr) => {
                    let loaded = self.load_list(*ptr)?;
                    loaded.as_basic_value_enum()
                }
                TypedValue::Struct(ptr, st) => {
                    let bt2: BasicTypeEnum = (*st).into();
                    self.builder.build_load(bt2, *ptr, "tuple_field").map_err(llvm_err)?
                }
                TypedValue::Enum(ptr, et, ..) => {
                    let bt2: BasicTypeEnum = (*et).into();
                    self.builder.build_load(bt2, *ptr, "tuple_field").map_err(llvm_err)?
                }
                _ => {
                    val.to_bv().unwrap_or_else(|| {
                        self.i64_ty().const_int(0, false).as_basic_value_enum()
                    })
                }
            };
            result = self.builder.build_insert_value(result, bv, i as u32, "tuple_elem")
                .map_err(llvm_err)?.into_struct_value();
        }
        self.builder.build_store(alloca, result).map_err(llvm_err)?;
        Ok(TypedValue::Struct(alloca, struct_ty))
    }

    /// Convert a compile result to a fat {i64, ptr} struct value for map/set runtime calls
    pub(super) fn to_fat_struct(&mut self, val: &TypedValue<'ctx>) -> Result<BasicValueEnum<'ctx>, String> {
        match val {
            TypedValue::Str(ptr) => Ok(self.load_string(*ptr)?.into()),
            TypedValue::Enum(ptr, ty, ..) => {
                let bt: BasicTypeEnum = (*ty).into();
                Ok(self.builder.build_load(bt, *ptr, "fat_enum").map_err(llvm_err)?)
            }
            TypedValue::Struct(ptr, ty) => {
                let bt: BasicTypeEnum = (*ty).into();
                Ok(self.builder.build_load(bt, *ptr, "fat_struct").map_err(llvm_err)?)
            }
            TypedValue::List(ptr) => {
                let undef = self.string_type.get_undef();
                let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(6, false), 0, "tag").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, *ptr, 1, "data").map_err(llvm_err)?;
                Ok(r2.as_basic_value_enum())
            }
            TypedValue::Map(ptr) => {
                let undef = self.string_type.get_undef();
                let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(7, false), 0, "tag").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, *ptr, 1, "data").map_err(llvm_err)?;
                Ok(r2.as_basic_value_enum())
            }
            TypedValue::Set(ptr) => {
                let undef = self.string_type.get_undef();
                let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(8, false), 0, "tag").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, *ptr, 1, "data").map_err(llvm_err)?;
                Ok(r2.as_basic_value_enum())
            }
            TypedValue::Task(ptr) => {
                let undef = self.string_type.get_undef();
                let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(9, false), 0, "tag").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, *ptr, 1, "data").map_err(llvm_err)?;
                Ok(r2.as_basic_value_enum())
            }
            TypedValue::Stream(ptr) => {
                let undef = self.string_type.get_undef();
                let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(10, false), 0, "tag").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, *ptr, 1, "data").map_err(llvm_err)?;
                Ok(r2.as_basic_value_enum())
            }
            _ => {
                // Scalar value: wrap in {scalar, null}
                let bv = val.to_bv().unwrap_or_else(|| self.i64_ty().const_int(0, false).into());
                let undef = self.string_type.get_undef();
                let r1 = self.builder.build_insert_value(undef, bv, 0, "wrap0").map_err(llvm_err)?;
                let r2 = self.builder.build_insert_value(r1, self.ptr_ty().const_zero(), 1, "wrap1").map_err(llvm_err)?;
                Ok(r2.as_basic_value_enum())
            }
        }
    }

}
