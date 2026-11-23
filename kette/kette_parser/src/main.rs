use std::path::Path;

fn main() {
    let mut failed = false;
    for path in std::env::args().skip(1) {
        let source = std::fs::read_to_string(&path).expect("read");
        let parsed = kette_parser::parse(&source);
        if let Err(reason) = parsed.tree.validate() {
            failed = true;
            eprintln!("fail  {path}: invalid tree: {reason}");
            continue;
        }
        let name = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&path);
        if parsed.errors.is_empty() {
            println!("ok    {name} ({} nodes)", parsed.tree.nodes().len());
        } else {
            failed = true;
            eprintln!("fail  {path}");
            for error in parsed.errors {
                eprintln!(
                    "      {}..{}: {}",
                    error.range.start, error.range.end, error.message
                );
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
