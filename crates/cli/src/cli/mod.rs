//! CLI surface and composition root (FR-IFACE-01..06, FR-MODEL-06, §3.9).
//!
//! This module owns every concrete choice: which provider client to build,
//! which tools exist, where sessions and reports are written. Nothing below
//! the interface layer knows any of it — `app` sees port trait objects only.

pub mod clipboard;
pub mod emit;
pub mod out;

use crate::outln;
pub mod logging;
pub mod tui;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use app::{AgentLoop, App, AppError, ExecutionRequest, NullEmitter};
use domain::{AgentMode, CancelFlag, ImageRef, LogLevel, LoggerPort, SessionStorePort};
use infra_config::{user_config_candidates, which_on_path, Config, LayerKind, Loader, Provider};
use infra_llm::{
    AnthropicLlm, DeepSeekLlm, OllamaLlm, OpenAiLlm, OpenRouterLlm, RetryPolicy, VllmLlm,
};
use infra_session::UuidSessionStore;
use infra_telemetry::{JsonTelemetry, OpencodeTelemetry};
use tools::ToolRegistry;

use emit::PrettyEmitter;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("VERGEN_GIX_SHA", "unknown");
pub const BUILD_PROFILE: &str = env!("VERGEN_BUILD_PROFILE", "unknown");
pub const BUILD_TIME: &str = env!("ZCODE_BUILD_TIME", "unknown");

/// Exit code for a run stopped by Ctrl-C, per shell convention.
const EXIT_INTERRUPTED: u8 = 130;
/// Exit code for a command-line usage error, per convention.
const EXIT_USAGE: u8 = 2;

type CliResult = Result<ExitCode, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Parser)]
#[command(
    name = "zcode",
    version,
    about = "zcode — a lean terminal coding agent",
    long_about = "zcode runs coding tasks against an LLM with native file, \
shell, MCP and LSP tools.\n\nConfigure it with zcode.json or zcode.toml; API keys are read from the environment \
by the name given in `api_key_env`."
)]
pub struct Cli {
    // FR-IFACE-02: no subcommand launches the TUI.
    /// Run without a subcommand to open the interactive TUI.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    // FR-CLI-01
    /// Print the version, git commit, and build profile.
    Version,
    // FR-IFACE-01
    /// Run a single task and exit.
    Run(RunArgs),
    // FR-IFACE-02
    /// Open the interactive TUI.
    Repl(ReplArgs),
    // FR-SESSION-01..05
    /// Create, resume, fork, import, or export saved sessions.
    Session {
        #[command(subcommand)]
        command: SessionCmd,
    },
    /// Inspect the tool registry.
    Tools {
        #[command(subcommand)]
        command: ListCmd,
    },
    /// Show where configuration is read from and what it resolves to.
    Config(ConfigArgs),
    // FR-OUTPUT-09
    /// List the markdown skills the agent can load.
    Skills {
        #[command(subcommand)]
        command: ListCmd,
    },
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// The task, in natural language.
    pub prompt: String,
    // FR-MODEL-08
    /// Attach an image for vision-capable models. Repeatable.
    #[arg(long = "image", value_name = "FILE")]
    pub images: Vec<PathBuf>,
    /// planning (read-only) | editing (edits files) | auto (edits and runs shell).
    #[arg(long, value_parser = parse_mode)]
    pub mode: Option<AgentMode>,
    /// Which provider to use: a name from the `providers` array, or a
    /// built-in kind (openai, anthropic, openrouter, ollama, …).
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// Resume an existing session id.
    #[arg(long)]
    pub session: Option<String>,
    // FR-OUTPUT-01
    /// Stream one JSON object per event to stdout (JSONL).
    #[arg(long)]
    pub json: bool,
    /// Event schema for `--json`: zcode's own flat log, or opencode's
    /// `session.next.*` envelopes for consumers written against opencode.
    #[arg(long = "json-format", value_name = "FORMAT", default_value = "zcode")]
    pub json_format: JsonFormat,
    /// Config file to use instead of ./zcode.json or ./zcode.toml.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    // FR-IFACE-05
    /// Give up after this many seconds and checkpoint the session.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,
}

