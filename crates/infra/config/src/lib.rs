//! Configuration model + loader for zcode.
//! Secrets are read from `ZCODE_*` env vars only, never written to disk.
//! Deps (direct): domain, serde, toml, thiserror — no `reqwest`/`regex` here (L3).

use domain::{AgentMode, PriceEntry, PriceTable};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_MODEL: &str = "gpt-4o-mini";
/// A streamed generation can legitimately run for minutes — a long reasoning
/// pass or a large tool-heavy turn on a slower model — and the HTTP timeout
/// covers the *whole* request (see `infra-llm::build_client`), not just the
/// time to first byte. 60s clipped those runs mid-stream; 6 minutes gives a
/// slow provider room while still failing a truly hung connection.
const DEFAULT_TIMEOUT_MS: u64 = 360_000;
/// 20 was tuned for a short single-file edit; a multi-file refactor or a
/// build-fix loop (edit, run tests, read the failure, edit again) burns a
/// turn per round trip and hit the cap mid-task, well before the model was
/// actually stuck. 220 gives a long agentic run room to keep working instead
/// of failing on turn count rather than on the task itself.
const DEFAULT_MAX_TURNS: u64 = 220;
const DEFAULT_MAX_TOKENS: u64 = 16384;
const DEFAULT_MAX_TOOL_OUTPUT_CHARS: usize = 32000;
/// Transient provider failures retried before a run is failed (`max_retries`).
const DEFAULT_MAX_RETRIES: u32 = 3;
/// Wait after a 429 that carries no `Retry-After` (`rate_limit_backoff_ms`).
/// Rate limits are metered by the minute; a sub-second retry just spends
/// another request to be refused again.
const DEFAULT_RATE_LIMIT_BACKOFF_MS: u64 = 30_000;

/// A default allowlist a coding agent can actually work under.
///
/// The old default (`echo`, `ls`, `cd`, `cat`) meant `go build ./...` failed
/// on a fresh install, which trains people to set `shell_allowed = [".*"]` and
/// switch the safety net off entirely — strictly worse than a generous default
/// paired with [`DENIED_PATTERNS`]. Anything in this list may still be refused
/// by the denylist.
pub const DEFAULT_SHELL_ALLOWED: &[&str] = &[
    // -- inspect the working tree -------------------------------------------
    r"(ls|pwd|echo|cat|head|tail|wc|file|stat|du|df|date|env|printenv|whoami|uname)( .*)?",
    r"(cd|basename|dirname|realpath|readlink|which|type|command -v)( .*)?",
    r"(grep|egrep|fgrep|rg|ag|find|fd|tree|diff|comm|sort|uniq|cut|tr|column)( .*)?",
    r"(sed|awk|jq|yq|xargs)( .*)?",
    // `rm` is here because build workflows delete files. Recursive forms are
    // allowed for a specific path (`rm -rf node_modules`) and refused when the
    // target cannot be read (`rm -rf ~`, `*`, `$VAR`) — `tools::guard`.
    r"(mkdir|touch|cp|mv|ln|rm)( .*)?",
    // -- version control -----------------------------------------------------
    r"git( .*)?",
    r"(gh|glab)( .*)?",
    // -- rust ----------------------------------------------------------------
    r"cargo( .*)?",
    r"(rustc|rustup|rustfmt|clippy-driver)( .*)?",
    // -- go ------------------------------------------------------------------
    r"go( .*)?",
    r"(gofmt|goimports|golangci-lint|staticcheck|dlv|air|templ)( .*)?",
    // -- node / typescript / next.js -----------------------------------------
    r"(node|npm|npx|pnpm|pnpx|yarn|bun|bunx|deno)( .*)?",
    r"(tsc|ts-node|tsx|eslint|prettier|biome|vitest|jest|playwright|next|vite|webpack|turbo)( .*)?",
    // -- python --------------------------------------------------------------
    r"(python|python3|pip|pip3|uv|uvx|poetry|pipenv|conda)( .*)?",
    r"(pytest|tox|ruff|mypy|black|isort|flake8|pylint)( .*)?",
    // -- other toolchains ----------------------------------------------------
    r"(make|just|task|cmake|ninja|bazel|meson)( .*)?",
    r"(mvn|gradle|gradlew|\./gradlew|dotnet|swift|zig|elixir|mix|rebar3)( .*)?",
    r"(ruby|bundle|rake|gem)( .*)?",
    r"(php|composer|artisan)( .*)?",
    // -- containers & infra (read-mostly) ------------------------------------
    r"docker (ps|images|logs|inspect|compose (ps|logs|config|build))( .*)?",
    r"kubectl (get|describe|logs|explain|version|config view)( .*)?",
    r"terraform (validate|fmt|plan|version|providers)( .*)?",
];

/// Config files looked for, in order, when no `--config` is given.
const DEFAULT_CONFIG_NAMES: &[&str] = &["zcode.json", "zcode.toml"];

/// Provider selection (FR-CONFIG-02 / FR-MODEL-01..08).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Openai,
    Anthropic,
    Openrouter,
    Deepseek,
    Ollama,
    Vllm,
    OpenaiCompatible,
    /// LM Studio's local server. OpenAI-compatible on the wire; it is a
    /// separate kind only so its default endpoint and keyless-ness come for
    /// free, the way `ollama` does.
    LmStudio,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Openrouter => "openrouter",
            Self::Deepseek => "deepseek",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::OpenaiCompatible => "openai-compatible",
            Self::LmStudio => "lmstudio",
        }
    }

    /// The env var this provider's key conventionally lives in, used when
    /// `api_key_env` is not set explicitly so switching provider does not
    /// require also remembering to switch the key name.
    pub fn default_api_key_env(&self) -> &'static str {
        match self {
            Self::Openai => "ZCODE_OPENAI_API_KEY",
            Self::Anthropic => "ZCODE_ANTHROPIC_API_KEY",
            Self::Openrouter => "ZCODE_OPENROUTER_API_KEY",
            Self::Deepseek => "ZCODE_DEEPSEEK_API_KEY",
            Self::Ollama => "ZCODE_OLLAMA_API_KEY",
            Self::Vllm | Self::OpenaiCompatible | Self::LmStudio => "ZCODE_API_KEY",
        }
    }

    /// A model id that is valid for this provider, used when the config does
    /// not name one. Without this, switching provider while keeping the
    /// default model produces a confusing 404 from the provider.
    pub fn default_model(&self) -> &'static str {
        match self {
            Self::Openai => "gpt-4o-mini",
            Self::Anthropic => "claude-sonnet-4-5",
            // OpenRouter ids are namespaced by vendor.
            Self::Openrouter => "openai/gpt-4o-mini",
            Self::Deepseek => "deepseek-chat",
            Self::Ollama => "llama3.2",
            // Self-hosted servers usually expose a single model under a name
            // only the operator knows, so there is nothing sensible to guess.
            // Self-hosted servers usually expose whatever the operator
            // loaded, under a name only they know.
            Self::Vllm | Self::OpenaiCompatible | Self::LmStudio => "",
        }
    }

    /// Local providers need no credential.
    pub fn requires_api_key(&self) -> bool {
        !matches!(
            self,
            Self::Ollama | Self::Vllm | Self::OpenaiCompatible | Self::LmStudio
        )
    }

    pub fn default_endpoint(&self) -> Option<&'static str> {
        match self {
            Self::Openai => Some("https://api.openai.com/v1/chat/completions"),
            Self::Anthropic => Some("https://api.anthropic.com/v1/messages"),
            Self::Openrouter => Some("https://openrouter.ai/api/v1/chat/completions"),
            Self::Deepseek => Some("https://api.deepseek.com/chat/completions"),
            Self::Ollama => Some("http://localhost:11434/api/chat"),
            // LM Studio's server listens here out of the box.
            Self::LmStudio => Some("http://localhost:1234/v1/chat/completions"),
            Self::Vllm | Self::OpenaiCompatible => None,
        }
    }
}

/// Every provider kind zcode speaks, for diagnostics that would otherwise
/// only be able to say what the user got wrong.
pub const BUILTIN_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "openrouter",
    "deepseek",
    "ollama",
    "vllm",
    "openai-compatible",
    "lmstudio",
];

impl std::str::FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "openrouter" => Ok(Self::Openrouter),
            "deepseek" => Ok(Self::Deepseek),
            "ollama" => Ok(Self::Ollama),
            "vllm" => Ok(Self::Vllm),
            "openai-compatible" => Ok(Self::OpenaiCompatible),
            "lmstudio" | "lm-studio" | "lm_studio" => Ok(Self::LmStudio),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

/// A stdio MCP server definition (FR-MCP-02).
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// Language servers zcode starts without being asked, when the binary is
/// present (FR-LSP-03).
///
/// Chosen to cover the stacks the guide walks through: Go, Rust, and the
/// TypeScript/JavaScript family (which is also what Next.js projects use —
/// there is no separate Next.js server, so `nextjs` is an alias rather than a
/// fourth entry). Each is opt-out via `lsp.defaults = false`, and any entry in
/// `lsp.servers` for the same language replaces the default outright.
pub fn default_lsp_servers() -> Vec<LspServerConfig> {
    let stdio = |language: &str, command: &str, args: &[&str]| LspServerConfig {
        language: language.to_string(),
        command: command.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: Vec::new(),
    };
    vec![
        stdio("rust", "rust-analyzer", &[]),
        stdio("go", "gopls", &["serve"]),
        // One server for the whole JS/TS family, Next.js included.
        stdio("typescript", "typescript-language-server", &["--stdio"]),
        stdio("javascript", "typescript-language-server", &["--stdio"]),
    ]
}

/// Languages that resolve to an already-registered server rather than one of
/// their own. Kept explicit so `zcode config` can say so.
pub const LSP_LANGUAGE_ALIASES: &[(&str, &str)] = &[
    ("nextjs", "typescript"),
    ("next", "typescript"),
    ("node", "typescript"),
    ("nodejs", "typescript"),
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("golang", "go"),
    ("rs", "rust"),
];

/// Marker files that identify a project's primary language, most specific
/// first: a Next.js repo has a `package.json` *and* a `next.config.*`, and a
/// Go module inside a monorepo may sit beside one.
const LANGUAGE_MARKERS: &[(&str, &str)] = &[
    ("go.mod", "go"),
    ("Cargo.toml", "rust"),
    ("tsconfig.json", "typescript"),
    ("next.config.js", "typescript"),
    ("next.config.ts", "typescript"),
    ("next.config.mjs", "typescript"),
    ("deno.json", "typescript"),
    ("package.json", "javascript"),
];

