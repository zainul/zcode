//! Slash commands typed into the prompt.
//!
//! Parsing is separated from execution so the whole command surface is
//! testable without a terminal, and so an unknown `/word` can be reported as a
//! mistake instead of being silently sent to the model as a prompt — which is
//! what made `/exit` look like it "didn't work" before it existed.

use domain::AgentMode;

/// A parsed prompt-line command.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    /// Leave the TUI.
    Exit,
    /// Show the command list.
    Help,
    /// Switch mode, or cycle to the next when no argument is given.
    Mode(Option<AgentMode>),
    /// Empty both panes.
    Clear,
    /// Start a fresh session, dropping the conversation context.
    New,
    /// Show the running token/cost breakdown.
    Cost,
    /// Show provider, model, and where the config came from.
    Model,
    /// List the configured providers, or switch to one of them.
    Provider(Option<String>),
    /// Show the current session id.
    Session,
    /// List the tools the model can call in the current mode.
    Tools,
    /// Cancel the turn in flight.
    Stop,
    /// A `/word` that is not a command — reported, not sent to the model.
    Unknown(String),
}

/// Every command, with its help text. Single source for `/help` and the docs.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "show this list"),
    ("/exit", "quit zcode (also /quit, or Ctrl-C)"),
    (
        "/mode [planning|editing|auto]",
        "show or change what the agent is allowed to do",
    ),
    ("/cost", "token usage and estimated spend for this session"),
    ("/model", "provider, model, and config source"),
    (
        "/provider [NAME]",
        "list the configured providers, or switch to one",
    ),
    ("/session", "current session id and where it is stored"),
    ("/tools", "tools available in the current mode"),
    ("/new", "start a fresh session (clears the model's context)"),
    ("/clear", "clear the screen, keep the session"),
    ("/stop", "cancel the turn in flight (also Esc)"),
];

/// Keys worth telling the user about, for `/help`.
pub const KEYS: &[(&str, &str)] = &[
    ("Enter", "send"),
    ("Alt-Enter", "newline without sending"),
    ("Esc", "cancel the turn in flight, else clear the prompt"),
    ("Ctrl-C", "cancel if busy, otherwise quit"),
    ("Ctrl-A / Ctrl-E", "start / end of line"),
    ("Ctrl-W", "delete the previous word"),
    ("Ctrl-U / Ctrl-K", "delete to start / end of line"),
    (
        "PageUp / PageDown",
        "scroll the conversation (also the mouse wheel)",
    ),
    ("Ctrl-Up / Ctrl-Down", "scroll one line"),
    ("Shift-Tab", "cycle mode"),
];

/// Told to the user once, in `/help`: the mouse wheel scrolls because zcode
/// asks the terminal for mouse events, and a terminal that is reporting the
/// mouse no longer selects text with it. Every terminal keeps a modifier for
/// the old behaviour, and not saying so makes it look like copy/paste broke.
pub const MOUSE_NOTE: &str =
    "the mouse wheel scrolls; hold Shift (Option on macOS Terminal) to select text";

/// Parse a prompt line. `None` means "this is a prompt, send it".
pub fn parse(line: &str) -> Option<SlashCommand> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('/')?;
    // A lone "/" is a typo, not a command; and "/usr/local/bin" is a path
    // someone is asking about, not a command — both fall through to the model
    // only when they contain a separator, which no command does.
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let name = parts.next()?.to_ascii_lowercase();
    let arg = parts.next();
    Some(match name.as_str() {
        "exit" | "quit" | "q" => SlashCommand::Exit,
        "help" | "?" | "h" => SlashCommand::Help,
        "mode" | "m" => match arg {
            Some(raw) => match raw.parse::<AgentMode>() {
                Ok(mode) => SlashCommand::Mode(Some(mode)),
                Err(_) => SlashCommand::Unknown(format!("/mode {raw}")),
            },
            None => SlashCommand::Mode(None),
        },
        "clear" | "cls" => SlashCommand::Clear,
        "new" | "reset" => SlashCommand::New,
        "cost" | "usage" | "tokens" => SlashCommand::Cost,
        "model" => SlashCommand::Model,
        // `/provider` on its own lists; with a name it switches. The name is
        // taken verbatim — profiles are named by the user, so validating the
        // spelling here would mean duplicating the config's own lookup.
        "provider" | "providers" | "p" => SlashCommand::Provider(arg.map(|a| a.trim().to_string())),
        "session" | "sessions" => SlashCommand::Session,
        "tools" => SlashCommand::Tools,
        "stop" | "cancel" => SlashCommand::Stop,
        other => SlashCommand::Unknown(format!("/{other}")),
    })
}

