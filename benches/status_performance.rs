use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

use helix::helix_index::api::HelixIndex;

fn init_test_repo(path: &Path, file_count: usize) -> anyhow::Result<()> {
    fs::create_dir_all(path.join(".git"))?;
    Command::new("git")
        .args(&["init"])
        .current_dir(path)
        .output()?;
    Command::new("git")
        .args(&["config", "user.name", "Test"])
        .current_dir(path)
        .output()?;
    Command::new("git")
        .args(&["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()?;

    // Create files with variety
    for i in 0..file_count {
        let dir = format!("dir_{}", i / 10);
        fs::create_dir_all(path.join(&dir))?;
        let filename = format!("{}/file_{:04}.txt", dir, i);
        fs::write(path.join(&filename), format!("content {}", i))?;
    }

    // Stage some files
    Command::new("git")
        .args(&["add", "."])
        .current_dir(path)
        .output()?;

    // Commit
    Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(path)
        .output()?;

    // Modify some files
    for i in 0..file_count / 10 {
        let dir = format!("dir_{}", i / 10);
        let filename = format!("{}/file_{:04}.txt", dir, i);
        fs::write(path.join(&filename), format!("modified {}", i))?;
    }

    // Stage half of them
    for i in 0..file_count / 20 {
        let dir = format!("dir_{}", i / 10);
        let filename = format!("{}/file_{:04}.txt", dir, i);
        Command::new("git")
            .args(&["add", &filename])
            .current_dir(path)
            .output()?;
    }

    Ok(())
}

fn bench_git_status_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("git_status_baseline");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let output = Command::new("git")
                    .args(&["status", "--porcelain"])
                    .current_dir(temp_dir.path())
                    .output()
                    .unwrap();
                black_box(output);
            });
        });
    }

    group.finish();
}

fn bench_helix_index_first_run(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_first_run");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter_batched(
                || {
                    let temp_dir = TempDir::new().unwrap();
                    init_test_repo(temp_dir.path(), size).unwrap();
                    temp_dir
                },
                |temp_dir| {
                    black_box(HelixIndex::load_or_rebuild(temp_dir.path()).unwrap());
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_helix_index_cached_run(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_cached_run");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        // Pre-build index
        use helix::helix_index::api::HelixIndex;
        HelixIndex::load_or_rebuild(temp_dir.path()).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                black_box(HelixIndex::load_or_rebuild(temp_dir.path()).unwrap());
            });
        });
    }

    group.finish();
}

fn bench_helix_index_after_change(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_after_change");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();
        let mut index = HelixIndex::load_or_rebuild(temp_dir.path()).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                // Simulate a change
                std::thread::sleep(std::time::Duration::from_millis(10));
                let filename = "dir_0/file_0000.txt";
                fs::write(temp_dir.path().join(filename), "changed").unwrap();
                Command::new("git")
                    .args(&["add", filename])
                    .current_dir(temp_dir.path())
                    .output()
                    .unwrap();

                black_box(index.full_refresh().unwrap());
            });
        });
    }

    group.finish();
}

fn bench_query_staged(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_staged");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();
        let index = HelixIndex::load_or_rebuild(temp_dir.path()).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                black_box(index.get_staged());
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_git_status_baseline,
    bench_helix_index_first_run,
    bench_helix_index_cached_run,
    bench_helix_index_after_change,
    bench_query_staged,
);
criterion_main!(benches);
