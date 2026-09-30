//! Built-in discovery rules (PRD-CTX-EFF-003 Appendix C).
//!
//! They shape *discovery* — search, listing, indexing — never explicit access:
//! a path the model names is always served (FR-FILTER-02).

/// Directories pruned wherever they appear, matched by name. Dependency
/// trees, VCS metadata, build output and caches: large, generated, and
/// almost never what a coding task is about.
pub const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "bower_components",
    "vendor",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".cache",
    "coverage",
    ".nyc_output",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".venv",
    "venv",
    ".gradle",
    ".idea",
    ".terraform",
    ".zcode",
];

/// A directory named `env` is only a Python virtualenv when it holds this
/// marker; excluding every `env/` would hide real source directories.
pub const VENV_MARKER: &str = "pyvenv.cfg";

/// Files excluded by pattern: generated, binary, or lock files. Lockfiles
/// are thousands of lines no task needs to search; `read Cargo.lock` still
/// works.
pub const EXCLUDED_FILES: &[&str] = &[
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "go.sum",
    "*.pyc",
    "*.class",
    "*.o",
    "*.a",
    "*.so",
    "*.dylib",
    "*.dll",
    "*.exe",
    "*.wasm",
    "*.png",
    "*.jpg",
    "*.jpeg",
    "*.gif",
    "*.webp",
    "*.ico",
    "*.bmp",
    "*.tiff",
    "*.mp4",
    "*.mov",
    "*.webm",
    "*.avi",
    "*.mp3",
    "*.wav",
    "*.ogg",
    "*.flac",
    "*.zip",
    "*.tar",
    "*.gz",
    "*.tgz",
    "*.bz2",
    "*.xz",
    "*.7z",
    "*.rar",
    "*.jar",
    "*.woff",
    "*.woff2",
    "*.ttf",
    "*.otf",
    "*.eot",
    "*.pdf",
    ".DS_Store",
    ".vscode/**",
];

/// Secret-shaped files kept out of discovery by default (FR-FILTER-03), so a
/// search for `API_KEY` cannot carry a credential into a prompt.
pub const SECRET_FILES: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "id_rsa*",
    "id_ed25519*",
    ".npmrc",
    ".pypirc",
    ".netrc",
];

/// Exceptions to the rules above: templates that document configuration and
/// hold no secret, and the editor settings a project shares on purpose.
pub const ALWAYS_INCLUDED: &[&str] = &[
    ".env.example",
    ".env.sample",
    ".env.template",
    ".vscode/settings.json",
    ".vscode/extensions.json",
];
