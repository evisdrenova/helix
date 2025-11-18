// use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
// use helix::index::Index;
// use std::fs;
// use std::hint::black_box;
// use std::path::{Path, PathBuf};
// use std::process::Command;

// /// Real-world repository configurations
// struct RepoConfig {
//     name: &'static str,
//     url: &'static str,
//     category: &'static str,
//     expected_files: usize,
// }

// const REAL_REPOS: &[RepoConfig] = &[
//     // Small repos (100-1000 files)
//     RepoConfig {
//         name: "ripgrep",
//         url: "https://github.com/BurntSushi/ripgrep.git",
//         category: "small",
//         expected_files: 500,
//     },
//     RepoConfig {
//         name: "fd",
//         url: "https://github.com/sharkdp/fd.git",
//         category: "small",
//         expected_files: 300,
//     },
//     // Medium repos (1000-10000 files)
//     RepoConfig {
//         name: "rust-analyzer",
//         url: "https://github.com/rust-lang/rust-analyzer.git",
//         category: "medium",
//         expected_files: 5000,
//     },
//     RepoConfig {
//         name: "tokio",
//         url: "https://github.com/tokio-rs/tokio.git",
//         category: "medium",
//         expected_files: 3000,
//     },
//     // // Large repos (10000-50000 files)
//     // RepoConfig {
//     //     name: "rust",
//     //     url: "https://github.com/rust-lang/rust.git",
//     //     category: "large",
//     //     expected_files: 45000,
//     // },
//     // RepoConfig {
//     //     name: "vscode",
//     //     url: "https://github.com/microsoft/vscode.git",
//     //     category: "large",
//     //     expected_files: 35000,
//     // },
//     // // Extra large repos (50000-150000 files)
//     // RepoConfig {
//     //     name: "tensorflow",
//     //     url: "https://github.com/tensorflow/tensorflow.git",
//     //     category: "xlarge",
//     //     expected_files: 80000,
//     // },
//     // RepoConfig {
//     //     name: "llvm-project",
//     //     url: "https://github.com/llvm/llvm-project.git",
//     //     category: "xlarge",
//     //     expected_files: 120000,
//     // },
// ];

// /// Clone or update a repository
// fn ensure_repo(config: &RepoConfig) -> PathBuf {
//     let repo_path = PathBuf::from(format!("/tmp/helix-bench-repos/{}", config.name));

//     if repo_path.exists() && is_valid_repo(&repo_path) {
//         println!("✓ Using existing repo: {} at {:?}", config.name, repo_path);

//         // Optionally update (commented out to speed up benchmarks)
//         // update_repo(&repo_path);

//         return repo_path;
//     }

//     // Create parent directory
//     fs::create_dir_all(repo_path.parent().unwrap()).ok();

//     println!(
//         "Cloning {} ({}) from {}...",
//         config.name, config.category, config.url
//     );

//     // Clone with depth=1 for faster download (except for large repos where we want full history)
//     let depth_arg = if config.category == "large" {
//         vec![]
//     } else {
//         vec!["--depth", "1"]
//     };

//     let mut clone_cmd = Command::new("git");
//     clone_cmd.arg("clone");
//     clone_cmd.args(&depth_arg);
//     clone_cmd.arg(config.url);
//     clone_cmd.arg(repo_path.to_str().unwrap());

//     let output = clone_cmd.output().expect("Failed to clone repo");

//     if !output.status.success() {
//         eprintln!(
//             "Failed to clone {}: {}",
//             config.name,
//             String::from_utf8_lossy(&output.stderr)
//         );
//         panic!("Clone failed");
//     }

//     println!("✓ Cloned {} successfully", config.name);

//     // Verify file count
//     let actual_files = count_files(&repo_path);
//     println!(
//         "  Files: {} (expected ~{})",
//         actual_files, config.expected_files
//     );

//     repo_path
// }

// /// Check if a repo is valid
// fn is_valid_repo(path: &Path) -> bool {
//     path.join(".git").exists() && path.join(".git/index").exists()
// }

// /// Update an existing repo
// fn update_repo(path: &Path) {
//     println!("Updating repo at {:?}...", path);

//     Command::new("git")
//         .current_dir(path)
//         .args(&["fetch", "origin"])
//         .output()
//         .ok();

//     Command::new("git")
//         .current_dir(path)
//         .args(&["reset", "--hard", "origin/HEAD"])
//         .output()
//         .ok();
// }

// /// Count files in a repo
// fn count_files(path: &Path) -> usize {
//     let output = Command::new("git")
//         .current_dir(path)
//         .args(&["ls-files"])
//         .output()
//         .expect("Failed to count files");

//     String::from_utf8_lossy(&output.stdout).lines().count()
// }

// fn bench_index_read_by_repo(c: &mut Criterion) {
//     let mut group = c.benchmark_group("index_read");

//     // Ensure all repos are available
//     let repos: Vec<_> = REAL_REPOS
//         .iter()
//         .map(|config| (config, ensure_repo(config)))
//         .collect();

//     println!("\n========================================");
//     println!("Starting benchmarks...");
//     println!("========================================\n");

//     // Benchmark each repo
//     for (config, repo_path) in &repos {
//         let bench_name = format!("{}_{}", config.category, config.name);

//         // Helix implementation
//         group.bench_with_input(
//             BenchmarkId::new("helix", &bench_name),
//             repo_path,
//             |b, path| {
//                 b.iter(|| {
//                     let idx = Index::open(black_box(path)).unwrap();
//                     black_box(idx.entries().count())
//                 });
//             },
//         );

