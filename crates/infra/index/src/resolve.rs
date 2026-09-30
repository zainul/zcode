//! Import specs → project files (FR-INDEX-07).
//!
//! Syntactic and best-effort: an import that names something outside the
//! project (`std::…`, `react`, `net/http`) resolves to nothing, and so does
//! one whose target is not indexed. A wrong edge would be worse than a
//! missing one, so every candidate must be an indexed file.

use std::collections::{HashMap, HashSet};

use crate::lang::Lang;

/// What resolution needs to know about the project.
pub(crate) struct Ctx<'a> {
    pub files: HashSet<&'a str>,
    /// Go: the module path from the root `go.mod`.
    pub go_module: Option<String>,
    /// Rust: crate name (`infra_llm`) → its `src/lib.rs`.
    pub crates: HashMap<String, &'a str>,
}

impl<'a> Ctx<'a> {
    pub fn new(paths: impl Iterator<Item = &'a str>, go_module: Option<String>) -> Self {
        let files: HashSet<&str> = paths.collect();
        let mut crates = HashMap::new();
        for p in &files {
            let Some(dir) = p.strip_suffix("src/lib.rs") else {
                continue;
            };
            let dir = dir.trim_end_matches('/');
            // `crates/infra/llm` could be `llm` or `infra_llm`; offer both.
            let segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
            if let Some(last) = segs.last() {
                crates.entry(last.replace('-', "_")).or_insert(*p);
                if segs.len() >= 2 {
                    let pair = format!("{}_{}", segs[segs.len() - 2], last).replace('-', "_");
                    crates.entry(pair).or_insert(*p);
                }
            }
        }
        Self {
            files,
            go_module,
            crates,
        }
    }

    fn has(&self, p: &str) -> bool {
        self.files.contains(p)
    }

    fn first(&self, candidates: impl IntoIterator<Item = String>) -> Option<String> {
        candidates.into_iter().find(|c| self.has(c))
    }
}

/// The module path declared by a `go.mod`.
pub fn go_module(go_mod: &str) -> Option<String> {
    go_mod
        .lines()
        .find_map(|l| l.trim().strip_prefix("module "))
        .map(|m| m.trim().trim_matches('"').to_string())
}

