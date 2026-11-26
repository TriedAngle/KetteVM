use std::sync::Arc;

use bytecode::{ModuleDecl, ModuleExport, ModuleImport, SourceEntry, SourceInfo, SourceSpan};

use crate::RawCell;

/// One function in a compile unit.
pub struct SourceFunction {
    pub span: SourceSpan,
    pub entries: Box<[SourceEntry]>,
}

/// A compiled unit kept for source queries.
pub struct SourceUnit {
    pub id: u32,
    pub name: String,
    pub hash: u64,
    pub mode: SourceInfo,
    pub source: Arc<str>,
    pub functions: Vec<SourceFunction>,
    // ODO: move these somewhere else once we implement JS modules.
    pub modules: Vec<ModuleDecl>,
    pub exports: Vec<ModuleExport>,
    pub imports: Vec<ModuleImport>,
    /// Rooted array of callable infos, indexed by function id.
    pub callables: Option<Box<RawCell>>,
}

impl SourceUnit {
    pub fn from_program(
        id: u32,
        name: String,
        source: &str,
        mode: SourceInfo,
        program: &bytecode::Program,
    ) -> Self {
        let text: Arc<str> = if mode == SourceInfo::Omit {
            Arc::from("")
        } else {
            Arc::from(source)
        };
        let functions = program
            .functions()
            .map(|function| SourceFunction {
                span: function.span,
                entries: if mode == SourceInfo::Complete {
                    function.source.to_vec().into_boxed_slice()
                } else {
                    Box::new([])
                },
            })
            .collect();
        Self {
            id,
            name,
            hash: hash_source(source),
            mode,
            source: text,
            functions,
            modules: program.modules().to_vec(),
            exports: program.exports().to_vec(),
            imports: program.imports().to_vec(),
            callables: None,
        }
    }

    pub fn function(&self, id: u32) -> Option<&SourceFunction> {
        self.functions.get(id as usize)
    }
}

/// FNV-1a over the source bytes.
pub fn hash_source(source: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in source.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