#[derive(clap::Args)]
pub struct ConfigArgs {
    /// Config file to use instead of ./zcode.json or ./zcode.toml.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct ReplArgs {
    /// planning (read-only) | editing (edits files) | auto (edits and runs shell).
    #[arg(long, value_parser = parse_mode)]
    pub mode: Option<AgentMode>,
    /// Which provider to start on: a name from the `providers` array, or a
    /// built-in kind. Switch again in the TUI with `/provider <name>`.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// Config file to use instead of ./zcode.json or ./zcode.toml.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Resume an existing session id.
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(Subcommand)]
pub enum SessionCmd {
    // FR-SESSION-01
    /// Allocate a new session id.
    Create,
    // FR-SESSION-02
    /// Continue a session. With a prompt it runs headless; without one it
    /// opens the TUI on that session.
    Continue {
        /// The session id to resume.
        id: String,
        /// The next task. Omit it to open the TUI on this session instead.
        prompt: Option<String>,
        /// Stream one JSON object per event to stdout (JSONL).
        #[arg(long)]
        json: bool,
        /// Event schema for `--json`: `zcode` or `opencode`.
        #[arg(long = "json-format", value_name = "FORMAT", default_value = "zcode")]
        json_format: JsonFormat,
    },
    // FR-SESSION-03
    /// Branch a session into an independent copy.
    Fork {
        /// The session id to branch from.
        id: String,
        /// Id for the copy. Omit it to allocate a fresh one.
        #[arg(long = "as", value_name = "NEW_ID")]
        new_id: Option<String>,
    },
    // FR-SESSION-04
    /// Import a session JSON file under a fresh id.
    Import {
        /// Path to a session JSON file, e.g. one produced by `session export`.
        file: PathBuf,
    },
    // FR-SESSION-05
    /// Write a session transcript to a file.
    Export {
        /// The session id to write out.
        id: String,
        /// Destination file.
        #[arg(long = "to", value_name = "FILE")]
        to: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum ListCmd {
    /// List everything available.
    List,
}

/// Which event schema `--json` writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum JsonFormat {
    /// zcode's own flat JSONL: one object per event, documented in the guide.
    Zcode,
    /// opencode's event envelopes (`session.next.*`), for tools written
    /// against opencode's bus.
    Opencode,
}

fn parse_mode(raw: &str) -> Result<AgentMode, String> {
    raw.parse::<AgentMode>()
}

// ---------------------------------------------------------------------------
// Composition root
// ---------------------------------------------------------------------------

/// Bridges `LoggerPort` onto the `log` crate so `RUST_LOG` controls verbosity.
struct StdLogger {
    fields: Vec<(String, String)>,
}

impl StdLogger {
    fn new() -> Self {
        Self { fields: Vec::new() }
    }
}

impl LoggerPort for StdLogger {
    fn log(&self, level: LogLevel, msg: &str) {
        let mut line = String::with_capacity(msg.len() + 16 * self.fields.len());
        line.push_str(msg);
        for (k, v) in &self.fields {
            line.push_str(&format!(" {k}={v}"));
        }
        match level {
            LogLevel::Trace => log::trace!("{line}"),
            LogLevel::Debug => log::debug!("{line}"),
            LogLevel::Info => log::info!("{line}"),
            LogLevel::Warn => log::warn!("{line}"),
            LogLevel::Error => log::error!("{line}"),
        }
    }

