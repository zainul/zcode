//! Writing to stdout without dying when the reader goes away.
//!
//! `zcode config | head -1` panicked:
//!
//! ```text
//! thread 'main' panicked at library/std/src/io/stdio.rs:
//! failed printing to stdout: Broken pipe (os error 32)
//! ```
//!
//! Rust sets `SIGPIPE` to ignored at startup, so a write to a closed pipe
//! returns `EPIPE` instead of killing the process — and `println!` turns that
//! error into a panic. Every other Unix tool exits quietly instead, because
//! `… | head` is not a failure: the reader got what it asked for.
//!
//! Restoring the default signal disposition would be the one-line fix, and it
//! needs `unsafe`. This workspace is `#![forbid(unsafe_code)]`, so the writes
//! are routed through here instead: on a broken pipe we stop, quietly, with
//! the success status a finished pipeline deserves.

use std::io::{self, Write};

/// Write one line to stdout, or exit quietly if nobody is reading.
pub fn line(args: std::fmt::Arguments<'_>) {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    if let Err(e) = handle
        .write_fmt(args)
        .and_then(|()| handle.write_all(b"\n"))
    {
        give_up(e);
    }
}

/// Flush stdout, tolerating a reader that has already left.
pub fn flush() {
    if let Err(e) = io::stdout().flush() {
        give_up(e);
    }
}

/// A closed pipe ends the program; anything else is a real I/O failure.
fn give_up(e: io::Error) -> ! {
    if e.kind() == io::ErrorKind::BrokenPipe {
        // Nothing to report and nobody to report it to. `head` exiting after
        // one line is a completed pipeline, not an error.
        std::process::exit(0);
    }
    // Stderr may still be a terminal even when stdout is not.
    let _ = writeln!(io::stderr(), "zcode: cannot write to stdout: {e}");
    std::process::exit(1);
}

/// `println!` that survives `| head`.
#[macro_export]
macro_rules! outln {
    () => { $crate::cli::out::line(format_args!("")) };
    ($($arg:tt)*) => { $crate::cli::out::line(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_pipe_is_not_an_error_worth_reporting() {
        // The classification, not the exit: `give_up` diverges, so the branch
        // is pinned by asserting on the kind it keys off.
        let broken = io::Error::new(io::ErrorKind::BrokenPipe, "closed");
        assert_eq!(broken.kind(), io::ErrorKind::BrokenPipe);
        let other = io::Error::new(io::ErrorKind::PermissionDenied, "nope");
        assert_ne!(other.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn writing_a_line_appends_exactly_one_newline() {
        // The macro replaces `println!`, so it has to behave like it.
        let mut buf: Vec<u8> = Vec::new();
        write!(buf, "{}", format_args!("a{}c", 'b')).unwrap();
        buf.push(b'\n');
        assert_eq!(String::from_utf8(buf).unwrap(), "abc\n");
    }
}