//         // Git baseline
//         group.bench_with_input(
//             BenchmarkId::new("git", &bench_name),
//             repo_path,
//             |b, path| {
//                 b.iter(|| {
//                     let repo = git2::Repository::open(black_box(path)).unwrap();
//                     let idx = repo.index().unwrap();
//                     black_box(idx.iter().count())
//                 });
//             },
//         );
//     }

//     group.finish();
// }

// fn bench_index_iteration(c: &mut Criterion) {
//     let mut group = c.benchmark_group("index_iteration");

//     let repos: Vec<_> = REAL_REPOS
//         .iter()
//         .map(|config| (config, ensure_repo(config)))
//         .collect();

//     for (config, repo_path) in &repos {
//         let bench_name = format!("{}_{}", config.category, config.name);

//         // Test full iteration through all entries
//         group.bench_with_input(
//             BenchmarkId::new("helix", &bench_name),
//             repo_path,
//             |b, path| {
//                 let idx = Index::open(path).unwrap();
//                 b.iter(|| {
//                     for entry in idx.entries() {
//                         black_box(&entry.path);
//                         black_box(&entry.oid);
//                         black_box(&entry.size);
//                         black_box(&entry.mtime);
//                     }
//                 });
//             },
//         );

//         group.bench_with_input(
//             BenchmarkId::new("git", &bench_name),
//             repo_path,
//             |b, path| {
//                 let repo = git2::Repository::open(path).unwrap();
//                 let idx = repo.index().unwrap();
//                 b.iter(|| {
//                     for entry in idx.iter() {
//                         black_box(entry.path);
//                         black_box(entry.id);
//                         black_box(entry.file_size);
//                         black_box(entry.mtime.seconds());
//                     }
//                 });
//             },
//         );
//     }

//     group.finish();
// }

// fn bench_index_open_only(c: &mut Criterion) {
//     let mut group = c.benchmark_group("index_open");

//     let repos: Vec<_> = REAL_REPOS
//         .iter()
//         .map(|config| (config, ensure_repo(config)))
//         .collect();

//     for (config, repo_path) in &repos {
//         let bench_name = format!("{}_{}", config.category, config.name);

//         // Just opening the index (memory mapping)
//         group.bench_with_input(
//             BenchmarkId::new("helix", &bench_name),
//             repo_path,
//             |b, path| {
//                 b.iter(|| black_box(Index::open(black_box(path)).unwrap()));
//             },
//         );

//         group.bench_with_input(
//             BenchmarkId::new("git", &bench_name),
//             repo_path,
//             |b, path| {
//                 b.iter(|| {
//                     let repo = git2::Repository::open(black_box(path)).unwrap();
//                     black_box(repo.index().unwrap())
//                 });
//             },
//         );
//     }

//     group.finish();
// }

// // Helper function to setup repos before benchmarking
// fn setup_repos() {
//     println!("\n========================================");
//     println!("Setting up test repositories...");
//     println!("========================================\n");

//     for config in REAL_REPOS {
//         ensure_repo(config);
//     }

//     println!("\n✓ All repositories ready!");
// }

// criterion_group! {
//     name = benches;
//     config = Criterion::default()
//         .sample_size(50)  // Reduce for large repos
//         .warm_up_time(std::time::Duration::from_secs(2))
//         .measurement_time(std::time::Duration::from_secs(10));
//     targets = bench_index_read_by_repo, bench_index_iteration, bench_index_open_only
// }

// criterion_main!(benches);

// // use criterion::{criterion_group, criterion_main, Criterion};
// // use helix::index::Index;
// // use std::hint::black_box;
// // use std::path::Path;

// // fn bench_simple(c: &mut Criterion) {
// //     // Use current directory or specify a known git repo
// //     let repo_path = Path::new(".");

// //     c.bench_function("helix_index_open", |b| {
// //         b.iter(|| Index::open(black_box(repo_path)).unwrap());
// //     });

// //     c.bench_function("git2_index_open", |b| {
// //         b.iter(|| {
// //             let repo = git2::Repository::open(black_box(repo_path)).unwrap();
// //             repo.index().unwrap()
// //         });
// //     });
// // }

// // criterion_group!(benches, bench_simple);
// // criterion_main!(benches);

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use helix::index::Index;
use std::collections::HashMap;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Real-world repository configurations
struct RepoConfig {
    name: &'static str,
    url: &'static str,
    category: &'static str,
    expected_files: usize,
}

const REAL_REPOS: &[RepoConfig] = &[
    RepoConfig {
        name: "ripgrep",
        url: "https://github.com/BurntSushi/ripgrep.git",
        category: "small",
        expected_files: 500,
    },
    RepoConfig {
        name: "fd",
        url: "https://github.com/sharkdp/fd.git",
        category: "small",
        expected_files: 300,
    },
    RepoConfig {
        name: "rust-analyzer",
        url: "https://github.com/rust-lang/rust-analyzer.git",
        category: "medium",
        expected_files: 5000,
    },
    RepoConfig {
        name: "tokio",
        url: "https://github.com/tokio-rs/tokio.git",
        category: "medium",
        expected_files: 3000,
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
                    let repo = git2::Repository::open(black_box(path)).unwrap();
                    let idx = repo.index().unwrap();
                    black_box(idx.iter().count())
                });
            },
        );
    }

    group.finish();

    // Now collect detailed stats
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

    // Print summary table
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