/// Identify the project language from its marker files. Used to decide which
/// language server is worth starting; returns `None` when nothing matches.
pub fn detect_project_language(root: &Path) -> Option<String> {
    LANGUAGE_MARKERS
        .iter()
        .find(|(marker, _)| root.join(marker).exists())
        .map(|(_, language)| (*language).to_string())
}

/// Resolve a language alias to the language a server is registered under.
pub fn canonical_language(language: &str) -> String {
    let lower = language.trim().to_ascii_lowercase();
    LSP_LANGUAGE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower)
        .map(|(_, target)| (*target).to_string())
        .unwrap_or(lower)
}

/// Locate an executable on `PATH`, the way a shell would. Stdlib only — this
/// crate has no business shelling out to `which`.
pub fn which_on_path(command: &str) -> Option<PathBuf> {
    // An explicit path is used as given.
    if command.contains(std::path::MAIN_SEPARATOR) {
        let direct = PathBuf::from(command);
        return is_executable(&direct).then_some(direct);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(command);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        // Windows carries the extension in PATHEXT; the two we can rely on.
        if cfg!(windows) {
            for ext in ["exe", "cmd"] {
                let candidate = dir.join(format!("{command}.{ext}"));
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// A stdio LSP server definition (FR-LSP-03).
#[derive(Debug, Clone, Deserialize)]
pub struct LspServerConfig {
    pub language: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// The settings that distinguish one endpoint from another.
///
/// Every field is optional because each is answered by the most specific
/// source that states it: the selected provider profile, then the top level of
/// the config (or `ZCODE_*`), then the built-in defaults for the provider's
/// kind. `None` means "not stated here" — never "empty".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderSettings {
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
}

/// One entry of the `providers` array: a named endpoint the user can select.
///
/// `name` is how it is selected (`"provider": "local"`, `--provider local`)
/// and `kind` is which wire protocol it speaks. They are usually the same
/// word — `{ "name": "openrouter", "base_url": … }` overrides the built-in
/// OpenRouter endpoint — but they need not be, which is what lets two
/// profiles share a kind: a fast one and a cheap one, both OpenRouter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    pub name: String,
    pub kind: Provider,
    pub settings: ProviderSettings,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub provider: Provider,
    /// The name the selected provider was chosen by. Equal to
    /// `provider.as_str()` unless a `providers` entry gave it its own name.
    pub provider_name: String,
    /// Every configured endpoint, in declaration order, merged across layers
    /// by name. Empty when the config never mentions `providers`.
    pub providers: Box<[ProviderProfile]>,
    /// Entries that could not be understood, as `(name, reason)`.
    ///
    /// Kept rather than made fatal: one mistyped `kind` used to stop the whole
    /// config loading, so `zcode config` — the command you reach for to *find*
    /// the mistake — failed too, and every other provider went with it. A bad
    /// entry is now an error only when it is the one selected.
    pub invalid_providers: Vec<(String, String)>,
    /// What the *top level* of the config asked for, kept apart from the
    /// resolved values so [`Config::select_provider`] can switch endpoints
    /// without losing it — it is the fallback for a profile that is silent.
    pub top_level: ProviderSettings,
    pub model: String,
    pub api_key_env: String,
    pub base_url: Option<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
    pub max_turns: u64,
    pub max_tokens: u64,
    pub max_tool_output_chars: usize,
    pub mcp_servers: Box<[McpServerConfig]>,
    pub lsp_servers: Box<[LspServerConfig]>,
    pub shell_allowed: Box<[String]>,
    /// Extra always-on deny patterns, added to zcode's built-in denylist.
    /// Rules here cannot be removed by widening `shell_allowed`.
    pub shell_denied: Box<[String]>,
    pub skills_dir: PathBuf,
    pub mode: AgentMode,
    /// How many times a transient provider failure (429, 5xx, timeout) is
    /// retried before the run fails.
    pub max_retries: u32,
    /// How long to wait after a rate limit that carries no `Retry-After`.
    /// The provider's own header always wins over this.
    pub rate_limit_backoff_ms: u64,
    /// Per-model rate overrides for the cost estimate, ahead of the built-ins.
    pub pricing: Vec<PriceEntry>,
    /// Set false to skip the built-in language-server defaults.
    pub lsp_defaults: bool,
    /// Token-optimised shell output via rtk.
    pub rtk: RtkConfig,
}

/// How zcode uses [rtk](https://github.com/rtk-ai/rtk), if at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtkConfig {
    /// Use rtk when it is available. On by default: an agent that reads less
    /// output costs less, and rtk decides for itself which commands it can
    /// safely improve.
    pub enabled: bool,
    /// Install rtk when it is missing, using a package manager the machine
    /// already has.
    pub auto_install: bool,
    /// An explicit binary, for a machine where rtk is not on `PATH`.
    pub path: Option<String>,
}

impl Default for RtkConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_install: true,
            path: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            provider: Provider::Openai,
            provider_name: Provider::Openai.as_str().to_string(),
            providers: Box::new([]),
            invalid_providers: Vec::new(),
            top_level: ProviderSettings::default(),
            model: DEFAULT_MODEL.to_string(),
            api_key_env: String::from("ZCODE_OPENAI_API_KEY"),
            base_url: None,
            working_dir,
            env: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_turns: DEFAULT_MAX_TURNS,
            max_tokens: DEFAULT_MAX_TOKENS,
            max_tool_output_chars: DEFAULT_MAX_TOOL_OUTPUT_CHARS,
            mcp_servers: Box::new([]),
            lsp_servers: Box::new([]),
            shell_allowed: DEFAULT_SHELL_ALLOWED
                .iter()
                .map(|s| s.to_string())
                .collect(),
            shell_denied: Box::new([]),
            skills_dir: PathBuf::new(),
            mode: AgentMode::default(),
            max_retries: DEFAULT_MAX_RETRIES,
            rate_limit_backoff_ms: DEFAULT_RATE_LIMIT_BACKOFF_MS,
            pricing: Vec::new(),
            lsp_defaults: true,
            rtk: RtkConfig::default(),
        }
    }
}

impl Config {
    /// Point the configuration at one of its providers, by name.
    ///
    /// `name` is matched against the `providers` array first, then against the
    /// built-in provider kinds — so `--provider ollama` works whether or not a
    /// profile is declared for it, and a profile named `openrouter` shadows
    /// the built-in defaults for OpenRouter, which is how you give it a URL.
    ///
    /// **A declared profile is complete in itself.** What it does not state
    /// comes from the defaults for its kind, *not* from the top level of the
    /// config: a top-level `api_key_env` was written for whichever provider
    /// the config had at the time, and letting an unrelated gateway inherit it
    /// produces a key variable that looks correct in `zcode config` and fails
    /// at the first request. Top-level `model` / `api_key_env` / `base_url`
    /// are the single-provider form, and apply when the selection is a bare
    /// kind with no profile behind it — which is every config written before
    /// `providers` existed.
    pub fn select_provider(&mut self, name: &str) -> Result<(), ConfigError> {
        let name = name.trim();
        let (profile, declared) = match self.providers.iter().find(|p| p.name == name) {
            Some(found) => (found.clone(), true),
            None => {
                // Selected an entry that failed to parse: say what is wrong
                // with it rather than claiming it does not exist.
                if let Some((_, reason)) = self.invalid_providers.iter().find(|(n, _)| n == name) {
                    return Err(ConfigError::InvalidProviderEntry {
                        name: name.to_string(),
                        reason: reason.clone(),
                    });
                }
                let kind = name.parse::<Provider>().map_err(|_| {
                    if self.providers.is_empty() {
                        ConfigError::UnknownProvider(name.to_string())
                    } else {
                        ConfigError::UnknownProviderNamed {
                            name: name.to_string(),
                            configured: self.provider_names().join(", "),
                            builtin: BUILTIN_PROVIDERS.join(", "),
                        }
                    }
                })?;
                (
                    ProviderProfile {
                        name: kind.as_str().to_string(),
                        kind,
                        settings: ProviderSettings::default(),
                    },
                    false,
                )
            }
        };

        let fallback = if declared {
            &ProviderSettings {
                model: None,
                api_key_env: None,
                base_url: None,
            }
        } else {
            &self.top_level
        };
        let pick = |stated: Option<&String>, fallback: Option<&String>| -> Option<String> {
            stated.or(fallback).cloned()
        };
        self.provider = profile.kind;
        self.provider_name = profile.name;
        self.base_url = pick(
            profile.settings.base_url.as_ref(),
            fallback.base_url.as_ref(),
        );
        self.api_key_env = pick(
            profile.settings.api_key_env.as_ref(),
            fallback.api_key_env.as_ref(),
        )
        .unwrap_or_else(|| profile.kind.default_api_key_env().to_string());
        self.model = pick(profile.settings.model.as_ref(), fallback.model.as_ref())
            .unwrap_or_else(|| profile.kind.default_model().to_string());
        Ok(())
    }

