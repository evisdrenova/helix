use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use helix::helix_index::{api::HelixIndex, sync::SyncEngine, verify::Verifier, Reader, Writer};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

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

    // Create files
    for i in 0..file_count {
        let filename = format!("file_{:04}.txt", i);
        fs::write(path.join(&filename), format!("content {}", i))?;
    }

    // Stage all files
    Command::new("git")
        .args(&["add", "."])
        .current_dir(path)
        .output()?;

    Ok(())
}

fn bench_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_open");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        // Pre-build index
        let SyncEngine = SyncEngine::new(temp_dir.path());
        SyncEngine.sync().unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let reader = Reader::new(temp_dir.path());
                black_box(reader.read().unwrap());
            });
        });
    }

    group.finish();
}

fn bench_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_verify");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        // Pre-build index
        let SyncEngine = SyncEngine::new(temp_dir.path());
        SyncEngine.sync().unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let verifier = Verifier::new(temp_dir.path());
                black_box(verifier.verify().unwrap());
            });
        });
    }

    group.finish();
}

fn bench_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_sync");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let SyncEngine = SyncEngine::new(temp_dir.path());
                black_box(SyncEngine.sync().unwrap());
            });
        });
    }

    group.finish();
}

fn bench_sync_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_sync_incremental");

    for change_count in [1, 10, 50].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), 1000).unwrap();

        // Initial sync
        let SyncEngine = SyncEngine::new(temp_dir.path());
        SyncEngine.sync().unwrap();

        group.throughput(Throughput::Elements(*change_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(change_count),
            change_count,
            |b, &changes| {
                b.iter(|| {
                    // Modify N files
                    for i in 0..changes {
                        let filename = format!("file_{:04}.txt", i);
                        fs::write(temp_dir.path().join(&filename), "modified").unwrap();
                    }

                    Command::new("git")
                        .args(&["add", "."])
                        .current_dir(temp_dir.path())
                        .output()
                        .unwrap();

                    black_box(SyncEngine.sync().unwrap());
                });
            },
        );
    }

    group.finish();
}

fn bench_get_staged(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_get_staged");

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

fn bench_load_or_rebuild_cached(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_load_cached");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        // Pre-build index
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

fn bench_load_or_rebuild_stale(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_load_stale");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        // Pre-build index
        HelixIndex::load_or_rebuild(temp_dir.path()).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                // Make index stale
                std::thread::sleep(std::time::Duration::from_millis(10));
                fs::write(temp_dir.path().join("new_file.txt"), "new").unwrap();
                Command::new("git")
                    .args(&["add", "new_file.txt"])
                    .current_dir(temp_dir.path())
                    .output()
                    .unwrap();

                black_box(HelixIndex::load_or_rebuild(temp_dir.path()).unwrap());
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_open,
    bench_verify,
    bench_sync,
    bench_sync_incremental,
    bench_get_staged,
    bench_load_or_rebuild_cached,
    bench_load_or_rebuild_stale,
);
criterion_main!(benches);
