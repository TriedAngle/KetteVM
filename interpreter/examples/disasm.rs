use bytecode::{Constant, Operand, SourceMode, decode};
use js_compiler::compile_js;
use js_compiler::native::NativeIds;
use vm_core::NativeIndex;

fn show_const(c: &Constant) -> String {
    match c {
        Constant::String(b) => format!("\"{}\"", String::from_utf8_lossy(b)),
        Constant::Float(f) => format!("{f}"),
        Constant::Smi(v) => format!("{v}"),
        Constant::Callable(id) => format!("<fn#{}>", id.0),
        Constant::ScopeInfo(layout) => {
            format!("<scope:{} flags:{}>", layout.locals.len(), layout.flags)
        }
        Constant::ObjectPrototype => "<Object.prototype>".into(),
        Constant::FunctionPrototype => "<Function.prototype>".into(),
        Constant::GlobalDeclarations(d) => {
            format!("<globals:{} vars:{} funcs>", d.vars.len(), d.funcs.len())
        }
        Constant::GlobalObject => "<globalThis>".into(),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: disasm <file.js>");
    let filter = args.next();
    let src = std::fs::read_to_string(&path).expect("read");
    let prog = compile_js(&src, SourceMode::Script, &native_ids()).expect("compile");
    for id in prog.function_ids() {
        let f = prog.function(id);
        let name = f
            .name
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "<anonymous>".into());
        if let Some(pat) = &filter
            && !name.contains(pat.as_str())
        {
            continue;
        }
        println!(
            "=== fn#{} {}  regs={} arity={} fb={} constants={} ===",
            id.0,
            name,
            f.register_count,
            f.arity,
            f.feedback_count,
            f.constants.len()
        );
        let mut pc = 0usize;
        while pc < f.code.len() {
            let (op, ops, next) = decode(&f.code, pc);
            let kinds = op.operands();
            let mut rendered = Vec::new();
            for (i, kind) in kinds.iter().enumerate() {
                let raw = match kind {
                    Operand::Register => ops.reg(i),
                    Operand::RegisterListStart => ops.reg_list(i),
                    Operand::RegisterCount => ops.reg_count(i) as i32,
                    Operand::Immediate => ops.imm(i),
                    Operand::UImmediate => ops.uimm(i) as i32,
                    Operand::Index => ops.idx(i) as i32,
                };
                rendered.push(format!("{raw}"));
            }
            println!(
                "  @{pc:>4}: {:<24} {}",
                format!("{op:?}"),
                rendered.join(", ")
            );
            pc = next;
        }
        if !f.constants.is_empty() {
            println!("  constants:");
            for (i, c) in f.constants.iter().enumerate() {
                println!("    [{i}] {}", show_const(c));
            }
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
