//! Putting text on the system clipboard, without a dependency.
//!
//! Two mechanisms, tried in order:
//!
//! 1. **The platform's own tool** — `pbcopy`, `wl-copy`, `xclip`, `xsel`,
//!    `clip.exe`. Where one exists it is exact: it reaches the same clipboard
//!    every other application uses, over a local pipe, with no terminal in the
//!    middle.
//! 2. **OSC 52** — an escape sequence asking the *terminal* to set the
//!    clipboard on our behalf. This is the only thing that works over SSH,
//!    where no local clipboard tool can help, but it is not universal:
//!    Terminal.app ignores it, and tmux and screen need it enabled.
//!
//! Neither can be verified — a clipboard is write-only from here — so the
//! caller is told which mechanism ran rather than that it worked.

use std::io::Write;
use std::process::{Command, Stdio};

/// How the text was handed over, for a message the user can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// A local clipboard tool accepted it.
    Tool(&'static str),
    /// The terminal was asked to do it (OSC 52).
    Terminal,
}

impl Copied {
    pub fn describe(self) -> String {
        match self {
            Copied::Tool(name) => format!("copied ({name})"),
            Copied::Terminal => "copied (terminal clipboard)".to_string(),
        }
    }
}

/// Candidate clipboard tools, in the order worth trying.
///
/// Wayland before X11 because a Wayland session usually still has `xclip`
/// installed for XWayland, where it writes to a clipboard nothing reads.
const TOOLS: &[(&str, &[&str])] = &[
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
    ("clip.exe", &[]),
];

/// Put `text` on the clipboard. `out` is the terminal, for the OSC 52 path.
pub fn copy(text: &str, out: &mut impl Write) -> Result<Copied, String> {
    if text.is_empty() {
        return Err("nothing selected".to_string());
    }
    for (tool, args) in TOOLS {
        match run_tool(tool, args, text) {
            Ok(true) => return Ok(Copied::Tool(tool)),
            // Not installed: try the next one. A tool that *is* installed and
            // failed is reported, because that is a real problem rather than a
            // different platform.
            Ok(false) => continue,
            Err(e) => return Err(format!("{tool}: {e}")),
        }
    }
    osc52(text, out).map(|()| Copied::Terminal)
}

/// `Ok(false)` means the tool is not on this machine.
fn run_tool(tool: &str, args: &[&str], text: &str) -> Result<bool, String> {
    let spawned = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    // Dropping stdin closes the pipe, which is what tells the tool it has the
    // whole selection; without it `wl-copy` waits forever.
    drop(child.stdin.take());
    match child.wait() {
        Ok(status) if status.success() => Ok(true),
        Ok(status) => Err(format!("exited with {status}")),
        Err(e) => Err(e.to_string()),
    }
}

/// `ESC ] 52 ; c ; <base64> BEL` — ask the terminal to set the clipboard.
fn osc52(text: &str, out: &mut impl Write) -> Result<(), String> {
    // Terminals cap what they will accept, and a truncated payload would
    // decode to nothing at all. Being told the selection was too large beats
    // a clipboard that silently did not change.
    const MAX: usize = 74_994; // 100 KB of base64, the common ceiling.
    let encoded = base64(text.as_bytes());
    if encoded.len() > MAX {
        return Err("selection is too large for the terminal clipboard".to_string());
    }
    write!(out, "\x1b]52;c;{encoded}\x07").map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

/// Standard base64. Twenty lines is cheaper than a dependency for the one
/// place this project needs it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            // Positions past the end of a short chunk are padding, not data.
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_vectors() {
        // RFC 4648 §10, which is what a terminal will decode against.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_round_trips_through_a_decoder() {
        // Padding is the part that is easy to get subtly wrong, so check the
        // length invariant over every remainder.
        for n in 0..64 {
            let input: Vec<u8> = (0..n).map(|i| (i * 7 % 256) as u8).collect();
            let encoded = base64(&input);
            assert_eq!(encoded.len(), input.len().div_ceil(3) * 4, "n={n}");
            assert_eq!(
                encoded.chars().filter(|c| *c == '=').count(),
                match n % 3 {
                    0 => 0,
                    1 => 2,
                    _ => 1,
                },
                "n={n} padding"
            );
        }
    }

    #[test]
    fn base64_handles_utf8_selections() {
        // Timeline rows carry ✔, ├ and em dashes; multi-byte input must not
        // be chunked on a character boundary assumption.
        let text = "├ ✔ read — package main";
        let encoded = base64(text.as_bytes());
        assert_eq!(encoded.len(), text.len().div_ceil(3) * 4);
    }

    #[test]
    fn osc52_wraps_the_payload_the_way_terminals_expect() {
        let mut out: Vec<u8> = Vec::new();
        osc52("foobar", &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b]52;c;Zm9vYmFy\x07");
    }

    #[test]
    fn an_oversized_selection_is_reported_not_truncated() {
        // A clipped payload decodes to nothing; saying so beats a clipboard
        // that silently did not change.
        let mut out: Vec<u8> = Vec::new();
        let huge = "x".repeat(200_000);
        assert!(osc52(&huge, &mut out).is_err());
        assert!(out.is_empty(), "nothing should have been written");
    }

    #[test]
    fn copying_nothing_is_an_error_not_a_no_op() {
        let mut out: Vec<u8> = Vec::new();
        assert!(copy("", &mut out).is_err());
    }
}