/// The `/help` body, rendered as transcript lines.
pub fn help_lines() -> Vec<String> {
    let mut out = vec!["commands:".to_string()];
    for (name, description) in COMMANDS {
        out.push(format!("  {name:<32} {description}"));
    }
    out.push(String::new());
    out.push("keys:".to_string());
    for (keys, description) in KEYS {
        out.push(format!("  {keys:<32} {description}"));
    }
    out.push(String::new());
    out.push(format!("  {MOUSE_NOTE}"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_has_the_spellings_people_try() {
        for line in ["/exit", "/quit", "/q", "  /EXIT  "] {
            assert_eq!(parse(line), Some(SlashCommand::Exit), "{line}");
        }
    }

    #[test]
    fn provider_lists_without_an_argument_and_switches_with_one() {
        for line in ["/provider", "/providers", "/p", "  /PROVIDER  "] {
            assert_eq!(parse(line), Some(SlashCommand::Provider(None)), "{line}");
        }
        assert_eq!(
            parse("/provider local"),
            Some(SlashCommand::Provider(Some("local".into())))
        );
        // Profile names are the user's own words, so the argument is taken
        // verbatim rather than validated against a list this module cannot see.
        assert_eq!(
            parse("/provider My-Gateway"),
            Some(SlashCommand::Provider(Some("My-Gateway".into())))
        );
    }

    #[test]
    fn model_no_longer_doubles_as_provider() {
        // `/model` used to accept "provider" as an alias; it now has its own
        // command that can switch, and conflating them would make `/provider
        // local` silently print the current model instead.
        assert_eq!(parse("/model"), Some(SlashCommand::Model));
        assert!(matches!(
            parse("/provider"),
            Some(SlashCommand::Provider(None))
        ));
    }

    #[test]
    fn plain_text_is_a_prompt() {
        assert_eq!(parse("fix the failing test"), None);
        assert_eq!(parse(""), None);
        // A bare slash is not a command.
        assert_eq!(parse("/"), None);
    }

    #[test]
    fn a_path_is_not_mistaken_for_a_command() {
        // "what is in /usr/local/bin?" must reach the model.
        assert_eq!(parse("/usr/local/bin"), None);
        assert_eq!(parse("/etc/hosts is missing"), None);
    }

    #[test]
    fn mode_takes_an_argument_or_cycles() {
        assert_eq!(
            parse("/mode planning"),
            Some(SlashCommand::Mode(Some(AgentMode::Planning)))
        );
        assert_eq!(
            parse("/mode editing"),
            Some(SlashCommand::Mode(Some(AgentMode::Editing)))
        );
        assert_eq!(
            parse("/mode auto"),
            Some(SlashCommand::Mode(Some(AgentMode::Auto)))
        );
        // The v0.1 spelling still resolves.
        assert_eq!(
            parse("/mode build"),
            Some(SlashCommand::Mode(Some(AgentMode::Auto)))
        );
        assert_eq!(parse("/mode"), Some(SlashCommand::Mode(None)));
    }

    #[test]
    fn an_unknown_mode_is_reported_not_guessed() {
        assert_eq!(
            parse("/mode yolo"),
            Some(SlashCommand::Unknown("/mode yolo".into()))
        );
    }

    #[test]
    fn an_unknown_command_is_reported_not_sent_to_the_model() {
        assert_eq!(
            parse("/exitt"),
            Some(SlashCommand::Unknown("/exitt".into()))
        );
    }

    #[test]
    fn every_command_in_the_help_list_parses() {
        for (spec, _) in COMMANDS {
            let name = spec.split_whitespace().next().unwrap();
            let parsed = parse(name);
            assert!(parsed.is_some(), "{name} does not parse");
            assert!(
                !matches!(parsed, Some(SlashCommand::Unknown(_))),
                "{name} parses as unknown"
            );
        }
    }

    #[test]
    fn help_mentions_exit_and_the_cancel_key() {
        let text = help_lines().join("\n");
        assert!(text.contains("/exit"), "{text}");
        assert!(text.contains("Esc"), "{text}");
    }
}
