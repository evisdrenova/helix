use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use helix::index::Index;
use std::collections::HashMap;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

struct RepoConfig {
    name: &'static str,
    url: &'static str,
    category: &'static str,
}

const REAL_REPOS: &[RepoConfig] = &[
    RepoConfig {
        name: "ripgrep",
        url: "https://github.com/BurntSushi/ripgrep.git",
        category: "small",
    },
    RepoConfig {
        name: "fd",
        url: "https://github.com/sharkdp/fd.git",
        category: "small",
    },
    RepoConfig {
        name: "rust-analyzer",
        url: "https://github.com/rust-lang/rust-analyzer.git",
        category: "medium",
    },
    RepoConfig {
        name: "tokio",
        url: "https://github.com/tokio-rs/tokio.git",
        category: "medium",
    },
];

#[derive(Clone)]
struct BenchResult {
    repo: String,
    category: String,
    operation: String,
    helix_mean: f64,
    helix_median: f64,
    helix_std_dev: f64,
    git_mean: f64,
    git_median: f64,
    git_std_dev: f64,
}
// from the perspective of helix, big number is good, small number is bad
impl BenchResult {
    fn speedup_mean(&self) -> f64 {
        self.git_mean / self.helix_mean
    }

    fn speedup_median(&self) -> f64 {
        self.git_median / self.helix_median
    }

    fn improvement_pct(&self) -> f64 {
        ((self.git_mean - self.helix_mean) / self.git_mean) * 100.0
    }
}

fn print_summary_table(results: &[BenchResult]) {
    println!("\n\n");
    println!("╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗");
    println!("║                                              HELIX vs GIT BENCHMARK SUMMARY                                                              ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╣");
    println!("║ Repo            │ Operation  │      Helix (μs)       │       Git (μs)        │    Speedup    │  Improvement  ║");
    println!("║                 │            │  Mean │  Med │ StdDev │  Mean │  Med │ StdDev │  Mean │  Med  │     (Mean)    ║");
    println!("╠═════════════════╪════════════╪═══════╪══════╪════════╪═══════╪══════╪════════╪═══════╪═══════╪═══════════════╣");

    for result in results {
        println!(
            "║ {:<15} │ {:<10} │ {:>5.1} │ {:>4.1} │ {:>6.1} │ {:>5.1} │ {:>4.1} │ {:>6.1} │ {:>5.1}x │ {:>5.1}x │ {:>11.1}% ║",
            result.repo,
            result.operation,
            result.helix_mean,
            result.helix_median,
            result.helix_std_dev,
            result.git_mean,
            result.git_median,
            result.git_std_dev,
            result.speedup_mean(),
            result.speedup_median(),
            result.improvement_pct(),
        );
    }

    println!("╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝");

    // Category summaries
    let mut by_category: HashMap<&str, Vec<&BenchResult>> = HashMap::new();
    for result in results {
        by_category
            .entry(&result.category)
            .or_default()
            .push(result);
    }

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║            AVERAGE SPEEDUP BY CATEGORY                   ║");
    println!("╠═══════════════════════════════════════════════════════════╣");
    println!("║ Category        │ Avg Speedup │ Avg Improvement         ║");
    println!("╠═════════════════╪═════════════╪═════════════════════════╣");

    for (category, cat_results) in by_category.iter() {
        let avg_speedup: f64 =
            cat_results.iter().map(|r| r.speedup_mean()).sum::<f64>() / cat_results.len() as f64;
        let avg_improvement: f64 =
            cat_results.iter().map(|r| r.improvement_pct()).sum::<f64>() / cat_results.len() as f64;
        println!(
            "║ {:<15} │ {:>9.2}x │ {:>20.1}% ║",
            category, avg_speedup, avg_improvement
        );
    }

    println!("╚═══════════════════════════════════════════════════════════╝");

    // Overall summary
    let overall_speedup: f64 =
        results.iter().map(|r| r.speedup_mean()).sum::<f64>() / results.len() as f64;
    let overall_improvement: f64 =
        results.iter().map(|r| r.improvement_pct()).sum::<f64>() / results.len() as f64;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║                  OVERALL SUMMARY                          ║");
    println!("╠═══════════════════════════════════════════════════════════╣");
    println!("║ Average Speedup:       {:>28.2}x ║", overall_speedup);
    println!("║ Average Improvement:   {:>27.1}% ║", overall_improvement);
    println!("╚═══════════════════════════════════════════════════════════╝\n");
}

