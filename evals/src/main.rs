//! `zcode-evals` — the token-efficiency evaluation harness
//! (FR-BUDGET-07, PRD-CTX-EFF-003 §10, CE-DQ23).
//!
//! ```text
//! zcode-evals self-test [--zcode PATH]                 replay a recorded session twice; hermetic
//! zcode-evals run --label L [--routes F] [--tasks a,b] [--reps N] [--zcode PATH]
//! zcode-evals compare BASE.json NEW.json               PRD §2.3 table; exit 2 if a guardrail fails
//! zcode-evals validate                                 check corpus.toml / repos.toml / routes.toml
//! ```
//!
//! The harness drives the `zcode` binary exactly as a user does and depends on
//! no zcode crate, so what it measures is the shipped product.

mod config;
mod events;
mod report;
mod runner;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use config::{Corpus, Repos, Route, Routes};
use report::Results;

fn evals_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The most recently built `zcode` in the workspace's target dir — a stale
/// release binary must never silently stand in for the code under test.
fn default_zcode() -> PathBuf {
    let root = evals_dir().join("..").join("target");
    ["release", "debug"]
        .iter()
        .map(|profile| root.join(profile).join("zcode"))
        .filter_map(|p| {
            let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((modified, p))
        })
        .max()
        .map(|(_, p)| p)
        .unwrap_or_else(|| PathBuf::from("zcode"))
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// `--zcode PATH`, made absolute: runs execute inside fresh working trees,
/// where a relative path would resolve to nothing.
fn zcode_arg(args: &[String]) -> Result<PathBuf, String> {
    match flag(args, "--zcode") {
        Some(p) => std::fs::canonicalize(&p).map_err(|e| format!("--zcode {p}: {e}")),
        None => Ok(default_zcode()),
    }
}

fn load_all() -> Result<(Corpus, Repos), String> {
    let dir = evals_dir();
    let corpus: Corpus = config::load(&dir.join("corpus.toml"))?;
    let repos: Repos = config::load(&dir.join("repos.toml"))?;
    config::validate(&corpus, &repos)?;
    Ok((corpus, repos))
}

fn write_results(path: &Path, results: &Results) -> Result<(), String> {
    let json = serde_json::to_string_pretty(results).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_results(path: &str) -> Result<Results, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
}

fn zcode_version(zcode: &Path) -> String {
    std::process::Command::new(zcode)
        .arg("version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Replays `replay/selftest` twice against the py-lib fixture and requires
/// identical token figures and a passing grade (FR-BUDGET-07 acceptance).
fn self_test(args: &[String]) -> Result<(), String> {
    let (corpus, repos) = load_all()?;
    let dir = evals_dir();
    let task = corpus
        .tasks
        .iter()
        .find(|t| t.id == "selftest")
        .ok_or("corpus has no `selftest` task")?;
    let repo = repos
        .repos
        .iter()
        .find(|r| r.name == task.repo)
        .ok_or("selftest repo missing")?;
    let route = Route {
        name: "replay".into(),
        model: "openai/replayed".into(),
        key_env: None,
        family: "openai".into(),
        judge_model: None,
    };
    let env = runner::Env {
        evals_dir: dir.clone(),
        zcode: zcode_arg(args)?,
        work_dir: dir.join(".work"),
        replay: Some(dir.join("replay").join("selftest")),
    };
    let a = runner::run_one(&env, task, repo, &route, 1, "selftest");
    let b = runner::run_one(&env, task, repo, &route, 2, "selftest");
    for r in [&a, &b] {
        if let Some(e) = &r.error {
            return Err(format!("self-test run {} failed: {e}", r.rep));
        }
        if !r.success {
            return Err(format!("self-test run {} was graded as a failure", r.rep));
        }
    }
    let figures = |r: &report::Row| {
        (
            r.steps,
            r.input_tokens,
            r.output_tokens,
            r.cache_read_tokens,
            r.discovery_tokens,
        )
    };
    if figures(&a) != figures(&b) {
        return Err(format!(
            "replayed runs differ: {:?} vs {:?}",
            figures(&a),
            figures(&b)
        ));
    }
    println!(
        "self-test OK: 2 replayed runs identical — {} steps, {} input tokens, {} discovery tokens",
        a.steps, a.input_tokens, a.discovery_tokens
    );
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    let (corpus, repos) = load_all()?;
    let dir = evals_dir();
    let label = flag(args, "--label").ok_or("run needs --label")?;
    let routes: Routes = config::load(
        &flag(args, "--routes")
            .map(PathBuf::from)
            .unwrap_or_else(|| dir.join("routes.toml")),
    )?;
    let reps: u32 = flag(args, "--reps")
        .map(|r| r.parse().map_err(|_| format!("bad --reps {r}")))
        .transpose()?
        .unwrap_or(3);
    let only: Option<Vec<String>> =
        flag(args, "--tasks").map(|t| t.split(',').map(str::to_string).collect());
    let env = runner::Env {
        evals_dir: dir.clone(),
        zcode: zcode_arg(args)?,
        work_dir: dir.join(".work"),
        replay: None,
    };
    for route in &routes.routes {
        if let Some(key) = &route.key_env {
            if std::env::var_os(key).is_none() {
                return Err(format!(
                    "route `{}` needs ${key} set for a live run",
                    route.name
                ));
            }
        }
    }
    let mut results = Results {
        label: label.clone(),
        zcode_version: zcode_version(&env.zcode),
        rows: Vec::new(),
    };
    let out = dir.join("results").join(format!("{label}.json"));
    for task in corpus.tasks.iter().filter(|t| t.id != "selftest") {
        if only.as_ref().is_some_and(|o| !o.contains(&task.id)) {
            continue;
        }
        let repo = repos
            .repos
            .iter()
            .find(|r| r.name == task.repo)
            .ok_or_else(|| format!("repo {} missing", task.repo))?;
        for route in &routes.routes {
            for rep in 1..=reps {
                eprintln!("[{label}] {} × {} #{rep}", task.id, route.name);
                let row = runner::run_one(&env, task, repo, route, rep, &label);
                if let Some(e) = &row.error {
                    eprintln!("    error: {e}");
                }
                results.rows.push(row);
                // Written after every run: a live corpus run costs money, and an
                // interruption must not throw away what was already measured.
                write_results(&out, &results)?;
            }
        }
    }
    print!("{}", report::summarize(&results));
    println!("wrote {}", out.display());
    Ok(())
}

fn compare(args: &[String]) -> Result<bool, String> {
    let (base, new) = match (args.first(), args.get(1)) {
        (Some(b), Some(n)) => (read_results(b)?, read_results(n)?),
        _ => return Err("usage: compare BASE.json NEW.json".into()),
    };
    let checks = report::compare(&base, &new);
    println!("{} → {}\n", base.label, new.label);
    print!("{}", report::render_checks(&checks));
    Ok(report::guardrails_hold(&checks))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("help", &[][..]),
    };
    let outcome = match cmd {
        "self-test" => self_test(rest).map(|_| true),
        "run" => run(rest).map(|_| true),
        "compare" => compare(rest),
        "validate" => load_all().map(|(c, _)| {
            println!("corpus OK: {} tasks", c.tasks.len());
            true
        }),
        _ => {
            eprintln!(
                "usage: zcode-evals <self-test|run|compare|validate> …\n\
                 see evals/README.md"
            );
            return ExitCode::from(64);
        }
    };
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("guardrail failed");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("zcode-evals: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed corpus is always structurally valid (runs in `make ci`).
    #[test]
    fn committed_corpus_is_valid_and_ids_are_unique() {
        let (corpus, repos) = load_all().unwrap();
        assert!(corpus.tasks.iter().any(|t| t.id == "selftest"));
        // PRD §10.1: every category is covered for every repo family.
        for repo in repos.repos.iter().filter(|r| r.name != "selftest-lib") {
            for cat in config::CATEGORIES {
                assert!(
                    corpus
                        .tasks
                        .iter()
                        .any(|t| t.repo == repo.name && t.category == *cat),
                    "repo {} lacks a {cat} task",
                    repo.name
                );
            }
        }
    }

    #[test]
    fn committed_routes_parse() {
        let routes: Routes = config::load(&evals_dir().join("routes.toml")).unwrap();
        assert!(routes.routes.iter().any(|r| r.family == "anthropic"));
        assert!(routes.routes.iter().any(|r| r.family == "openai"));
    }
}
