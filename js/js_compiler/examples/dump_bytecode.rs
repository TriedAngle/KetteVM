//! Disassemble a script: parse → compile → print per-function bytecode.

use js_compiler::native::NativeIds;
use vm_core::NativeIndex;

use bytecode::Constant;
use bytecode::decode;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dump_bytecode <script.js>");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let program = match js_compiler::compile_js(&src, bytecode::SourceMode::Script, &native_ids()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    };
    for (i, f) in program.functions().enumerate() {
        println!(
            "=== function {i} kind={:?} name={:?} params={} regs={} ===",
            f.kind,
            program.name(f).map(String::from_utf8_lossy),
            f.arity,
            f.register_count
        );
        let code = program.code(f);
        let mut pc = 0usize;
        while pc < code.len() {
            let (op, ops, next) = decode(code, pc);
            print!("  {pc:4}: {op:?}");
            for (i, kind) in op.operands().iter().enumerate() {
                use bytecode::Operand;
                match *kind {
                    Operand::RegisterCount => print!(" {}", ops.reg_count(i)),
                    Operand::RegisterListStart => print!(" {}", ops.reg_list(i)),
                    Operand::Register => print!(" r{}", ops.reg(i)),
                    Operand::Immediate => print!(" #{}", ops.imm(i)),
                    Operand::Index => print!(" idx{}", ops.idx(i)),
                    Operand::UImmediate => print!(" {}", ops.uimm(i)),
                }
            }
            // annotate constant-pool indices
            let idx_ops: Vec<usize> = op
                .operands()
                .iter()
                .enumerate()
                .filter(|(_, k)| matches!(k, bytecode::Operand::Index))
                .map(|(i, _)| ops.idx(i))
                .collect();
            for idx in idx_ops {
                if let Some(c) = program.constants(f).get(idx) {
                    match c {
                        Constant::String(s) => {
                            print!("  ; {:?} = {:?}", idx, String::from_utf8_lossy(s))
                        }
                        Constant::Callable(fid) => print!("  ; {idx} = fn{fid:?}"),
                        other => print!("  ; {idx} = {:?}", other),
                    }
                }
            }
            println!();
            pc = next;
        }
    }
}

/// Ids are placeholders: this tool never executes the program.
fn native_ids() -> NativeIds {
    NativeIds {
        get_iterator: NativeIndex(0),
        iterator_next: NativeIndex(0),
        iterator_done: NativeIndex(0),
        iterator_value: NativeIndex(0),
        has_property: NativeIndex(0),
        copy_data_properties: NativeIndex(0),
        create_private_name: NativeIndex(0),
        private_get: NativeIndex(0),
        private_set: NativeIndex(0),
        private_in: NativeIndex(0),
        set_class_fields: NativeIndex(0),
        init_instance_fields: NativeIndex(0),
        require_object_coercible: NativeIndex(0),
        delete_property_operator: NativeIndex(0),
        delete_identifier_sloppy: NativeIndex(0),
        delete_super_property: NativeIndex(0),
        for_in_enumerate: NativeIndex(0),
        for_in_next: NativeIndex(0),
        set_function_name: NativeIndex(0),
        install_accessor: NativeIndex(0),
        define_own_property: NativeIndex(0),
        set_prototype: NativeIndex(0),
        throw_if_not_constructor_or_null: NativeIndex(0),
        throw_if_not_object_or_null: NativeIndex(0),
        throw_super_not_called_if_hole: NativeIndex(0),
        throw_super_already_called_if_not_hole: NativeIndex(0),
        construct_super: NativeIndex(0),
        construct_super_all_args: NativeIndex(0),
        construct_super_via: NativeIndex(0),
        load_dynamic_name: NativeIndex(0),
        store_dynamic_name: NativeIndex(0),
        declare_eval_var: NativeIndex(0),
        declare_eval_function: NativeIndex(0),
        resolve_direct_eval: NativeIndex(0),
        store_repl_global_init: NativeIndex(0),
        validate_eval_declarations: NativeIndex(0),
        declare_globals: NativeIndex(0),
        throw_const_assign: NativeIndex(0),
        create_rest_parameter: NativeIndex(0),
        super_get_property: NativeIndex(0),
        super_set_property: NativeIndex(0),
    }
}
