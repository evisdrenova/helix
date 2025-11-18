use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use helix::index::Index;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Real-world repository configurations
struct RepoConfig {
    name: &'static str,
    url: &'static str,
    category: &'static str,
    expected_files: usize,
}

const REAL_REPOS: &[RepoConfig] = &[
    // Small repos (100-1000 files)
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
    // Medium repos (1000-10000 files)
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
    // // Large repos (10000-50000 files)
    // RepoConfig {
    //     name: "rust",
    //     url: "https://github.com/rust-lang/rust.git",
    //     category: "large",
    //     expected_files: 45000,
    // },
    // RepoConfig {
    //     name: "vscode",
    //     url: "https://github.com/microsoft/vscode.git",
    //     category: "large",
    //     expected_files: 35000,
    // },
    // // Extra large repos (50000-150000 files)
    // RepoConfig {
    //     name: "tensorflow",
    //     url: "https://github.com/tensorflow/tensorflow.git",
    //     category: "xlarge",
    //     expected_files: 80000,
    // },
    // RepoConfig {
    //     name: "llvm-project",
    //     url: "https://github.com/llvm/llvm-project.git",
    //     category: "xlarge",
    //     expected_files: 120000,
    // },
];

/// Clone or update a repository
fn ensure_repo(config: &RepoConfig) -> PathBuf {
    let repo_path = PathBuf::from(format!("/tmp/helix-bench-repos/{}", config.name));

    if repo_path.exists() && is_valid_repo(&repo_path) {
        println!("✓ Using existing repo: {} at {:?}", config.name, repo_path);

        // Optionally update (commented out to speed up benchmarks)
        // update_repo(&repo_path);

        return repo_path;
    }

    // Create parent directory
    fs::create_dir_all(repo_path.parent().unwrap()).ok();

    println!(
        "Cloning {} ({}) from {}...",
        config.name, config.category, config.url
    );

    // Clone with depth=1 for faster download (except for large repos where we want full history)
    let depth_arg = if config.category == "large" {
        vec![]
    } else {
        vec!["--depth", "1"]
    };

    let mut clone_cmd = Command::new("git");
    clone_cmd.arg("clone");
    clone_cmd.args(&depth_arg);
    clone_cmd.arg(config.url);
    clone_cmd.arg(repo_path.to_str().unwrap());

    let output = clone_cmd.output().expect("Failed to clone repo");

    if !output.status.success() {
        eprintln!(
            "Failed to clone {}: {}",
            config.name,
            String::from_utf8_lossy(&output.stderr)
        );
        panic!("Clone failed");
    }

    println!("✓ Cloned {} successfully", config.name);

    // Verify file count
    let actual_files = count_files(&repo_path);
    println!(
        "  Files: {} (expected ~{})",
        actual_files, config.expected_files
    );

    repo_path
}

/// Check if a repo is valid
fn is_valid_repo(path: &Path) -> bool {
    path.join(".git").exists() && path.join(".git/index").exists()
}

/// Update an existing repo
fn update_repo(path: &Path) {
    println!("Updating repo at {:?}...", path);

    Command::new("git")
        .current_dir(path)
        .args(&["fetch", "origin"])
        .output()
        .ok();

    Command::new("git")
        .current_dir(path)
        .args(&["reset", "--hard", "origin/HEAD"])
        .output()
        .ok();
}

/// Count files in a repo
fn count_files(path: &Path) -> usize {
    let output = Command::new("git")
        .current_dir(path)
        .args(&["ls-files"])
        .output()
        .expect("Failed to count files");

    String::from_utf8_lossy(&output.stdout).lines().count()
}

fn bench_index_read_by_repo(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_read");

    // Ensure all repos are available
    let repos: Vec<_> = REAL_REPOS
        .iter()
        .map(|config| (config, ensure_repo(config)))
        .collect();

    println!("\n========================================");
    println!("Starting benchmarks...");
    println!("========================================\n");

    // Benchmark each repo
    for (config, repo_path) in &repos {
        let bench_name = format!("{}_{}", config.category, config.name);

        // Helix implementation
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

        // Git baseline
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
}

fn bench_index_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_iteration");

    let repos: Vec<_> = REAL_REPOS
        .iter()
        .map(|config| (config, ensure_repo(config)))
        .collect();

    for (config, repo_path) in &repos {
        let bench_name = format!("{}_{}", config.category, config.name);

        // Test full iteration through all entries
        group.bench_with_input(
            BenchmarkId::new("helix", &bench_name),
            repo_path,
            |b, path| {
                let idx = Index::open(path).unwrap();
                b.iter(|| {
                    for entry in idx.entries() {
                        black_box(&entry.path);
                        black_box(&entry.oid);
                        black_box(&entry.size);
                        black_box(&entry.mtime);
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("git", &bench_name),
            repo_path,
            |b, path| {
                let repo = git2::Repository::open(path).unwrap();
                let idx = repo.index().unwrap();
                b.iter(|| {
                    for entry in idx.iter() {
                        black_box(entry.path);
                        black_box(entry.id);
                        black_box(entry.file_size);
                        black_box(entry.mtime.seconds());
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_index_open_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_open");

    let repos: Vec<_> = REAL_REPOS
        .iter()
        .map(|config| (config, ensure_repo(config)))
        .collect();

    for (config, repo_path) in &repos {
        let bench_name = format!("{}_{}", config.category, config.name);

        // Just opening the index (memory mapping)
        group.bench_with_input(
            BenchmarkId::new("helix", &bench_name),
            repo_path,
            |b, path| {
                b.iter(|| black_box(Index::open(black_box(path)).unwrap()));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("git", &bench_name),
            repo_path,
            |b, path| {
                b.iter(|| {
                    let repo = git2::Repository::open(black_box(path)).unwrap();
                    black_box(repo.index().unwrap())
                });
            },
        );
    }

    group.finish();
}

// Helper function to setup repos before benchmarking
fn setup_repos() {
    println!("\n========================================");
    println!("Setting up test repositories...");
    println!("========================================\n");

    for config in REAL_REPOS {
        ensure_repo(config);
    }

    println!("\n✓ All repositories ready!");
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(50)  // Reduce for large repos
        .warm_up_time(std::time::Duration::from_secs(2))
        .measurement_time(std::time::Duration::from_secs(10));
    targets = bench_index_read_by_repo, bench_index_iteration, bench_index_open_only
}

criterion_main!(benches);

// use criterion::{criterion_group, criterion_main, Criterion};
// use helix::index::Index;
// use std::hint::black_box;
// use std::path::Path;

// fn bench_simple(c: &mut Criterion) {
//     // Use current directory or specify a known git repo
//     let repo_path = Path::new(".");

//     c.bench_function("helix_index_open", |b| {
//         b.iter(|| Index::open(black_box(repo_path)).unwrap());
//     });

//     c.bench_function("git2_index_open", |b| {
//         b.iter(|| {
//             let repo = git2::Repository::open(black_box(repo_path)).unwrap();
//             repo.index().unwrap()
//         });
//     });
// }

// criterion_group!(benches, bench_simple);
// criterion_main!(benches);
