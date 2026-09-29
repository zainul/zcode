//! Results files, aggregation, and the baseline comparison that decides a
//! phase gate (PRD-CTX-EFF-003 §2.3, §10.2, §11).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One (task, route, repetition) run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub task: String,
    pub route: String,
    pub family: String,
    pub category: String,
    pub rep: u32,
    pub success: bool,
    pub steps: u64,
    pub wall_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// PRD Appendix A, in base-input-token equivalents.
    pub effective_input: f64,
    /// PRD M2.
    pub raw_prompt: u64,
    /// PRD M3.
    pub discovery_tokens: u64,
    pub hit_ratio: Option<f64>,
    pub peak_context_tokens: u64,
    pub peak_pct: Option<f64>,
    pub compactions: u64,
    pub context_errors: u64,
    pub shell_search_calls: u64,
    pub cost_usd: Option<f64>,
    /// Why the run could not be measured, if it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Results {
    pub label: String,
    pub zcode_version: String,
    pub rows: Vec<Row>,
}

pub fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    })
}

/// Interquartile range by the median-of-halves method.
pub fn iqr(values: &mut [f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    let (lo, hi) = (
        &mut values[..n / 2].to_vec(),
        &mut values[n.div_ceil(2)..].to_vec(),
    );
    Some(median(hi)? - median(lo)?)
}

/// Per-category median and IQR of the headline metrics (PRD §10.2).
pub fn summarize(results: &Results) -> String {
    let mut by_cat: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for r in results.rows.iter().filter(|r| r.error.is_none()) {
        by_cat.entry(r.category.as_str()).or_default().push(r);
        by_cat.entry("(all)").or_default().push(r);
    }
    let mut out = format!(
        "{:<22} {:>5} {:>8} {:>20} {:>20}\n",
        "category", "runs", "success", "effective in (IQR)", "discovery (IQR)"
    );
    for (cat, rows) in by_cat {
        let eff: Vec<f64> = rows.iter().map(|r| r.effective_input).collect();
        let disc: Vec<f64> = rows.iter().map(|r| r.discovery_tokens as f64).collect();
        let fmt = |v: &[f64]| {
            let (mut a, mut b) = (v.to_vec(), v.to_vec());
            match (median(&mut a), iqr(&mut b)) {
                (Some(m), Some(i)) => format!("{m:.0} ({i:.0})"),
                (Some(m), None) => format!("{m:.0}"),
                _ => "n/a".into(),
            }
        };
        let success = rows.iter().filter(|r| r.success).count() as f64 / rows.len() as f64;
        out.push_str(&format!(
            "{:<22} {:>5} {:>7.0}% {:>20} {:>20}\n",
            cat,
            rows.len(),
            success * 100.0,
            fmt(&eff),
            fmt(&disc)
        ));
    }
    out
}

/// A per-(task, route) summary: the median over repetitions of each metric.
#[derive(Debug, Clone, Default)]
pub struct Cell {
    pub success_rate: f64,
    pub steps: f64,
    pub wall_ms: f64,
    pub effective_input: f64,
    pub raw_prompt: f64,
    pub discovery_tokens: f64,
    pub hit_ratio: Option<f64>,
    pub peak_pct: Option<f64>,
    pub context_errors: u64,
}

pub fn cells(results: &Results) -> BTreeMap<(String, String), Cell> {
    let mut groups: BTreeMap<(String, String), Vec<&Row>> = BTreeMap::new();
    for r in results.rows.iter().filter(|r| r.error.is_none()) {
        groups
            .entry((r.task.clone(), r.route.clone()))
            .or_default()
            .push(r);
    }
    groups
        .into_iter()
        .map(|(k, rows)| {
            let m = |f: &dyn Fn(&Row) -> f64| {
                median(&mut rows.iter().map(|r| f(r)).collect::<Vec<_>>())
            };
            let opt = |f: &dyn Fn(&Row) -> Option<f64>| {
                median(&mut rows.iter().filter_map(|r| f(r)).collect::<Vec<_>>())
            };
            let cell = Cell {
                success_rate: rows.iter().filter(|r| r.success).count() as f64 / rows.len() as f64,
                steps: m(&|r| r.steps as f64).unwrap_or(0.0),
                wall_ms: m(&|r| r.wall_ms as f64).unwrap_or(0.0),
                effective_input: m(&|r| r.effective_input).unwrap_or(0.0),
                raw_prompt: m(&|r| r.raw_prompt as f64).unwrap_or(0.0),
                discovery_tokens: m(&|r| r.discovery_tokens as f64).unwrap_or(0.0),
                hit_ratio: opt(&|r| r.hit_ratio),
                peak_pct: rows
                    .iter()
                    .filter_map(|r| r.peak_pct)
                    .fold(None, |acc: Option<f64>, v| {
                        Some(acc.map_or(v, |a| a.max(v)))
                    }),
                context_errors: rows.iter().map(|r| r.context_errors).sum(),
            };
            (k, cell)
        })
        .collect()
}

/// One line of the PRD §2.3 table.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: &'static str,
    pub label: &'static str,
    pub value: Option<f64>,
    pub target: String,
    pub pass: Option<bool>,
    /// Guardrails fail the comparison; primary metrics are reported.
    pub guardrail: bool,
}

