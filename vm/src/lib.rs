pub use vm_core::*;

#[cfg(not(feature = "fast"))]
pub use ::interpreter::match_loop::MatchInterpreter;
/// Compatibility alias for the old crate's name.
#[cfg(not(feature = "fast"))]
pub use ::interpreter::match_loop::MatchInterpreter as MatchLoopInterpreter;
pub use vm_core::Interpreter;

#[cfg(feature = "fast")]
pub use ::interpreter::BecomeInterpreter;

/// The interpreter the `kettevm` binary runs on. Enabling the `fast` feature
/// (nightly-only) picks the tail-call interpreter; otherwise the portable
/// match-loop interpreter is used.
#[cfg(feature = "fast")]
pub type DefaultInterpreter = ::interpreter::BecomeInterpreter;
#[cfg(not(feature = "fast"))]
pub type DefaultInterpreter = ::interpreter::match_loop::MatchInterpreter;

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
        let declare_globals = vm.runtime::<KetteRuntime>().declare_globals;
        kette_compiler::compile_kette(source, mode, declare_globals)
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