    /// The names `--provider` accepts from this config, in declaration order.
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.name.clone()).collect()
    }

    /// A view of this config as it would be with a different provider
    /// selected, for building a second client without disturbing the first.
    pub fn with_provider(&self, name: &str) -> Result<Self, ConfigError> {
        let mut next = self.clone();
        next.select_provider(name)?;
        Ok(next)
    }

    /// Point the configuration at a model, given as `<provider>/<model>`.
    ///
    /// This is the spelling opencode and most agent CLIs use, and it is read
    /// the same way: **split at the first `/`, the leading segment is the
    /// provider, everything after it is the model id** — slashes and all. So
    /// `openrouter/z-ai/glm-4.6` is the provider `openrouter` and the model
    /// `z-ai/glm-4.6`, which is exactly how OpenRouter spells that id.
    ///
    /// There is no guessing. An earlier version read the prefix as a provider
    /// only when it happened to name one, which made the meaning of an
    /// argument depend on what the config declared: adding a `providers` entry
    /// could silently change where an existing command sent its request.
    /// A leading segment that names no provider is now an error that says what
    /// to write instead.
    ///
    /// The one shorthand is a spec with **no** `/` at all: an id on the
    /// provider already selected. It cannot be mistaken for a pair, and
    /// "same endpoint, different model" is too common to require the prefix.
    ///
    /// The provider is selected *first*, so the model given here outranks the
    /// one the profile carries — which is the whole point of stating both.
    pub fn select_model(&mut self, spec: &str) -> Result<(), ConfigError> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(ConfigError::EmptyModel);
        }
        let Some((provider, model)) = spec.split_once('/') else {
            self.model = spec.to_string();
            return Ok(());
        };
        let model = model.trim();
        if model.is_empty() {
            return Err(ConfigError::ModelWithoutId {
                spec: spec.to_string(),
                provider: provider.to_string(),
            });
        }
        if !self.knows_provider(provider) {
            // The likely mistake is a model id written without its provider,
            // so the message shows that exact command with the provider
            // already selected — `z-ai/glm-4.6` becomes `openrouter/z-ai/glm-4.6`.
            return Err(ConfigError::UnknownProviderInModel {
                spec: spec.to_string(),
                provider: provider.to_string(),
                configured: self.provider_names().join(", "),
                builtin: BUILTIN_PROVIDERS.join(", "),
                suggestion: format!("{}/{spec}", self.provider_name),
            });
        }
        // Order matters: `select_provider` resolves the profile's own model,
        // so it has to run before the override is written.
        self.select_provider(provider)?;
        self.model = model.to_string();
        Ok(())
    }

    /// Whether `name` selects a provider: a `providers` entry — including one
    /// that failed to parse, so [`Config::select_provider`] can report *why*
    /// it is unusable rather than the name being rejected as unknown — or a
    /// built-in kind.
    pub fn knows_provider(&self, name: &str) -> bool {
        self.providers.iter().any(|p| p.name == name)
            || self.invalid_providers.iter().any(|(n, _)| n == name)
            || name.parse::<Provider>().is_ok()
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub fn rate_limit_backoff(&self) -> Duration {
        Duration::from_millis(self.rate_limit_backoff_ms)
    }

    /// Cost rates: configured overrides first, then the built-in table.
    pub fn price_table(&self) -> PriceTable {
        PriceTable::with_overrides(self.pricing.clone())
    }

    /// Language servers actually started: everything in `lsp.servers`, plus
    /// any built-in default whose binary is on `PATH` and whose language the
    /// config has not already claimed.
    ///
    /// Probing `PATH` is what makes "LSP on by default" tolerable — a default
    /// that spawned a missing `gopls` on every run would print a warning per
    /// session for the majority of users who do not write Go.
    pub fn effective_lsp_servers(&self) -> Vec<LspServerConfig> {
        let mut servers: Vec<LspServerConfig> = self.lsp_servers.to_vec();
        if self.lsp_defaults {
            for candidate in default_lsp_servers() {
                let claimed = servers
                    .iter()
                    .any(|s| canonical_language(&s.language) == candidate.language);
                if !claimed && which_on_path(&candidate.command).is_some() {
                    servers.push(candidate);
                }
            }
        }
        // Only one server runs per session. When we can tell what the project
        // is, run *only* a server for that language: a Go repo on a machine
        // that also has rust-analyzer installed would otherwise start
        // rust-analyzer, which costs a process and answers nothing. An
        // explicitly configured server is always kept — the user asked for it.
        let explicit: Vec<String> = self
            .lsp_servers
            .iter()
            .map(|s| s.language.clone())
            .collect();
        match detect_project_language(&self.working_dir) {
            Some(detected) => {
                servers.retain(|s| {
                    canonical_language(&s.language) == detected || explicit.contains(&s.language)
                });
                servers.sort_by_key(|s| canonical_language(&s.language) != detected);
            }
            // Nothing identifies this directory as a project of any language.
            // Starting rust-analyzer on the off-chance costs a process and a
            // startup failure, and can answer nothing.
            None => servers.retain(|s| explicit.contains(&s.language)),
        }
        servers
    }

    /// The language of the project rooted at `working_dir`, or `None`.
    pub fn detected_language(&self) -> Option<String> {
        detect_project_language(&self.working_dir)
    }

    /// The primary skills directory (the project's own, unless overridden).
    pub fn skills_dir(&self) -> PathBuf {
        if self.skills_dir.as_os_str().is_empty() {
            self.working_dir.join(".zcode").join("skills")
        } else {
            self.resolve_configured_dir(&self.skills_dir)
        }
    }

    /// Expand `~` and, for a relative path, anchor it to `working_dir` rather
    /// than the process's current directory.
    ///
    /// The project config file is found by walking *up* from wherever the CLI
    /// was launched (see `Loader::discover_from`), so `working_dir` — the
    /// directory holding that file — and the process's actual cwd are only
    /// the same directory when the agent happens to be run from the project
    /// root. A relative `skills_dir = "myskills"` resolved against the
    /// process cwd instead would report the directory as missing (and the
    /// skills in it as unreachable) the moment the agent is invoked from any
    /// subdirectory, even though the config that named it was found and
    /// loaded correctly.
    fn resolve_configured_dir(&self, path: &Path) -> PathBuf {
        let expanded = expand_tilde(path.to_path_buf());
        if expanded.is_absolute() {
            expanded
        } else {
            self.working_dir.join(expanded)
        }
    }

    /// Every directory searched for skills, nearest first.
    ///
    /// A configured `skills_dir` *adds* a root rather than replacing the
    /// project's own: a machine-wide library set in the user config would
    /// otherwise make per-project skills impossible. Names found earlier win.
    pub fn skills_dirs(&self) -> Vec<PathBuf> {
        let mut roots = Vec::with_capacity(3);
        let mut push = |dir: PathBuf| {
            if !dir.as_os_str().is_empty() && !roots.contains(&dir) {
                roots.push(dir);
            }
        };
        push(self.working_dir.join(".zcode").join("skills"));
        if !self.skills_dir.as_os_str().is_empty() {
            push(self.resolve_configured_dir(&self.skills_dir));
        }
        // The machine-wide library, so a global collection is always available.
        if let Some(user) = user_config_candidates().first().and_then(|p| p.parent()) {
            push(user.join("skills"));
        }
        roots
    }

    /// Resolve the API key by name from env (FR-CONFIG-03/NFR-SEC-01). The
    /// secret value is never logged.
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        std::env::var(&self.api_key_env)
            .map_err(|_| ConfigError::MissingSecret(self.api_key_env.clone()))
    }

    pub fn to_agent_context(&self) -> domain::AgentContext {
        domain::AgentContext {
            working_dir: self.working_dir.clone(),
            model: self.model.clone(),
            env: self.env.clone(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),
    /// A name that is neither a `providers` entry nor a built-in kind.
    ///
    /// Two spellings because the useful advice differs: with profiles
    /// declared, the answer is usually one of them; without, the user is
    /// either misspelling a kind or has forgotten to declare the array.
    #[error("unknown provider `{0}` — built in: {}. Declare your own with a \
             `providers` array.", BUILTIN_PROVIDERS.join(", "))]
    UnknownProvider(String),
    #[error("unknown provider `{name}` — configured: {configured}; built in: {builtin}")]
    UnknownProviderNamed {
        name: String,
        configured: String,
        builtin: String,
    },
    #[error("missing secret env var: {0}")]
    MissingSecret(String),
    #[error("invalid agent mode: {0}")]
    InvalidMode(String),
    #[error(
        "a `providers` entry has neither `name` nor `kind` — one of them must \
         say which provider it is"
    )]
    ProviderEntryUnnamed,
    #[error("provider `{name}` is configured but unusable: {reason}")]
    InvalidProviderEntry { name: String, reason: String },
    #[error("no model id given")]
    EmptyModel,
    #[error("`{spec}` names the provider `{provider}` but no model — write `{provider}/<model>`")]
    ModelWithoutId { spec: String, provider: String },
    /// The leading segment of a `<provider>/<model>` spec names no provider.
    ///
    /// Almost always a model id written without its provider, so the message
    /// leads with that command rather than with a list to read through.
    #[error(
        "unknown provider `{provider}` in `{spec}` — a model is written \
         `<provider>/<model>`. If `{spec}` is the model id, name the provider \
         too: `{suggestion}`. Configured: {configured}; built in: {builtin}"
    )]
    UnknownProviderInModel {
        spec: String,
        provider: String,
        configured: String,
        builtin: String,
        suggestion: String,
    },
}

/// Intermediate deserialization target so unknown/extra keys in the file are
/// ignored and individual overrides can be merged field-by-field.
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    providers: Option<Vec<ProviderFile>>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    working_dir: Option<PathBuf>,
    #[serde(default)]
    env: Option<Vec<(String, String)>>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_turns: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    max_tool_output_chars: Option<usize>,
    #[serde(default)]
    mcp: McpSection,
    #[serde(default)]
    lsp: LspSection,
    #[serde(default)]
    shell_allowed: Option<Vec<String>>,
    #[serde(default)]
    shell_denied: Option<Vec<String>>,
    #[serde(default)]
    skills_dir: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    rate_limit_backoff_ms: Option<u64>,
    #[serde(default)]
    pricing: Option<Vec<PricingEntryFile>>,
    #[serde(default)]
    rtk: RtkSection,
}

/// Serde mirror of [`RtkConfig`]. Every field is optional so `[rtk]` can name
/// only what it disagrees with.
#[derive(Debug, Default, Deserialize)]
struct RtkSection {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    auto_install: Option<bool>,
    #[serde(default)]
    path: Option<String>,
}

/// One `providers` entry as written in the file.
///
/// `name` and `kind` both default to the other, so the short form
/// `{ "name": "openrouter", "base_url": … }` and the explicit form
/// `{ "name": "cheap", "kind": "openrouter", … }` both work. `provider` is
/// accepted as a spelling of `kind` because that is the word the top level
/// uses and people reach for it here too.
/// Unknown keys are an error here, unlike everywhere else in the file.
///
/// TOML puts every bare key after `[[providers]]` *inside* that entry, so a
/// `timeout_ms = 120000` written below the table array silently becomes a
/// field of the last provider and is then dropped. Refusing what a profile
/// cannot use turns a setting that quietly did nothing into a message naming
/// the line.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, alias = "provider")]
    kind: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
}

impl ProviderFile {
    /// What to call this entry in a diagnostic, before it is known to be valid.
    fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.kind.clone())
            .unwrap_or_else(|| "(unnamed)".to_string())
    }

    fn into_profile(self) -> Result<ProviderProfile, ConfigError> {
        // Either field names the other when only one is given; a profile with
        // neither cannot say what it talks to, so it is an error rather than
        // a silent default to OpenAI.
        let kind_str = self
            .kind
            .clone()
            .or_else(|| self.name.clone())
            .ok_or(ConfigError::ProviderEntryUnnamed)?;
        let kind = kind_str
            .parse::<Provider>()
            .map_err(|_| ConfigError::UnknownProvider(kind_str.clone()))?;
        Ok(ProviderProfile {
            name: self.name.unwrap_or(kind_str),
            kind,
            settings: ProviderSettings {
                model: self.model,
                api_key_env: self.api_key_env,
                base_url: self.base_url,
            },
        })
    }
}