    fn with_field(&self, key: &str, value: &str) -> Box<dyn LoggerPort + Send + Sync> {
        let mut fields = self.fields.clone();
        fields.push((key.to_string(), value.to_string()));
        Box::new(StdLogger { fields })
    }
}

/// Build the provider client named by the configuration (FR-MODEL-01..06).
/// An unusable combination is a typed error, never a panic (NFR-REL-02).
pub(crate) fn build_llm(cfg: &Config) -> Result<Box<dyn domain::LlmPort + Send>, AppError> {
    // Local/self-hosted providers are keyless; hosted ones fail fast so the
    // user learns about a missing key before a request is attempted.
    let api_key = if cfg.provider.requires_api_key() {
        cfg.resolve_api_key()
            .map_err(|e| AppError::Config(e.to_string()))?
    } else {
        cfg.resolve_api_key().unwrap_or_default()
    };

    // The HTTP timeout must cover a whole streamed generation.
    let timeout = cfg.timeout();

    let base_url = || -> Result<String, AppError> {
        cfg.base_url.clone().ok_or_else(|| {
            AppError::Config(format!(
                "provider `{}` requires `base_url` in the config file (or ZCODE_BASE_URL)",
                cfg.provider.as_str()
            ))
        })
    };
    let endpoint_or_default = |provider: Provider| -> String {
        cfg.base_url
            .clone()
            .unwrap_or_else(|| provider.default_endpoint().unwrap_or_default().to_string())
    };

    // Every client gets the same retry policy, so a 429 behaves the same way
    // whichever provider issued it.
    let retries = RetryPolicy::default()
        .with_max_retries(cfg.max_retries)
        .with_rate_limit_backoff(cfg.rate_limit_backoff());
    let llm: Box<dyn domain::LlmPort + Send> = match cfg.provider {
        Provider::Openai => {
            let mut client = OpenAiLlm::with_timeout(
                &endpoint_or_default(Provider::Openai),
                &api_key,
                &cfg.model,
                timeout,
            );
            client.set_retry_policy(retries);
            Box::new(client)
        }
        // These three have their own hosts, but `base_url` is documented as an
        // endpoint override and has to work here too — for a gateway, a proxy,
        // or a stub. Hardcoding them meant `zcode config` printed the
        // configured URL while the client quietly used another.
        Provider::Anthropic => {
            let mut client = AnthropicLlm::at(
                &endpoint_or_default(Provider::Anthropic),
                &api_key,
                &cfg.model,
                timeout,
            );
            client.set_retry_policy(retries);
            Box::new(client)
        }
        Provider::Openrouter => {
            let mut client = OpenRouterLlm::at(
                &endpoint_or_default(Provider::Openrouter),
                &api_key,
                &cfg.model,
                timeout,
            );
            client.set_retry_policy(retries);
            Box::new(client)
        }
        Provider::Deepseek => {
            let mut client = DeepSeekLlm::at(
                &endpoint_or_default(Provider::Deepseek),
                &api_key,
                &cfg.model,
                timeout,
            );
            client.set_retry_policy(retries);
            Box::new(client)
        }
        Provider::Ollama => {
            let mut client = OllamaLlm::with_timeout(
                &endpoint_or_default(Provider::Ollama),
                &cfg.model,
                timeout,
            );
            client.set_retry_policy(retries);
            Box::new(client)
        }
        // LM Studio speaks the OpenAI wire format, so it shares the client;
        // only its default endpoint differs.
        Provider::Vllm | Provider::OpenaiCompatible | Provider::LmStudio => {
            let mut client = VllmLlm::with_timeout(&base_url()?, &api_key, &cfg.model, timeout);
            client.set_retry_policy(retries);
            Box::new(client)
        }
    };
    Ok(llm)
}

/// Wire the whole agent: provider client, tool registry (native + MCP + LSP),
/// session store, and telemetry.
///
/// `telemetry_out` is stdout for `--json` and a sink otherwise; the report
/// file is written either way (FR-OUTPUT-02).
pub fn wire(cfg: &Config, telemetry_out: Box<dyn Write + Send>) -> Result<App, AppError> {
    wire_with_format(cfg, telemetry_out, JsonFormat::Zcode)
}

/// As [`wire`], choosing which event schema is written to `telemetry_out`.
///
/// The run report is written either way: it is a record of what happened, not
/// a rendering choice, so it does not follow `--json-format`.
pub fn wire_with_format(
    cfg: &Config,
    telemetry_out: Box<dyn Write + Send>,
    format: JsonFormat,
) -> Result<App, AppError> {
    let llm = build_llm(cfg)?;

    let registry = ToolRegistry::from_config(cfg).map_err(|e| AppError::Config(e.to_string()))?;
    // MCP/LSP servers that would not start are reported, not fatal (FR-MCP-05).
    for warning in registry.warnings() {
        log::warn!("{warning}");
    }

    let ag_dir = cfg.working_dir.join(".zcode");
    let sessions = UuidSessionStore::new(ag_dir.join("sessions"));
    let telemetry: Box<dyn domain::TelemetryPort + Send> = match format {
        JsonFormat::Zcode => Box::new(JsonTelemetry::new(telemetry_out, ag_dir.join("reports"))),
        // opencode's stream goes to stdout; the report still needs writing, so
        // the standard emitter runs alongside it over a sink.
        JsonFormat::Opencode => Box::new(TeeTelemetry {
            stream: Box::new(OpencodeTelemetry::new(telemetry_out)),
            report: Box::new(JsonTelemetry::new(
                Box::new(std::io::sink()),
                ag_dir.join("reports"),
            )),
        }),
    };

    Ok(App::new(
        llm,
        Box::new(registry),
        Box::new(sessions),
        telemetry,
        Box::new(StdLogger::new()),
    )
    .with_pricing(cfg.price_table()))
}

/// Sends every event to two ports. Only `report` writes the report file, so
/// the run leaves exactly one on disk however the stream is rendered.
struct TeeTelemetry {
    stream: Box<dyn domain::TelemetryPort + Send>,
    report: Box<dyn domain::TelemetryPort + Send>,
}

impl domain::TelemetryPort for TeeTelemetry {
    fn emit(&mut self, ev: domain::TelemetryEvent) {
        self.stream.emit(ev.clone());
        self.report.emit(ev);
    }

