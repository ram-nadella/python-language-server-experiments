use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use pylight::{SearchEngine, Symbol, SymbolKind};
use std::sync::Arc;

fn generate_symbols(count: usize) -> Vec<Arc<Symbol>> {
    let mut symbols = Vec::new();

    for i in 0..count {
        if i % 3 == 0 {
            symbols.push(Arc::new(Symbol::new(
                format!("function_{i}"),
                SymbolKind::Function,
                format!("file{}.py", i % 10),
                i,
                0,
            )));
        } else if i % 3 == 1 {
            symbols.push(Arc::new(Symbol::new(
                format!("Class_{i}"),
                SymbolKind::Class,
                format!("file{}.py", i % 10),
                i,
                0,
            )));
        } else {
            symbols.push(Arc::new(
                Symbol::new(
                    format!("method_{i}"),
                    SymbolKind::Method,
                    format!("file{}.py", i % 10),
                    i,
                    4,
                )
                .with_container(format!("Class_{}", i - 1)),
            ));
        }
    }

    symbols
}

fn bench_exact_search(c: &mut Criterion) {
    let engine = SearchEngine::new();
    let symbols = generate_symbols(1000);

    c.bench_function("search_exact_match", |b| {
        b.iter(|| {
            let results = engine.search(black_box("function_500"), &symbols);
            black_box(results);
        });
    });
}

fn bench_fuzzy_search(c: &mut Criterion) {
    let engine = SearchEngine::new();
    let symbols = generate_symbols(1000);

    c.bench_function("search_fuzzy_match", |b| {
        b.iter(|| {
            let results = engine.search(black_box("func"), &symbols);
            black_box(results);
        });
    });
}

fn bench_search_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_by_symbol_count");
    let engine = SearchEngine::new();

    let sizes = vec![100, 500, 1000, 5000, 10000];

    for size in sizes {
        let symbols = generate_symbols(size);

        group.bench_with_input(BenchmarkId::from_parameter(size), &symbols, |b, symbols| {
            b.iter(|| {
                let results = engine.search(black_box("test"), symbols);
                black_box(results);
            });
        });
    }

    group.finish();
}

fn bench_no_matches(c: &mut Criterion) {
    let engine = SearchEngine::new();
    let symbols = generate_symbols(1000);

    c.bench_function("search_no_matches", |b| {
        b.iter(|| {
            let results = engine.search(black_box("xyz123nonexistent"), &symbols);
            black_box(results);
        });
    });
}

criterion_group!(
    benches,
    bench_exact_search,
    bench_fuzzy_search,
    bench_search_sizes,
    bench_no_matches
);
criterion_main!(benches);
