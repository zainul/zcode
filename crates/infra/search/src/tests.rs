//! Behaviour tests against a real tree built at test time (a committed
//! fixture cannot hold a nested `.git`, and some cases — a symlink loop, a
//! 3 MB file — are better generated than stored).

use super::*;
use domain::{CaseMode, SearchPort};
use std::fs;

struct Tree {
    dir: tempfile::TempDir,
    search: RipgrepSearch,
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, content).expect("write");
}

fn tree_with(cfg: FilterConfig) -> Tree {
    let dir = tempfile::tempdir().expect("tempdir");
    let r = dir.path();
    write(r, ".gitignore", "build/\n*.log\n!keep.log\n");
    write(r, ".zcodeignore", "secret-notes/\n");
    write(r, ".env", "API_KEY=sk-live-do-not-leak\n");
    write(r, ".env.example", "API_KEY=\n");
    write(r, ".github/workflows/ci.yml", "name: ci\n");
    write(
        r,
        "node_modules/react/index.js",
        "export function useState() {} // API_KEY\n",
    );
    write(r, "node_modules/react/.git/HEAD", "ref: main\n");
    write(
        r,
        "src/main.rs",
        "fn main() {\n    let api_key = load();\n}\n",
    );
    write(r, "src/app.log", "API_KEY in a log\n");
    write(r, "src/keep.log", "kept log mentions API_KEY\n");
    write(
        r,
        "src/lib.rs",
        "pub fn load() -> String {\n    std::env::var(\"API_KEY\").unwrap()\n}\n",
    );
    write(r, "build/out.txt", "API_KEY build output\n");
    write(r, "target/debug/x.txt", "API_KEY target\n");
    write(r, "vendor/lib.go", "package lib // API_KEY\n");
    write(r, "secret-notes/a.md", "API_KEY notes\n");
    write(r, "Cargo.lock", "API_KEY lock\n");
    let search = RipgrepSearch::new(r, &cfg).expect("filter");
    Tree { dir, search }
}

fn tree() -> Tree {
    tree_with(FilterConfig::default())
}

impl Tree {
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn files(&self) -> Vec<String> {
        self.search
            .walk_files(self.root())
            .expect("walk")
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }
    fn grep(&self, pattern: &str) -> GrepOutcome {
        self.search
            .grep(&GrepQuery::new(pattern, self.root()), &|| false)
            .expect("grep")
    }
    fn grep_paths(&self, pattern: &str) -> Vec<String> {
        self.grep(pattern)
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect()
    }
}

#[test]
fn defaults_exclude_dependencies_vcs_build_output_and_lockfiles() {
    let files = tree().files();
    for hidden in [
        "node_modules/react/index.js",
        "target/debug/x.txt",
        "vendor/lib.go",
        "Cargo.lock",
    ] {
        assert!(
            !files.contains(&hidden.to_string()),
            "{hidden} leaked: {files:?}"
        );
    }
    assert!(files.contains(&"src/main.rs".to_string()));
}

#[test]
fn gitignore_applies_outside_a_git_repository_with_negation() {
    let files = tree().files();
    assert!(!files.contains(&"build/out.txt".to_string()));
    assert!(!files.contains(&"src/app.log".to_string()));
    assert!(
        files.contains(&"src/keep.log".to_string()),
        "!keep.log re-admits"
    );
}

#[test]
fn zcodeignore_is_honoured() {
    assert!(!tree().files().contains(&"secret-notes/a.md".to_string()));
}

#[test]
fn dotfiles_are_visible() {
    assert!(tree()
        .files()
        .contains(&".github/workflows/ci.yml".to_string()));
}

#[test]
fn secrets_never_surface_in_a_search_but_templates_do() {
    let t = tree();
    let paths = t.grep_paths("API_KEY");
    assert!(!paths.contains(&".env".to_string()), "{paths:?}");
    assert!(paths.contains(&".env.example".to_string()), "{paths:?}");
    // …while an explicit read of the file is still possible: explicit access
    // is not discovery.
    let explicit = t
        .search
        .grep(&GrepQuery::new("sk-live", t.root().join(".env")), &|| false)
        .expect("grep");
    assert_eq!(explicit.total_files, 1);
}

#[test]
fn config_include_readmits_and_exclude_adds() {
    let t = tree_with(FilterConfig {
        exclude: vec!["src/*.rs".into()],
        include: vec!["vendor/**".into()],
    });
    let files = t.files();
    assert!(files.contains(&"vendor/lib.go".to_string()), "{files:?}");
    assert!(!files.contains(&"src/main.rs".to_string()), "{files:?}");
}