    fn flush_report(
        &mut self,
        session_id: &str,
        total: domain::TelemetryTotals,
    ) -> Result<PathBuf, domain::BoxError> {
        self.report.flush_report(session_id, total)
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() -> CliResult {
    logging::init();
    // `--help` and `--version` arrive as clap "errors". They are successful
    // output, not failures: print them as clap intends and exit 0, rather than
    // routing them through the error handler with a `zcode:` prefix and exit 1.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return Ok(if e.use_stderr() {
                ExitCode::from(EXIT_USAGE)
            } else {
                ExitCode::SUCCESS
            });
        }
    };
    match cli.command {
        None => {
            let cfg = load_config(None, None, None)?;
            let cancel = install_signal_handler();
            tui::run_tui(cfg, cancel, None)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Commands::Version) => {
            outln!("zcode v{VERSION} (git: {GIT_SHA}, built: {BUILD_TIME}, {BUILD_PROFILE})");
            Ok(ExitCode::SUCCESS)
        }
        Some(Commands::Run(args)) => cmd_run(args),
        Some(Commands::Repl(args)) => {
            let cfg = load_config(args.config.as_deref(), args.mode, args.provider.as_deref())?;
            let cancel = install_signal_handler();
            tui::run_tui(cfg, cancel, args.session)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Commands::Session { command }) => cmd_session(command),
        Some(Commands::Config(args)) => cmd_config(args),
        Some(Commands::Tools { command: _ }) => cmd_tools_list(),
        Some(Commands::Skills { command: _ }) => cmd_skills_list(),
    }
}

/// One line on rtk for `zcode config`: what it found, or why it did not.
fn describe_rtk(cfg: &infra_config::RtkConfig) -> String {
    if !cfg.enabled {
        return "off (rtk.enabled = false)".to_string();
    }
    match tools::Rtk::detect(cfg.path.as_deref()) {
        Some(found) => format!(
            "{} — shell output is token-optimised  [{}]",
            found.version(),
            found.path().display()
        ),
        None if cfg.path.is_some() => {
            format!(
                "NOT FOUND at the configured rtk.path — {}",
                tools::rtk::MANUAL_INSTALL_HINT
            )
        }
        None if cfg.auto_install => "not installed — will be installed on the next run".to_string(),
        None => format!("not installed — {}", tools::rtk::MANUAL_INSTALL_HINT),
    }
}

fn load_config(
    path: Option<&Path>,
    mode: Option<AgentMode>,
    provider: Option<&str>,
) -> Result<Config, Box<dyn std::error::Error + Send + Sync>> {
    let mut cfg = Loader::with_default().load_with_override(path)?;
    // A CLI flag beats both the file and the environment (FR-MODE-01/02).
    if let Some(mode) = mode {
        cfg.mode = mode;
    }
    // Re-resolves model, key variable and URL from the chosen profile, so
    // `--provider local` is one word rather than four overrides.
    if let Some(name) = provider {
        cfg.select_provider(name)?;
    }
    Ok(cfg)
}

/// Flip a shared flag on SIGINT so the engine can checkpoint and exit cleanly
/// instead of dying mid-write (FR-IFACE-05, DQ12).
fn install_signal_handler() -> CancelFlag {
    let flag = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone()) {
            log::warn!("could not install SIGINT handler: {e}");
        }
    }
    CancelFlag::from_shared(flag)
}

