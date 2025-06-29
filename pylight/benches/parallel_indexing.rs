use criterion::{criterion_group, criterion_main, Criterion};
use pylight::SymbolIndex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use walkdir::WalkDir;

fn download_django_if_needed() -> PathBuf {
    let test_dir = std::env::temp_dir().join("pylight-benchmark");
    let django_dir = test_dir.join("django");

    if !django_dir.exists() {
        eprintln!("Downloading Django repository for benchmarking...");
        std::fs::create_dir_all(&test_dir).unwrap();

        let status = std::process::Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "https://github.com/django/django.git",
            ])
            .current_dir(&test_dir)
            .status()
            .expect("Failed to clone Django repo");

        if !status.success() {
            panic!("Failed to clone Django repository");
        }
    }

    django_dir
}

fn benchmark_parallel_indexing(c: &mut Criterion) {
    // Download Django repo if needed
    let django_dir = download_django_if_needed();

    // Collect Python files once
    let python_files: Vec<PathBuf> = WalkDir::new(&django_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "py"))
        .map(|e| e.path().to_path_buf())
        .collect();
    let file_count = python_files.len();

    eprintln!("Found {file_count} Python files in Django repository");

    // Create a benchmark group
    let mut group = c.benchmark_group("parallel_indexing");
    group.sample_size(10); // Reduce sample size for faster benchmarking
    group.measurement_time(Duration::from_secs(30)); // Give enough time for meaningful measurements

    // Benchmark with different thread counts
    for num_threads in [1, 2, 4, 8, 16] {
        if num_threads > num_cpus::get() {
            continue; // Skip thread counts higher than available CPUs
        }

        group.bench_function(format!("{num_threads}_threads"), |b| {
            b.iter(|| {
                // Configure rayon thread pool for this benchmark
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(num_threads)
                    .build()
                    .unwrap();

                pool.install(|| {
                    // Create a fresh index for each iteration
                    let index = Arc::new(SymbolIndex::default());

                    // Parse and index files
                    // Return the result to prevent optimization
                    index.parse_and_index_files(python_files.clone()).unwrap()
                });
            });
        });
    }

    group.finish();
}

fn benchmark_parallel_vs_sequential(c: &mut Criterion) {
    let django_dir = download_django_if_needed();
    let python_files: Vec<PathBuf> = WalkDir::new(&django_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "py"))
        .map(|e| e.path().to_path_buf())
        .collect();

    // Take a subset for faster comparison
    let subset: Vec<_> = python_files.into_iter().take(100).collect();

    let mut group = c.benchmark_group("parallel_vs_sequential");
    group.sample_size(10);

    // Sequential baseline (1 thread)
    group.bench_function("sequential", |b| {
        b.iter(|| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap();

            pool.install(|| {
                let index = Arc::new(SymbolIndex::default());
                index.parse_and_index_files(subset.clone()).unwrap()
            });
        });
    });

    // Parallel with all available CPUs
    group.bench_function("parallel", |b| {
        b.iter(|| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(num_cpus::get())
                .build()
                .unwrap();

            pool.install(|| {
                let index = Arc::new(SymbolIndex::default());
                index.parse_and_index_files(subset.clone()).unwrap()
            });
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    benchmark_parallel_indexing,
    benchmark_parallel_vs_sequential
);
criterion_main!(benches);
