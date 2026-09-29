#!/bin/sh
set -eu
! grep -rn "fn reason_str" crates/app/src
grep -rqE "fn as_str\(self\) -> &'static str" crates/domain/src
grep -rq "as_str()" crates/app/src/lib.rs
cargo test -q -p domain -p app
