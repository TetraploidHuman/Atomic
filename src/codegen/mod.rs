// Atomic CodeGen — LLVM IR code generation
// Core types and compilation entry point. See submodules for other methods.

// Atomic CodeGen — LLVM IR code generation
//
// File structure (line ranges approximate):
//   Lines    1-11   Imports
//   Lines   12-75   Scope / ScopeVar / ValKind types
//   Lines   77-127  TypedValue type
//   Lines  129-163  CodeGen struct, TcoState
//   Lines  165-203  CodeGen::new() + type helpers (i64_ty, f64_ty, ptr_ty, etc.)
//   Lines  204-4074 define_runtime() — LLVM runtime function declarations (~3900 lines)
//   Lines 4076-4116 Runtime helpers: call_rt, load_string, load_list, etc.
//   Lines 4119-4300 Type inference: infer_return_type, infer_expr_type, build_fn_type, etc.
//   Lines 4302-4418 compile(), print_ir(), verify()
//   Lines 4423-5081 compile_stmt(), compile_fun_def(), compile_let, etc.
//   Lines 5081-5975 compile_expr(), compile_lambda(), compile_binary(), compile_unary(), compile_call()
//   Lines 5975-6395 compile_call() (continued)
//   Lines 6396-8237 Builtin functions: print, list, map, filter, fold, flat_map, etc.
//   Lines 8238-10902 builtin_stdlib() — stdlib function dispatcher (~2600 lines)
//   Lines 10903-11544 Pattern matching: compile_when, compile_pattern_match, bind_pattern_vars
//   Lines 11545-12267 For loops: compile_for, compile_for_iterate, compile_for_yield, etc.
//   Lines 12260-13308 Expressions: compile_range, compile_if, compile_block, compile_index,
//          compile_field_access, compile_struct_lit, compile_tuple, compile_map_lit, compile_set_lit,
//          compile_string_interp, compile_safe_call, compile_ufcs_call, compile_enum_construct
//   Lines 13058-13220 Map/Set operations: builtin_map_insert, builtin_set_contains, etc.
//   Lines 13290-13343 run_jit(), TypedValue helpers
//
// To split further: break the `impl<'ctx> CodeGen<'ctx>` block into submodules
// by closing/reopening at the boundaries marked above.

use atomic::ast::*;
use atomic::typecheck::TypeRegistry;
use inkwell::builder::BuilderError;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FunctionType, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValue, BasicValueEnum, IntValue, PointerValue};
use std::collections::{HashMap, HashSet};


// ---- Scope ----
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum ValKind {
    Int,
    Float,
    Bool,
    Str,
    Fn,
    List,
    Map,
    Set,
    Task,
    Stream,
    #[allow(dead_code)]
    LazyList,
    #[allow(dead_code)]
    CString,
    #[allow(dead_code)]
    Ptr,
    FileHandle,
    Struct,
    Enum,
    Unit,
}

struct ScopeVar<'ctx> {
    ptr: PointerValue<'ctx>,
    ty: inkwell::types::BasicTypeEnum<'ctx>,
    kind: ValKind,
    fn_type: Option<FunctionType<'ctx>>,
    mutable: bool,
    /// For lazy val: pointer to i1 initialized flag
    lazy_flag: Option<PointerValue<'ctx>>,
    /// For lazy val: the initializer expression (cloned)
    lazy_init_expr: Option<Expr>,
    /// AST-level type for enum resolution (e.g., Option<Date>)
    ast_type: Option<Type>,
    /// For Enum values: the inner type (Int, Float, Str) to preserve through loads
    enum_inner_type: Option<InnerType>,
    /// For Enum values with heap-allocated data: whether the data pointer needs RC cleanup
    enum_data_rc_managed: bool,
}

pub(super) struct Scope<'ctx> {
    variables: HashMap<String, ScopeVar<'ctx>>,
    parent: Option<Box<Scope<'ctx>>>,
}

impl<'ctx> Scope<'ctx> {
    fn new() -> Self {
        Scope { variables: HashMap::new(), parent: None }
    }
    fn with_parent(parent: Scope<'ctx>) -> Self {
        Scope { variables: HashMap::new(), parent: Some(Box::new(parent)) }
    }
    fn get(&self, name: &str) -> Option<&ScopeVar<'ctx>> {
        self.variables.get(name).or_else(|| self.parent.as_ref().and_then(|p| p.get(name)))
    }
    fn set(&mut self, name: String, ptr: PointerValue<'ctx>, ty: inkwell::types::BasicTypeEnum<'ctx>, kind: ValKind) {
        self.variables.insert(name, ScopeVar { ptr, ty, kind, fn_type: None, mutable: false, lazy_flag: None, lazy_init_expr: None, ast_type: None, enum_inner_type: None, enum_data_rc_managed: false });
    }
    fn set_with_fn_type(&mut self, name: String, ptr: PointerValue<'ctx>, ty: inkwell::types::BasicTypeEnum<'ctx>, kind: ValKind, fn_type: Option<FunctionType<'ctx>>) {
        self.variables.insert(name, ScopeVar { ptr, ty, kind, fn_type, mutable: false, lazy_flag: None, lazy_init_expr: None, ast_type: None, enum_inner_type: None, enum_data_rc_managed: false });
    }
    fn set_mutable(&mut self, name: String, ptr: PointerValue<'ctx>, ty: inkwell::types::BasicTypeEnum<'ctx>, kind: ValKind, fn_type: Option<FunctionType<'ctx>>) {
        self.variables.insert(name, ScopeVar { ptr, ty, kind, fn_type, mutable: true, lazy_flag: None, lazy_init_expr: None, ast_type: None, enum_inner_type: None, enum_data_rc_managed: false });
    }
    fn set_lazy(&mut self, name: String, ptr: PointerValue<'ctx>, ty: inkwell::types::BasicTypeEnum<'ctx>, kind: ValKind, flag: PointerValue<'ctx>, init_expr: Expr) {
        self.variables.insert(name, ScopeVar { ptr, ty, kind, fn_type: None, mutable: false, lazy_flag: Some(flag), lazy_init_expr: Some(init_expr), ast_type: None, enum_inner_type: None, enum_data_rc_managed: false });
    }
    fn set_with_ast_type(&mut self, name: String, ptr: PointerValue<'ctx>, ty: inkwell::types::BasicTypeEnum<'ctx>, kind: ValKind, fn_type: Option<FunctionType<'ctx>>, ast_type: Type) {
        self.variables.insert(name, ScopeVar { ptr, ty, kind, fn_type, mutable: false, lazy_flag: None, lazy_init_expr: None, ast_type: Some(ast_type), enum_inner_type: None, enum_data_rc_managed: false });
    }
    fn set_enum_inner_type(&mut self, name: &str, inner_type: InnerType) {
        if let Some(var) = self.variables.get_mut(name) {
            var.enum_inner_type = Some(inner_type);
        }
    }
    fn set_enum_data_rc_managed(&mut self, name: &str, managed: bool) {
        if let Some(var) = self.variables.get_mut(name) {
            var.enum_data_rc_managed = managed;
        }
    }
    fn local_variables(&self) -> &HashMap<String, ScopeVar<'ctx>> {
        &self.variables
    }
}

