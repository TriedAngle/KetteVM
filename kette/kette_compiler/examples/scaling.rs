use std::fmt::Write;
use std::hint::black_box;
use std::time::Instant;

use kette_parser::parse;
use kette_syntax::resolve::resolve;

fn main() {
    for count in [500, 1000, 2000, 4000, 8000] {
        let mut source = String::new();
        for index in 0..count {
            writeln!(source, "let v{index} = {index}").unwrap();
        }
        for index in (0..count).rev() {
            writeln!(source, "v{index}").unwrap();
        }

        let start = Instant::now();
        let parsed = parse(&source);
        let parse_time = start.elapsed();
        assert!(parsed.errors.is_empty());

        let start = Instant::now();
        let resolved = resolve(&parsed.tree);
        let resolve_time = start.elapsed();
        black_box(&resolved);

        println!(
            "n={count:<6} bytes={:<9} nodes={:<8} tokens={:<8} scopes={:<6} bindings={:<6} parse={:>8.2} ms resolve={:>8.2} ms ({:.1} ns/binding/use)",
            source.len(),
            parsed.tree.nodes().len(),
            parsed.tree.tokens().len(),
            resolved.scopes().len(),
            resolved.bindings().len(),
            parse_time.as_secs_f64() * 1e3,
            resolve_time.as_secs_f64() * 1e3,
            resolve_time.as_secs_f64() * 1e9 / (count as f64 * count as f64),
        );
    }
}