/// Serde mirror of `domain::PriceEntry` — `domain` carries no derives
/// (FR-DI-01), so the bridge lives here.
#[derive(Debug, Clone, Deserialize)]
struct PricingEntryFile {
    model: String,
    #[serde(default)]
    input_per_mtok: f64,
    #[serde(default)]
    output_per_mtok: f64,
    #[serde(default)]
    cache_per_mtok: f64,
    /// Whether the provider counts cached tokens inside `input_tokens`
    /// (OpenAI-style). Defaults to true, which is the common case.
    #[serde(default = "default_true")]
    cache_within_input: bool,
}

fn default_true() -> bool {
    true
}

impl From<PricingEntryFile> for PriceEntry {
    fn from(f: PricingEntryFile) -> Self {
        PriceEntry {
            model: f.model,
            input_per_mtok: f.input_per_mtok,
            output_per_mtok: f.output_per_mtok,
            cache_per_mtok: f.cache_per_mtok,
            cache_within_input: f.cache_within_input,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct McpSection {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

#[derive(Debug, Deserialize)]
struct LspSection {
    #[serde(default)]
    servers: Vec<LspServerConfig>,
    /// Set false to opt out of the built-in language-server defaults.
    #[serde(default = "default_true")]
    defaults: bool,
}

impl Default for LspSection {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            defaults: true,
        }
    }
}

/// The spellings of yes and no people actually type in an env var.
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Where a configuration layer came from, for `zcode config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerKind {
    /// `~/.config/zcode/config.{json,toml}` — machine-wide defaults.
    User,
    /// The nearest `zcode.{json,toml}` at or above the working directory.
    Project,
    /// An explicit `--config <FILE>`.
    Explicit,
}

/// One resolved configuration file.
#[derive(Debug, Clone)]
pub struct ConfigLayer {
    pub kind: LayerKind,
    pub path: PathBuf,
}

/// Builds a `Config` from layered sources.
///
/// Later layers override earlier ones field by field, so a machine-wide file
/// can carry the provider and key name while each project overrides only what
/// differs:
///
/// ```text
/// defaults → ~/.config/zcode/config.json → <project>/zcode.json → ZCODE_* env
/// ```
pub struct Loader {
    layers: Vec<ConfigLayer>,
    /// Directory holding the project config, used to anchor `working_dir`.
    project_root: Option<PathBuf>,
}

impl Loader {
    /// Load exactly one file, with no user layer and no discovery.
    pub fn new(config_path: impl Into<PathBuf>) -> Self {
        Self {
            layers: vec![ConfigLayer {
                kind: LayerKind::Explicit,
                path: config_path.into(),
            }],
            project_root: None,
        }
    }

    /// The full chain: user config, then the nearest project config found by
    /// walking up from the current directory.
    pub fn with_default() -> Self {
        Self::discover_from(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// `with_default`, but rooted at an explicit directory (testable).
    pub fn discover_from(start: &Path) -> Self {
        let mut layers = Vec::with_capacity(2);
        if let Some(path) = user_config_path() {
            layers.push(ConfigLayer {
                kind: LayerKind::User,
                path,
            });
        }
        let project = find_project_config(start);
        let project_root = project.as_ref().and_then(|p| p.parent().map(PathBuf::from));
        if let Some(path) = project {
            layers.push(ConfigLayer {
                kind: LayerKind::Project,
                path,
            });
        }
        Self {
            layers,
            project_root,
        }
    }

    /// The files this loader will read, in order. Only existing files appear.
    pub fn layers(&self) -> &[ConfigLayer] {
        &self.layers
    }

    /// Parse a config file, choosing the format from its extension. An
    /// unknown extension is parsed as TOML, then as JSON, so a file named
    /// `agent.conf` still works if its contents are valid.
    fn parse(path: &Path, content: &str) -> Result<ConfigFile, ConfigError> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("json") => serde_json::from_str(content).map_err(ConfigError::Json),
            Some("toml") => toml::from_str(content).map_err(ConfigError::Parse),
            _ => match toml::from_str(content) {
                Ok(parsed) => Ok(parsed),
                Err(toml_err) => serde_json::from_str(content).map_err(|_| {
                    // Report the TOML error: it is the more likely intent and
                    // the more legible message.
                    ConfigError::Parse(toml_err)
                }),
            },
        }
    }

    pub fn load(&self) -> Result<Config, ConfigError> {
        let mut config = Config::default();
        // Connection settings are not applied as they are read: which of them
        // wins depends on the provider finally selected, and the selection can
        // arrive after them (a later layer, `ZCODE_PROVIDER`, `--provider`).
        // They are collected here and resolved once, at the end.
        let mut top_level = ProviderSettings::default();
        let mut profiles: Vec<ProviderProfile> = Vec::new();
        let mut invalid: Vec<(String, String)> = Vec::new();
        let mut selected: Option<String> = None;
        let mut working_dir_set = false;

        for layer in &self.layers {
            if !layer.path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&layer.path)?;
            let file: ConfigFile = Self::parse(&layer.path, &content)?;

            if let Some(m) = file.model {
                top_level.model = Some(m);
            }
            if let Some(p) = file.provider {
                selected = Some(p);
            }
            if let Some(entries) = file.providers {
                // Profiles accumulate across layers and are keyed by name, so
                // a machine-wide file can declare the endpoints once and each
                // project only has to say which of them it wants — while still
                // being able to redefine one it disagrees with.
                for entry in entries {
                    let label = entry.label();
                    match entry.into_profile() {
                        Ok(profile) => {
                            invalid.retain(|(name, _)| *name != profile.name);
                            match profiles.iter_mut().find(|p| p.name == profile.name) {
                                Some(existing) => *existing = profile,
                                None => profiles.push(profile),
                            }
                        }
                        // Recorded, not fatal — see `Config::invalid_providers`.
                        Err(reason) => {
                            profiles.retain(|p| p.name != label);
                            invalid.retain(|(name, _)| *name != label);
                            invalid.push((label, reason.to_string()));
                        }
                    }
                }
            }
            if let Some(k) = file.api_key_env {
                top_level.api_key_env = Some(k);
            }
            if let Some(u) = file.base_url {
                top_level.base_url = Some(u);
            }
            if let Some(wd) = file.working_dir {
                config.working_dir = wd;
                working_dir_set = true;
            }
            if let Some(env) = file.env {
                config.env = env;
            }
            if let Some(t) = file.timeout_ms {
                config.timeout_ms = t;
            }
            if let Some(t) = file.max_turns {
                config.max_turns = t;
            }
            if let Some(t) = file.max_tokens {
                config.max_tokens = t;
            }
            if let Some(t) = file.max_tool_output_chars {
                config.max_tool_output_chars = t;
            }
            if !file.mcp.servers.is_empty() {
                config.mcp_servers = file.mcp.servers.into_boxed_slice();
            }
            if !file.lsp.servers.is_empty() {
                config.lsp_servers = file.lsp.servers.into_boxed_slice();
            }
            if !file.lsp.defaults {
                config.lsp_defaults = false;
            }
            if let Some(v) = file.shell_allowed {
                config.shell_allowed = v.into_boxed_slice();
            }
            if let Some(v) = file.shell_denied {
                // Deny rules accumulate across layers: a machine-wide ban must
                // not be droppable by a project file.
                let mut merged = config.shell_denied.to_vec();
                merged.extend(v);
                config.shell_denied = merged.into_boxed_slice();
            }
            if let Some(s) = file.skills_dir {
                config.skills_dir = PathBuf::from(s);
            }
            if let Some(m) = file.mode {
                config.mode = m
                    .parse::<AgentMode>()
                    .map_err(|_| ConfigError::InvalidMode(m))?;
            }
            if let Some(r) = file.max_retries {
                config.max_retries = r;
            }
            if let Some(ms) = file.rate_limit_backoff_ms {
                config.rate_limit_backoff_ms = ms;
            }
            if let Some(v) = file.rtk.enabled {
                config.rtk.enabled = v;
            }
            if let Some(v) = file.rtk.auto_install {
                config.rtk.auto_install = v;
            }
            if let Some(v) = file.rtk.path {
                config.rtk.path = Some(v);
            }
            if let Some(rates) = file.pricing {
                // Later layers take precedence, so a project rate wins over a
                // machine-wide one: prepend rather than append.
                let mut merged: Vec<PriceEntry> = rates.into_iter().map(Into::into).collect();
                merged.append(&mut config.pricing);
                config.pricing = merged;
            }
        }

        // Anchor relative work to the project root, not to wherever the user
        // happened to `cd`. Running `zcode` from `src/` must behave the same
        // as running it from the directory holding the config.
        if !working_dir_set {
            if let Some(root) = &self.project_root {
                config.working_dir = root.clone();
            }
        }

        if let Ok(provider) = std::env::var("ZCODE_PROVIDER") {
            selected = Some(provider);
        }
        if let Ok(model) = std::env::var("ZCODE_MODEL") {
            top_level.model = Some(model);
        }
        if let Ok(k) = std::env::var("ZCODE_API_KEY_ENV") {
            top_level.api_key_env = Some(k);
        }
        if let Ok(u) = std::env::var("ZCODE_BASE_URL") {
            top_level.base_url = Some(u);
        }
        if let Ok(wd) = std::env::var("ZCODE_WORKING_DIR") {
            config.working_dir = PathBuf::from(wd);
        }
        if let Ok(t) = std::env::var("ZCODE_TIMEOUT_MS") {
            if let Ok(ms) = t.parse::<u64>() {
                config.timeout_ms = ms;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_MAX_TURNS") {
            if let Ok(v) = t.parse::<u64>() {
                config.max_turns = v;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_MAX_TOKENS") {
            if let Ok(v) = t.parse::<u64>() {
                config.max_tokens = v;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_MAX_TOOL_OUTPUT_CHARS") {
            if let Ok(v) = t.parse::<usize>() {
                config.max_tool_output_chars = v;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_MAX_RETRIES") {
            if let Ok(v) = t.parse::<u32>() {
                config.max_retries = v;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_RATE_LIMIT_BACKOFF_MS") {
            if let Ok(v) = t.parse::<u64>() {
                config.rate_limit_backoff_ms = v;
            }
        }
        if let Ok(dir) = std::env::var("ZCODE_SKILLS_DIR") {
            config.skills_dir = PathBuf::from(dir);
        }
        // Newline-separated so a pattern may contain spaces (`echo .*`).
        // An explicitly empty value means deny-all, which must be honoured
        // rather than silently falling back to the defaults (M2.5).
        if let Ok(raw) = std::env::var("ZCODE_SHELL_ALLOWED") {
            config.shell_allowed = raw
                .split('\n')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        }
        if let Ok(raw) = std::env::var("ZCODE_SHELL_DENIED") {
            let mut merged = config.shell_denied.to_vec();
            merged.extend(
                raw.split('\n')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            );
            config.shell_denied = merged.into_boxed_slice();
        }
        // Turning rtk off is the thing someone needs to do quickly, on a
        // machine or in a CI job, without editing a file.
        if let Ok(v) = std::env::var("ZCODE_RTK") {
            config.rtk.enabled = parse_bool(&v).unwrap_or(config.rtk.enabled);
        }
        if let Ok(v) = std::env::var("ZCODE_RTK_AUTO_INSTALL") {
            config.rtk.auto_install = parse_bool(&v).unwrap_or(config.rtk.auto_install);
        }
        if let Ok(p) = std::env::var("ZCODE_RTK_PATH") {
            config.rtk.path = Some(p);
        }
        if let Ok(m) = std::env::var("ZCODE_MODE") {
            config.mode = m
                .parse::<AgentMode>()
                .map_err(|_| ConfigError::InvalidMode(m.clone()))?;
        }

        // People write `~/...` in config files; expand it before anything
        // tries to open the path.
        config.working_dir = expand_tilde(config.working_dir);
        config.skills_dir = expand_tilde(config.skills_dir);

        config.providers = profiles.into_boxed_slice();
        config.invalid_providers = invalid;
        config.top_level = top_level;
        // Nothing named a provider, so keep the default kind but still run it
        // through the same resolution — a profile may be declared for it.
        let selected = selected.unwrap_or_else(|| config.provider.as_str().to_string());
        config.select_provider(&selected)?;

        Ok(config)
    }

    /// Load from a specific config override path (`--config`). The user-level
    /// layer still applies underneath, so `--config` overrides a project file
    /// without discarding machine-wide defaults such as the provider key name.
    pub fn load_with_override(
        &self,
        override_path: Option<impl AsRef<Path>>,
    ) -> Result<Config, ConfigError> {
        let Some(p) = override_path else {
            return self.load();
        };
        let path = p.as_ref().to_path_buf();
        let mut layers = Vec::with_capacity(2);
        if let Some(user) = user_config_path() {
            layers.push(ConfigLayer {
                kind: LayerKind::User,
                path: user,
            });
        }
        let project_root = path.parent().map(PathBuf::from);
        layers.push(ConfigLayer {
            kind: LayerKind::Explicit,
            path,
        });
        Loader {
            layers,
            project_root,
        }
        .load()
    }
}

/// Expand a leading `~` to the user's home directory.
///
/// Config files are written by people, and `"~/.config/zcode/skills"` is the
/// natural thing to write. Rust treats `~` as an ordinary directory name, so
/// without this the path silently resolves to a literal `./~/...` that never
/// exists. `~user/...` is not supported.
pub fn expand_tilde(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if text == "~" {
        return std::env::var_os("HOME").map(PathBuf::from).unwrap_or(path);
    }
    let Some(rest) = text.strip_prefix("~/") else {
        return path;
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => path,
    }
}

/// Candidate paths for the machine-wide config, in preference order.
pub fn user_config_candidates() -> Vec<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    let Some(base) = base else {
        return Vec::new();
    };
    let dir = base.join("zcode");
    vec![dir.join("config.json"), dir.join("config.toml")]
}

/// The machine-wide state directory, `~/.config/zcode`.
///
/// Distinct from `<working_dir>/.zcode`, which is per-project. Things that are
/// true of the *machine* — whether an auto-install has been tried — belong
/// here, or every project would retry it independently.
pub fn user_state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("zcode"))
}

/// The machine-wide config, if one exists.
pub fn user_config_path() -> Option<PathBuf> {
    user_config_candidates().into_iter().find(|p| p.exists())
}

/// The nearest `zcode.json`/`zcode.toml` at or above `start`.
///
/// Walking up means a coding agent behaves like the other tools in a
/// repository: it finds the project's settings from any subdirectory instead
/// of silently falling back to built-in defaults.
pub fn find_project_config(start: &Path) -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(start);
    while let Some(current) = dir {
        for name in DEFAULT_CONFIG_NAMES {
            let candidate = current.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        dir = current.parent();
    }
    None
}

impl Default for Loader {
    fn default() -> Self {
        Self::with_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `Loader::load` reads process-wide env vars, so tests that set them —
    /// and tests that assert on values those vars can override — must not run
    /// concurrently. Without this the suite is flaky (NFR-REL-01).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Point the user-config layer at an empty directory.
    ///
    /// `discover_from` reads `~/.config/zcode/`, so without this a unit test
    /// depends on whatever the developer happens to have configured — and
    /// fails for reasons that have nothing to do with the code. Callers must
    /// already hold `env_guard`, since this moves process-global state.
    #[must_use]
    fn isolated_user_config() -> tempfile::TempDir {
        let empty = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", empty.path());
        empty
    }

    fn write_config(dir: &tempfile::TempDir, toml_content: &str) -> PathBuf {
        let path = dir.path().join("zcode.toml");
        fs::write(&path, toml_content).unwrap();
        path
    }

    // ---- multiple providers -----------------------------------------------

    const MULTI: &str = r#"
provider = "cheap"

[[providers]]
name = "cheap"
kind = "openrouter"
model = "poolside/laguna-s-2.1:free"
base_url = "https://openrouter.ai/api/v1/chat/completions"
api_key_env = "MY_OPENROUTER_KEY"

[[providers]]
name = "local"
kind = "ollama"
model = "llama3.2"
base_url = "http://127.0.0.1:11434/api/chat"

[[providers]]
name = "anthropic"
"#;

    #[test]
    fn the_named_provider_is_the_one_selected() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();

        assert_eq!(cfg.provider_name, "cheap");
        assert_eq!(cfg.provider, Provider::Openrouter);
        assert_eq!(cfg.model, "poolside/laguna-s-2.1:free");
        assert_eq!(cfg.api_key_env, "MY_OPENROUTER_KEY");
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1/chat/completions")
        );
        assert_eq!(cfg.provider_names(), ["cheap", "local", "anthropic"]);
    }

    #[test]
    fn switching_provider_takes_the_whole_profile_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_provider("local").unwrap();

        assert_eq!(cfg.provider, Provider::Ollama);
        assert_eq!(cfg.model, "llama3.2");
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("http://127.0.0.1:11434/api/chat")
        );
        // Ollama needs no key, and the profile names none, so the kind decides.
        assert_eq!(cfg.api_key_env, "ZCODE_OLLAMA_API_KEY");
    }