#[test]
fn an_explicit_root_inside_an_excluded_directory_is_searched() {
    let t = tree();
    let hits = t
        .search
        .grep(
            &GrepQuery::new("useState", t.root().join("node_modules/react")),
            &|| false,
        )
        .expect("grep");
    assert_eq!(hits.total_files, 1, "{hits:?}");
    // The .git nested inside it is still pruned.
    let listed = t
        .search
        .list(&t.root().join("node_modules/react"), 2)
        .expect("list");
    assert!(
        listed.iter().all(|e| !e.path.ends_with(".git/HEAD")),
        "{listed:?}"
    );
}

#[test]
fn listing_shows_excluded_directories_collapsed() {
    let t = tree();
    let entries = t.search.list(t.root(), 1).expect("list");
    let node_modules = entries
        .iter()
        .find(|e| e.path == "node_modules")
        .expect("node_modules is listed");
    assert!(node_modules.excluded && node_modules.is_dir);
    assert!(entries.iter().any(|e| e.path == "build" && e.excluded));
    assert!(entries.iter().any(|e| e.path == "src" && !e.excluded));
    assert!(
        entries.iter().all(|e| !e.path.starts_with("node_modules/")),
        "never its contents"
    );
}

#[test]
fn listings_sort_by_path_component() {
    let t = tree();
    write(t.root(), "a/b.txt", "");
    write(t.root(), "a-c.txt", "");
    let paths: Vec<String> = t
        .search
        .list(t.root(), 2)
        .expect("list")
        .iter()
        .map(|e| e.path.clone())
        .collect();
    let a = paths.iter().position(|p| p == "a").expect("a");
    let ab = paths.iter().position(|p| p == "a/b.txt").expect("a/b");
    let ac = paths.iter().position(|p| p == "a-c.txt").expect("a-c");
    assert!(a < ab && ab < ac, "{paths:?}");
}

