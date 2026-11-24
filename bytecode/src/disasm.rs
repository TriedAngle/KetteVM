use std::fmt::Write;

use crate::{
    AtomSwitchTable, Constant, Function, FunctionId, Operand, Program, SwitchTable,
    opcodes::IndexKind, try_decode,
};

/// Render a whole program as human-readable bytecode.
pub fn disassemble(program: &Program) -> String {
    let mut out = String::new();
    for id in program.function_ids() {
        if !out.is_empty() {
            out.push('\n');
        }
        write_function(&mut out, program, id);
    }
    out
}

/// Render one function as human-readable bytecode.
pub fn disassemble_function(program: &Program, id: FunctionId) -> String {
    let mut out = String::new();
    write_function(&mut out, program, id);
    out
}

fn write_function(out: &mut String, program: &Program, id: FunctionId) {
    let function = program.function(id);
    let name = function
        .name
        .as_deref()
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_else(|| "<anonymous>".to_string());
    writeln!(
        out,
        "function {} {name} arity={} registers={} feedback={} tail_callable={}",
        id.0,
        function.arity,
        function.register_count,
        function.feedback_count,
        function.tail_callable,
    )
    .expect("write to string");

    if !function.constants.is_empty() {
        writeln!(out, "  constants:").expect("write to string");
        for (index, constant) in function.constants.iter().enumerate() {
            writeln!(out, "    c{index} = {}", constant_summary(constant))
                .expect("write to string");
        }
    }

    writeln!(out, "  code:").expect("write to string");
    let mut pc = 0usize;
    while pc < function.code.len() {
        let Some((op, operands, next)) = try_decode(&function.code, pc) else {
            writeln!(out, "    {pc:04} <truncated or invalid>").expect("write to string");
            break;
        };
        write!(out, "    {pc:04} {:<28}", format!("{op:?}")).expect("write to string");
        for index in 0..op.operands().len() {
            if index > 0 {
                out.push(' ');
            }
            write_operand(out, function, op, &operands, index);
        }
        out.push('\n');
        pc = next;
    }

    if !function.handlers.is_empty() {
        writeln!(out, "  handlers:").expect("write to string");
        for handler in &function.handlers {
            writeln!(
                out,
                "    try={}..{} catch={}",
                handler.try_start, handler.try_end, handler.handler_pc
            )
            .expect("write to string");
        }
    }
}

fn write_operand(
    out: &mut String,
    function: &Function,
    op: crate::Opcode,
    operands: &crate::Operands<'_>,
    index: usize,
) {
    match op.operands()[index] {
        Operand::Register => write!(out, "r{}", operands.reg(index)).expect("write to string"),
        Operand::RegisterListStart => {
            write!(out, "r{}", operands.reg_list(index)).expect("write to string")
        }
        Operand::RegisterCount => {
            write!(out, "#{}", operands.reg_count(index)).expect("write to string")
        }
        Operand::Immediate => write!(out, "{}", operands.imm(index)).expect("write to string"),
        Operand::UImmediate => write!(out, "{}", operands.uimm(index)).expect("write to string"),
        Operand::Index => {
            let value = operands.idx(index);
            match op.index_kinds()[index] {
                IndexKind::ConstantPool => {
                    let summary = function
                        .constants
                        .get(value)
                        .map(short_constant)
                        .unwrap_or_else(|| "?".to_string());
                    write!(out, "c{value}({summary})").expect("write to string")
                }
                IndexKind::Feedback => write!(out, "f{value}").expect("write to string"),
                IndexKind::Unchecked => write!(out, "{value}").expect("write to string"),
            }
        }
    }
}

fn short_constant(constant: &Constant) -> String {
    match constant {
        Constant::String(bytes) => {
            format!("{:?}", String::from_utf8_lossy(bytes))
        }
        Constant::Atom(bytes) => {
            format!(":{}", String::from_utf8_lossy(bytes))
        }
        Constant::Float(value) => format!("{value:?}"),
        Constant::Smi(value) => value.to_string(),
        Constant::Callable(id) => format!("fn{}", id.0),
        Constant::ScopeInfo(layout) => format!("scope({} locals)", layout.locals.len()),
        Constant::GlobalDeclarations(declarations) => {
            format!("globals({} vars)", declarations.vars.len())
        }
        Constant::ObjectPrototype => "ObjectPrototype".to_string(),
        Constant::FunctionPrototype => "FunctionPrototype".to_string(),
        Constant::GlobalObject => "GlobalObject".to_string(),
        Constant::SwitchTable(table) => switch_summary(table),
        Constant::AtomSwitchTable(table) => atom_switch_summary(table),
    }
}

fn constant_summary(constant: &Constant) -> String {
    match constant {
        Constant::String(bytes) => {
            format!("String({:?})", String::from_utf8_lossy(bytes))
        }
        Constant::Atom(bytes) => {
            format!("Atom(:{})", String::from_utf8_lossy(bytes))
        }
        Constant::Float(value) => format!("Float({value:?})"),
        Constant::Smi(value) => format!("Smi({value})"),
        Constant::Callable(id) => format!("Callable({})", id.0),
        Constant::ScopeInfo(layout) => format!(
            "ScopeInfo({:?})",
            layout
                .locals
                .iter()
                .map(|local| String::from_utf8_lossy(&local.name).into_owned())
                .collect::<Vec<_>>()
        ),
        Constant::GlobalDeclarations(declarations) => format!(
            "GlobalDeclarations(vars={:?}, funcs={:?})",
            declarations
                .vars
                .iter()
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .collect::<Vec<_>>(),
            declarations
                .funcs
                .iter()
                .map(|function| (
                    String::from_utf8_lossy(&function.name).into_owned(),
                    function.function.0,
                ))
                .collect::<Vec<_>>()
        ),
        Constant::ObjectPrototype => "ObjectPrototype".to_string(),
        Constant::FunctionPrototype => "FunctionPrototype".to_string(),
        Constant::GlobalObject => "GlobalObject".to_string(),
        Constant::SwitchTable(table) => switch_summary(table),
        Constant::AtomSwitchTable(table) => atom_switch_summary(table),
    }
}

fn switch_summary(table: &SwitchTable) -> String {
    format!(
        "SwitchTable(min={}, targets={:?})",
        table.min, table.targets
    )
}

fn atom_switch_summary(table: &AtomSwitchTable) -> String {
    let atoms: Vec<String> = table
        .atoms
        .iter()
        .map(|atom| format!(":{}", String::from_utf8_lossy(atom)))
        .collect();
    format!(
        "AtomSwitchTable(atoms={atoms:?}, targets={:?})",
        table.targets
    )
}
