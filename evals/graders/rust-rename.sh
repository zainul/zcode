#!/bin/sh
set -eu
! grep -rn "truncate_tool_output" crates
grep -rq "fn cap_tool_output" crates/app/src
cargo test -q -p app
