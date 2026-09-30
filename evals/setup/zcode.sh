#!/bin/sh
# Plant the bug the Rust single-file-fix task asks the agent to fix:
# canonical_tool_name stops mapping ':' to '_', which breaks
# `domain::naming::tests::maps_prd_spellings_to_wire_names`.
set -eu
f=crates/domain/src/naming.rs
sed "s/':' => out.push('_'),/':' => out.push(':'),/" "$f" > "$f.tmp" && mv "$f.tmp" "$f"
grep -q "':' => out.push(':')," "$f"
