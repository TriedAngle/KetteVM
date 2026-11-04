use bytecode::{Opcode, Operand, SourceMode, try_decode};
use js_compiler::compile_js;
use js_compiler::native::NativeIds;
use vm_core::NativeIndex;

const SAMPLE: &str = r#"
const counter = { value: 0 };

function bump(obj, n) {
  for (let i = 0; i < n; i++) {
    obj.value = obj.value + 1;
  }
  return obj.value;
}

bump(counter, 3);
"#;

fn main() {
    let source = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(path).unwrap(),
        None => SAMPLE.to_string(),
    };
    let program = compile_js(&source, SourceMode::Script, &native_ids()).expect("compile");
    for id in program.function_ids() {
        let f = program.function(id);
        let name = f
            .name
            .as_deref()
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_else(|| "<entry>".into());
        println!(
            "=== {name} (regs={}, arity={}, length={}, kind={:?}) ===",
            f.register_count, f.arity, f.length, f.kind
        );
        if !f.constants.is_empty() {
            for (i, c) in f.constants.iter().enumerate() {
                println!("  const[{i}] = {c:?}");
            }
        }
        let mut pc = 0;
        while pc < f.code.len() {
            let (op, ops, next) = try_decode(&f.code, pc).unwrap();
            let jump_operand = match op {
                Opcode::Jump | Opcode::JumpLoop => Some(0),
                Opcode::JumpIfTruthy | Opcode::JumpIfFalsy | Opcode::JumpIfNotUndefined => Some(0),
                Opcode::CompareJump => Some(2),
                _ => None,
            };
            let mut text = format!("{pc:4}  {op:?}");
            for (i, kind) in op.operands().iter().enumerate() {
                match kind {
                    Operand::Register | Operand::RegisterListStart => {
                        text.push_str(&format!(" r{}", ops.reg(i)))
                    }
                    Operand::RegisterCount => text.push_str(&format!(" #{}", ops.reg_count(i))),
                    Operand::Immediate => {
                        text.push_str(&format!(" {}", ops.imm(i)));
                        if jump_operand == Some(i) {
                            text.push_str(&format!(
                                " -> {}",
                                bytecode::jump_target(pc, ops.imm(i))
                            ));
                        }
                    }
                    Operand::UImmediate => text.push_str(&format!(" {}", ops.uimm(i))),
                    Operand::Index => text.push_str(&format!(" [{}]", ops.idx(i))),
                }
            }
            print!("{text}");
            if op.reads_acc() && op.writes_acc() {
                print!("   ; acc -> acc");
            } else if op.reads_acc() {
                print!("   ; acc in");
            } else if op.writes_acc() {
                print!("   ; -> acc");
            }
            println!();
            pc = next;
        }
        println!();
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
