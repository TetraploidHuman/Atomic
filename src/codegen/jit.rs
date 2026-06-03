// Submodule: jit

use inkwell::OptimizationLevel;

use super::CodeGen;

impl<'ctx> CodeGen<'ctx> {
    // ---- JIT ----

    pub fn run_jit(&self) -> Result<(), String> {
        // Verify the module before JIT
        if let Err(e) = self.module.verify() {
            return Err(format!("LLVM module verification failed: {}", e));
        }
        let opt = match self.opt_level {
            0 => OptimizationLevel::None,
            1 => OptimizationLevel::Less,
            2 => OptimizationLevel::Default,
            _ => OptimizationLevel::Aggressive,
        };
        let engine = self.module.create_jit_execution_engine(opt)
            .map_err(|e| e.to_string())?;
        unsafe {
            let main: inkwell::execution_engine::JitFunction<unsafe extern "C" fn()> =
                engine.get_function("main").map_err(|e| e.to_string())?;
            main.call();
        }
        Ok(())
    }
}