/// `a/b/../c` → `a/c`; `None` if it climbs above the root.
fn normalise(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// Resolve `spec`, imported by the file `from`, to an indexed path. Go
/// imports name a package — a directory — and resolve to `dir/`.
pub(crate) fn resolve(from: &str, lang: Lang, spec: &str, ctx: &Ctx<'_>) -> Option<String> {
    match lang {
        Lang::Rust => rust(from, spec, ctx),
        Lang::Go => go(spec, ctx),
        Lang::TypeScript | Lang::Tsx => typescript(from, spec, ctx),
        Lang::Python => python(from, spec, ctx),
    }
}

fn rust(from: &str, spec: &str, ctx: &Ctx<'_>) -> Option<String> {
    // `a::b::{C, D}` and `a::b::*` import from `a::b`.
    let spec = spec.split('{').next().unwrap_or(spec);
    let spec = spec.trim().trim_end_matches("::*").trim_end_matches("::");
    let mut segs: Vec<&str> = spec.split("::").map(str::trim).collect();
    if segs.first() == Some(&"") {
        segs.remove(0); // `::std::…`
    }
    let first = *segs.first()?;
    let file_dir = dir_of(from);
    let stem = from
        .rsplit('/')
        .next()
        .unwrap_or(from)
        .trim_end_matches(".rs");
    // The directory holding this module's children.
    let module_dir = if matches!(stem, "mod" | "lib" | "main") {
        file_dir.to_string()
    } else {
        join(file_dir, stem)
    };
    let (base, rest) = match first {
        "crate" => {
            let src = match from.rfind("src/") {
                Some(i) => from[..i + 3].to_string(),
                None => file_dir.to_string(),
            };
            (src, &segs[1..])
        }
        "self" => (module_dir, &segs[1..]),
        "super" => {
            let mut dir = module_dir;
            let mut i = 0;
            while segs.get(i) == Some(&"super") {
                dir = dir_of(&dir).to_string();
                i += 1;
            }
            (dir, &segs[i..])
        }
        name => {
            let lib = ctx.crates.get(name)?;
            if segs.len() == 1 {
                return Some((*lib).to_string());
            }
            (dir_of(lib).to_string(), &segs[1..])
        }
    };
    // Longest prefix of the path that is a module file: `a::b::Item` is
    // usually `a/b.rs` holding `Item`, but may be `a/b/Item.rs`.
    for k in (1..=rest.len()).rev() {
        let stem = join(&base, &rest[..k].join("/"));
        if let Some(hit) = ctx.first([format!("{stem}.rs"), format!("{stem}/mod.rs")]) {
            return Some(hit);
        }
    }
    // `crate::Item` — the crate root itself.
    ctx.first([
        join(&base, "lib.rs"),
        join(&base, "main.rs"),
        join(&base, "mod.rs"),
    ])
}

fn go(spec: &str, ctx: &Ctx<'_>) -> Option<String> {
    let module = ctx.go_module.as_deref()?;
    let rel = if spec == module {
        ""
    } else {
        spec.strip_prefix(module)?.strip_prefix('/')?
    };
    let dir = if rel.is_empty() {
        String::new()
    } else {
        format!("{rel}/")
    };
    // A package exists if some indexed Go file sits directly in it.
    ctx.files
        .iter()
        .any(|f| {
            f.ends_with(".go")
                && f.strip_prefix(dir.as_str())
                    .is_some_and(|rest| !rest.contains('/'))
        })
        .then_some(dir)
}

const TS_SUFFIXES: [&str; 10] = [
    "",
    ".ts",
    ".tsx",
    ".d.ts",
    ".js",
    ".jsx",
    ".mjs",
    "/index.ts",
    "/index.tsx",
    "/index.js",
];

fn typescript(from: &str, spec: &str, ctx: &Ctx<'_>) -> Option<String> {
    let bases: Vec<String> = if spec.starts_with("./") || spec.starts_with("../") {
        vec![normalise(&join(dir_of(from), spec))?]
    } else if let Some(rest) = spec.strip_prefix("@/").or_else(|| spec.strip_prefix("~/")) {
        // The conventional `@/` alias for the source root.
        vec![format!("src/{rest}"), rest.to_string()]
    } else {
        return None; // a package
    };
    // `./x.js` in TypeScript source means `./x.ts`.
    let bases = bases.into_iter().flat_map(|b| {
        let stripped = b
            .strip_suffix(".js")
            .or_else(|| b.strip_suffix(".mjs"))
            .map(str::to_string);
        std::iter::once(b).chain(stripped)
    });
    let candidates: Vec<String> = bases
        .flat_map(|b| TS_SUFFIXES.iter().map(move |s| format!("{b}{s}")))
        .collect();
    ctx.first(candidates)
}

fn python(from: &str, spec: &str, ctx: &Ctx<'_>) -> Option<String> {
    let dots = spec.chars().take_while(|c| *c == '.').count();
    let module = &spec[dots..];
    let tail = module.replace('.', "/");
    let bases: Vec<String> = if dots > 0 {
        let mut dir = dir_of(from).to_string();
        for _ in 1..dots {
            dir = dir_of(&dir).to_string();
        }
        vec![dir]
    } else {
        vec![String::new(), "src".to_string()]
    };
    let candidates: Vec<String> = bases
        .iter()
        .flat_map(|b| {
            if tail.is_empty() {
                vec![join(b, "__init__.py")]
            } else {
                let stem = join(b, &tail);
                vec![
                    format!("{stem}.py"),
                    format!("{stem}.pyi"),
                    format!("{stem}/__init__.py"),
                ]
            }
        })
        .collect();
    ctx.first(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: &[&str] = &[
        "crates/app/src/lib.rs",
        "crates/app/src/context.rs",
        "crates/app/src/engine/mod.rs",
        "crates/app/src/engine/step.rs",
        "crates/infra/llm/src/lib.rs",
        "crates/infra/llm/src/record.rs",
        "cmd/server/main.go",
        "internal/store/store.go",
        "web/src/lib/api.ts",
        "web/src/components/Button.tsx",
        "web/src/components/index.ts",
        "src/util/format.ts",
        "pkg/__init__.py",
        "pkg/models.py",
        "pkg/sub/helpers.py",
    ];

    fn ctx() -> Ctx<'static> {
        Ctx::new(FILES.iter().copied(), Some("example.com/svc".into()))
    }

    #[test]
    fn rust_paths_resolve_through_crate_self_super_and_sibling_crates() {
        let c = ctx();
        let r = |from, spec| resolve(from, Lang::Rust, spec, &c);
        assert_eq!(
            r("crates/app/src/lib.rs", "crate::context::ContextManager").as_deref(),
            Some("crates/app/src/context.rs")
        );
        assert_eq!(
            r("crates/app/src/engine/mod.rs", "self::step::{a, b}").as_deref(),
            Some("crates/app/src/engine/step.rs")
        );
        assert_eq!(
            r("crates/app/src/engine/step.rs", "super::super::context::X").as_deref(),
            Some("crates/app/src/context.rs")
        );
        assert_eq!(
            r("crates/app/src/lib.rs", "infra_llm::record::ReplayLlm").as_deref(),
            Some("crates/infra/llm/src/record.rs")
        );
        assert_eq!(
            r("crates/app/src/lib.rs", "std::collections::HashMap"),
            None
        );
    }

    #[test]
    fn go_imports_resolve_to_package_directories_inside_the_module() {
        let c = ctx();
        assert_eq!(
            resolve(
                "cmd/server/main.go",
                Lang::Go,
                "example.com/svc/internal/store",
                &c
            )
            .as_deref(),
            Some("internal/store/")
        );
        assert_eq!(
            resolve("cmd/server/main.go", Lang::Go, "net/http", &c),
            None
        );
        assert_eq!(
            go_module("module example.com/svc\n\ngo 1.22\n").as_deref(),
            Some("example.com/svc")
        );
    }

    #[test]
    fn typescript_resolves_relative_alias_and_index_files() {
        let c = ctx();
        let r = |from, spec| resolve(from, Lang::TypeScript, spec, &c);
        assert_eq!(
            r("web/src/components/Button.tsx", "../lib/api").as_deref(),
            Some("web/src/lib/api.ts")
        );
        assert_eq!(
            r("web/src/lib/api.ts", "../components").as_deref(),
            Some("web/src/components/index.ts")
        );
        assert_eq!(
            r("web/src/lib/api.ts", "./api.js").as_deref(),
            Some("web/src/lib/api.ts")
        );
        assert_eq!(
            r("x.ts", "@/util/format").as_deref(),
            Some("src/util/format.ts")
        );
        assert_eq!(r("x.ts", "react"), None);
        assert_eq!(r("x.ts", "../../outside"), None);
    }

    #[test]
    fn python_resolves_absolute_and_relative_modules() {
        let c = ctx();
        let r = |from, spec| resolve(from, Lang::Python, spec, &c);
        assert_eq!(r("main.py", "pkg.models").as_deref(), Some("pkg/models.py"));
        assert_eq!(r("main.py", "pkg").as_deref(), Some("pkg/__init__.py"));
        assert_eq!(
            r("pkg/sub/helpers.py", "..models").as_deref(),
            Some("pkg/models.py")
        );
        assert_eq!(
            r("pkg/models.py", ".sub.helpers").as_deref(),
            Some("pkg/sub/helpers.py")
        );
        assert_eq!(r("main.py", "os"), None);
    }
}
