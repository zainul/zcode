use criterion::{
    black_box, criterion_group, criterion_main, measurement::WallTime, BenchmarkGroup, Criterion,
};
use domain::{DomainError, FileEdit, Task, TaskStatus};
use std::path::PathBuf;

fn construct_task(c: &mut BenchmarkGroup<'_, WallTime>) {
    c.bench_function("Task::construct", |b| {
        b.iter(|| {
            black_box(Task {
                id: "t-001".into(),
                description: "Fix the file edit bug".into(),
                status: TaskStatus::Pending,
                constraints: Box::new(["fast".to_string(), "safe".to_string()]),
            })
        })
    });
}

fn construct_file_edit(c: &mut BenchmarkGroup<'_, WallTime>) {
    c.bench_function("FileEdit::construct", |b| {
        b.iter(|| {
            black_box(FileEdit {
                path: PathBuf::from("/tmp/example.rs"),
                old_content: "old".repeat(256),
                new_content: "new".repeat(256),
            })
        })
    });
}

fn domain_error_roundtrip(c: &mut BenchmarkGroup<'_, WallTime>) {
    c.bench_function("DomainError::format", |b| {
        b.iter(|| {
            let err = DomainError::NotFound("entity not found".into());
            black_box(format!("{err}"))
        })
    });
}

fn criterion_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("smoke");
    construct_task(&mut g);
    construct_file_edit(&mut g);
    domain_error_roundtrip(&mut g);
    g.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
