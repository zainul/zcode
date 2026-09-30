//! Languages with a grammar compiled in (CE-DQ16) and their queries (CE-DQ15).

use std::sync::OnceLock;

use tree_sitter::{Language, Query};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Go,
    TypeScript,
    Tsx,
    Python,
}

/// The three queries every language provides.
pub struct Queries {
    pub defs: Query,
    pub imports: Query,
    pub idents: Query,
}

impl Lang {
    /// The language of `path`, by extension — `None` for anything without a
    /// grammar in this build. `.js`/`.jsx` go to the TypeScript grammars,
    /// which parse modern JavaScript well enough for definitions.
    pub fn for_path(path: &str) -> Option<Lang> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        let lang = match ext.as_str() {
            "rs" => Lang::Rust,
            "go" => Lang::Go,
            "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => Lang::TypeScript,
            "tsx" | "jsx" => Lang::Tsx,
            "py" | "pyi" => Lang::Python,
            _ => return None,
        };
        lang.available().then_some(lang)
    }

    fn available(self) -> bool {
        match self {
            Lang::Rust => cfg!(feature = "lang-rust"),
            Lang::Go => cfg!(feature = "lang-go"),
            Lang::TypeScript | Lang::Tsx => cfg!(feature = "lang-typescript"),
            Lang::Python => cfg!(feature = "lang-python"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Go => "go",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::Python => "python",
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Lang::Rust => 1,
            Lang::Go => 2,
            Lang::TypeScript => 3,
            Lang::Tsx => 4,
            Lang::Python => 5,
        }
    }

    pub fn from_code(code: u8) -> Option<Lang> {
        Some(match code {
            1 => Lang::Rust,
            2 => Lang::Go,
            3 => Lang::TypeScript,
            4 => Lang::Tsx,
            5 => Lang::Python,
            _ => return None,
        })
    }

    /// How qualified names are joined.
    pub fn separator(self) -> &'static str {
        match self {
            Lang::Rust => "::",
            _ => ".",
        }
    }

    pub fn language(self) -> Option<Language> {
        match self {
            #[cfg(feature = "lang-rust")]
            Lang::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            #[cfg(feature = "lang-go")]
            Lang::Go => Some(tree_sitter_go::LANGUAGE.into()),
            #[cfg(feature = "lang-typescript")]
            Lang::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            #[cfg(feature = "lang-typescript")]
            Lang::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            #[cfg(feature = "lang-python")]
            Lang::Python => Some(tree_sitter_python::LANGUAGE.into()),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    fn sources(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Lang::Rust => (
                include_str!("../queries/rust/defs.scm"),
                include_str!("../queries/rust/imports.scm"),
                include_str!("../queries/rust/idents.scm"),
            ),
            Lang::Go => (
                include_str!("../queries/go/defs.scm"),
                include_str!("../queries/go/imports.scm"),
                include_str!("../queries/go/idents.scm"),
            ),
            Lang::TypeScript | Lang::Tsx => (
                include_str!("../queries/typescript/defs.scm"),
                include_str!("../queries/typescript/imports.scm"),
                include_str!("../queries/typescript/idents.scm"),
            ),
            Lang::Python => (
                include_str!("../queries/python/defs.scm"),
                include_str!("../queries/python/imports.scm"),
                include_str!("../queries/python/idents.scm"),
            ),
        }
    }

    /// The compiled queries, built once per language per process. `None`
    /// when the grammar is not compiled in or a query does not compile —
    /// the latter is a bug the tests catch, never a crash at runtime.
    pub fn queries(self) -> Option<&'static Queries> {
        static CELLS: [OnceLock<Option<Queries>>; 5] = [
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
        ];
        CELLS[usize::from(self.code() - 1)]
            .get_or_init(|| {
                let language = self.language()?;
                let (defs, imports, idents) = self.sources();
                let compile = |src: &str| match Query::new(&language, src) {
                    Ok(q) => Some(q),
                    Err(e) => {
                        log::error!("{} query does not compile: {e}", self.as_str());
                        None
                    }
                };
                Some(Queries {
                    defs: compile(defs)?,
                    imports: compile(imports)?,
                    idents: compile(idents)?,
                })
            })
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_compiled_in_language_has_queries_that_compile() {
        for lang in [
            Lang::Rust,
            Lang::Go,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::Python,
        ] {
            if lang.available() {
                assert!(
                    lang.queries().is_some(),
                    "{lang:?} queries failed to compile"
                );
            }
        }
    }

    #[test]
    fn languages_are_chosen_by_extension() {
        assert_eq!(Lang::for_path("src/lib.rs"), Some(Lang::Rust));
        assert_eq!(Lang::for_path("app/page.tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::for_path("x.mjs"), Some(Lang::TypeScript));
        assert_eq!(Lang::for_path("a/b.py"), Some(Lang::Python));
        assert_eq!(Lang::for_path("README.md"), None);
        assert_eq!(Lang::for_path("Makefile"), None);
    }
}
