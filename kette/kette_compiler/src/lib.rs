pub mod emit;
pub mod plan;

use bytecode::{FrontendDiagnostic, FrontendError, Program, SourceMode};
use kette_parser::parse;
use kette_syntax::resolve::{Resolved, resolve};
use kette_syntax::validate::validate;
use kette_syntax::{TextRange, Tree};
use vm_core::NativeIndex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub range: TextRange,
    pub message: String,
}

impl core::fmt::Display for CompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "unsupported: {} at {}..{}",
            self.message, self.range.start, self.range.end
        )
    }
}

impl std::error::Error for CompileError {}

pub fn compile(
    tree: &Tree,
    resolved: &Resolved,
    mode: SourceMode,
    declare_globals: NativeIndex,
    install_accessor: NativeIndex,
) -> Result<Program, CompileError> {
    let _pipeline = trace::info_span!("kette::compile_new");
    emit::generate(tree, resolved, mode, declare_globals, install_accessor)
}

pub fn compile_kette(
    source: &str,
    mode: SourceMode,
    declare_globals: NativeIndex,
    install_accessor: NativeIndex,
) -> Result<Program, FrontendError> {
    let parsed = parse(source);
    if !parsed.errors.is_empty() {
        let diagnostics = parsed
            .errors
            .into_iter()
            .map(|error| FrontendDiagnostic {
                span: Some((error.range.start, error.range.end)),
                line: None,
                column: None,
                message: error.message,
            })
            .collect();
        return Err(FrontendError::syntax_many(diagnostics));
    }
    let diagnostics = validate(&parsed.tree);
    if !diagnostics.is_empty() {
        let diagnostics = diagnostics
            .into_iter()
            .map(|diagnostic| FrontendDiagnostic {
                span: Some((diagnostic.range.start, diagnostic.range.end)),
                line: None,
                column: None,
                message: diagnostic.message,
            })
            .collect();
        return Err(FrontendError::compile_many(diagnostics));
    }
    let resolved = resolve(&parsed.tree);
    compile(
        &parsed.tree,
        &resolved,
        mode,
        declare_globals,
        install_accessor,
    )
    .map_err(|error| FrontendError::compile(error.to_string()))
}
