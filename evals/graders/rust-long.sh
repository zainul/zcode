#!/bin/sh
set -eu
grep -q '"/version"' crates/cli/src/cli/tui/command.rs
grep -q 'parse("/version")' crates/cli/src/cli/tui/command.rs
cargo test -q -p zcode
