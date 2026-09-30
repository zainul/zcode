//! Corpus, fixture-repo and route definitions (PRD-CTX-EFF-003 §10.1).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// PRD §10.1 task categories.
pub const CATEGORIES: &[&str] = &[
    "locate-and-explain",
    "single-file-fix",
    "multi-file-refactor",
    "rename",
    "add-test",
    "long-horizon",
];

#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    #[serde(rename = "task", default)]
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub repo: String,
    pub category: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub prompt: String,
    pub grader: Grader,
    #[serde(default = "default_max_turns")]
    pub max_turns: u64,
    /// Wall-clock cap for one run, seconds.
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

fn default_mode() -> String {
    "auto".into()
}
fn default_max_turns() -> u64 {
    40
}
fn default_timeout() -> u64 {
    900
}

/// How a run is judged.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Grader {
    /// `graders/<script>` run in the post-run working tree; exit 0 = pass.
    Command { script: String },
    /// Every criterion must be judged satisfied by the judge model.
    Rubric { criteria: Vec<String> },
    /// The final answer must contain every string (case-insensitive).
    /// Deterministic — used by the replay self-test.
    Contains { all: Vec<String> },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Repos {
    #[serde(rename = "repo", default)]
    pub repos: Vec<Repo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepoSource {
    /// `git archive <rev>` of the git repository at `path` (relative to the
    /// evals directory) — pinned by the revision, no network needed.
    GitArchive { path: String, rev: String },
    /// A directory committed under `evals/`, copied fresh for every run.
    Fixture { path: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Repo {
    pub name: String,
    #[serde(flatten)]
    pub source: RepoSource,
    /// Script (relative to the evals directory) run in the fresh working tree
    /// before zcode starts, e.g. to generate a large `node_modules`.
    #[serde(default)]
    pub setup: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Routes {
    #[serde(rename = "route", default)]
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    pub name: String,
    /// `<provider>/<model>`, passed to `zcode run --model`.
    pub model: String,
    /// Env var that must hold the provider key for a live run.
    #[serde(default)]
    pub key_env: Option<String>,
    /// `anthropic` or `openai`: which cache target (PRD M4/M5) applies, and
    /// whether cached tokens are counted inside `input_tokens`.
    pub family: String,
    /// Model for rubric grading; defaults to the route's own model.
    #[serde(default)]
    pub judge_model: Option<String>,
}

impl Route {
    pub fn cache_within_input(&self) -> bool {
        self.family != "anthropic"
    }
}

pub fn load<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Structural checks a corpus must pass before anything runs.
pub fn validate(corpus: &Corpus, repos: &Repos) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    let repo_names: BTreeSet<&str> = repos.repos.iter().map(|r| r.name.as_str()).collect();
    for t in &corpus.tasks {
        if !ids.insert(t.id.as_str()) {
            return Err(format!("duplicate task id `{}`", t.id));
        }
        if !repo_names.contains(t.repo.as_str()) {
            return Err(format!("task `{}` names unknown repo `{}`", t.id, t.repo));
        }
        if !CATEGORIES.contains(&t.category.as_str()) {
            return Err(format!(
                "task `{}` has unknown category `{}`",
                t.id, t.category
            ));
        }
        if !["planning", "editing", "auto"].contains(&t.mode.as_str()) {
            return Err(format!("task `{}` has unknown mode `{}`", t.id, t.mode));
        }
        match &t.grader {
            Grader::Rubric { criteria } if criteria.is_empty() => {
                return Err(format!("task `{}` has an empty rubric", t.id))
            }
            Grader::Contains { all } if all.is_empty() => {
                return Err(format!("task `{}` has an empty contains-grader", t.id))
            }
            _ => {}
        }
    }
    Ok(())
}
