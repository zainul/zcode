//! The repo map (FR-INDEX-08, CE-DQ18): files ranked by a personalised
//! PageRank over "A mentions a name B defines", rendered as signatures
//! within a token budget.

use std::collections::HashMap;

use crate::snapshot::Snapshot;

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 20;
/// Top-level definitions shown per file.
const DEFS_PER_FILE: usize = 8;
/// Names too common to say anything about who depends on whom.
const STOPLIST: &[&str] = &[
    "new", "get", "set", "len", "err", "ctx", "self", "this", "init", "main",
];

/// Prompt words that would match half the paths in any repository.
const PROMPT_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "add", "fix", "make", "use",
    "when", "what", "where", "which", "should", "can", "not", "all", "any", "are", "was",
];

pub const HEADER: &str = "Files ranked by relevance; signatures only. Use outline/read for detail.";

fn words(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = prompt
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !PROMPT_STOPWORDS.contains(&w.as_str()))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Files by relevance to `prompt`, most relevant first: `(slot, rank)`.
/// Deterministic — ties go to the path order.
pub(crate) fn rank(snap: &Snapshot, prompt: &str) -> Vec<(u32, f64)> {
    let files: Vec<u32> = snap
        .files
        .iter()
        .enumerate()
        .filter(|(_, r)| r.live)
        .map(|(i, _)| i as u32)
        .collect();
    let n = files.len();
    if n == 0 {
        return Vec::new();
    }
    let pos: HashMap<u32, usize> = files.iter().enumerate().map(|(i, s)| (*s, i)).collect();

    // Who defines each name, and how many definitions carry it: a name
    // defined in forty places is weak evidence of a dependency on any one.
    let mut definers: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut defs_named: HashMap<u32, u32> = HashMap::new();
    for (i, slot) in files.iter().enumerate() {
        for d in snap.files[*slot as usize].defs.iter() {
            *defs_named.entry(d.name).or_default() += 1;
            let v = definers.entry(d.name).or_default();
            if v.last() != Some(&i) {
                v.push(i);
            }
        }
    }

    let mut edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for (a, slot) in files.iter().enumerate() {
        let mut names: Vec<u32> = snap.files[*slot as usize]
            .idents
            .iter()
            .map(|(name, _)| *name)
            .collect();
        names.sort_unstable();
        names.dedup();
        let mut out: HashMap<usize, f64> = HashMap::new();
        for name in names {
            let Some(targets) = definers.get(&name) else {
                continue;
            };
            let text = snap.strings.get(name);
            if text.chars().count() < 3 || STOPLIST.contains(&text) {
                continue;
            }
            let w = 1.0 / (2.0 + f64::from(defs_named[&name])).ln();
            for b in targets {
                if *b != a {
                    *out.entry(*b).or_default() += w;
                }
            }
        }
        let mut out: Vec<(usize, f64)> = out.into_iter().collect();
        out.sort_by_key(|(b, _)| *b);
        edges[a] = out;
    }

    // Personalisation: the prompt's words pull the walk towards files whose
    // path or definitions they name.
    let words = words(prompt);
    let mut teleport: Vec<f64> = files
        .iter()
        .map(|slot| {
            let rec = &snap.files[*slot as usize];
            let path = snap.path(rec).to_lowercase();
            // Per word, so a path matching two of them ("checkout page")
            // outranks one matching only the generic one ("page").
            let mut t =
                1.0 + 10.0 * words.iter().filter(|w| path.contains(w.as_str())).count() as f64;
            for d in rec.defs.iter() {
                let name = snap.strings.get(d.name).to_lowercase();
                if words.binary_search(&name).is_ok() {
                    t += 5.0;
                }
            }
            t
        })
        .collect();
    let total: f64 = teleport.iter().sum();
    teleport.iter_mut().for_each(|t| *t /= total);

    let out_weight: Vec<f64> = edges
        .iter()
        .map(|e| e.iter().map(|(_, w)| w).sum())
        .collect();
    let mut rank = teleport.clone();
    for _ in 0..ITERATIONS {
        let mut next: Vec<f64> = teleport.iter().map(|t| (1.0 - DAMPING) * t).collect();
        let mut dangling = 0.0;
        for (a, e) in edges.iter().enumerate() {
            if out_weight[a] == 0.0 {
                dangling += rank[a];
                continue;
            }
            for (b, w) in e {
                next[*b] += DAMPING * rank[a] * w / out_weight[a];
            }
        }
        for (i, t) in teleport.iter().enumerate() {
            next[i] += DAMPING * dangling * t;
        }
        rank = next;
    }

    let mut ranked: Vec<(u32, f64)> = files.iter().map(|s| (*s, rank[pos[s]])).collect();
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1).then_with(|| {
            infra_search::path_order(
                snap.path(&snap.files[a.0 as usize]),
                snap.path(&snap.files[b.0 as usize]),
            )
        })
    });
    ranked
}

/// Render ranked files as `path` + indented top-level signatures, stopping
/// before the estimate passes `budget_tokens`. `signatures` reads a file's
/// signature lines back (they are not stored).
pub(crate) fn render(
    snap: &Snapshot,
    ranked: &[(u32, f64)],
    budget_tokens: u32,
    partial: bool,
    signatures: &dyn Fn(&str, &[u32]) -> Vec<String>,
) -> String {
    let mut out = String::from(HEADER);
    if partial {
        out.push_str(" (partial: the index was still building)");
    }
    out.push('\n');
    let budget = u64::from(budget_tokens);
    let mut files = 0;
    for (slot, _) in ranked {
        let rec = &snap.files[*slot as usize];
        let path = snap.path(rec);
        let top: Vec<u32> = rec
            .defs
            .iter()
            .filter(|d| d.depth == 0)
            .map(|d| d.name_line)
            .collect();
        let mut block = format!("{path}\n");
        let shown = top.len().min(DEFS_PER_FILE);
        for sig in signatures(path, &top[..shown]) {
            block.push_str("  ");
            block.push_str(&sig);
            block.push('\n');
        }
        if top.len() > shown {
            block.push_str(&format!("  (+{} more)\n", top.len() - shown));
        }
        // The estimate is not additive (code and prose divide differently),
        // so measure the map as it would be sent.
        let candidate = format!("{out}{block}");
        if domain::tokens::estimate_tokens(candidate.trim_end()) > budget {
            break;
        }
        out = candidate;
        files += 1;
    }
    if files == 0 {
        return String::new();
    }
    out.truncate(out.trim_end().len());
    out
}
