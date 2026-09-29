#!/bin/sh
set -eu
# The test must be unchanged and pass.
grep -q 'canonical_tool_name("zcode:skill"), "zcode_skill"' crates/domain/src/naming.rs
cargo test -q -p domain naming
