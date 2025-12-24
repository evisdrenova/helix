use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use helix_cli::add::{add, AddOptions};
use helix_cli::commit::{commit, CommitOptions};
use helix_cli::helix_index::api::HelixIndexData;
use helix_cli::helix_index::tree::TreeBuilder;
use helix_cli::helix_index::Reader;
use helix_protocol::storage::FsObjectStore;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn init_test_repo(path: &Path, file_count: usize) -> anyhow::Result<()> {
    // Initialize pure Helix repo
    helix_cli::init::init_helix_repo(path, None)?;

    // Set up author config
    let config_path = path.join(".helix/config.toml");
    fs::write(
        &config_path,
        r#"
[user]
name = "Bench User"
email = "bench@test.com"
"#,
    )?;

    // Create files
    for i in 0..file_count {
        let filename = format!("file_{:04}.txt", i);
        fs::write(path.join(&filename), format!("content {}", i))?;
    }

    // Stage all files using helix add
    let paths: Vec<PathBuf> = (0..file_count)
        .map(|i| PathBuf::from(format!("file_{:04}.txt", i)))
        .collect();

    add(path, &paths, AddOptions::default())?;

    Ok(())
}

fn bench_index_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_read");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

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

fn bench_index_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_load");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                black_box(HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap());
            });
        });
    }

    group.finish();
}

fn bench_add_command(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_add_command");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &file_count| {
            b.iter_batched(
                || {
                    // Setup: create fresh repo for each iteration
                    let temp_dir = TempDir::new().unwrap();
                    helix_cli::init::init_helix_repo(temp_dir.path(), None).unwrap();

                    let config_path = temp_dir.path().join(".helix/config.toml");
                    fs::write(
                        &config_path,
                        r#"
[user]
name = "Bench User"
email = "bench@test.com"
"#,
                    )
                    .unwrap();

                    // Create files
                    for i in 0..file_count {
                        let filename = format!("file_{:04}.txt", i);
                        fs::write(temp_dir.path().join(&filename), format!("content {}", i))
                            .unwrap();
                    }

                    temp_dir
                },
                |temp_dir| {
                    // Benchmark: add all files
                    add(
                        temp_dir.path(),
                        &[PathBuf::from(".")],
                        AddOptions::default(),
                    )
                    .unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_blob_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_blob_write");

    for size in [1, 10, 100].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &count| {
            b.iter_batched(
                || {
                    let temp_dir = TempDir::new().unwrap();
                    helix_cli::init::init_helix_repo(temp_dir.path(), None).unwrap();
                    let storage = FsObjectStore::new(temp_dir.path());
                    let contents: Vec<Vec<u8>> = (0..count)
                        .map(|i| format!("content {}", i).into_bytes())
                        .collect();
                    (temp_dir, storage, contents)
                },
                |(temp_dir, storage, contents)| {
                    for content in &contents {
                        black_box(
                            storage
                                .write_object(&helix_protocol::message::ObjectType::Blob, content)
                                .unwrap(),
                        );
                    }
                    drop(temp_dir);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_tree_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_tree_build");

    for size in [10, 100, 1000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        let index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let builder = TreeBuilder::new(temp_dir.path());
                black_box(builder.build_from_entries(&index.entries()).unwrap());
            });
        });
    }

    group.finish();
}

fn bench_commit_command(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_commit_command");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &file_count| {
            b.iter_batched(
                || {
                    // Setup: create repo with staged files
                    let temp_dir = TempDir::new().unwrap();
                    init_test_repo(temp_dir.path(), file_count).unwrap();
                    temp_dir
                },
                |temp_dir| {
                    // Benchmark: commit
                    commit(
                        temp_dir.path(),
                        CommitOptions {
                            message: "Benchmark commit".to_string(),
                            author: Some("Bench User <bench@test.com>".to_string()),
                            allow_empty: false,
                            amend: false,
                            verbose: false,
                        },
                    )
                    .unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_get_staged(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_get_staged");

    for size in [10, 100, 1000, 10000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        let index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                black_box(index.get_staged());
            });
        });
    }

    group.finish();
}