#[test]
fn grep_counts_and_orders_deterministically_under_parallelism() {
    let t = tree();
    for i in 0..60 {
        write(
            t.root(),
            &format!("gen/m{i:02}.rs"),
            &"fn target() {}\n".repeat(3),
        );
    }
    let first = t.grep("fn target");
    assert_eq!(first.total_files, 60);
    assert_eq!(first.total_matches, 180);
    for _ in 0..20 {
        assert_eq!(t.grep("fn target"), first);
    }
    let paths: Vec<&str> = first.files.iter().map(|f| f.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort_by(|a, b| path_order(a, b));
    assert_eq!(paths, sorted);
}

#[test]
fn grep_reports_one_based_lines_and_the_match_column() {
    let t = tree();
    let out = t
        .search
        .grep(
            &GrepQuery::new("api_key", t.root().join("src/main.rs")),
            &|| false,
        )
        .expect("grep");
    let line = &out.files[0].lines[0];
    assert_eq!(line.line, 2);
    assert_eq!(line.match_col, 8);
    assert!(!line.is_context);
}

#[test]
fn smart_case_literal_and_context() {
    let t = tree();
    // Lower-case pattern → case-insensitive under smart case.
    assert!(t.grep("api_key").total_files >= 3);
    let mut q = GrepQuery::new("api_key", t.root());
    q.case = CaseMode::Sensitive;
    let sensitive = t.search.grep(&q, &|| false).expect("grep");
    assert_eq!(
        sensitive.total_files, 1,
        "only main.rs has lower-case api_key"
    );
    let mut q = GrepQuery::new("env::var(", t.root());
    q.literal = true;
    q.context = 1;
    let out = t.search.grep(&q, &|| false).expect("grep");
    assert!(out.files[0].lines.iter().any(|l| l.is_context));
}

#[test]
fn a_bad_regex_is_a_pattern_error_naming_the_problem() {
    let t = tree();
    let err = t
        .search
        .grep(&GrepQuery::new("fn (unclosed", t.root()), &|| false)
        .expect_err("invalid regex");
    assert!(err.to_string().contains("invalid pattern"), "{err}");
}

#[test]
fn type_and_glob_filters_restrict_the_search() {
    let t = tree();
    let mut q = GrepQuery::new("API_KEY", t.root());
    q.types = vec!["rust".to_string()].into_boxed_slice();
    let paths: Vec<String> = t
        .search
        .grep(&q, &|| false)
        .expect("grep")
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect();
    assert_eq!(paths, ["src/lib.rs"]);
    let mut q = GrepQuery::new("API_KEY", t.root());
    q.globs = vec!["*.log".to_string()].into_boxed_slice();
    let out = t.search.grep(&q, &|| false).expect("grep");
    assert_eq!(out.total_files, 1, "only keep.log survives .gitignore");
    let mut q = GrepQuery::new("x", t.root());
    q.types = vec!["no-such-type".to_string()].into_boxed_slice();
    assert!(t.search.grep(&q, &|| false).is_err());
}

#[test]
fn binary_and_oversized_files_are_skipped_and_counted() {
    let t = tree();
    fs::write(t.root().join("blob.bin"), b"API_KEY\0\x01\x02 API_KEY").expect("write");
    write(
        t.root(),
        "big.txt",
        &"API_KEY filler line\n".repeat(150_000),
    );
    let mut q = GrepQuery::new("API_KEY", t.root());
    q.max_file_bytes = 2_000_000;
    let out = t.search.grep(&q, &|| false).expect("grep");
    let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
    assert!(!paths.contains(&"blob.bin"), "{paths:?}");
    assert!(!paths.contains(&"big.txt"), "{paths:?}");
    assert_eq!(out.skipped_large, 1);
}

#[test]
fn lines_kept_per_file_are_capped_but_counted() {
    let t = tree();
    write(t.root(), "many.txt", &"needle\n".repeat(500));
    let out = t
        .search
        .grep(
            &GrepQuery::new("needle", t.root().join("many.txt")),
            &|| false,
        )
        .expect("grep");
    assert_eq!(out.files[0].count, 500);
    assert_eq!(out.files[0].lines.len(), 10);
}

#[test]
fn a_minified_line_is_windowed_around_the_match() {
    let t = tree();
    let line = format!("{}NEEDLE{}", "a".repeat(50_000), "b".repeat(50_000));
    write(t.root(), "min.js.txt", &line);
    let out = t
        .search
        .grep(
            &GrepQuery::new("NEEDLE", t.root().join("min.js.txt")),
            &|| false,
        )
        .expect("grep");
    let hit = &out.files[0].lines[0];
    assert!(hit.text.len() < 1_100, "{}", hit.text.len());
    let col = hit.match_col as usize;
    assert_eq!(&hit.text[col..col + 6], "NEEDLE");
}

#[test]
fn cancellation_returns_partial_results_quickly() {
    let t = tree();
    for i in 0..400 {
        write(t.root(), &format!("bulk/{i}.txt"), "needle\n");
    }
    let started = std::time::Instant::now();
    let out = t
        .search
        .grep(&GrepQuery::new("needle", t.root()), &|| true)
        .expect("grep");
    assert!(out.partial);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn symlink_loops_do_not_hang() {
    let t = tree();
    fs::create_dir_all(t.root().join("loop")).expect("mkdir");
    std::os::unix::fs::symlink(t.root(), t.root().join("loop/back")).expect("symlink");
    let started = std::time::Instant::now();
    let _ = t.grep("API_KEY");
    let _ = t.files();
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn glob_matches_relative_to_its_root_and_bare_patterns_match_at_any_depth() {
    let t = tree();
    let q = GlobQuery {
        patterns: vec!["*.rs".to_string()].into_boxed_slice(),
        root: t.root().to_path_buf(),
        kind: EntryKind::File,
    };
    let paths: Vec<String> = t
        .search
        .glob(&q)
        .expect("glob")
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(paths, ["src/lib.rs", "src/main.rs"]);
    let q = GlobQuery {
        patterns: vec!["**/*.yml".to_string()].into_boxed_slice(),
        root: t.root().to_path_buf(),
        kind: EntryKind::File,
    };
    assert_eq!(
        t.search.glob(&q).expect("glob")[0].path,
        ".github/workflows/ci.yml"
    );
    let q = GlobQuery {
        patterns: vec!["src".to_string()].into_boxed_slice(),
        root: t.root().to_path_buf(),
        kind: EntryKind::Dir,
    };
    assert_eq!(t.search.glob(&q).expect("glob").len(), 1);
}

#[test]
fn explain_names_the_rule_and_where_it_came_from() {
    let t = tree();
    let e = t
        .search
        .explain(Path::new("node_modules/react/index.js"))
        .expect("hidden");
    assert_eq!(
        (e.rule.as_str(), e.source.as_str()),
        ("node_modules/", "built-in")
    );
    let e = t
        .search
        .explain(Path::new("build/out.txt"))
        .expect("hidden");
    assert_eq!((e.rule.as_str(), e.source.as_str()), ("build/", "built-in"));
    let e = t.search.explain(Path::new("src/app.log")).expect("hidden");
    assert_eq!(
        (e.rule.as_str(), e.source.as_str()),
        ("*.log", ".gitignore")
    );
    let e = t
        .search
        .explain(Path::new("secret-notes/a.md"))
        .expect("hidden");
    assert_eq!(e.source, ".zcodeignore");
    assert!(t.search.explain(Path::new("src/main.rs")).is_none());
    assert!(t.search.explain(Path::new("src/keep.log")).is_none());
}

#[test]
fn index_walk_skips_generated_and_minified_files() {
    let t = tree();
    write(
        t.root(),
        "src/gen.rs",
        "// @generated by build.rs\npub fn x() {}\n",
    );
    write(t.root(), "src/bundle.js", &"x".repeat(8_000));
    let indexed: Vec<String> = t
        .search
        .walk_files_for_index(t.root())
        .expect("walk")
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert!(indexed.contains(&"src/main.rs".to_string()));
    assert!(!indexed.contains(&"src/gen.rs".to_string()));
    assert!(!indexed.contains(&"src/bundle.js".to_string()));
}
