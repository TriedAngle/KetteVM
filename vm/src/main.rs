use std::io::Write;
use std::path::Path;

use kette_compiler::KetteCompiler;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{Compiler, DefaultInterpreter, JSRuntime, KetteRuntime, Thread, VM};
use vm::{DenseString, Float, Heap, JavascriptCompiler, SlotName, Smi, Tagged, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Language {
    Javascript,
    Kette,
    Infer,
}

impl Language {
    fn of(path: &str) -> Option<Self> {
        match Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("js") => Some(Self::Javascript),
            Some("ktt") => Some(Self::Kette),
            _ => None,
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "js" | "javascript" => Some(Self::Javascript),
            "ktt" | "kette" => Some(Self::Kette),
            "auto" | "infer" => Some(Self::Infer),
            _ => None,
        }
    }
}

fn main() {
    trace::init();
    let vm = VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<JSRuntime>()
        .expect("failed to install js runtime")
        .add::<KetteRuntime>()
        .expect("failed to install kette runtime");
    vm.arm_gc_stress();
    let mut thread = vm.attach();

    let mut files = Vec::new();
    let mut repl_language = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg == "--repl" {
            let language = args.peek().and_then(|next| Language::parse(next));
            repl_language = Some(match language {
                Some(language) => {
                    args.next();
                    language
                }
                None => Language::Infer,
            });
        } else if let Some(name) = arg.strip_prefix("--repl=") {
            repl_language = Some(Language::parse(name).unwrap_or_else(|| {
                eprintln!("kettevm: unknown REPL language '{name}' (expected js or kette)");
                std::process::exit(1);
            }));
        } else {
            files.push(arg);
        }
    }
    if repl_language == Some(Language::Infer) {
        repl_language = files.last().and_then(|path| Language::of(path));
        if repl_language.is_none() {
            eprintln!("kettevm: --repl needs a language; use --repl js or --repl kette");
            std::process::exit(1);
        }
    }

    for path in &files {
        run_file(&mut thread, path);
        if thread.vm().is_shutdown() {
            return;
        }
    }
    if let Some(language) = repl_language
        && !thread.vm().is_shutdown()
    {
        repl(&mut thread, language);
    }
}

// the frontend is chosen at the call site; the VM stays language-agnostic
fn run_file(thread: &mut Thread, path: &str) {
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("kettevm: cannot read {path}: {e}");
            std::process::exit(1);
        }
    };
    match Language::of(path) {
        Some(Language::Javascript) => run_with::<JavascriptCompiler>(thread, &src, path),
        Some(Language::Kette) => run_with::<KetteCompiler>(thread, &src, path),
        Some(Language::Infer) | None => {
            eprintln!("kettevm: cannot tell the language of {path} (expected .js or .ktt)");
            std::process::exit(1);
        }
    }
}

fn run_with<C: Compiler>(thread: &mut Thread, src: &str, path: &str) {
    match thread.eval::<C>(src) {
        Ok(_) => {
            if report_uncaught(thread) {
                eprintln!("{path}: uncaught exception");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn repl(thread: &mut Thread, language: Language) {
    let stdin = std::io::stdin();
    loop {
        if thread.vm().is_shutdown() {
            break;
        }
        print!("> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("kettevm: read error: {e}");
                break;
            }
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == ".exit" {
            break;
        }
        let result = match language {
            Language::Javascript => thread.eval_repl::<JavascriptCompiler>(line),
            Language::Kette => thread.eval_repl::<KetteCompiler>(line),
            Language::Infer => unreachable!("infer is resolved before the REPL starts"),
        };
        match result {
            Ok(v) => {
                if report_uncaught(thread) {
                    continue;
                }
                println!("{}", show_value(thread, v));
            }
            Err(e) => eprintln!("{e}"),
        }
    }
}

fn report_uncaught(thread: &mut Thread) -> bool {
    match thread.take_pending_exception() {
        Some(ex) => {
            eprintln!("uncaught exception: {}", show_value(thread, ex));
            true
        }
        None => false,
    }
}

fn show_value(thread: &mut Thread, v: Value) -> String {
    if let Some(smi) = Smi::decode(v) {
        return smi.value().to_string();
    }
    thread.handle_scope(|thread, scope| {
        let heap = thread.heap();
        // Safety: `v` comes from this execution and no collection has run
        // since it was produced.
        let tagged = unsafe { Tagged::<Value>::from_value_unchecked(v) };
        let known = heap.known();
        if tagged == known.undefined.get(heap) {
            return "undefined".into();
        }
        if tagged == known.null.get(heap) {
            return "null".into();
        }
        if tagged == known.true_object.get(heap) {
            return "true".into();
        }
        if tagged == known.false_object.get(heap) {
            return "false".into();
        }
        if let Some(f) = tagged.get_as::<Float>(heap) {
            return f.value.get().to_string();
        }
        if let Some(s) = tagged.get_as::<DenseString>(heap) {
            return s.to_rust_string(heap);
        }
        if let Some(text) = error_text(heap, &scope, tagged) {
            return text;
        }
        format!("{tagged:?}")
    })
}

fn error_text(heap: &Heap, scope: &vm::HandleScope<'_>, v: Tagged<'_, Value>) -> Option<String> {
    v.as_heap_object()?;
    let known = heap.known();
    let name = error_property(heap, scope, v, known.strings.name.get(heap));
    let message = error_property(heap, scope, v, known.strings.message.get(heap));
    match (name, message) {
        (Some(n), Some(m)) if !m.is_empty() => Some(format!("{n}: {m}")),
        (Some(n), _) => Some(n),
        (None, Some(m)) if !m.is_empty() => Some(m),
        _ => None,
    }
}

fn error_property<'a>(
    heap: &'a Heap,
    scope: &vm::HandleScope<'_>,
    v: Tagged<'a, Value>,
    name: Tagged<'a, SlotName>,
) -> Option<String> {
    // error formatting must not run getters or traps
    let value =
        vm::Object::get_data_property(heap, scope, scope.handle(v), scope.handle(name.erase()));
    value
        .get_as::<DenseString>(heap)
        .map(|s| s.to_rust_string(heap))
}
