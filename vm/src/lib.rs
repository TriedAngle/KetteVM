pub use vm_core::*;

/// The interpreter the `kettevm` binary runs on: tail-call in fast builds,
/// loop-call in debug builds, and loop-match in stable release builds.
pub use ::interpreter::DefaultInterpreter;
pub use vm_core::Interpreter;

pub use js_runtime::JSRuntime;
pub use kette_runtime::KetteRuntime;

/// The JS front end paired with [`JSRuntime`]: its compiler emits the native
/// ids the JS runtime registered.
pub struct JavascriptCompiler;

impl Compiler for JavascriptCompiler {
    fn compile(
        vm: &VM,
        source: &str,
        mode: bytecode::SourceMode,
    ) -> Result<bytecode::Program, bytecode::FrontendError> {
        js_compiler::compile_js(source, mode, &vm.runtime::<JSRuntime>().ids)
    }

    const RUNTIME: RuntimeId = JSRuntime::ID;
}

/// The Kette front end paired with [`KetteRuntime`].
pub struct KetteCompiler;

impl Compiler for KetteCompiler {
    fn compile(
        vm: &VM,
        source: &str,
        mode: bytecode::SourceMode,
    ) -> Result<bytecode::Program, bytecode::FrontendError> {
        let runtime = vm.runtime::<KetteRuntime>();
        kette_compiler::compile_kette(
            source,
            mode,
            runtime.declare_globals,
            runtime.install_accessor,
        )
    }

    const RUNTIME: RuntimeId = KetteRuntime::ID;
}

pub trait VmEval {
    fn eval<C: Compiler>(&self, source: &str) -> Result<Value, ScriptError>;
}

impl VmEval for VM {
    fn eval<C: Compiler>(&self, source: &str) -> Result<Value, ScriptError> {
        let mut thread = self.attach();
        thread.eval::<C>(source)
    }
}
