//! NFR-CTX-PERF-04: compaction tiers 1+2 on a ~150k-token transcript must
//! take ≤ 50 ms (Tier 3 is bounded by the provider call instead).

use std::time::Instant;

use app::context::{CompactDeps, ContextManager};
use app::ContextConfig;
use criterion::{criterion_group, criterion_main, Criterion};
use domain::{LlmMessage, LlmToolCall, LlmToolResult, MessageMeta, Subject};

fn transcript() -> Vec<LlmMessage> {
    let mut h = vec![LlmMessage::system("sys"), LlmMessage::user("task")];
    // 120 steps × ~1.25k tokens ≈ 150k tokens; every fourth read repeats a file.
    for s in 1..=120u32 {
        let mut a = LlmMessage::assistant("");
        a.meta.step = s;
        a.tool_calls = Box::new([LlmToolCall {
            id: format!("c{s}"),
            name: "read".into(),
            arguments: format!(r#"{{"path":"f{}.rs"}}"#, s % 30),
        }]);
        h.push(a);
        let content = "let value = compute(input, &config);\n".repeat(130);
        let mut r = LlmMessage::tool_result_message(LlmToolResult {
            tool_call_id: format!("c{s}"),
            content: content.clone(),
        });
        r.meta = MessageMeta {
            step: s,
            subject: Some(Subject::FileRange {
                path: format!("f{}.rs", s % 30),
                start: 1,
                end: 130,
                hash: 0,
            }),
            tokens_est: domain::estimate_tokens(&content) as u32,
            ..MessageMeta::default()
        };
        h.push(r);
    }
    h
}

fn bench(c: &mut Criterion) {
    let base = transcript();
    println!(
        "transcript ≈ {} tokens",
        domain::tokens::estimate_messages(&base)
    );
    let run = || {
        let mut h = base.clone();
        let mut m = ContextManager::new(ContextConfig::default());
        let mut archived = Vec::new();
        let none = |_: &str, _: &str| None;
        let mut deps = CompactDeps {
            session_id: "s",
            spill: None,
            elide_args: &none,
            archived: &mut archived,
            summarise: None,
        };
        m.force_compact(&mut h, Some(200_000), &mut deps)
            .expect("valid")
    };
    let started = Instant::now();
    let record = run();
    println!(
        "NFR-CTX-PERF-04: one compaction {:?} — {record:?} (target ≤ 50ms)",
        started.elapsed()
    );
    c.bench_function("compaction tiers 1+2 (150k tokens)", |b| b.iter(run));
}

criterion_group!(benches, bench);
criterion_main!(benches);
