#!/bin/sh
set -eu
f=crates/domain/src/model_id.rs
[ "$(grep -c '#\[test\]' "$f")" -gt 2 ]
grep -q 'openrouter/' "$f"
grep -q ':' "$f"
cargo test -q -p domain model_id
