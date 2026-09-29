//! NFR-CTX-PERF-01: `grep` on ripgrep's engine versus the `rg` binary on the
//! same tree. Run with `cargo bench -p zcode-benches --bench search`; set
//! `ZCODE_BENCH_FILES` to size the tree (default 20 000 files — the PRD's
//! 100 k-file figure is `ZCODE_BENCH_FILES=100000`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};
use domain::{GrepQuery, SearchPort};
use infra_search::{FilterConfig, RipgrepSearch};

fn fixture() -> PathBuf {
    let files: usize = std::env::var("ZCODE_BENCH_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/bench-fixtures")
        .join(format!("tree-{files}"));
    if !root.join(".complete").exists() {
        for i in 0..files {
            let dir = root.join(format!("d{:03}", i % 500));
            std::fs::create_dir_all(&dir).expect("mkdir");
            let body = if i % 97 == 0 {
                "fn needle_function() {}\nlet x = 1;\n"
            } else {
                "fn ordinary() {}\nlet y = 2;\nlet z = y + 3;\n"
            };
            std::fs::write(dir.join(format!("f{i}.rs")), body).expect("write");
        }
        std::fs::write(root.join(".complete"), "").expect("marker");
    }
    root
}

fn time<F: FnMut()>(mut f: F) -> Duration {
    // Warm the page cache first; measure the best of five.
    f();
    (0..5)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap_or_default()
}

fn bench(c: &mut Criterion) {
    let root = fixture();
    let search = RipgrepSearch::new(&root, &FilterConfig::default()).expect("filter");
    let q = GrepQuery::new("needle_function", root.clone());
    let mut group = c.benchmark_group("grep");
    group.sample_size(10);
    group.bench_function("zcode grep (files mode)", |b| {
        b.iter(|| search.grep(&q, &|| false).expect("grep"))
    });
    group.finish();

    let ours = time(|| {
        search.grep(&q, &|| false).expect("grep");
    });
    match Command::new("rg").arg("--version").output() {
        Ok(_) => {
            let theirs = time(|| {
                let _ = Command::new("rg")
                    .args(["-l", "needle_function"])
                    .arg(&root)
                    .output();
            });
            let ratio = ours.as_secs_f64() / theirs.as_secs_f64().max(1e-9);
            println!(
                "NFR-CTX-PERF-01: zcode {ours:?} vs rg {theirs:?} — ratio {ratio:.2} (target ≤ 1.5)"
            );
        }
        Err(_) => println!("rg not on PATH; NFR-CTX-PERF-01 ratio not measured (zcode: {ours:?})"),
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