fn bench_get_tracked(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_get_tracked");

    for size in [10, 100, 1000, 10000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        let index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                black_box(index.get_tracked());
            });
        });
    }

    group.finish();
}

fn bench_stage_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_stage_file");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &file_count| {
            b.iter_batched(
                || {
                    let temp_dir = TempDir::new().unwrap();
                    init_test_repo(temp_dir.path(), file_count).unwrap();

                    // Unstage all files first
                    let mut index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();
                    index.unstage_all().unwrap();
                    index.persist().unwrap();

                    (temp_dir, file_count)
                },
                |(temp_dir, count)| {
                    let mut index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();

                    // Stage all files one by one
                    for i in 0..count {
                        let filename = PathBuf::from(format!("file_{:04}.txt", i));
                        index.stage_file(&filename).unwrap();
                    }

                    index.persist().unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_stage_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_stage_all");

    for size in [10, 100, 1000, 10000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        // Unstage all first
        let mut index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();
        index.unstage_all().unwrap();
        index.persist().unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let mut index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();
                index.stage_all().unwrap();
                black_box(index.persist().unwrap());
            });
        });
    }

    group.finish();
}

fn bench_persist(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_index_persist");

    for size in [10, 100, 1000, 10000].iter() {
        let temp_dir = TempDir::new().unwrap();
        init_test_repo(temp_dir.path(), *size).unwrap();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let mut index = HelixIndexData::load_or_rebuild(temp_dir.path()).unwrap();

                // Make a small change
                index.stage_file(Path::new("file_0000.txt")).unwrap();

                black_box(index.persist().unwrap());
            });
        });
    }

    group.finish();
}

fn bench_incremental_add(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_add_incremental");

    for change_count in [1, 10, 50].iter() {
        group.throughput(Throughput::Elements(*change_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(change_count),
            change_count,
            |b, &changes| {
                b.iter_batched(
                    || {
                        let temp_dir = TempDir::new().unwrap();
                        init_test_repo(temp_dir.path(), 1000).unwrap();
                        temp_dir
                    },
                    |temp_dir| {
                        // Modify N files
                        let paths: Vec<PathBuf> = (0..changes)
                            .map(|i| {
                                let filename = format!("file_{:04}.txt", i);
                                fs::write(temp_dir.path().join(&filename), "modified").unwrap();
                                PathBuf::from(filename)
                            })
                            .collect();

                        add(temp_dir.path(), &paths, AddOptions::default()).unwrap();
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_full_workflow(c: &mut Criterion) {
    let mut group = c.benchmark_group("helix_full_workflow");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &file_count| {
            b.iter_batched(
                || {
                    let temp_dir = TempDir::new().unwrap();
                    helix_cli::init::init_helix_repo(temp_dir.path(), None).unwrap();

                    let config_path = temp_dir.path().join(".helix/config.toml");
                    fs::write(
                        &config_path,
                        r#"
[user]
name = "Bench User"
email = "bench@test.com"
"#,
                    )
                    .unwrap();

                    // Create files
                    for i in 0..file_count {
                        let filename = format!("file_{:04}.txt", i);
                        fs::write(temp_dir.path().join(&filename), format!("content {}", i))
                            .unwrap();
                    }

                    temp_dir
                },
                |temp_dir| {
                    // Full workflow: add → commit
                    add(
                        temp_dir.path(),
                        &[PathBuf::from(".")],
                        AddOptions::default(),
                    )
                    .unwrap();

                    commit(
                        temp_dir.path(),
                        CommitOptions {
                            message: "Benchmark commit".to_string(),
                            author: Some("Bench User <bench@test.com>".to_string()),
                            allow_empty: false,
                            amend: false,
                            verbose: false,
                        },
                    )
                    .unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_index_read,
    bench_index_load,
    bench_add_command,
    bench_blob_write,
    bench_tree_build,
    bench_commit_command,
    bench_get_staged,
    bench_get_tracked,
    bench_stage_file,
    bench_stage_all,
    bench_persist,
    bench_incremental_add,
    bench_full_workflow,
);
criterion_main!(benches);