/// The type of value stored inside an enum variant (Some/Ok).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum InnerType {
    Int,
    Float,
    Str,
}

// ---- TypedValue ----
#[derive(Clone, Copy)]
pub(super) enum TypedValue<'ctx> {
    Int(IntValue<'ctx>),
    Float(inkwell::values::FloatValue<'ctx>),
    Bool(IntValue<'ctx>),
    Str(PointerValue<'ctx>),
    /// Function pointer (lambda) with its function type for correct indirect calls
    Fn(PointerValue<'ctx>, FunctionType<'ctx>),
    /// List value (pointer to {ptr, i64, i64} alloca)
    List(PointerValue<'ctx>),
    /// Struct value (alloca pointer, LLVM struct type)
    Struct(PointerValue<'ctx>, StructType<'ctx>),
    /// Enum value (alloca pointer to {i64, i8*}, LLVM enum type, inner type, rc_managed)
    Enum(PointerValue<'ctx>, StructType<'ctx>, InnerType, bool),
    /// Map value (alloca pointer to {ptr, i64, i64}, same layout as list)
    Map(PointerValue<'ctx>),
    /// Set value (alloca pointer to {ptr, i64, i64}, same layout as list)
    Set(PointerValue<'ctx>),
    /// Task<T> value (alloca pointer to {ptr, i64, i64}, same layout as list, stores single fat struct)
    Task(PointerValue<'ctx>),
    /// Stream<T> value (alloca pointer to {ptr, i64, i64}, same layout as list)
    Stream(PointerValue<'ctx>),
    /// LazyList<T> value (alloca pointer to {i64, ptr, i64, i64} struct)
    LazyList(PointerValue<'ctx>),
    /// CString value (pointer to null-terminated C string)
    CString(PointerValue<'ctx>),
    /// Ptr<T> value (opaque pointer for FFI)
    Ptr(PointerValue<'ctx>),
    /// FileHandle value (wraps FILE* pointer)
    FileHandle(PointerValue<'ctx>),
    Unit,
}

impl<'ctx> TypedValue<'ctx> {
    fn to_bv(&self) -> Option<BasicValueEnum<'ctx>> {
        match self {
            TypedValue::Int(v) => Some(v.as_basic_value_enum()),
            TypedValue::Float(v) => Some(v.as_basic_value_enum()),
            TypedValue::Bool(v) => Some(v.as_basic_value_enum()),
            TypedValue::Str(_v) => None,
            TypedValue::Fn(ptr, _) => Some(ptr.as_basic_value_enum()),
            TypedValue::List(_) => None,
            TypedValue::Map(_) => None,
            TypedValue::Set(_) => None,
            TypedValue::Task(_) => None,
            TypedValue::Stream(_) | TypedValue::LazyList(_) | TypedValue::CString(_) | TypedValue::Ptr(_) | TypedValue::FileHandle(_) => None,
            TypedValue::Struct(_, _) => None,
            TypedValue::Enum(..) => None,
            TypedValue::Unit => None,
        }
    }
}

pub(super) fn llvm_err(e: BuilderError) -> String {
    format!("LLVM: {:?}", e)
}

// ---- CodeGen ----
pub struct CodeGen<'ctx> {
    pub(super) context: &'ctx Context,
    pub(super) module: Module<'ctx>,
    pub(super) builder: inkwell::builder::Builder<'ctx>,
    pub(super) scope: Scope<'ctx>,
    pub(super) string_type: StructType<'ctx>,
    pub(super) list_type: StructType<'ctx>,
    pub(super) lambda_count: usize,
    pub(super) str_pat_counter: usize,
    pub(super) registry: TypeRegistry,
    pub(super) named_structs: HashMap<String, StructType<'ctx>>,
    pub(super) enum_types: HashMap<String, StructType<'ctx>>,
    pub(super) anon_structs: HashMap<Vec<String>, StructType<'ctx>>,
    /// Compile-time constants: name → (global pointer, element type, ValKind)
    pub(super) consts: HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>, ValKind)>,
    /// Target block for `continue` — set inside for loops, cleared on exit
    pub(super) continue_target: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    /// Target block for `break` — set inside for loops, cleared on exit
    pub(super) break_target: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    /// Extension method mapping: "TypeName.method" → "TypeName_method"
    pub(super) extension_methods: HashMap<String, String>,
    /// TCO (Tail Call Optimization) state for the current function
    pub(super) tco_state: Option<TcoState<'ctx>>,
    /// Coroutine: list alloca where launch results are collected inside coroutineScope.
    /// None means we are not inside a coroutineScope.
    pub(super) coroutine_collector: Option<inkwell::values::PointerValue<'ctx>>,
    /// Task type: {pthread: i64, done: i64, cancelled: i64, result_list: {ptr, i64, i64}}
    pub(super) task_type: StructType<'ctx>,
    /// LazyList type: {head_val: i64, step_fn: i8*, state: i64, take_count: i64, map_fn: i8*, filter_fn: i8*}
    /// take_count = -1 means infinite (or no step fn), >=0 means take that many
    /// map_fn is an optional transformer applied during to_list evaluation
    /// filter_fn is an optional predicate; elements failing the predicate are skipped
    pub(super) lazylist_type: StructType<'ctx>,
    /// Range type: {start: i64, end: i64, inclusive: i64}
    pub(super) range_type: StructType<'ctx>,
    /// Stream type: {mutex: [40 x i8], list: {ptr, i64, i64}} (mutex-protected buffer)
    pub(super) stream_type: StructType<'ctx>,
    /// Fat return type: named {i64, ptr} struct distinct from enum types.
    /// Used for untyped function/lambda returns. When packed with a scalar,
    /// field 0 holds the value and field 1 is null. When wrapping an enum,
    /// field 0 is the tag and field 1 is the data pointer.
    pub(super) fat_return_type: StructType<'ctx>,
    /// Last fat_ret alloca from unpack_fat_return/bv_to_typed, for potential
    /// bitcast when the result is returned from a typed function (e.g., enum).
    pub(super) last_fat_ret: Option<(PointerValue<'ctx>, StructType<'ctx>)>,
    /// Overloaded function mapping: base name → [(param_types, mangled_name)]
    /// e.g., "add" → [([Int, Int], "add_Int_Int"), ([Float, Float], "add_Float_Float")]
    pub(super) overloaded_functions: HashMap<String, Vec<(Vec<Type>, String)>>,
    /// Whether we are currently compiling inside an `unsafe { }` block
    pub(super) in_unsafe: bool,
    /// External C functions declared via `external fun`: name → LLVM function value
    #[allow(dead_code)]
    pub(super) external_fns: HashMap<String, inkwell::values::FunctionValue<'ctx>>,
    /// Builtin wrappers needed for :: function references (e.g., List::head)
    pub(super) builtin_wrappers_needed: HashSet<String>,
    /// LLVM optimization level (0-3)
    pub(super) opt_level: u8,
    /// Target triple for cross-compilation (None = native)
    pub(super) target_triple: Option<String>,
    /// Counter for unique wrapper function names (lazy_map, lazy_filter, etc.)
    pub(super) wrapper_counter: u64,
}

pub(super) struct TcoState<'ctx> {
    /// Target block to jump to for tail-recursive calls
    tail_entry: inkwell::basic_block::BasicBlock<'ctx>,
    /// Parameter allocas: (alloca, type, valkind)
    param_slots: Vec<(inkwell::values::PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>, ValKind)>,
    /// Original AST function name (unmangled) for self-recognition in TCO
    fn_name: String,
}

impl<'ctx> CodeGen<'ctx> {
    pub fn new(context: &'ctx Context, name: &str, registry: TypeRegistry, target_triple: Option<String>) -> Self {
        let module = context.create_module(name);
        let builder = context.create_builder();
        // Named type to distinguish from anonymous {i64, i8*} enum types
        let string_type = context.opaque_struct_type("__atomic_str");
        string_type.set_body(
            &[context.i64_type().into(), context.ptr_type(inkwell::AddressSpace::default()).into()],
            false,
        );
        let list_type = context.struct_type(
            &[
                context.ptr_type(inkwell::AddressSpace::default()).into(), // data ptr
                context.i64_type().into(), // length
                context.i64_type().into(), // capacity
            ],
            false,
        );
        // Task type: {pthread: i64, done: i64, cancelled: i64, scheduler: i64, result_list: {ptr, i64, i64}}
        let task_type = context.struct_type(
            &[
                context.i64_type().into(),    // pthread_t (opaque thread handle)
                context.i64_type().into(),    // done flag (0=not done, 1=done)
                context.i64_type().into(),    // cancelled flag (0=not cancelled, 1=cancelled)
                context.i64_type().into(),    // scheduler (0=default, 1=io, 2=cpu)
                list_type.into(),             // result list
            ],
            false,
        );
        // LazyList type: {head_val: i64, step_fn: i8*, state: i64, take_count: i64, map_fn: i8*, filter_fn: i8*}
        let lazylist_type = context.struct_type(
            &[
                context.i64_type().into(),  // head value (i64 for Int lazy lists)
                context.ptr_type(inkwell::AddressSpace::default()).into(),  // step_fn ptr
                context.i64_type().into(),  // state
                context.i64_type().into(),  // take_count (-1 = infinite, >=0 = count)
                context.ptr_type(inkwell::AddressSpace::default()).into(),  // map_fn ptr (null = no mapping)
                context.ptr_type(inkwell::AddressSpace::default()).into(),  // filter_fn ptr (null = no filter)
            ],
            false,
        );
        // Stream type: {mutex: [40 x i8], cond: [48 x i8], closed: i64, list: {ptr, i64, i64}}
        let stream_type = context.struct_type(
            &[
                context.i8_type().array_type(40).into(),  // pthread_mutex_t = 40 bytes
                context.i8_type().array_type(48).into(),  // pthread_cond_t = 48 bytes
                context.i64_type().into(),                 // closed flag
                list_type.into(),                          // data buffer list
            ],
            false,
        );
        // Range type: {start: i64, end: i64, inclusive: i64}
        let range_type = context.struct_type(
            &[
                context.i64_type().into(),
                context.i64_type().into(),
                context.i64_type().into(),
            ],
            false,
        );
        let fat_return_type = context.opaque_struct_type("__fat_ret");
        fat_return_type.set_body(
            &[context.i64_type().into(), context.ptr_type(inkwell::AddressSpace::default()).into()],
            false,
        );
        CodeGen {
            context, module, builder, scope: Scope::new(),
            string_type, list_type, lambda_count: 0, str_pat_counter: 0,
            registry,
            named_structs: HashMap::new(),
            enum_types: HashMap::new(),
            anon_structs: HashMap::new(),
            consts: HashMap::new(),
            continue_target: None,
            break_target: None,
            extension_methods: HashMap::new(),
            tco_state: None,
            coroutine_collector: None,
            task_type,
            lazylist_type,
            range_type,
            stream_type,
            fat_return_type,
            last_fat_ret: None,
            overloaded_functions: HashMap::new(),
            in_unsafe: false,
            external_fns: HashMap::new(),
            builtin_wrappers_needed: HashSet::new(),
            opt_level: 0,
            target_triple,
            wrapper_counter: 0,
        }
    }

    pub fn set_opt_level(&mut self, level: u8) {
        self.opt_level = level.min(3);
    }

    /// Convert Int or Float TypedValue to FloatValue (Int gets converted via sitofp).
    pub(super) fn typed_to_float(&self, val: &TypedValue<'ctx>) -> Result<inkwell::values::FloatValue<'ctx>, String> {
        match val {
            TypedValue::Float(fv) => Ok(*fv),
            TypedValue::Int(iv) => self.builder.build_signed_int_to_float(*iv, self.f64_ty(), "i2f").map_err(|e| format!("LLVM error: {}", e)),
            _ => Err("Expected Int or Float".to_string()),
        }
    }

    pub(super) fn i64_ty(&self) -> inkwell::types::IntType<'ctx> { self.context.i64_type() }
    pub(super) fn i32_ty(&self) -> inkwell::types::IntType<'ctx> { self.context.i32_type() }
    pub(super) fn f64_ty(&self) -> inkwell::types::FloatType<'ctx> { self.context.f64_type() }
    pub(super) fn bool_ty(&self) -> inkwell::types::IntType<'ctx> { self.context.bool_type() }
    pub(super) fn void_ty(&self) -> inkwell::types::VoidType<'ctx> { self.context.void_type() }
    pub(super) fn ptr_ty(&self) -> inkwell::types::PointerType<'ctx> { self.context.ptr_type(inkwell::AddressSpace::default()) }

    pub(super) fn make_global_str(&self, name: &str, content: &[u8]) -> inkwell::values::PointerValue<'ctx> {
        let i8 = self.context.i8_type();
        let arr_ty = i8.array_type(content.len() as u32);
        let global = self.module.add_global(arr_ty, None, name);
        let arr = self.context.const_string(content, false);
        global.set_initializer(&arr);
        global.as_pointer_value()
    }

    fn call_rt(&self, name: &str, args: &[BasicMetadataValueEnum<'ctx>]) -> Result<inkwell::values::CallSiteValue<'ctx>, String> {
        let func = self.module.get_function(name)
            .ok_or_else(|| format!("Runtime fn '{}' not found", name))?;
        self.builder.build_call(func, args, "").map_err(llvm_err)
    }

    /// Allocate memory with a refcount header. Returns data pointer (ptr+8).
    #[allow(dead_code)]
    pub(super) fn malloc_rc(&self, size: IntValue<'ctx>) -> Result<PointerValue<'ctx>, String> {
        let func = self.module.get_function("atomic_malloc_rc")
            .ok_or("atomic_malloc_rc not found")?;
        let result = self.builder.build_call(func, &[size.into()], "malloc_rc").map_err(llvm_err)?;
        Ok(result.try_as_basic_value().basic().ok_or("malloc_rc returned void")?.into_pointer_value())
    }

    /// Increment refcount on a heap-allocated value.
    pub(super) fn rc_inc(&self, ptr: PointerValue<'ctx>) -> Result<(), String> {
        self.call_rt("atomic_rc_inc", &[ptr.into()])?;
        Ok(())
    }

    /// Decrement refcount on a heap-allocated value (frees if refcount reaches 0).
    pub(super) fn rc_dec(&self, ptr: PointerValue<'ctx>) -> Result<(), String> {
        self.call_rt("atomic_rc_dec", &[ptr.into()])?;
        Ok(())
    }

    /// Emit RC decrement for all heap-typed variables in the scope.
    pub(super) fn emit_scope_cleanup(&self) -> Result<(), String> {
        for (_name, var) in self.scope.local_variables() {
            match var.kind {
                ValKind::Str => {
                    let str_val = self.load_string(var.ptr)?;
                    let data_ptr = self.builder.build_extract_value(str_val, 1, "data").map_err(llvm_err)?.into_pointer_value();
                    self.rc_dec(data_ptr)?;
                }
                ValKind::List | ValKind::Map | ValKind::Set => {
                    let list_val = self.load_list(var.ptr)?;
                    let data_ptr = self.builder.build_extract_value(list_val, 0, "data").map_err(llvm_err)?.into_pointer_value();
                    self.rc_dec(data_ptr)?;
                }
                ValKind::LazyList => {
                    // LazyList is stack-only ({i64, ptr, i64, i64}), no heap data to clean up
                }
                ValKind::Enum if var.enum_data_rc_managed => {
                    let loaded = self.builder.build_load(var.ty, var.ptr, "enum_cleanup").map_err(llvm_err)?;
                    let data_ptr = self.builder.build_extract_value(loaded.into_struct_value(), 1, "edata").map_err(llvm_err)?.into_pointer_value();
                    self.rc_dec(data_ptr)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Increment RC for a heap-typed value being bound to a variable.
    pub(super) fn rc_inc_typed_value(&self, val: &TypedValue<'ctx>) -> Result<(), String> {
        match val {
            TypedValue::Str(ptr) => {
                let str_val = self.load_string(*ptr)?;
                let data_ptr = self.builder.build_extract_value(str_val, 1, "data").map_err(llvm_err)?.into_pointer_value();
                self.rc_inc(data_ptr)?;
            }
            TypedValue::List(ptr) | TypedValue::Map(ptr) | TypedValue::Set(ptr) => {
                let list_val = self.load_list(*ptr)?;
                let data_ptr = self.builder.build_extract_value(list_val, 0, "data").map_err(llvm_err)?.into_pointer_value();
                self.rc_inc(data_ptr)?;
            }
            TypedValue::LazyList(_) => {
                // LazyList is stack-only ({i64, ptr, i64, i64}), no heap data to RC
            }
            TypedValue::Enum(alloca, _, _, true) => {
                let loaded = self.builder.build_load(self.string_type, *alloca, "enum_rcinc").map_err(llvm_err)?;
                let data_ptr = self.builder.build_extract_value(loaded.into_struct_value(), 1, "edata").map_err(llvm_err)?.into_pointer_value();
                self.rc_inc(data_ptr)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Load a string struct value from its alloca pointer
    fn load_string(&self, ptr: PointerValue<'ctx>) -> Result<inkwell::values::StructValue<'ctx>, String> {
        let loaded = self.builder.build_load(self.string_type, ptr, "str_load").map_err(llvm_err)?;
        Ok(loaded.into_struct_value())
    }

    /// Call a runtime function with a string argument (loads from alloca first)
    fn call_rt_with_str(&self, name: &str, str_ptr: PointerValue<'ctx>) -> Result<inkwell::values::CallSiteValue<'ctx>, String> {
        let str_val = self.load_string(str_ptr)?;
        self.call_rt(name, &[str_val.into()])
    }

    /// Call a runtime function with two string arguments
    fn call_rt_with_2str(&self, name: &str, s1: PointerValue<'ctx>, s2: PointerValue<'ctx>) -> Result<inkwell::values::CallSiteValue<'ctx>, String> {
        let v1 = self.load_string(s1)?;
        let v2 = self.load_string(s2)?;
        self.call_rt(name, &[v1.into(), v2.into()])
    }

    /// Load a list struct value from its alloca pointer
    fn load_list(&self, ptr: PointerValue<'ctx>) -> Result<inkwell::values::StructValue<'ctx>, String> {
        let loaded = self.builder.build_load(self.list_type, ptr, "list_load").map_err(llvm_err)?;
        Ok(loaded.into_struct_value())
    }

    /// Extract list data pointer from a loaded list struct
    #[allow(dead_code)]
    fn list_data_ptr(&self, list: inkwell::values::StructValue<'ctx>) -> Result<PointerValue<'ctx>, String> {
        Ok(self.builder.build_extract_value(list, 0, "list_data").map_err(llvm_err)?.into_pointer_value())
    }

    /// Extract list length from a loaded list struct
    fn list_len_val(&self, list: inkwell::values::StructValue<'ctx>) -> Result<IntValue<'ctx>, String> {
        Ok(self.builder.build_extract_value(list, 1, "list_len").map_err(llvm_err)?.into_int_value())
    }

    /// Guess the return type from the function body expression when no annotation is provided.
    fn infer_return_type(&self, body: &Expr) -> Option<Type> {
        match body {
            Expr::Block(stmts) => {
                stmts.last().and_then(|s| match s {
                    Stmt::Expr { expr: e, .. } => Some(self.infer_expr_type(e)),
                    _ => None,
                })
            }
            _ => Some(self.infer_expr_type(body)),
        }
    }

    fn infer_expr_type(&self, expr: &Expr) -> Type {
        match expr {
            Expr::Literal(Literal::String(_)) | Expr::StringInterpolate(_) => Type::Named("String".into()),
            Expr::Literal(Literal::Int(_)) => Type::Named("Int".into()),
            Expr::Literal(Literal::Float(_)) => Type::Named("Float".into()),
            Expr::Literal(Literal::Bool(_)) => Type::Named("Bool".into()),
            Expr::Literal(Literal::Char(_)) => Type::Named("Char".into()),
            Expr::Binary(left, op, _) => {
                if *op == BinaryOp::Add {
                    // If either side is a string, result is string
                    if matches!(self.infer_expr_type(left), Type::Named(ref n) if n == "String") {
                        return Type::Named("String".into());
                    }
                }
                Type::Named("Int".into())
            }
            Expr::Call { func, .. } => {
                if let Expr::Ident(name) = func.as_ref() {
                    match name.as_str() {
                        "print" | "println" => Type::Unit,
                        "toString" | "toUpper" | "toLower" => Type::Named("String".into()),
                        "parse_date" | "date" => Type::Generic(Box::new(Type::Named("Option".into())), vec![Type::Named("Date".into())]),
                        "datetime" => Type::Generic(Box::new(Type::Named("Option".into())), vec![Type::Named("DateTime".into())]),
                        "format" => Type::Named("String".into()),
                        "now" => Type::Named("DateTime".into()),
                        "today" => Type::Named("Date".into()),
                        "find" => Type::Generic(Box::new(Type::Named("Option".into())), vec![Type::Named("Int".into())]),
                        "flip" | "constant" | "identity" => Type::Named("Int".into()),
                        "Random_new" => Type::Named("Random".into()),
                        "next_int" => Type::Generic(Box::new(Type::Named("Tuple".into())), vec![
                            Type::Named("Random".into()),
                            Type::Named("Int".into()),
                        ]),
                        "count" => Type::Named("Int".into()),
                        "partition" => Type::Generic(Box::new(Type::Named("Tuple".into())), vec![
                            Type::Named("List".into()), Type::Named("List".into())
                        ]),
                        _ => {
                            if self.registry.lookup_variant(name).is_some() {
                                let enum_name = self.registry.variant_to_enum.get(name)
                                    .cloned()
                                    .unwrap_or_default();
                                Type::Named(enum_name)
                            } else {
                                Type::Named("Int".into())
                            }
                        }
                    }
                } else {
                    Type::Named("Int".into())
                }
            }
            Expr::When(w) => self.infer_when_type(&w.kind),
            Expr::Continue | Expr::Break => Type::Unit,
            Expr::For(_) => Type::Unit,
            Expr::Block(stmts) => {
                stmts.last().map(|s| match s {
                    Stmt::Expr { expr: e, .. } => self.infer_expr_type(e),
                    _ => Type::Unit,
                }).unwrap_or(Type::Unit)
            }
            Expr::Ident(name) => {
                // Check scope first for AST type info
                if let Some(sv) = self.scope.get(name) {
                    if let Some(ref ast_type) = sv.ast_type {
                        return ast_type.clone();
                    }
                    // Fallback: use val_kind to infer basic type
                    match sv.kind {
                        ValKind::Enum => {
                            // Try to find which enum type
                            for enum_name in self.enum_types.keys() {
                                if sv.ty == (*self.enum_types.get(enum_name).unwrap()).into() {
                                    return Type::Named(enum_name.clone());
                                }
                            }
                            Type::Named("Int".into())
                        }
                        ValKind::Str => Type::Named("String".into()),
                        ValKind::Struct => Type::Named("Int".into()), // ambiguous, default
                        ValKind::List => Type::Named("List".into()),
                        ValKind::Map => Type::Named("Map".into()),
                        ValKind::Set => Type::Named("Set".into()),
                        ValKind::Fn => Type::Named("Int".into()),
                        _ => Type::Named("Int".into()),
                    }
                } else if self.registry.lookup_variant(name).is_some() {
                    let enum_name = self.registry.variant_to_enum.get(name)
                        .cloned().unwrap_or_default();
                    Type::Named(enum_name)
                } else {
                    Type::Named("Int".into())
                }
            }
            Expr::MapLiteral(_) => Type::Map(Box::new(Type::Named("String".into())), Box::new(Type::Named("Int".into()))),
            Expr::SetLiteral(_) => Type::Set(Box::new(Type::Named("Int".into()))),
            _ => Type::Named("Int".into()),
        }
    }

    fn infer_when_type(&self, kind: &WhenKind) -> Type {
        match kind {
            WhenKind::OneLine { then_expr, else_expr, .. } => {
                let t = self.infer_expr_type(then_expr);
                if matches!(t, Type::Unit) {
                    self.infer_expr_type(else_expr)
                } else {
                    t
                }
            }
            WhenKind::ValueMatch { arms, .. } | WhenKind::ConditionChain { arms } => {
                arms.first().map(|a| self.infer_expr_type(&a.body)).unwrap_or(Type::Unit)
            }
        }
    }

    fn build_fn_type(&self, ret_ast: Option<&Type>, name: &str, param_tys: &[BasicMetadataTypeEnum<'ctx>]) -> FunctionType<'ctx> {
        match ret_ast {
            Some(Type::Unit) => self.void_ty().fn_type(param_tys, false),
            Some(Type::Named(n)) => match n.as_str() {
                "Float" | "Double" => self.f64_ty().fn_type(param_tys, false),
                "Bool" => self.bool_ty().fn_type(param_tys, false),
                "String" | "Str" => self.string_type.fn_type(param_tys, false),
                "Unit" => self.void_ty().fn_type(param_tys, false),
                "Int" => self.i64_ty().fn_type(param_tys, false),
                name => {
                    if let Some(st) = self.named_structs.get(name) {
                        (*st).fn_type(param_tys, false)
                    } else if let Some(et) = self.enum_types.get(name) {
                        (*et).fn_type(param_tys, false)
                    } else {
                        self.i64_ty().fn_type(param_tys, false)
                    }
                }
            },
            Some(Type::Function(_, _)) => self.ptr_ty().fn_type(param_tys, false),
            None => {
                if name == "main" {
                    self.void_ty().fn_type(param_tys, false)
                } else {
                    // Use named fat-return type to distinguish from enum types
                    self.fat_return_type.fn_type(param_tys, false)
                }
            }
            Some(Type::Struct(fields)) => {
                let field_tys: Vec<BasicTypeEnum> = fields.iter()
                    .map(|(_, ty)| self.ast_type_to_basic_type(ty))
                    .collect();
                let st = self.context.struct_type(&field_tys, false);
                st.fn_type(param_tys, false)
            }
            Some(Type::Map(_, _)) | Some(Type::Set(_)) => {
                // Map and Set use the {ptr, i64, i64} list layout
                let fat_ty = self.list_type;
                fat_ty.fn_type(param_tys, false)
            }
            _ => self.string_type.fn_type(param_tys, false),
        }
    }

    /// Mangle a function name by appending param types: add(Int,Float) → add_Int_Float
    pub(super) fn mangle_name(name: &str, param_types: &[Type]) -> String {
        if param_types.is_empty() {
            return name.to_string();
        }
        let parts: Vec<String> = param_types.iter().map(|t| format!("{}", t)).collect();
        format!("{}_{}", name, parts.join("_"))
    }

    /// Map a TypedValue to a type name string for overload resolution.
    pub(super) fn typed_value_type_name(&self, v: &TypedValue<'ctx>) -> String {
        match v {
            TypedValue::Int(_) => "Int".to_string(),
            TypedValue::Float(_) => "Float".to_string(),
            TypedValue::Bool(_) => "Bool".to_string(),
            TypedValue::Str(_) => "String".to_string(),
            TypedValue::Fn(_, _) => "Fn".to_string(),
            TypedValue::List(_) => "List".to_string(),
            TypedValue::Struct(_, st) => {
                // Try to find the named struct type
                for (name, ty) in &self.named_structs {
                    if *ty == *st {
                        return name.clone();
                    }
                }
                "Struct".to_string()
            }
            TypedValue::Enum(..) => {
                // Enum types are anonymous {i64, ptr} — for overload resolution
                // we use the registry to find the enum name
                "Enum".to_string()
            }
            TypedValue::Map(_) => "Map".to_string(),
            TypedValue::Set(_) => "Set".to_string(),
            TypedValue::Task(_) => "Task".to_string(),
            TypedValue::Stream(_) => "Stream".to_string(),
            TypedValue::LazyList(_) => "LazyList".to_string(),
            TypedValue::CString(_) => "CString".to_string(),
            TypedValue::Ptr(_) => "Ptr".to_string(),
            TypedValue::FileHandle(_) => "FileHandle".to_string(),
            TypedValue::Unit => "Unit".to_string(),
        }
    }

    // ---- Main entry ----

    pub fn compile(&mut self, program: &Program) -> Result<(), String> {
        self.define_runtime()?;

        // Pass 0: Register type definitions and create LLVM types
        for stmt in &program.stmts {
            self.registry.register(stmt)?;
            match stmt {
                Stmt::TypeAlias { name, definition, .. } => {
                    if let Type::Struct(fields) = definition {
                        let field_tys: Vec<BasicTypeEnum> = fields.iter()
                            .map(|(_, ty)| self.ast_type_to_basic_type(ty))
                            .collect();
                        let struct_ty = self.context.struct_type(&field_tys, false);
                        self.named_structs.insert(name.clone(), struct_ty);
                    }
                }
                Stmt::Enum { name, .. } => {
                    let i64 = self.i64_ty();
                    let ptr = self.ptr_ty();
                    let enum_ty = self.context.struct_type(&[i64.into(), ptr.into()], false);
                    self.enum_types.insert(name.clone(), enum_ty);
                }
                _ => {}
            }
        }

        // Detect overloaded function names (non-extension, non-module functions)
        let mut name_counts: HashMap<String, usize> = HashMap::new();
        for stmt in &program.stmts {
            if let Stmt::Fun { name, params, .. } = stmt {
                if params.iter().all(|p| p.ty.is_some()) {
                    *name_counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
        }
        let overloaded_names: std::collections::HashSet<String> = name_counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(name, _)| name)
            .collect();

        // Pass 1: Declare all user-defined functions for forward references
        for stmt in &program.stmts {
            if let Stmt::Fun { name, params, return_type, body, .. } = stmt {
                let param_types: Vec<Type> = params.iter()
                    .map(|p| p.ty.clone().unwrap_or(Type::Named("Int".into())))
                    .collect();
                let all_typed = params.iter().all(|p| p.ty.is_some());
                let mangled = if all_typed && overloaded_names.contains(name.as_str()) {
                    Self::mangle_name(name, &param_types)
                } else {
                    name.clone()
                };

                // Record overload info for call dispatch
                if all_typed && overloaded_names.contains(name.as_str()) {
                    self.overloaded_functions
                        .entry(name.clone())
                        .or_insert_with(Vec::new)
                        .push((param_types.clone(), mangled.clone()));
                }

                let param_llvm_tys: Vec<BasicMetadataTypeEnum> = params.iter()
                    .map(|p| self.ast_type_to_llvm(p.ty.as_ref()))
                    .collect();
                let ret_type = if name == "main" {
                    None
                } else {
                    return_type.as_ref().cloned()
                        .or_else(|| {
                            if all_typed {
                                self.infer_return_type(body)
                            } else {
                                None
                            }
                        })
                };
                let fn_type = self.build_fn_type(ret_type.as_ref(), &mangled, &param_llvm_tys);
                self.module.add_function(&mangled, fn_type, None);
            }
            if let Stmt::Module { name: mod_name, body, .. } = stmt {
                let prefix = format!("{}_", mod_name);
                for inner_stmt in body {
                    if let Stmt::Fun { name: fn_name, params, return_type, body: fn_body, .. } = inner_stmt {
                        let mangled = format!("{}{}", prefix, fn_name);
                        let param_llvm_tys: Vec<BasicMetadataTypeEnum> = params.iter()
                            .map(|p| self.ast_type_to_llvm(p.ty.as_ref()))
                            .collect();
                        let ret_type = return_type.as_ref().cloned()
                            .or_else(|| {
                                if params.iter().all(|p| p.ty.is_some()) {
                                    self.infer_return_type(fn_body)
                                } else {
                                    None
                                }
                            });
                        let fn_type = self.build_fn_type(ret_type.as_ref(), &mangled, &param_llvm_tys);
                        self.module.add_function(&mangled, fn_type, None);
                    }
                }
            }
            if let Stmt::Extension { type_name, methods, .. } = stmt {
                for m in methods {
                    if let Stmt::Fun { name, params, return_type, body, .. } = m {
                        let fn_name = format!("{}_{}", type_name, name);
                        self.extension_methods.insert(
                            format!("{}.{}", type_name, name),
                            fn_name.clone(),
                        );
                        let param_llvm_tys: Vec<BasicMetadataTypeEnum> = params.iter()
                            .map(|p| self.ast_type_to_llvm(p.ty.as_ref()))
                            .collect();
                        let ret_type = return_type.as_ref().cloned()
                            .or_else(|| {
                                if params.iter().all(|p| p.ty.is_some()) {
                                    self.infer_return_type(body)
                                } else {
                                    None
                                }
                            });
                        let fn_type = self.build_fn_type(ret_type.as_ref(), &fn_name, &param_llvm_tys);
                        self.module.add_function(&fn_name, fn_type, None);
                    }
                }
            }
        }

        // Pass 2: Compile function bodies and let/val/expr statements
        let mut has_main = false;

        // Check for main function
        for stmt in &program.stmts {
            if let Stmt::Fun { name, .. } = stmt {
                if name == "main" { has_main = true; }
            }
        }

        // If no explicit main, create one first so that top-level
        // Let/Val/Expr statements compile into the correct function.
        if !has_main {
            let main_fn = self.void_ty().fn_type(&[], false);
            let main_func = self.module.add_function("main", main_fn, None);
            let entry = self.context.append_basic_block(main_func, "entry");
            self.builder.position_at_end(entry);

            for stmt in &program.stmts {
                match stmt {
                    Stmt::Fun { .. } | Stmt::Extension { .. } => {
                        // Compile function bodies into their own LLVM functions
                        self.compile_stmt(stmt)?;
                    }
                    Stmt::TypeAlias { .. } | Stmt::Enum { .. } => {
                        // Skip pure type-level declarations
                    }
                    _ => {
                        self.compile_stmt(stmt)?;
                    }
                }
            }
            let _ = self.builder.build_return(None);
        } else {
            for stmt in &program.stmts {
                self.compile_stmt(stmt)?;
            }
        }

        Ok(())
    }

    pub fn print_ir(&self) -> String {
        // LLVMPrintModuleToString crashes on Windows (LLVM 20.x bug).
        // Work around it by printing to a temp file and reading it back.
        let tmp = std::env::temp_dir().join(format!("atomic_ir_{}.ll", std::process::id()));
        self.module
            .print_to_file(&tmp)
            .unwrap_or_else(|e| panic!("Failed to print IR to {}: {}", tmp.display(), e));
        let ir = std::fs::read_to_string(&tmp)
            .unwrap_or_else(|e| panic!("Failed to read IR from {}: {}", tmp.display(), e));
        let _ = std::fs::remove_file(&tmp);
        ir
    }

    pub fn verify(&self) -> Result<(), String> {
        // LLVMVerifyModule crashes on Windows (LLVM 20.x pre-built binary bug).
        // The IR is still valid — the crash is in LLVM's internal verification pass.
        #[cfg(target_os = "windows")]
        {
            let _ = self;
            return Ok(());
        }
        #[cfg(not(target_os = "windows"))]
        self.module.verify().map_err(|e| e.to_string())
    }

    /// Write LLVM bitcode to a file.
    /// Uses write_bitcode_to_memory + file write as a workaround for
    /// write_bitcode_to_path crash on Windows (LLVM 20.x bug).
    pub fn emit_bitcode(&self, path: &std::path::Path) -> Result<(), String> {
        let buf = self.module.write_bitcode_to_memory();
        std::fs::write(path, buf.as_slice())
            .map_err(|e| format!("Failed to write bitcode to {}: {}", path.display(), e))
    }

    /// Write assembly or object file via target machine
    fn emit_via_target_machine(&self, path: &std::path::Path, file_type: inkwell::targets::FileType) -> Result<(), String> {
        use inkwell::targets::{Target, TargetMachine, InitializationConfig};
        let triple_str = self.target_triple.as_deref().unwrap_or("native");
        let (target, cpu, features, target_triple) = match triple_str {
            "native" | "" => {
                Target::initialize_native(&InitializationConfig::default())
                    .map_err(|e| format!("Failed to initialize native target: {}", e))?;
                let tt = TargetMachine::get_default_triple();
                let t = Target::from_triple(&tt)
                    .map_err(|e| format!("Failed to get target: {}", e))?;
                let cpu = TargetMachine::get_host_cpu_name().to_string();
                let features = TargetMachine::get_host_cpu_features().to_string();
                (t, cpu, features, tt)
            }
            "linux-x64" | "x86_64-unknown-linux-gnu" => {
                Target::initialize_x86(&InitializationConfig::default());
                let tt = inkwell::targets::TargetTriple::create("x86_64-unknown-linux-gnu");
                let t = Target::from_triple(&tt)
                    .map_err(|e| format!("Failed to get target: {}", e))?;
                (t, "generic".to_string(), "".to_string(), tt)
            }
            "linux-arm64" | "aarch64-unknown-linux-gnu" => {
                Target::initialize_aarch64(&InitializationConfig::default());
                let tt = inkwell::targets::TargetTriple::create("aarch64-unknown-linux-gnu");
                let t = Target::from_triple(&tt)
                    .map_err(|e| format!("Failed to get target: {}", e))?;
                (t, "generic".to_string(), "".to_string(), tt)
            }
            "windows-x64" | "x86_64-pc-windows-gnu" => {
                Target::initialize_x86(&InitializationConfig::default());
                let tt = inkwell::targets::TargetTriple::create("x86_64-pc-windows-gnu");
                let t = Target::from_triple(&tt)
                    .map_err(|e| format!("Failed to get target: {}", e))?;
                (t, "generic".to_string(), "".to_string(), tt)
            }
            "wasm" | "wasm32-unknown-unknown" => {
                Target::initialize_webassembly(&InitializationConfig::default());
                let tt = inkwell::targets::TargetTriple::create("wasm32-unknown-unknown");
                let t = Target::from_triple(&tt)
                    .map_err(|e| format!("Failed to get target: {}", e))?;
                (t, "generic".to_string(), "".to_string(), tt)
            }
            other => {
                // Try as a raw LLVM triple: initialize all common targets
                Target::initialize_native(&InitializationConfig::default())
                    .map_err(|e| format!("Failed to initialize native target: {}", e))?;
                Target::initialize_x86(&InitializationConfig::default());
                Target::initialize_aarch64(&InitializationConfig::default());
                Target::initialize_webassembly(&InitializationConfig::default());
                let tt = inkwell::targets::TargetTriple::create(other);
                let t = Target::from_triple(&tt)
                    .map_err(|e| format!("Unknown target '{}': {}", other, e))?;
                (t, "generic".to_string(), "".to_string(), tt)
            }
        };
        let opt = match self.opt_level {
            0 => inkwell::OptimizationLevel::None,
            1 => inkwell::OptimizationLevel::Less,
            2 => inkwell::OptimizationLevel::Default,
            _ => inkwell::OptimizationLevel::Aggressive,
        };
        let target_machine = target.create_target_machine(
            &target_triple,
            &cpu,
            &features,
            opt,
            inkwell::targets::RelocMode::Default,
            inkwell::targets::CodeModel::Default,
        ).ok_or_else(|| "Failed to create target machine".to_string())?;
        target_machine.write_to_file(&self.module, file_type, path)
            .map_err(|e| format!("Failed to write to {}: {}", path.display(), e))
    }

    pub fn emit_assembly(&self, path: &std::path::Path) -> Result<(), String> {
        self.emit_via_target_machine(path, inkwell::targets::FileType::Assembly)
    }

    pub fn emit_object(&self, path: &std::path::Path) -> Result<(), String> {
        self.emit_via_target_machine(path, inkwell::targets::FileType::Object)
    }

}

// ---- Submodules ----
mod runtime;
mod stmt;
mod expr;
mod builtins;
mod pattern;
mod for_loop;
mod misc;
mod map_set;
mod jit;

// ---- TypedValue helpers ----
impl<'ctx> TypedValue<'ctx> {
    fn get_type_for_alloca(&self, cg: &CodeGen<'ctx>) -> inkwell::types::BasicTypeEnum<'ctx> {
        match self {
            TypedValue::Int(_) => cg.i64_ty().into(),
            TypedValue::Float(_) => cg.f64_ty().into(),
            TypedValue::Bool(_) => cg.bool_ty().into(),
            TypedValue::Str(_) => cg.string_type.into(),
            TypedValue::Fn(_, _) => cg.ptr_ty().into(),
            TypedValue::List(_) => cg.list_type.into(),
            TypedValue::Map(_) => cg.list_type.into(),
            TypedValue::Set(_) => cg.list_type.into(),
            TypedValue::Task(_) => cg.ptr_ty().into(),
            TypedValue::Stream(_) => cg.ptr_ty().into(),
            TypedValue::LazyList(_) => cg.lazylist_type.into(),
            TypedValue::CString(_) | TypedValue::Ptr(_) | TypedValue::FileHandle(_) => cg.ptr_ty().into(),
            TypedValue::Struct(_, ty) => (*ty).into(),
            TypedValue::Enum(_, ty, ..) => (*ty).into(),
            TypedValue::Unit => cg.i64_ty().into(),
        }
    }

    fn val_kind(&self) -> ValKind {
        match self {
            TypedValue::Int(_) => ValKind::Int,
            TypedValue::Float(_) => ValKind::Float,
            TypedValue::Bool(_) => ValKind::Bool,
            TypedValue::Str(_) => ValKind::Str,
            TypedValue::Fn(_, _) => ValKind::Fn,
            TypedValue::List(_) => ValKind::List,
            TypedValue::Map(_) => ValKind::Map,
            TypedValue::Set(_) => ValKind::Set,
            TypedValue::Task(_) => ValKind::Task,
            TypedValue::Stream(_) => ValKind::Stream,
            TypedValue::LazyList(_) => ValKind::LazyList,
            TypedValue::CString(_) => ValKind::CString,
            TypedValue::Ptr(_) => ValKind::Ptr,
            TypedValue::FileHandle(_) => ValKind::FileHandle,
            TypedValue::Struct(_, _) => ValKind::Struct,
            TypedValue::Enum(..) => ValKind::Enum,
            TypedValue::Unit => ValKind::Unit,
        }
    }
}