/// Median over tasks of `new / base` for one metric, matching (task, route).
fn median_ratio(
    base: &BTreeMap<(String, String), Cell>,
    new: &BTreeMap<(String, String), Cell>,
    f: impl Fn(&Cell) -> f64,
) -> Option<f64> {
    let mut ratios: Vec<f64> = base
        .iter()
        .filter_map(|(k, b)| {
            let n = new.get(k)?;
            let (bv, nv) = (f(b), f(n));
            (bv > 0.0).then(|| nv / bv)
        })
        .collect();
    median(&mut ratios)
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let v: Vec<f64> = values.collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Compare `new` against `base`: PRD §2.3 primary metrics and guardrails.
pub fn compare(base: &Results, new: &Results) -> Vec<Check> {
    let (b, n) = (cells(base), cells(new));
    let family_of: BTreeMap<String, String> = new
        .rows
        .iter()
        .map(|r| (r.route.clone(), r.family.clone()))
        .collect();
    let hit = |cells: &BTreeMap<(String, String), Cell>, family: &str| {
        mean(
            cells
                .iter()
                .filter(|((_, route), _)| family_of.get(route).map(String::as_str) == Some(family))
                .filter_map(|(_, c)| c.hit_ratio),
        )
    };
    let reduction = |ratio: Option<f64>| ratio.map(|r| 1.0 - r);
    let mut checks = Vec::new();

    let m1 = reduction(median_ratio(&b, &n, |c| c.effective_input));
    checks.push(Check {
        id: "M1",
        label: "effective input cost per task (reduction)",
        value: m1,
        target: "≥ 60%".into(),
        pass: m1.map(|v| v >= 0.60),
        guardrail: false,
    });
    let m2 = reduction(median_ratio(&b, &n, |c| c.raw_prompt));
    checks.push(Check {
        id: "M2",
        label: "raw prompt tokens per task (reduction)",
        value: m2,
        target: "≥ 40%".into(),
        pass: m2.map(|v| v >= 0.40),
        guardrail: false,
    });
    let m3 = reduction(median_ratio(&b, &n, |c| c.discovery_tokens));
    checks.push(Check {
        id: "M3",
        label: "discovery tokens per task (reduction)",
        value: m3,
        target: "≥ 50%".into(),
        pass: m3.map(|v| v >= 0.50),
        guardrail: false,
    });
    let m4 = hit(&n, "anthropic");
    checks.push(Check {
        id: "M4",
        label: "cache hit ratio, anthropic routes",
        value: m4,
        target: "≥ 75%".into(),
        pass: m4.map(|v| v >= 0.75),
        guardrail: false,
    });
    let m5 = hit(&n, "openai");
    checks.push(Check {
        id: "M5",
        label: "cache hit ratio, openai routes",
        value: m5,
        target: "≥ 50%".into(),
        pass: m5.map(|v| v >= 0.50),
        guardrail: false,
    });
    let m6 = n.values().map(|c| c.context_errors).sum::<u64>() as f64;
    checks.push(Check {
        id: "M6",
        label: "runs hitting a context-length error",
        value: Some(m6),
        target: "0".into(),
        pass: Some(m6 == 0.0),
        guardrail: false,
    });
    let m7 = n
        .values()
        .filter_map(|c| c.peak_pct)
        .fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))));
    checks.push(Check {
        id: "M7",
        label: "worst peak context (% of window)",
        value: m7,
        target: "≤ 80%".into(),
        pass: m7.map(|v| v <= 0.80),
        guardrail: false,
    });

    let success =
        |cells: &BTreeMap<(String, String), Cell>| mean(cells.values().map(|c| c.success_rate));
    let (bs, ns) = (success(&b), success(&n));
    let gr1 = bs.zip(ns).map(|(bs, ns)| ns - bs);
    checks.push(Check {
        id: "GR1",
        label: "task success rate (change, points)",
        value: gr1,
        target: "≥ −3 pp".into(),
        pass: gr1.map(|d| d >= -0.03),
        guardrail: true,
    });
    let gr2 = median_ratio(&b, &n, |c| c.steps).map(|r| r - 1.0);
    checks.push(Check {
        id: "GR2",
        label: "steps per task (change)",
        value: gr2,
        target: "≤ +10%".into(),
        pass: gr2.map(|d| d <= 0.10),
        guardrail: true,
    });
    let gr3 = median_ratio(&b, &n, |c| c.wall_ms).map(|r| r - 1.0);
    checks.push(Check {
        id: "GR3",
        label: "wall-clock per task (change)",
        value: gr3,
        target: "≤ +10%".into(),
        pass: gr3.map(|d| d <= 0.10),
        guardrail: true,
    });
    // FR-CACHE-09: a cache regression is a failure, not a footnote.
    for family in ["anthropic", "openai"] {
        let (bh, nh) = (hit(&b, family), hit(&n, family));
        if let (Some(bh), Some(nh)) = (bh, nh) {
            checks.push(Check {
                id: "CACHE",
                label: if family == "anthropic" {
                    "cache hit ratio drop, anthropic"
                } else {
                    "cache hit ratio drop, openai"
                },
                value: Some(nh - bh),
                target: "≥ −10 pp".into(),
                pass: Some(nh - bh >= -0.10),
                guardrail: true,
            });
        }
    }
    checks
}