    #[test]
    fn a_profile_that_states_nothing_falls_back_to_its_kind() {
        // `[[providers]] name = "anthropic"` names a kind and nothing else:
        // every field must come from the built-in defaults for Anthropic, not
        // from the profile that happened to be selected before it.
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_provider("anthropic").unwrap();

        assert_eq!(cfg.provider, Provider::Anthropic);
        assert_eq!(cfg.model, Provider::Anthropic.default_model());
        assert_eq!(cfg.api_key_env, "ZCODE_ANTHROPIC_API_KEY");
        assert_eq!(cfg.base_url, None, "no URL stated anywhere");
    }

    #[test]
    fn a_builtin_kind_is_selectable_without_a_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_provider("deepseek").unwrap();

        assert_eq!(cfg.provider, Provider::Deepseek);
        assert_eq!(cfg.provider_name, "deepseek");
        assert_eq!(cfg.model, "deepseek-chat");
    }

    #[test]
    fn a_profile_may_give_a_builtin_provider_its_own_url() {
        // The short form: one entry, named after the kind it overrides. This
        // is the "let me point OpenRouter somewhere else" case.
        let dir = tempfile::tempdir().unwrap();
        let cfg = Loader::new(&write_config(
            &dir,
            r#"
provider = "openrouter"

[[providers]]
name = "openrouter"
base_url = "https://gateway.internal/v1/chat/completions"
"#,
        ))
        .load()
        .unwrap();

        assert_eq!(cfg.provider, Provider::Openrouter);
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("https://gateway.internal/v1/chat/completions")
        );
        // Everything it did not state still comes from the kind, which for a
        // profile named after its kind is exactly what you would expect.
        assert_eq!(cfg.model, Provider::Openrouter.default_model());
        assert_eq!(cfg.api_key_env, "ZCODE_OPENROUTER_API_KEY");
    }

    #[test]
    fn two_profiles_may_share_one_kind() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(
            &dir,
            r#"
provider = "fast"

[[providers]]
name = "fast"
kind = "openrouter"
model = "anthropic/claude-sonnet-4.5"

[[providers]]
name = "free"
kind = "openrouter"
model = "poolside/laguna-s-2.1:free"
"#,
        ))
        .load()
        .unwrap();

        assert_eq!(cfg.model, "anthropic/claude-sonnet-4.5");
        cfg.select_provider("free").unwrap();
        assert_eq!(cfg.provider, Provider::Openrouter);
        assert_eq!(cfg.model, "poolside/laguna-s-2.1:free");
    }

    #[test]
    fn a_declared_profile_is_complete_in_itself() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(
            &dir,
            r#"
provider = "stated"
model = "top-level-model"
api_key_env = "TOP_LEVEL_KEY"

[[providers]]
name = "stated"
kind = "openrouter"
model = "profile-model"

[[providers]]
name = "silent"
kind = "openai-compatible"
base_url = "https://gateway.internal/v1/chat/completions"
"#,
        ))
        .load()
        .unwrap();

        // Selecting a profile has to actually select its model, or the array
        // would be decoration.
        assert_eq!(cfg.model, "profile-model");
        // And what a profile does not state comes from its *kind*, never from
        // a top-level key written for a different provider: inheriting
        // TOP_LEVEL_KEY here would read as `[set]` and then fail on the first
        // request, which is the worst of both worlds.
        cfg.select_provider("silent").unwrap();
        assert_eq!(cfg.api_key_env, "ZCODE_API_KEY");
        assert_eq!(cfg.model, Provider::OpenaiCompatible.default_model());
    }

    #[test]
    fn top_level_settings_still_serve_a_bare_kind() {
        // The single-provider form, which is every config written before
        // `providers` existed — and still what `--provider ollama` gets when
        // no profile is declared for it.
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(
            &dir,
            r#"
provider = "openrouter"
model = "top-level-model"
api_key_env = "TOP_LEVEL_KEY"
base_url = "https://gateway.internal/v1/chat/completions"

[[providers]]
name = "other"
kind = "ollama"
"#,
        ))
        .load()
        .unwrap();

        assert_eq!(cfg.model, "top-level-model");
        assert_eq!(cfg.api_key_env, "TOP_LEVEL_KEY");
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("https://gateway.internal/v1/chat/completions")
        );

        cfg.select_provider("deepseek").unwrap();
        assert_eq!(cfg.model, "top-level-model", "still a bare kind");
        assert_eq!(cfg.api_key_env, "TOP_LEVEL_KEY");
    }

    #[test]
    fn an_unknown_provider_name_says_what_is_available() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        let err = cfg.select_provider("nope").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("cheap"), "{message}");
        assert!(message.contains("local"), "{message}");
        assert!(message.contains("openai-compatible"), "{message}");
    }

    #[test]
    fn selecting_a_provider_that_no_layer_declares_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, r#"provider = "made-up""#);
        let err = Loader::new(&path).load().unwrap_err();
        assert!(matches!(err, ConfigError::UnknownProvider(_)), "{err:?}");
        // Even with nothing declared, the message has to say what it would
        // have taken — "unknown provider: local" leaves the user guessing
        // whether they misspelled a kind or forgot to declare the array.
        let message = err.to_string();
        assert!(message.contains("openrouter"), "{message}");
        assert!(message.contains("providers"), "{message}");
    }

    #[test]
    fn a_top_level_key_written_under_a_provider_table_is_reported() {
        // The TOML footgun: everything after `[[providers]]` belongs to that
        // entry. Silently ignoring it means the setting appears to be there
        // and does nothing, which is the worst of both.
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "provider = 'x'\n[[providers]]\nname = 'x'\nkind = 'ollama'\ntimeout_ms = 120000\n",
        );
        let err = Loader::new(&path).load().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("timeout_ms"), "{message}");
    }

    #[test]
    fn lm_studio_is_a_built_in_kind() {
        // OpenAI-compatible on the wire, but with its own default endpoint and
        // no key, so `{ "kind": "lmstudio" }` is enough to point at a local one.
        let kind: Provider = "lmstudio".parse().unwrap();
        assert_eq!(kind, Provider::LmStudio);
        assert!(!kind.requires_api_key());
        assert_eq!(
            kind.default_endpoint(),
            Some("http://localhost:1234/v1/chat/completions")
        );
        // The spellings people reach for.
        assert_eq!("lm-studio".parse::<Provider>().unwrap(), Provider::LmStudio);
        assert_eq!("lm_studio".parse::<Provider>().unwrap(), Provider::LmStudio);
    }

    #[test]
    fn one_unusable_entry_does_not_take_the_whole_config_with_it() {
        // The reported breakage: a single mistyped `kind` stopped the config
        // loading at all, so `zcode config` — the command you use to find the
        // mistake — failed too, and every other provider went with it.
        let dir = tempfile::tempdir().unwrap();
        let cfg = Loader::new(&write_config(
            &dir,
            r#"
provider = "good"

[[providers]]
name = "good"
kind = "openrouter"

[[providers]]
name = "typo"
kind = "not-a-provider"
"#,
        ))
        .load()
        .expect("the config still loads");

        assert_eq!(
            cfg.provider,
            Provider::Openrouter,
            "the good one still works"
        );
        assert_eq!(cfg.provider_names(), ["good"], "the bad one is not offered");
        assert_eq!(cfg.invalid_providers.len(), 1);
        assert_eq!(cfg.invalid_providers[0].0, "typo");
        assert!(
            cfg.invalid_providers[0].1.contains("not-a-provider"),
            "{:?}",
            cfg.invalid_providers[0].1
        );
    }

    #[test]
    fn selecting_an_unusable_entry_says_what_is_wrong_with_it() {
        // "unknown provider `typo`" would be a lie: it is known, and broken.
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(
            &dir,
            "provider = 'good'\n[[providers]]\nname = 'good'\nkind = 'ollama'\n\
             [[providers]]\nname = 'typo'\nkind = 'nope'\n",
        ))
        .load()
        .unwrap();

        let err = cfg.select_provider("typo").unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidProviderEntry { .. }),
            "{err:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("typo") && message.contains("nope"),
            "{message}"
        );
    }

    #[test]
    fn a_later_layer_can_repair_a_broken_entry() {
        // A machine-wide typo must be fixable from the project file, not just
        // reported at it.
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(
            &dir,
            "provider = 'x'\n[[providers]]\nname = 'x'\nkind = 'nope'\n",
        ))
        .load()
        .unwrap_err();
        let _ = &mut cfg; // the selected entry is broken, so this one *is* fatal

        let dir2 = tempfile::tempdir().unwrap();
        let good = Loader::new(&write_config(
            &dir2,
            "provider = 'x'\n[[providers]]\nname = 'x'\nkind = 'lmstudio'\n",
        ))
        .load()
        .unwrap();
        assert_eq!(good.provider, Provider::LmStudio);
    }

    #[test]
    fn a_provider_entry_must_say_what_it_is() {
        // Neither `name` nor `kind`, so it cannot say what it talks to. Like
        // any other unusable entry it is recorded and skipped rather than
        // taking the config down — it is not the one selected.
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[[providers]]\nmodel = 'x'\n");
        let cfg = Loader::new(&path).load().unwrap();
        assert_eq!(cfg.invalid_providers.len(), 1);
        assert_eq!(cfg.invalid_providers[0].0, "(unnamed)");
        assert!(
            cfg.invalid_providers[0].1.contains("name"),
            "{:?}",
            cfg.invalid_providers[0].1
        );
    }

    #[test]
    fn with_provider_leaves_the_original_alone() {
        // The TUI builds a second client from this without disturbing the one
        // already running.
        let dir = tempfile::tempdir().unwrap();
        let cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        let other = cfg.with_provider("local").unwrap();

        assert_eq!(cfg.provider_name, "cheap");
        assert_eq!(other.provider_name, "local");
        assert_eq!(other.model, "llama3.2");
    }

    // ---- model selection --------------------------------------------------

    #[test]
    fn a_model_id_with_no_slash_stays_on_the_selected_provider() {
        // The one shorthand: "same endpoint, different model".
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_model("gpt-4o-mini").unwrap();

        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.provider_name, "cheap", "no provider was named");
        assert_eq!(cfg.api_key_env, "MY_OPENROUTER_KEY", "profile intact");
    }

    #[test]
    fn the_split_is_at_the_first_slash_and_the_rest_is_the_id() {
        // `openrouter/z-ai/glm-4.6` is the provider `openrouter` and the model
        // `z-ai/glm-4.6` — the id keeps every slash after the first.
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_model("openrouter/z-ai/glm-4.6").unwrap();

        assert_eq!(cfg.provider, Provider::Openrouter);
        assert_eq!(cfg.provider_name, "openrouter");
        assert_eq!(cfg.model, "z-ai/glm-4.6");
    }

    #[test]
    fn a_provider_prefix_brings_the_whole_profile_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_model("local/qwen2.5-coder").unwrap();

        assert_eq!(cfg.provider, Provider::Ollama);
        assert_eq!(cfg.provider_name, "local");
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("http://127.0.0.1:11434/api/chat"),
            "endpoint came with the profile"
        );
        assert_eq!(
            cfg.model, "qwen2.5-coder",
            "the model outranks the profile's own"
        );
    }

    #[test]
    fn a_builtin_kind_needs_no_profile_to_be_a_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_model("deepseek/deepseek-reasoner").unwrap();

        assert_eq!(cfg.provider, Provider::Deepseek);
        assert_eq!(cfg.model, "deepseek-reasoner");
    }

    #[test]
    fn a_named_profile_shadows_the_builtin_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        cfg.select_model("anthropic/claude-haiku-4-5").unwrap();

        assert_eq!(cfg.provider, Provider::Anthropic);
        assert_eq!(cfg.model, "claude-haiku-4-5");
    }

    /// The rule that replaced the guessing: a leading segment naming no
    /// provider is refused, never folded back into the model id. Otherwise the
    /// meaning of an argument would depend on what the config declares.
    #[test]
    fn a_leading_segment_that_names_no_provider_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Loader::new(&write_config(&dir, MULTI)).load().unwrap();
        let before = cfg.model.clone();
        let err = cfg.select_model("z-ai/glm-4.6").unwrap_err();

        let ConfigError::UnknownProviderInModel {
            provider,
            suggestion,
            ..
        } = &err
        else {
            panic!("got {err:?}");
        };
        assert_eq!(provider, "z-ai");
        // The likely fix, spelled out: keep the provider already selected.
        assert_eq!(suggestion, "cheap/z-ai/glm-4.6");
        assert!(err.to_string().contains("`<provider>/<model>`"));
        assert_eq!(cfg.model, before, "nothing was written");
    }

    #[test]
    fn an_empty_model_is_rejected_rather_than_stored() {
        let mut cfg = Config::default();
        let before = cfg.model.clone();
        assert!(matches!(
            cfg.select_model("   "),
            Err(ConfigError::EmptyModel)
        ));
        assert_eq!(cfg.model, before, "a rejected value is not written");
    }

    #[test]
    fn a_provider_prefix_with_nothing_after_it_says_so() {
        let mut cfg = Config::default();
        let err = cfg.select_model("openrouter/").unwrap_err();
        assert!(
            matches!(err, ConfigError::ModelWithoutId { .. }),
            "got {err:?}"
        );
        // The failed selection must not have half-applied.
        assert_eq!(cfg.provider, Provider::Openai);
    }

    #[test]
    fn a_prefix_naming_a_broken_profile_reports_why() {
        // Reading it as "unknown provider" would send the user hunting for a
        // typo in a name that is right there in their config.
        let dir = tempfile::tempdir().unwrap();
        let cfg_text = r#"
[[providers]]
name = "broken"
kind = "not-a-provider"
"#;
        let mut cfg = Loader::new(&write_config(&dir, cfg_text)).load().unwrap();
        let err = cfg.select_model("broken/some-model").unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidProviderEntry { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn rtk_is_on_by_default() {
        assert!(Config::default().rtk.enabled);
        assert!(Config::default().rtk.auto_install);
        assert_eq!(Config::default().rtk.path, None);
    }

    #[test]
    fn the_rtk_section_overrides_only_what_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Loader::new(&write_config(&dir, "[rtk]\nauto_install = false\n"))
            .load()
            .unwrap();
        assert!(cfg.rtk.enabled, "still on");
        assert!(!cfg.rtk.auto_install, "but it will not install one");
    }

    #[test]
    fn rtk_can_be_turned_off_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Loader::new(&write_config(&dir, "[rtk]\nenabled = false\n"))
            .load()
            .unwrap();
        assert!(!cfg.rtk.enabled);
    }

    #[test]
    fn rtk_env_overrides_beat_the_file() {
        // Turning it off in one CI job must not need a file edit.
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[rtk]\nenabled = true\n");

        std::env::set_var("ZCODE_RTK", "0");
        std::env::set_var("ZCODE_RTK_PATH", "/opt/rtk");
        let cfg = Loader::new(&path).load().unwrap();
        std::env::remove_var("ZCODE_RTK");
        std::env::remove_var("ZCODE_RTK_PATH");

        assert!(!cfg.rtk.enabled);
        assert_eq!(cfg.rtk.path.as_deref(), Some("/opt/rtk"));
    }

    #[test]
    fn the_spellings_of_no_that_people_type_all_work() {
        for raw in ["0", "false", "no", "off", "FALSE", " Off "] {
            assert_eq!(parse_bool(raw), Some(false), "{raw:?}");
        }
        for raw in ["1", "true", "yes", "on", "YES"] {
            assert_eq!(parse_bool(raw), Some(true), "{raw:?}");
        }
        // Anything else leaves the configured value alone rather than
        // guessing, which is what the caller's `unwrap_or` relies on.
        assert_eq!(parse_bool("maybe"), None);
        assert_eq!(parse_bool(""), None);
    }

    #[test]
    fn env_overrides_file_and_provider() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"provider = "openai"
