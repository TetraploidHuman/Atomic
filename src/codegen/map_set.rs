// Submodule: map_set

use atomic::ast::*;
use inkwell::values::{BasicValue, BasicValueEnum};
use inkwell::types::BasicTypeEnum;

use super::{CodeGen, TypedValue, llvm_err, InnerType};

impl<'ctx> CodeGen<'ctx> {
    pub(super) fn compile_map_lit(&mut self, entries: &[(Expr, Expr)]) -> Result<TypedValue<'ctx>, String> {
        let cap = self.i64_ty().const_int((entries.len() + 4) as u64, false);
        let cc = self.call_rt("atomic_map_create", &[cap.into()])?;
        let map_bv = cc.try_as_basic_value().basic().ok_or("map_create failed")?;
        let alloca = self.builder.build_alloca(self.list_type, "map_lit").map_err(llvm_err)?;
        self.builder.build_store(alloca, map_bv).map_err(llvm_err)?;

        for (key_expr, val_expr) in entries {
            let key_val = self.compile_expr(key_expr)?;
            let val_val = self.compile_expr(val_expr)?;
            let key_fat = self.to_fat_struct(&key_val)?;
            let val_fat = self.to_fat_struct(&val_val)?;
            let map_loaded = self.load_list(alloca)?;
            let cc = self.call_rt("atomic_map_insert", &[map_loaded.into(), key_fat.into(), val_fat.into()])?;
            let new_map = cc.try_as_basic_value().basic().ok_or("map_insert failed")?;
            self.builder.build_store(alloca, new_map).map_err(llvm_err)?;
        }