pub fn render_checks(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let value = match c.value {
            Some(v) if c.id == "M6" => format!("{v:.0}"),
            Some(v) => format!("{:+.1}%", v * 100.0),
            None => "n/a".into(),
        };
        let verdict = match c.pass {
            Some(true) => "PASS",
            Some(false) if c.guardrail => "FAIL",
            Some(false) => "miss",
            None => "—",
        };
        out.push_str(&format!(
            "{:<6} {:<46} {:>10}  target {:<8} {}\n",
            c.id, c.label, value, c.target, verdict
        ));
    }
    out
}

/// A comparison fails when any guardrail fails.
pub fn guardrails_hold(checks: &[Check]) -> bool {
    checks
        .iter()
        .filter(|c| c.guardrail)
        .all(|c| c.pass != Some(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(task: &str, rep: u32, success: bool, eff: f64, steps: u64, hit: f64) -> Row {
        Row {
            task: task.into(),
            route: "r".into(),
            family: "anthropic".into(),
            category: "locate-and-explain".into(),
            rep,
            success,
            steps,
            wall_ms: 1_000,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            effective_input: eff,
            raw_prompt: eff as u64,
            discovery_tokens: eff as u64,
            hit_ratio: Some(hit),
            peak_context_tokens: 0,
            peak_pct: Some(0.5),
            compactions: 0,
            context_errors: 0,
            shell_search_calls: 0,
            cost_usd: None,
            error: None,
        }
    }

    fn results(rows: Vec<Row>) -> Results {
        Results {
            label: "x".into(),
            zcode_version: "0".into(),
            rows,
        }
    }

    #[test]
    fn summary_has_a_line_per_category_and_an_overall_line() {
        let r = results(vec![
            row("a", 1, true, 100.0, 1, 0.5),
            row("b", 1, false, 300.0, 1, 0.5),
        ]);
        let text = summarize(&r);
        assert!(text.contains("locate-and-explain"));
        assert!(text.contains("(all)"));
        assert!(text.contains("50%"));
    }

    #[test]
    fn median_and_iqr() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&mut []), None);
        assert_eq!(
            iqr(&mut [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
            Some(4.0)
        );
    }

    #[test]
    fn a_halved_cost_is_a_50pct_reduction() {
        let base = results(vec![
            row("a", 1, true, 1000.0, 10, 0.8),
            row("b", 1, true, 400.0, 10, 0.8),
        ]);
        let new = results(vec![
            row("a", 1, true, 500.0, 10, 0.8),
            row("b", 1, true, 200.0, 10, 0.8),
        ]);
        let checks = compare(&base, &new);
        let m1 = checks.iter().find(|c| c.id == "M1").unwrap();
        assert!((m1.value.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(m1.pass, Some(false), "50% misses the 60% target");
        assert!(guardrails_hold(&checks));
    }

    #[test]
    fn a_success_drop_beyond_three_points_fails_the_gate() {
        let base = results(vec![
            row("a", 1, true, 100.0, 10, 0.8),
            row("b", 1, true, 100.0, 10, 0.8),
        ]);
        let new = results(vec![
            row("a", 1, true, 50.0, 10, 0.8),
            row("b", 1, false, 50.0, 10, 0.8),
        ]);
        assert!(!guardrails_hold(&compare(&base, &new)));
    }

    #[test]
    fn a_cache_hit_ratio_drop_beyond_ten_points_fails_the_gate() {
        let base = results(vec![row("a", 1, true, 100.0, 10, 0.90)]);
        let new = results(vec![row("a", 1, true, 100.0, 10, 0.70)]);
        let checks = compare(&base, &new);
        assert!(!guardrails_hold(&checks));
        assert!(checks
            .iter()
            .any(|c| c.id == "CACHE" && c.pass == Some(false)));
    }

    #[test]
    fn errored_rows_are_excluded_from_cells() {
        let mut bad = row("a", 2, false, 9e9, 99, 0.0);
        bad.error = Some("zcode not found".into());
        let r = results(vec![row("a", 1, true, 100.0, 10, 0.8), bad]);
        let c = cells(&r);
        assert_eq!(c.len(), 1);
        assert_eq!(c.values().next().unwrap().effective_input, 100.0);
    }

    #[test]
    fn rendering_marks_guardrail_failures_and_missed_targets_differently() {
        let checks = vec![
            Check {
                id: "M1",
                label: "l",
                value: Some(0.1),
                target: "t".into(),
                pass: Some(false),
                guardrail: false,
            },
            Check {
                id: "GR1",
                label: "l",
                value: Some(-0.2),
                target: "t".into(),
                pass: Some(false),
                guardrail: true,
            },
        ];
        let text = render_checks(&checks);
        assert!(text.contains("miss"));
        assert!(text.contains("FAIL"));
    }
}