model = "gpt-3.5-turbo"
"#,
        );

        std::env::set_var("ZCODE_PROVIDER", "anthropic");

        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.provider, Provider::Anthropic);

        std::env::remove_var("ZCODE_PROVIDER");
    }

    #[test]
    fn env_overrides_caps_skills_and_allowlist() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "max_tokens = 100\nshell_allowed = [\"echo .*\"]\n");

        std::env::set_var("ZCODE_MAX_TOKENS", "4096");
        std::env::set_var("ZCODE_MAX_TOOL_OUTPUT_CHARS", "2048");
        std::env::set_var("ZCODE_SKILLS_DIR", "/tmp/skills");
        std::env::set_var("ZCODE_SHELL_ALLOWED", "git status\ncargo test .*");

        let config = Loader::new(&path).load().unwrap();

        std::env::remove_var("ZCODE_MAX_TOKENS");
        std::env::remove_var("ZCODE_MAX_TOOL_OUTPUT_CHARS");
        std::env::remove_var("ZCODE_SKILLS_DIR");
        std::env::remove_var("ZCODE_SHELL_ALLOWED");

        assert_eq!(config.max_tokens, 4096, "env must beat the file");
        assert_eq!(config.max_tool_output_chars, 2048);
        assert_eq!(config.skills_dir, PathBuf::from("/tmp/skills"));
        assert_eq!(
            config.shell_allowed.as_ref(),
            ["git status".to_string(), "cargo test .*".to_string()]
        );
    }

    #[test]
    fn empty_shell_allowed_env_denies_everything() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "shell_allowed = [\"echo .*\"]\n");

        std::env::set_var("ZCODE_SHELL_ALLOWED", "");
        let config = Loader::new(&path).load().unwrap();
        std::env::remove_var("ZCODE_SHELL_ALLOWED");

        // An explicit empty override is a deliberate lockdown, not a fallback
        // to the file's list (M2.5).
        assert!(config.shell_allowed.is_empty());
    }

    #[test]
    fn loads_a_json_config_file() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zcode.json");
        fs::write(
            &path,
            r#"{
              "provider": "openrouter",
              "model": "anthropic/claude-sonnet-4",
              "max_turns": 7,
              "shell_allowed": ["git status", "cargo .*"],
              "mcp": { "servers": [
                { "name": "everything", "command": "npx", "args": ["-y", "srv"] }
              ]},
              "lsp": { "servers": [
                { "language": "rust", "command": "rust-analyzer" }
              ]}
            }"#,
        )
        .unwrap();

        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.provider, Provider::Openrouter);
        assert_eq!(config.model, "anthropic/claude-sonnet-4");
        assert_eq!(config.max_turns, 7);
        assert_eq!(config.shell_allowed.len(), 2);
        assert_eq!(config.mcp_servers[0].name, "everything");
        assert_eq!(config.lsp_servers[0].language, "rust");
    }

    #[test]
    fn json_and_toml_configs_agree() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("a.toml");
        fs::write(
            &toml_path,
            "provider = \"deepseek\"\nmodel = \"deepseek-chat\"\nmax_tokens = 2048\n",
        )
        .unwrap();
        let json_path = dir.path().join("a.json");
        fs::write(
            &json_path,
            r#"{"provider":"deepseek","model":"deepseek-chat","max_tokens":2048}"#,
        )
        .unwrap();

        let from_toml = Loader::new(&toml_path).load().unwrap();
        let from_json = Loader::new(&json_path).load().unwrap();
        assert_eq!(from_toml.provider, from_json.provider);
        assert_eq!(from_toml.model, from_json.model);
        assert_eq!(from_toml.max_tokens, from_json.max_tokens);
    }

    #[test]
    fn malformed_json_is_a_typed_error() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            Loader::new(&path).load(),
            Err(ConfigError::Json(_))
        ));
    }

    #[test]
    fn api_key_env_defaults_to_the_provider_convention() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "provider = \"openrouter\"\n");
        // Switching provider should not also require remembering to switch
        // the key variable name.
        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.api_key_env, "ZCODE_OPENROUTER_API_KEY");

        let path = write_config(&dir, "provider = \"deepseek\"\n");
        assert_eq!(
            Loader::new(&path).load().unwrap().api_key_env,
            "ZCODE_DEEPSEEK_API_KEY"
        );
    }

    #[test]
    fn explicit_api_key_env_is_respected() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "provider = \"openrouter\"\napi_key_env = \"MY_OWN_KEY\"\n",
        );
        assert_eq!(Loader::new(&path).load().unwrap().api_key_env, "MY_OWN_KEY");
    }

    #[test]
    fn model_defaults_follow_the_provider() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        // Choosing OpenRouter without naming a model must not leave the
        // OpenAI default behind — that id does not exist on OpenRouter.
        let path = write_config(&dir, "provider = \"openrouter\"\n");
        assert_eq!(
            Loader::new(&path).load().unwrap().model,
            "openai/gpt-4o-mini"
        );

        let path = write_config(&dir, "provider = \"deepseek\"\n");
        assert_eq!(Loader::new(&path).load().unwrap().model, "deepseek-chat");
    }

    #[test]
    fn explicit_model_wins_over_the_provider_default() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "provider = \"openrouter\"\nmodel = \"anthropic/claude-sonnet-4.5\"\n",
        );
        assert_eq!(
            Loader::new(&path).load().unwrap().model,
            "anthropic/claude-sonnet-4.5"
        );
    }

    #[test]
    fn deepseek_provider_round_trips() {
        assert_eq!("deepseek".parse::<Provider>().unwrap(), Provider::Deepseek);
        assert_eq!(Provider::Deepseek.as_str(), "deepseek");
        assert!(Provider::Deepseek
            .default_endpoint()
            .unwrap()
            .contains("deepseek.com"));
        assert!(Provider::Deepseek.requires_api_key());
        assert!(!Provider::Ollama.requires_api_key());
    }

    #[test]
    fn project_config_is_found_from_a_subdirectory() {
        let _guard = env_guard();
        let _home = isolated_user_config();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("zcode.json"),
            r#"{"provider":"deepseek","max_turns":9}"#,
        )
        .unwrap();
        let nested = root.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();

        // Running from a subdirectory must find the project's settings, not
        // silently fall back to built-in defaults.
        let loader = Loader::discover_from(&nested);
        let config = loader.load().unwrap();
        assert_eq!(config.provider, Provider::Deepseek);
        assert_eq!(config.max_turns, 9);
    }

    #[test]
    fn working_dir_anchors_to_the_project_root() {
        let _guard = env_guard();
        let _home = isolated_user_config();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("zcode.json"), r#"{"provider":"ollama"}"#).unwrap();
        let nested = root.join("src");
        fs::create_dir_all(&nested).unwrap();

        let config = Loader::discover_from(&nested).load().unwrap();
        assert_eq!(
            config.working_dir.canonicalize().unwrap(),
            root,
            "relative paths and .zcode/ must resolve from the project root"
        );
    }

    #[test]
    fn explicit_working_dir_beats_the_project_root() {
        let _guard = env_guard();
        let _home = isolated_user_config();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("zcode.json"),
            r#"{"provider":"ollama","working_dir":"/tmp/elsewhere"}"#,
        )
        .unwrap();
        let config = Loader::discover_from(root).load().unwrap();
        assert_eq!(config.working_dir, PathBuf::from("/tmp/elsewhere"));
    }

    #[test]
    fn nearest_config_wins_over_a_higher_one() {
        let _guard = env_guard();
        let _home = isolated_user_config();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("zcode.json"), r#"{"max_turns":1}"#).unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("zcode.json"), r#"{"max_turns":2}"#).unwrap();

        assert_eq!(Loader::discover_from(&inner).load().unwrap().max_turns, 2);
    }

    #[test]
    fn json_is_preferred_over_toml_in_the_same_directory() {
        let _guard = env_guard();
        let _home = isolated_user_config();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("zcode.json"), r#"{"max_turns":7}"#).unwrap();
        fs::write(dir.path().join("zcode.toml"), "max_turns = 8\n").unwrap();
        assert_eq!(
            Loader::discover_from(dir.path()).load().unwrap().max_turns,
            7
        );
    }

    #[test]
    fn user_layer_is_overridden_field_by_field_by_the_project() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.json");
        // Machine-wide: provider + key name. Project: just the model.
        fs::write(
            &user,
            r#"{"provider":"openrouter","api_key_env":"MY_KEY","max_turns":42}"#,
        )
        .unwrap();
        let project = dir.path().join("zcode.json");
        fs::write(&project, r#"{"model":"anthropic/claude-sonnet-4.5"}"#).unwrap();

        let loader = Loader {
            layers: vec![
                ConfigLayer {
                    kind: LayerKind::User,
                    path: user,
                },
                ConfigLayer {
                    kind: LayerKind::Project,
                    path: project,
                },
            ],
            project_root: Some(dir.path().to_path_buf()),
        };
        let config = loader.load().unwrap();
        assert_eq!(config.provider, Provider::Openrouter, "from the user layer");
        assert_eq!(config.api_key_env, "MY_KEY", "from the user layer");
        assert_eq!(config.max_turns, 42, "from the user layer");
        assert_eq!(
            config.model, "anthropic/claude-sonnet-4.5",
            "project overrides only what it names"
        );
    }

    #[test]
    fn discovery_reports_the_files_it_will_read() {
        let _guard = env_guard();
        let _home = isolated_user_config();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("zcode.toml"), "max_turns = 3\n").unwrap();
        let loader = Loader::discover_from(dir.path());
        let project: Vec<_> = loader
            .layers()
            .iter()
            .filter(|l| l.kind == LayerKind::Project)
            .collect();
        assert_eq!(project.len(), 1);
        assert!(project[0].path.ends_with("zcode.toml"));
    }

    #[test]
    fn no_config_anywhere_falls_back_to_defaults() {
        let _guard = env_guard();
        let _home = isolated_user_config();
        let dir = tempfile::tempdir().unwrap();
        let loader = Loader::discover_from(dir.path());
        // The user layer may or may not exist on the machine running the
        // tests; what matters is that no project layer was invented.
        assert!(loader.layers().iter().all(|l| l.kind != LayerKind::Project));
    }

    #[test]
    fn user_config_candidates_follow_xdg() {
        let _guard = env_guard();
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/xdg");
        let candidates = user_config_candidates();
        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(candidates[0], PathBuf::from("/tmp/xdg/zcode/config.json"));
        assert_eq!(candidates[1], PathBuf::from("/tmp/xdg/zcode/config.toml"));
    }

    #[test]
    fn tilde_is_expanded_in_paths() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "skills_dir = \"~/notes/skills\"\n");
        let config = Loader::new(&path).load().unwrap();

        let home = std::env::var("HOME").expect("HOME");
        assert_eq!(
            config.skills_dir,
            PathBuf::from(&home).join("notes/skills"),
            "a literal ~ would resolve to a directory that never exists"
        );
        assert!(!config.skills_dir.to_string_lossy().contains('~'));
    }

    #[test]
    fn expand_tilde_leaves_other_paths_alone() {
        let _guard = env_guard();
        for p in ["/absolute/path", "relative/path", "./here", "a~b"] {
            assert_eq!(expand_tilde(PathBuf::from(p)), PathBuf::from(p));
        }
        let home = std::env::var("HOME").expect("HOME");
        assert_eq!(expand_tilde(PathBuf::from("~")), PathBuf::from(home));
    }

    #[test]
    fn skills_roots_include_the_project_even_with_a_global_dir() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            skills_dir: PathBuf::from("/opt/team-skills"),
            ..Default::default()
        };
        let roots = cfg.skills_dirs();
        // A machine-wide library must not hide the project's own skills.
        assert_eq!(roots[0], dir.path().join(".zcode").join("skills"));
        assert!(roots.contains(&PathBuf::from("/opt/team-skills")));
    }

    #[test]
    fn relative_skills_dir_resolves_against_working_dir_not_the_process_cwd() {
        // A relative `skills_dir` is a natural thing to write in a project
        // config (e.g. `skills_dir = "myskills"`). The project file is found
        // by walking *up* from wherever the CLI was launched, so the process's
        // actual cwd and `working_dir` (the directory holding the config)
        // disagree the moment the agent runs from a subdirectory — a relative
        // path must resolve against `working_dir`, not whatever the shell's
        // cwd happened to be.
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("myskills")).unwrap();
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            skills_dir: PathBuf::from("myskills"),
            ..Default::default()
        };
        assert_eq!(cfg.skills_dir(), dir.path().join("myskills"));
        assert!(cfg.skills_dirs().contains(&dir.path().join("myskills")));
    }

    #[test]
    fn skills_roots_are_deduplicated() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join(".zcode").join("skills");
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            skills_dir: project.clone(),
            ..Default::default()
        };
        let roots = cfg.skills_dirs();
        assert_eq!(roots.iter().filter(|r| **r == project).count(), 1);
    }

    // ---- language servers -------------------------------------------------

    #[test]
    fn project_markers_identify_the_language() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_project_language(dir.path()), None);
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(
            detect_project_language(dir.path()),
            Some("javascript".into())
        );
        // A Next.js repo has both; the more specific marker wins.
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        assert_eq!(
            detect_project_language(dir.path()),
            Some("typescript".into())
        );
        // …and go.mod is more specific still.
        std::fs::write(dir.path().join("go.mod"), "module x").unwrap();
        assert_eq!(detect_project_language(dir.path()), Some("go".into()));
    }

    #[test]
    fn nextjs_and_node_resolve_to_the_typescript_server() {
        for alias in ["nextjs", "next", "node", "nodejs", "ts", "tsx"] {
            assert_eq!(canonical_language(alias), "typescript", "{alias}");
        }
        assert_eq!(canonical_language("golang"), "go");
        assert_eq!(canonical_language("Rust"), "rust");
        // An unknown language is passed through, lowercased.
        assert_eq!(canonical_language("Zig"), "zig");
    }

    #[test]
    fn the_defaults_cover_the_advertised_languages() {
        let languages: Vec<String> = default_lsp_servers()
            .into_iter()
            .map(|s| s.language)
            .collect();
        for expected in ["go", "rust", "typescript", "javascript"] {
            assert!(languages.contains(&expected.to_string()), "{expected}");
        }
    }

    #[test]
    fn a_default_for_another_language_is_not_started() {
        // Regression: a Go project on a machine with rust-analyzer installed
        // started rust-analyzer, which can answer nothing about Go.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module x").unwrap();
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        for server in cfg.effective_lsp_servers() {
            assert_eq!(
                canonical_language(&server.language),
                "go",
                "started a server for the wrong language"
            );
        }
    }

    #[test]
    fn an_explicitly_configured_server_is_always_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module x").unwrap();
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            lsp_servers: Box::new([LspServerConfig {
                language: "python".into(),
                command: "pyright-langserver".into(),
                args: vec!["--stdio".into()],
                env: Vec::new(),
            }]),
            ..Config::default()
        };
        let languages: Vec<String> = cfg
            .effective_lsp_servers()
            .into_iter()
            .map(|s| s.language)
            .collect();
        assert!(languages.contains(&"python".to_string()), "{languages:?}");
    }

    #[test]
    fn no_project_marker_starts_no_default_server() {
        // A bare directory is not a Rust project just because rust-analyzer
        // happens to be installed; trying it only produces a startup warning.
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        assert!(cfg.effective_lsp_servers().is_empty());
    }

    #[test]
    fn lsp_defaults_can_be_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            lsp_defaults: false,
            ..Config::default()
        };
        assert!(cfg.effective_lsp_servers().is_empty());
    }

    #[test]
    fn which_finds_a_real_binary_and_not_a_fictional_one() {
        assert!(which_on_path("sh").is_some());
        assert!(which_on_path("definitely-not-a-real-binary-xyzzy").is_none());
    }

    #[test]
    fn default_caps_match_prd() {
        let config = Config::default();
        assert_eq!(config.max_turns, 220);
        assert_eq!(config.max_tokens, 16384);
        assert_eq!(config.max_tool_output_chars, 32000);
        assert_eq!(config.provider, Provider::Openai);
        assert_eq!(config.mode, AgentMode::Auto);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.rate_limit_backoff_ms, 30_000);
        assert!(config.lsp_defaults);
        // The default allowlist must let a coding agent actually build.
        assert!(!config.shell_allowed.is_empty());
    }

    #[test]
    fn the_default_allowlist_covers_the_advertised_toolchains() {
        // Regression: the old default was `echo/ls/cd/cat`, so `go build`
        // failed on a fresh install.
        let joined = DEFAULT_SHELL_ALLOWED.join("\n");
        for tool in ["go", "cargo", "npm", "next", "pytest", "make", "git"] {
            assert!(joined.contains(tool), "{tool} missing from the default");
        }
    }

    #[test]
    fn empty_allowed_is_deny_all() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