fn ensure_repo(config: &RepoConfig) -> PathBuf {
    let repo_path = PathBuf::from(format!("/tmp/helix-bench-repos/{}", config.name));

    if repo_path.exists() && is_valid_repo(&repo_path) {
        eprintln!("✓ Using existing repo: {}", config.name);
        return repo_path;
    }

    fs::create_dir_all(repo_path.parent().unwrap()).ok();
    eprintln!("Cloning {} from {}...", config.name, config.url);

    let mut clone_cmd = Command::new("git");
    clone_cmd.args(&[
        "clone",
        "--depth",
        "1",
        config.url,
        repo_path.to_str().unwrap(),
    ]);

    let output = clone_cmd.output().expect("Failed to clone repo");

    if !output.status.success() {
        eprintln!(
            "Failed to clone {}: {}",
            config.name,
            String::from_utf8_lossy(&output.stderr)
        );
        panic!("Clone failed");
    }

    eprintln!("✓ Cloned {}", config.name);
    repo_path
}

fn is_valid_repo(path: &Path) -> bool {
    path.join(".git").exists() && path.join(".git/index").exists()
}

struct BenchStats {
    mean: f64,
    median: f64,
    std_dev: f64,
}

fn run_manual_bench<F>(mut f: F) -> BenchStats
where
    F: FnMut() -> usize,
{
    const ITERATIONS: usize = 100;
    let mut times = Vec::with_capacity(ITERATIONS);

    // Warmup
    for _ in 0..10 {
        black_box(f());
    }

    // Actual measurements
    for _ in 0..ITERATIONS {
        let start = std::time::Instant::now();
        black_box(f());
        let elapsed = start.elapsed();
        times.push(elapsed.as_micros() as f64);
    }

    times.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let median = times[times.len() / 2];
    let variance = times.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / times.len() as f64;
    let std_dev = variance.sqrt();

    BenchStats {
        mean,
        median,
        std_dev,
    }
}

fn bench_index_read_by_repo(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_read");
    group.sample_size(100);

    let repos: Vec<_> = REAL_REPOS
        .iter()
        .map(|config| (config, ensure_repo(config)))
        .collect();

    eprintln!("\n========================================");
    eprintln!("Running Criterion benchmarks...");
    eprintln!("========================================\n");

    // Run criterion benchmarks
    for (config, repo_path) in &repos {
        let bench_name = format!("{}_{}", config.category, config.name);

        group.bench_with_input(
            BenchmarkId::new("helix", &bench_name),
            repo_path,
            |b, path| {
                b.iter(|| {
                    let idx = Index::open(black_box(path)).unwrap();
                    black_box(idx.entries().count())
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("git", &bench_name),
            repo_path,
            |b, path| {
                b.iter(|| {
                    // let repo = git2::Repository::open(black_box(path)).unwrap();
                    // let idx = repo.index().unwrap();
                    // black_box(idx.iter().count())
                    let index_path = black_box(path).join(".git/index");
                    let idx = git2::Index::open(&index_path).unwrap();
                    black_box(idx.iter().count())
                });
            },
        );
    }

    group.finish();

    eprintln!("\n========================================");
    eprintln!("Collecting detailed statistics...");
    eprintln!("========================================\n");

    let mut results = Vec::new();

    for (config, repo_path) in &repos {
        eprintln!("Measuring {}...", config.name);

        let helix_times = run_manual_bench(|| {
            let idx = Index::open(repo_path).unwrap();
            idx.entries().count()
        });

        let git_times = run_manual_bench(|| {
            let repo = git2::Repository::open(repo_path).unwrap();
            let idx = repo.index().unwrap();
            idx.iter().count()
        });

        results.push(BenchResult {
            repo: config.name.to_string(),
            category: config.category.to_string(),
            operation: "read".to_string(),
            helix_mean: helix_times.mean,
            helix_median: helix_times.median,
            helix_std_dev: helix_times.std_dev,
            git_mean: git_times.mean,
            git_median: git_times.median,
            git_std_dev: git_times.std_dev,
        });
    }

    print_summary_table(&results);
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(100)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(5));
    targets = bench_index_read_by_repo
}

criterion_main!(benches);