        Ok(TypedValue::Map(alloca))
    }

    pub(super) fn compile_set_lit(&mut self, elements: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        // Set uses the same layout as map but with 2-i64 entries instead of 4-i64
        // For simplicity, use map layout but store elements as keys with null values
        let cap = self.i64_ty().const_int((elements.len() + 4) as u64, false);
        let cc = self.call_rt("atomic_map_create", &[cap.into()])?;
        let set_bv = cc.try_as_basic_value().basic().ok_or("map_create failed")?;
        let alloca = self.builder.build_alloca(self.list_type, "set_lit").map_err(llvm_err)?;
        self.builder.build_store(alloca, set_bv).map_err(llvm_err)?;

        let null_val: BasicValueEnum = {
            let undef = self.string_type.get_undef();
            let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(0, false), 0, "sn0").map_err(llvm_err)?;
            let r2 = self.builder.build_insert_value(r1, self.ptr_ty().const_zero(), 1, "sn1").map_err(llvm_err)?;
            r2.as_basic_value_enum()
        };

        for elem_expr in elements {
            let elem_val = self.compile_expr(elem_expr)?;
            let elem_fat = self.to_fat_struct(&elem_val)?;
            let set_loaded = self.load_list(alloca)?;
            let cc = self.call_rt("atomic_map_insert", &[set_loaded.into(), elem_fat.into(), null_val.into()])?;
            let new_set = cc.try_as_basic_value().basic().ok_or("map_insert failed")?;
            self.builder.build_store(alloca, new_set).map_err(llvm_err)?;
        }

        Ok(TypedValue::Set(alloca))
    }

    pub(super) fn builtin_set_of(&mut self, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        // Set.of(...) is equivalent to a set literal with the given elements
        let cap = self.i64_ty().const_int((args.len() + 4) as u64, false);
        let cc = self.call_rt("atomic_map_create", &[cap.into()])?;
        let set_bv = cc.try_as_basic_value().basic().ok_or("map_create failed")?;
        let alloca = self.builder.build_alloca(self.list_type, "set_of").map_err(llvm_err)?;
        self.builder.build_store(alloca, set_bv).map_err(llvm_err)?;

        let null_val: BasicValueEnum = {
            let undef = self.string_type.get_undef();
            let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(0, false), 0, "sn0").map_err(llvm_err)?;
            let r2 = self.builder.build_insert_value(r1, self.ptr_ty().const_zero(), 1, "sn1").map_err(llvm_err)?;
            r2.as_basic_value_enum()
        };

        for elem_expr in args {
            let elem_val = self.compile_expr(elem_expr)?;
            let elem_fat = self.to_fat_struct(&elem_val)?;
            let set_loaded = self.load_list(alloca)?;
            let cc = self.call_rt("atomic_map_insert", &[set_loaded.into(), elem_fat.into(), null_val.into()])?;
            let new_set = cc.try_as_basic_value().basic().ok_or("map_insert failed")?;
            self.builder.build_store(alloca, new_set).map_err(llvm_err)?;
        }

        Ok(TypedValue::Set(alloca))
    }

    pub(super) fn builtin_map_insert(&mut self, receiver: &Expr, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 2 {
            return Err("map.insert expects 2 arguments (key, value)".to_string());
        }
        let map_ptr = if let Expr::Ident(name) = receiver {
            if let Some(var) = self.scope.get(name) { var.ptr }
            else { return Err(format!("Undefined variable: {}", name)); }
        } else {
            let map_val = self.compile_expr(receiver)?;
            match map_val {
                TypedValue::Map(p) => p,
                _ => return Err("insert: receiver must be a map".to_string()),
            }
        };
        let key_val = self.compile_expr(&args[0])?;
        let val_val = self.compile_expr(&args[1])?;
        let key_fat = self.to_fat_struct(&key_val)?;
        let val_fat = self.to_fat_struct(&val_val)?;
        let map_loaded = self.load_list(map_ptr)?;
        let cc = self.call_rt("atomic_map_insert", &[map_loaded.into(), key_fat.into(), val_fat.into()])?;
        let new_map = cc.try_as_basic_value().basic().ok_or("map_insert failed")?;
        self.builder.build_store(map_ptr, new_map).map_err(llvm_err)?;
        Ok(TypedValue::Map(map_ptr))
    }

    pub(super) fn builtin_map_remove(&mut self, receiver: &Expr, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err("map.remove expects 1 argument (key)".to_string());
        }
        let map_ptr = if let Expr::Ident(name) = receiver {
            if let Some(var) = self.scope.get(name) { var.ptr }
            else { return Err(format!("Undefined variable: {}", name)); }
        } else {
            let map_val = self.compile_expr(receiver)?;
            match map_val {
                TypedValue::Map(p) => p,
                _ => return Err("remove: receiver must be a map".to_string()),
            }
        };
        let key_val = self.compile_expr(&args[0])?;
        let key_fat = self.to_fat_struct(&key_val)?;
        let map_loaded = self.load_list(map_ptr)?;
        let remove_fn = self.module.get_function("atomic_map_remove")
            .ok_or("atomic_map_remove not found")?;
        let rc = self.builder.build_call(remove_fn, &[map_loaded.into(), key_fat.into()], "remove").map_err(llvm_err)?;
        let new_map = rc.try_as_basic_value().basic().ok_or("remove failed")?;
        self.builder.build_store(map_ptr, new_map).map_err(llvm_err)?;
        Ok(TypedValue::Map(map_ptr))
    }

    pub(super) fn builtin_map_contains(&mut self, receiver: &Expr, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err("map.contains expects 1 argument (key)".to_string());
        }
        let map_val = self.compile_expr(receiver)?;
        let map_ptr = match map_val {
            TypedValue::Map(p) => p,
            _ => return Err("contains: receiver must be a map".to_string()),
        };
        let key_val = self.compile_expr(&args[0])?;
        let key_fat = self.to_fat_struct(&key_val)?;
        let map_loaded = self.load_list(map_ptr)?;
        let contains_fn = self.module.get_function("atomic_map_contains")
            .ok_or("atomic_map_contains not found")?;
        let cc = self.builder.build_call(contains_fn, &[map_loaded.into(), key_fat.into()], "contains").map_err(llvm_err)?;
        let contains = cc.try_as_basic_value().basic().ok_or("contains failed")?.into_int_value();
        Ok(TypedValue::Bool(contains))
    }

    pub(super) fn builtin_set_insert(&mut self, receiver: &Expr, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err("set.insert expects 1 argument (element)".to_string());
        }
        let set_ptr = if let Expr::Ident(name) = receiver {
            if let Some(var) = self.scope.get(name) { var.ptr }
            else { return Err(format!("Undefined variable: {}", name)); }
        } else {
            let set_val = self.compile_expr(receiver)?;
            match set_val {
                TypedValue::Set(p) => p,
                _ => return Err("insert: receiver must be a set".to_string()),
            }
        };
        let elem_val = self.compile_expr(&args[0])?;
        let elem_fat = self.to_fat_struct(&elem_val)?;
        let null_val: BasicValueEnum = {
            let undef = self.string_type.get_undef();
            let r1 = self.builder.build_insert_value(undef, self.i64_ty().const_int(0, false), 0, "sn0").map_err(llvm_err)?;
            let r2 = self.builder.build_insert_value(r1, self.ptr_ty().const_zero(), 1, "sn1").map_err(llvm_err)?;
            r2.as_basic_value_enum()
        };
        let set_loaded = self.load_list(set_ptr)?;
        // Check if element already exists
        let contains_fn = self.module.get_function("atomic_map_contains")
            .ok_or("atomic_map_contains not found")?;
        let cc = self.builder.build_call(contains_fn, &[set_loaded.into(), elem_fat.into()], "contains").map_err(llvm_err)?;
        let contains = cc.try_as_basic_value().basic().ok_or("contains failed")?.into_int_value();
        // If not contained, insert
        let current_fn = self.builder.get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("not in function")?;
        let insert_bb = self.context.append_basic_block(current_fn, "si_insert");
        let skip_bb = self.context.append_basic_block(current_fn, "si_skip");
        let _ = self.builder.build_conditional_branch(contains, skip_bb, insert_bb);
        self.builder.position_at_end(insert_bb);
        let set_loaded2 = self.load_list(set_ptr)?;
        let cc2 = self.call_rt("atomic_map_insert", &[set_loaded2.into(), elem_fat.into(), null_val.into()])?;
        let new_set = cc2.try_as_basic_value().basic().ok_or("map_insert failed")?;
        self.builder.build_store(set_ptr, new_set).map_err(llvm_err)?;
        let _ = self.builder.build_unconditional_branch(skip_bb);
        self.builder.position_at_end(skip_bb);
        Ok(TypedValue::Set(set_ptr))
    }

    pub(super) fn builtin_set_remove(&mut self, receiver: &Expr, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err("set.remove expects 1 argument (element)".to_string());
        }
        let set_ptr = if let Expr::Ident(name) = receiver {
            if let Some(var) = self.scope.get(name) { var.ptr }
            else { return Err(format!("Undefined variable: {}", name)); }
        } else {
            let set_val = self.compile_expr(receiver)?;
            match set_val {
                TypedValue::Set(p) => p,
                _ => return Err("remove: receiver must be a set".to_string()),
            }
        };
        let elem_val = self.compile_expr(&args[0])?;
        let elem_fat = self.to_fat_struct(&elem_val)?;
        let set_loaded = self.load_list(set_ptr)?;
        let remove_fn = self.module.get_function("atomic_map_remove")
            .ok_or("atomic_map_remove not found")?;
        let rc = self.builder.build_call(remove_fn, &[set_loaded.into(), elem_fat.into()], "remove").map_err(llvm_err)?;
        let new_set = rc.try_as_basic_value().basic().ok_or("remove failed")?;
        self.builder.build_store(set_ptr, new_set).map_err(llvm_err)?;
        Ok(TypedValue::Set(set_ptr))
    }

    pub(super) fn builtin_set_contains(&mut self, receiver: &Expr, args: &[Expr]) -> Result<TypedValue<'ctx>, String> {
        if args.len() != 1 {
            return Err("set.contains expects 1 argument (element)".to_string());
        }
        let set_ptr = if let Expr::Ident(name) = receiver {
            if let Some(var) = self.scope.get(name) { var.ptr }
            else { return Err(format!("Undefined variable: {}", name)); }
        } else {
            let set_val = self.compile_expr(receiver)?;
            match set_val {
                TypedValue::Set(p) => p,
                _ => return Err("contains: receiver must be a set".to_string()),
            }
        };
        let elem_val = self.compile_expr(&args[0])?;
        let elem_fat = self.to_fat_struct(&elem_val)?;
        let set_loaded = self.load_list(set_ptr)?;
        let contains_fn = self.module.get_function("atomic_map_contains")
            .ok_or("atomic_map_contains not found")?;
        let cc = self.builder.build_call(contains_fn, &[set_loaded.into(), elem_fat.into()], "contains").map_err(llvm_err)?;
        let contains = cc.try_as_basic_value().basic().ok_or("contains failed")?.into_int_value();
        Ok(TypedValue::Bool(contains))
    }

    pub(super) fn compile_enum_construct(
        &mut self,
        enum_info: &atomic::typecheck::EnumInfo,
        variant: &atomic::typecheck::EnumVariantInfo,
        args: &[Expr],
    ) -> Result<TypedValue<'ctx>, String> {
        let i64 = self.i64_ty();
        let ptr_ty = self.ptr_ty();

        // Get or create the enum LLVM type {i64, i8*}
        let enum_ty = *self.enum_types.get(&enum_info.name)
            .ok_or_else(|| format!("Enum '{}' not in type map", enum_info.name))?;

        // Allocate space for the enum struct on the stack
        let enum_bt: BasicTypeEnum = enum_ty.into();
        let alloca = self.builder.build_alloca(enum_bt, "enum_val").map_err(llvm_err)?;

        // Set the discriminant (tag)
        let tag_val = i64.const_int(variant.tag as u64, false);
        let undef = enum_ty.get_undef();
        let r1 = self.builder.build_insert_value(undef, tag_val, 0, "tag").map_err(llvm_err)?;

        // For data-carrying variants, allocate heap memory and store fields
        let data_ptr = if variant.params.is_empty() {
            ptr_ty.const_zero() // null pointer for unit variants
        } else {
            // Compile args first to determine sizes
            let compiled: Vec<TypedValue> = args.iter()
                .map(|a| self.compile_expr(a))
                .collect::<Result<Vec<_>, _>>()?;
            // Calculate total bytes: each field uses its alloca type size
            let mut total_bytes: u64 = 0;
            let mut offsets: Vec<u64> = Vec::new();
            for v in &compiled {
                offsets.push(total_bytes);
                let field_ty = v.get_type_for_alloca(self);
                total_bytes += if field_ty.is_struct_type() { 16 } else { 8 };
            }
            let buf = self.malloc_rc(i64.const_int(total_bytes as u64, false))?;

            // Store each field at its offset (LLVM 18 opaque pointers)
            for (i, v) in compiled.iter().enumerate() {
                let offset = offsets[i];
                let field_ptr = if offset == 0 {
                    buf
                } else {
                    let i8_ty = self.context.i8_type();
                    let offset_val = i8_ty.const_int(offset, false);
                    unsafe { self.builder.build_gep(i8_ty, buf, &[offset_val], "field_ptr") }
                        .map_err(llvm_err)?
                };
                // store_value_to_alloca handles load+store for complex types
                self.store_value_to_alloca(v, field_ptr)?;
            }
            // Set initial refcount to 1 (the enum owns the first reference)
            self.rc_inc(buf)?;
            buf
        };

        let r2 = self.builder.build_insert_value(r1, data_ptr, 1, "data").map_err(llvm_err)?;
        self.builder.build_store(alloca, r2).map_err(llvm_err)?;

        Ok(TypedValue::Enum(alloca, enum_ty, InnerType::Int, true))
    }

}