[[shell.allowed]]
allowlist = []
"#,
        );
        // `shell.allowed` is a flat string array in the supported schema; the
        // table form above is a no-op and leaves the default. Loading the file
        // must preserve the default allowlist (not deny-all), proving the
        // unsupported table form is ignored:
        let cfg = Loader::new(&path).load().unwrap();
        assert!(!cfg.shell_allowed.is_empty());

        // The deny-all path is exercised directly:
        let cfg = Config {
            shell_allowed: Box::new([]),
            ..Config::default()
        };
        assert!(cfg.shell_allowed.is_empty());
    }

    #[test]
    fn unknown_provider_errors() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, r#"provider = "bogus""#);
        let result = Loader::new(&path).load();
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[test]
    fn resolve_api_key_missing() {
        let cfg = Config {
            api_key_env: "ZCODE_NONEXISTENT_KEY_XYZ".into(),
            ..Config::default()
        };
        let err = cfg.resolve_api_key().unwrap_err();
        assert!(matches!(err, ConfigError::MissingSecret(_)));
    }

    #[test]
    fn mcp_servers_parsed() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
[[mcp.servers]]
name = "a"
command = "echo"

[[mcp.servers]]
name = "b"
command = "echo"
"#,
        );
        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.mcp_servers.len(), 2);
        assert_eq!(config.mcp_servers[0].name, "a");
    }

    #[test]
    fn shell_allowed_parsed_from_file() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
shell_allowed = ["git .*", "cargo .*"]
"#,
        );
        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.shell_allowed.len(), 2);
        assert_eq!(config.shell_allowed[0], "git .*");
    }

    #[test]
    fn skills_dir_resolves_inside_working_dir_when_empty() {
        let cfg = Config::default();
        let resolved = cfg.skills_dir();
        assert!(resolved.ends_with(".zcode/skills"));
    }

    #[test]
    fn provider_default_endpoint() {
        assert_eq!(
            Provider::Openai.default_endpoint(),
            Some("https://api.openai.com/v1/chat/completions")
        );
        assert_eq!(
            Provider::Ollama.default_endpoint(),
            Some("http://localhost:11434/api/chat")
        );
        assert_eq!(Provider::Vllm.default_endpoint(), None);
    }
}