fn cmd_run(args: RunArgs) -> CliResult {
    let cfg = load_config(args.config.as_deref(), args.mode, args.provider.as_deref())?;
    let cancel = install_signal_handler();

    let telemetry_out: Box<dyn Write + Send> = if args.json {
        Box::new(std::io::stdout())
    } else {
        Box::new(std::io::sink())
    };
    let mut app = wire_with_format(&cfg, telemetry_out, args.json_format)?;
    app.set_cancel(cancel);
    if args.json {
        // JSONL is the output; a second pretty layer would corrupt it.
        app.set_emitter(Box::new(NullEmitter));
    } else {
        app.set_emitter(Box::new(PrettyEmitter::new(std::io::stdout())));
    }

    let mut images = Vec::with_capacity(args.images.len());
    for path in &args.images {
        images.push(load_image(path)?);
    }

    let mut req = ExecutionRequest::new(args.prompt);
    req.mode = cfg.mode;
    req.session_id = args.session;
    req.images = images.into_boxed_slice();
    req.max_turns = cfg.max_turns;
    req.max_tokens = cfg.max_tokens;
    req.max_tool_output_chars = cfg.max_tool_output_chars;
    req.timeout_ms = args.timeout.map(|s| s.saturating_mul(1_000));

    let ctx = cfg.to_agent_context();
    match app.execute(&ctx, req) {
        Ok(result) => {
            if !args.json {
                // Summary on stderr keeps stdout to the model's answer.
                eprintln!(
                    "\n[{} step(s) · {} in / {} out / {} cached tokens · {} · session {}]",
                    result.steps,
                    result.input_tokens,
                    result.output_tokens,
                    result.cache_tokens,
                    result.cost.render(),
                    result.session_id
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(AppError::Interrupted) => {
            eprintln!("\ninterrupted — session checkpointed");
            Ok(ExitCode::from(EXIT_INTERRUPTED))
        }
        Err(e) => Err(Box::new(e)),
    }
}

fn cmd_session(command: SessionCmd) -> CliResult {
    let cfg = load_config(None, None, None)?;
    let mut store = UuidSessionStore::new(cfg.working_dir.join(".zcode").join("sessions"));

    match command {
        SessionCmd::Create => {
            outln!("{}", store.create()?);
        }
        SessionCmd::Continue {
            id,
            prompt,
            json,
            json_format,
        } => {
            return match prompt {
                Some(prompt) => cmd_run(RunArgs {
                    prompt,
                    images: Vec::new(),
                    mode: None,
                    provider: None,
                    session: Some(id),
                    json,
                    json_format,
                    config: None,
                    timeout: None,
                }),
                None => {
                    let cancel = install_signal_handler();
                    tui::run_tui(cfg, cancel, Some(id))?;
                    Ok(ExitCode::SUCCESS)
                }
            };
        }
        SessionCmd::Fork { id, new_id } => {
            // Without an explicit id, allocate a fresh one so the child is a
            // real, loadable session rather than a name the store rejects.
            let new_id = match new_id {
                Some(id) => id,
                None => store.create()?,
            };
            store.fork(&id, &new_id)?;
            outln!("{new_id}");
        }
        SessionCmd::Import { file } => {
            outln!("{}", store.import_from(&file)?);
        }
        SessionCmd::Export { id, to } => {
            store.export_to(&id, &to)?;
            outln!("{}", to.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `zcode config` — which files were consulted, and what they add up to.
/// Secrets are never printed: only whether the named variable resolves.
fn cmd_config(args: ConfigArgs) -> CliResult {
    let loader = Loader::with_default();
    let cfg = loader.load_with_override(args.config.as_deref())?;

    outln!("Config sources (later overrides earlier)");
    let layers = loader.layers();
    if args.config.is_some() {
        // `load_with_override` builds its own chain; show what it used.
        for path in user_config_candidates().into_iter().filter(|p| p.exists()) {
            outln!("  {:<9} {}", "user", path.display());
        }
        if let Some(path) = &args.config {
            outln!("  {:<9} {}  (--config)", "explicit", path.display());
        }
    } else if layers.is_empty() {
        outln!("  (none found — built-in defaults are in use)");
    } else {
        for layer in layers {
            let kind = match layer.kind {
                LayerKind::User => "user",
                LayerKind::Project => "project",
                LayerKind::Explicit => "explicit",
            };
            outln!("  {:<9} {}", kind, layer.path.display());
        }
    }

    outln!("\nSearch paths");
    let candidates = user_config_candidates();
    if candidates.is_empty() {
        outln!("  user      (no HOME or XDG_CONFIG_HOME set)");
    } else {
        for path in &candidates {
            let mark = if path.exists() { "found" } else { "not found" };
            outln!("  user      {}  [{mark}]", path.display());
        }
    }
    outln!("  project   zcode.json, then zcode.toml — searched upward from the");
    outln!("            current directory to the filesystem root");

    let key_state = match cfg.resolve_api_key() {
        Ok(_) => "set",
        Err(_) => "NOT SET",
    };
    let base_url = cfg.base_url.clone().unwrap_or_else(|| {
        cfg.provider
            .default_endpoint()
            .map(|e| format!("{e} (provider default)"))
            .unwrap_or_else(|| "(required for this provider)".to_string())
    });

    outln!("\nEffective configuration");
    // When a profile was selected, its name is what the user typed and the
    // kind is what it speaks — showing only one of them would leave them
    // guessing which endpoint they are actually pointed at.
    if cfg.provider_name == cfg.provider.as_str() {
        outln!("  {:<22} {}", "provider", cfg.provider.as_str());
    } else {
        outln!(
            "  {:<22} {}  ({})",
            "provider",
            cfg.provider_name,
            cfg.provider.as_str()
        );
    }
    outln!("  {:<22} {}", "model", cfg.model);
    outln!("  {:<22} {}  [{key_state}]", "api_key_env", cfg.api_key_env);
    outln!("  {:<22} {}", "endpoint", base_url);
    // Every endpoint the user can switch to, in the same shape as the LSP
    // list below: the key, then one indented row each.
    if !cfg.providers.is_empty() {
        outln!(
            "  {:<22} {}  (--provider NAME, or /provider NAME in the TUI)",
            "providers",
            cfg.providers.len()
        );
        for profile in cfg.providers.iter() {
            let marker = if profile.name == cfg.provider_name {
                "▸"
            } else {
                " "
            };
            let model = profile
                .settings
                .model
                .clone()
                .unwrap_or_else(|| profile.kind.default_model().to_string());
            let endpoint = profile
                .settings
                .base_url
                .clone()
                .or_else(|| profile.kind.default_endpoint().map(str::to_string))
                .unwrap_or_else(|| "NO ENDPOINT — set base_url".to_string());
            outln!(
                "    {marker} {:<12} {:<18} {}",
                profile.name,
                profile.kind.as_str(),
                if model.is_empty() {
                    "(no default model)"
                } else {
                    &model
                }
            );
            outln!("      {:<12} {endpoint}", "");
        }
    }
    outln!("  {:<22} {}", "working_dir", cfg.working_dir.display());
    outln!("  {:<22} {}", "mode", cfg.mode.as_str());
    outln!("  {:<22} {}", "timeout_ms", cfg.timeout_ms);
    outln!("  {:<22} {}", "max_turns", cfg.max_turns);
    outln!("  {:<22} {}", "max_tokens", cfg.max_tokens);
    outln!(
        "  {:<22} {}",
        "max_tool_output_chars",
        cfg.max_tool_output_chars
    );
    outln!("  {:<22} {}", "max_retries", cfg.max_retries);
    outln!(
        "  {:<22} {}ms  (after a 429 with no Retry-After)",
        "rate_limit_backoff_ms",
        cfg.rate_limit_backoff_ms
    );
    outln!("  {:<22} {}", "skills_dir", cfg.skills_dir().display());
    outln!(
        "  {:<22} {} pattern(s){}",
        "shell_allowed",
        cfg.shell_allowed.len(),
        if cfg.shell_allowed.is_empty() {
            " — every shell command is denied"
        } else if tools::allowlist_is_unrestricted(&cfg.shell_allowed) {
            " — unrestricted: anything the denylist permits, pipes and `&&` included"
        } else {
            ""
        }
    );
    outln!(
        "  {:<22} {} built-in + {} from config",
        "shell_denied",
        tools::builtin_deny_rule_count(),
        cfg.shell_denied.len()
    );
    // Whether shell output is being shrunk before it reaches the model is a
    // fact about every future token bill, so it is stated rather than implied.
    outln!("  {:<22} {}", "rtk", describe_rtk(&cfg.rtk));
    outln!("  {:<22} {}", "mcp servers", cfg.mcp_servers.len());

    // LSP is on by default, so say which servers that actually resolved to —
    // "2 configured" is useless when the interesting number is what starts.
    let lsp = cfg.effective_lsp_servers();
    let detected = cfg.detected_language();
    outln!(
        "  {:<22} {}{}",
        "lsp servers",
        lsp.len(),
        match &detected {
            Some(language) => format!("  (project looks like {language})"),
            None => String::new(),
        }
    );
    for (i, server) in lsp.iter().enumerate() {
        let marker = if i == 0 { "▸" } else { " " };
        let source = if cfg
            .lsp_servers
            .iter()
            .any(|s| s.language == server.language && s.command == server.command)
        {
            "config"
        } else {
            "default"
        };
        let path = which_on_path(&server.command)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("{} — NOT on PATH", server.command));
        outln!("    {marker} {:<12} {path}  [{source}]", server.language);
    }
    if lsp.is_empty() && cfg.lsp_defaults {
        outln!("    (none — no language server for this project is installed)");
    }

    let rates = cfg.price_table();
    match rates.lookup(&cfg.model) {
        Some(entry) => outln!(
            "  {:<22} ${}/${} per Mtok in/out (matched `{}`)",
            "pricing",
            entry.input_per_mtok,
            entry.output_per_mtok,
            entry.model
        ),
        // Ask the table rather than assuming: a `:free` route has no entry but
        // is still priced, and reporting it as unknown here would contradict
        // the `$0.00` every run prints.
        None if rates.knows(&cfg.model) => {
            outln!("  {:<22} free route — cost will show as $0.00", "pricing")
        }
        None => outln!(
            "  {:<22} no rate for `{}` — cost will show as n/a",
            "pricing",
            cfg.model
        ),
    }

    // Catch the mistakes that would otherwise only surface on the first run.
    let mut problems: Vec<String> = Vec::new();
    for (name, reason) in &cfg.invalid_providers {
        problems.push(format!(
            "provider entry `{name}` is unusable and was skipped: {reason}"
        ));
    }
    if let Err(e) = tools::GuardedShell::with_denylist(
        infra_shell::StdShell::new(),
        &cfg.shell_allowed,
        &cfg.shell_denied,
    ) {
        problems.push(e.to_string());
    }
    if cfg.provider.requires_api_key() && cfg.resolve_api_key().is_err() {
        problems.push(format!(
            "{} is not set — export it, or point `api_key_env` at the variable you use",
            cfg.api_key_env
        ));
    }
    if cfg.model.is_empty() {
        problems.push(format!(
            "no model set, and provider `{}` has no default — add \"model\" to your config",
            cfg.provider.as_str()
        ));
    }
    let skills = cfg.skills_dir();
    if !cfg.skills_dir.as_os_str().is_empty() && !skills.exists() {
        problems.push(format!("skills_dir does not exist: {}", skills.display()));
    }

    if problems.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    outln!("\nProblems");
    for problem in &problems {
        for (i, line) in problem.lines().enumerate() {
            if i == 0 {
                outln!("  - {line}");
            } else {
                outln!("    {line}");
            }
        }
    }
    // Non-zero so CI notices a config that cannot run.
    Ok(ExitCode::FAILURE)
}

fn cmd_tools_list() -> CliResult {
    let cfg = load_config(None, None, None)?;
    // No LLM is constructed here, so `zcode tools list` works without an API key.
    let registry = ToolRegistry::from_config(&cfg)?;
    for warning in registry.warnings() {
        eprintln!("warning: {warning}");
    }
    for spec in domain::ToolRegistryPort::list(&registry).iter() {
        // A description may run to several lines (the skill tool lists its
        // catalogue); keep this listing to one line per tool.
        let summary = spec
            .description
            .lines()
            .next()
            .unwrap_or_default()
            .trim_end();
        outln!("{:<28} {}", spec.name, summary);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_skills_list() -> CliResult {
    let cfg = load_config(None, None, None)?;
    let roots = cfg.skills_dirs();
    let index = tools::SkillIndex::discover(&roots);

    outln!("Searched");
    for root in &roots {
        let mark = if root.is_dir() { "" } else { "  (missing)" };
        outln!("  {}{}", root.display(), mark);
    }

    if index.is_empty() {
        outln!("\nNo skills found.");
        outln!("A skill is a markdown file in one of those directories, either");
        outln!("`<name>.md` or `<name>/SKILL.md`. Create one and the agent can load it:");
        outln!("\n  mkdir -p {}", roots[0].display());
        outln!(
            "  printf '# House style\\n\\nUse doc comments.\\n' > {}/style.md",
            roots[0].display()
        );
        return Ok(ExitCode::SUCCESS);
    }

    outln!("\n{} skill(s) offered to the model", index.entries().len());
    let width = index
        .entries()
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0)
        .min(32);
    for entry in index.entries() {
        outln!("  {:<width$}  {}", entry.name, entry.summary, width = width);
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Vision input
// ---------------------------------------------------------------------------

fn load_image(path: &Path) -> Result<ImageRef, Box<dyn std::error::Error + Send + Sync>> {
    let bytes = std::fs::read(path)?;
    Ok(ImageRef {
        mime: mime_for(path).to_string(),
        data: base64_encode(&bytes),
    })
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

/// Standard base64 with padding. Hand-rolled to keep the dependency budget
/// intact — this is the only place the agent needs it.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_command_parses() {
        let cli = Cli::try_parse_from(["zcode", "version"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Version)));
    }

    #[test]
    fn bare_command_opens_the_tui() {
        let cli = Cli::try_parse_from(["zcode"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn run_parses_every_flag() {
        let cli = Cli::try_parse_from([
            "zcode",
            "run",
            "rename foo to bar",
            "--mode",
            "planning",
            "--json",
            "--timeout",
            "10",
            "--session",
            "abc",
            "--image",
            "a.png",
            "--image",
            "b.png",
        ])
        .unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected run");
        };
        assert_eq!(args.prompt, "rename foo to bar");
        assert_eq!(args.mode, Some(AgentMode::Planning));
        assert!(args.json);
        assert_eq!(args.timeout, Some(10));
        assert_eq!(args.session.as_deref(), Some("abc"));
        assert_eq!(args.images.len(), 2);
    }

    #[test]
    fn run_defaults_leave_mode_to_the_config() {
        let cli = Cli::try_parse_from(["zcode", "run", "hello"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected run");
        };
        assert!(args.mode.is_none());
        assert!(!args.json);
    }

    #[test]
    fn invalid_mode_is_rejected() {
        assert!(Cli::try_parse_from(["zcode", "run", "x", "--mode", "wat"]).is_err());
    }

    /// `--help`/`--version` are successful output, and clap reports them as
    /// errors; they must not be printed as failures or exit non-zero.
    #[test]
    fn help_and_version_are_not_failures() {
        for args in [
            vec!["zcode", "--help"],
            vec!["zcode", "--version"],
            vec!["zcode", "run", "--help"],
        ] {
            let Err(err) = Cli::try_parse_from(&args) else {
                panic!("clap reports {args:?} as an error");
            };
            assert!(
                !err.use_stderr(),
                "{args:?} should print to stdout as success"
            );
        }
        // A genuine usage error still goes to stderr.
        let Err(err) = Cli::try_parse_from(["zcode", "run"]) else {
            panic!("a missing prompt must be rejected");
        };
        assert!(err.use_stderr());
    }

    #[test]
    fn session_subcommands_parse() {
        let cli = Cli::try_parse_from(["zcode", "session", "create"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Session {
                command: SessionCmd::Create
            })
        ));

        let cli = Cli::try_parse_from(["zcode", "session", "fork", "id1", "--as", "id2"]).unwrap();
        let Some(Commands::Session {
            command: SessionCmd::Fork { id, new_id },
        }) = cli.command
        else {
            panic!("expected fork");
        };
        assert_eq!(id, "id1");
        assert_eq!(new_id.as_deref(), Some("id2"));

        let cli =
            Cli::try_parse_from(["zcode", "session", "export", "id1", "--to", "out.json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Session {
                command: SessionCmd::Export { .. }
            })
        ));
    }

    #[test]
    fn config_command_parses() {
        assert!(matches!(
            Cli::try_parse_from(["zcode", "config"]).unwrap().command,
            Some(Commands::Config(_))
        ));
        let cli = Cli::try_parse_from(["zcode", "config", "--config", "other.json"]).unwrap();
        let Some(Commands::Config(args)) = cli.command else {
            panic!("expected config");
        };
        assert_eq!(args.config.as_deref(), Some(Path::new("other.json")));
    }

    #[test]
    fn tools_and_skills_list_parse() {
        assert!(matches!(
            Cli::try_parse_from(["zcode", "tools", "list"])
                .unwrap()
                .command,
            Some(Commands::Tools { .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["zcode", "skills", "list"])
                .unwrap()
                .command,
            Some(Commands::Skills { .. })
        ));
    }

    #[test]
    fn build_metadata_is_embedded() {
        // The values are compile-time constants; this asserts they exist and
        // that the `version` line can be formatted from them (FR-CLI-01/03).
        let line =
            format!("zcode v{VERSION} (git: {GIT_SHA}, built: {BUILD_TIME}, {BUILD_PROFILE})");
        assert!(line.starts_with("zcode v"));
        assert!(line.contains("git: "));
        assert!(line.contains("built: "));
    }

    /// FR-MODEL-06: every known provider constructs; a missing key is a typed
    /// error rather than a panic.
    #[test]
    fn wire_dispatches_each_provider() {
        let dir = tempfile::tempdir().unwrap();
        for provider in [
            Provider::Openai,
            Provider::Anthropic,
            Provider::Openrouter,
            Provider::Deepseek,
            Provider::Ollama,
            Provider::Vllm,
            Provider::OpenaiCompatible,
        ] {
            let cfg = Config {
                provider,
                working_dir: dir.path().to_path_buf(),
                // A name that exists so no test depends on ambient env vars.
                api_key_env: "PATH".into(),
                base_url: Some("http://localhost:9999/v1".into()),
                ..Default::default()
            };
            assert!(
                wire(&cfg, Box::new(std::io::sink())).is_ok(),
                "provider {} failed to wire",
                provider.as_str()
            );
        }
    }

    #[test]
    fn missing_api_key_is_a_typed_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            provider: Provider::Openai,
            working_dir: dir.path().to_path_buf(),
            api_key_env: "ZCODE_DEFINITELY_NOT_SET_XYZ".into(),
            ..Default::default()
        };
        let Err(err) = wire(&cfg, Box::new(std::io::sink())) else {
            panic!("expected a config error");
        };
        assert!(matches!(err, AppError::Config(_)), "got {err:?}");
    }

    #[test]
    fn base_url_providers_require_a_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            provider: Provider::Vllm,
            working_dir: dir.path().to_path_buf(),
            base_url: None,
            ..Default::default()
        };
        let Err(err) = wire(&cfg, Box::new(std::io::sink())) else {
            panic!("expected a config error");
        };
        assert!(err.to_string().contains("base_url"), "got {err}");
    }

    #[test]
    fn wire_rejects_an_invalid_shell_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            provider: Provider::Ollama,
            working_dir: dir.path().to_path_buf(),
            shell_allowed: Box::new(["(unclosed".into()]),
            ..Default::default()
        };
        assert!(wire(&cfg, Box::new(std::io::sink())).is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn image_mime_is_derived_from_the_extension() {
        assert_eq!(mime_for(Path::new("a.PNG")), "image/png");
        assert_eq!(mime_for(Path::new("a.webp")), "image/webp");
        assert_eq!(mime_for(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(mime_for(Path::new("noext")), "image/jpeg");
    }

    #[test]
    fn images_load_as_base64_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, b"foo").unwrap();
        let image = load_image(&path).unwrap();
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.data, "Zm9v");
    }
}
