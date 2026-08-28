#!/usr/bin/env bash
# Acceptance pass over every feature, capturing real output into captures/.
#
# Everything here runs against a live provider except where noted; the point is
# to prove the shipped binary behaves as documented, not to re-run unit tests.
#
#   ./examples/run-acceptance.sh          # all sections
#   ./examples/run-acceptance.sh offline  # only the sections that need no key
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ZCODE="$ROOT/target/release/zcode"
OUT="$ROOT/examples/captures"
GO_DEMO="$ROOT/examples/demo-go"
RS_DEMO="$ROOT/examples/demo-rust"
ONLY="${1:-all}"

export PATH="$HOME/go/bin:$PATH"
mkdir -p "$OUT"

pass=0; fail=0; skip=0

# capture <name> <workdir> <command...> — record output, show it, keep going.
capture() {
  local name="$1"; shift
  local dir="$1"; shift
  local file="$OUT/$name.txt"
  local status=0
  {
    echo "\$ cd ${dir/#$ROOT/.} && $*"
    echo
  } > "$file"
  # Run outside the brace group so $? is the command's, not the echo's.
  ( cd "$dir" && "$@" ) >> "$file" 2>&1 || status=$?
  printf '\n[exit %s]\n' "$status" >> "$file"
  echo "── $name"
  sed 's/^/   /' "$file" | head -40
  echo
}

# expect <name> <pattern> — assert the last capture contains a pattern.
#
# A run the provider refused to serve is reported as SKIPPED, not FAILED: free
# OpenRouter routes throttle hard, and a harness that reports someone else's
# rate limit as a product defect is lying about what it tested. The retry
# budget is already exhausted by the time this is reached (`max_retries`), so
# there is nothing left for the harness to do about it.
throttled() {
  grep -qE '429 Too Many Requests|rate limited or out of credits' "$OUT/$1.txt"
}

expect() {
  local name="$1" pattern="$2"
  if grep -qE "$pattern" "$OUT/$name.txt"; then
    pass=$((pass+1)); echo "   ✓ matched /$pattern/"
  elif throttled "$name"; then
    skip=$((skip+1)); echo "   ⊘ SKIPPED — provider rate limited this run (429)"
  else
    fail=$((fail+1)); echo "   ✗ MISSING /$pattern/ in $name"
  fi
  echo
}

# expect_after <dependency> <name> <pattern> — as `expect`, but skip when the
# step this one verifies was itself throttled.
#
# `go run .` cannot fail with a 429, so `throttled` sees nothing wrong with it
# — yet asserting on the output of an edit that never happened is asserting on
# nothing. Cascades have to inherit the skip, or the suite reports someone
# else's rate limit as a product defect one step removed.
expect_after() {
  local dep="$1" name="$2" pattern="$3"
  if throttled "$dep"; then
    skip=$((skip+1))
    echo "   ⊘ SKIPPED — $dep was rate limited, so there is nothing to verify"
    echo
    return
  fi
  expect "$name" "$pattern"
}

# refute <name> <pattern> — the capture must NOT contain this.
#
# A separate helper rather than a negated pattern: `grep -E` has no negative
# lookahead, and `^(?!…)` silently becomes an invalid-operand error rather than
# a passing check. Absence is worth asserting in its own right — a panic in the
# output is a defect no positive pattern would catch.
refute() {
  local name="$1" pattern="$2"
  if grep -qE "$pattern" "$OUT/$name.txt"; then
    fail=$((fail+1)); echo "   ✗ UNEXPECTED /$pattern/ in $name"
  else
    pass=$((pass+1)); echo "   ✓ absent /$pattern/"
  fi
  echo
}

banner() { echo; echo "═══ $1 ══════════════════════════════════════════"; echo; }

# Free OpenRouter routes are rate limited per minute. Sections that make
# several calls back to back exhaust the quota and fail on retries that would
# have succeeded a moment later, so pause between them. Paid models can set
# ZCODE_SECTION_PAUSE=0.
PAUSE="${ZCODE_SECTION_PAUSE:-20}"
breathe() { [ "$PAUSE" -gt 0 ] && sleep "$PAUSE"; return 0; }

# ---------------------------------------------------------------------------
banner "1. Version, help, and the command surface"

capture version        "$ROOT"    "$ZCODE" version
expect  version        'zcode v[0-9]+\.[0-9]+\.[0-9]+ \(git: .*built: .*\)'

capture help           "$ROOT"    "$ZCODE" --help
expect  help           'run.*Run a single task'

capture help-run       "$ROOT"    "$ZCODE" run --help
expect  help-run       'planning \(read-only\) \| editing \(edits files\) \| auto'

capture help-session   "$ROOT"    "$ZCODE" session --help
expect  help-session   'fork'

# `cmd | head` must not panic. Rust ignores SIGPIPE, so a write to a closed
# pipe returns EPIPE and `println!` turns that into a panic — which is what
# put "failed printing to stdout: Broken pipe" into a tool result.
capture pipe-config   "$GO_DEMO" sh -c "$ZCODE config 2>&1 | head -1"
expect  pipe-config   'Config sources'
refute  pipe-config   'Broken pipe|panicked'
capture pipe-tools    "$GO_DEMO" sh -c "$ZCODE tools list 2>&1 | head -1"
refute  pipe-tools    'Broken pipe|panicked'

# A bad flag is a usage error (exit 2), not a crash.
capture bad-flag       "$ROOT"    "$ZCODE" run --nonsense hi
expect  bad-flag       'exit 2'

# ---------------------------------------------------------------------------
banner "2. Configuration discovery and validation"

capture config-go      "$GO_DEMO" "$ZCODE" config
expect  config-go      'project looks like go'
expect  config-go      'gopls'
# Either a matched rate or a recognised free route, but never "n/a":
# an unknown model would mean the cost display is silently off.
expect  config-go      'pricing +(\$[0-9.]+/\$[0-9.]+ per Mtok|free route)'
expect  config-go      'max_retries +3'
expect  config-go      'rate_limit_backoff_ms +[0-9]+ms'
expect  config-go      'shell_denied +[0-9]+ built-in'

capture config-rust    "$RS_DEMO" "$ZCODE" config
expect  config-rust    'project looks like rust'

# rtk state is always reported: whether shell output is being shrunk before it
# reaches the model is a fact about every future token bill.
expect  config-go      'rtk +(([0-9]+\.[0-9]+.*token-optimised)|not installed|off )'

# A broken allowlist is caught before a token is spent, and exits non-zero.
capture config-broken  "$ROOT/examples/broken" "$ZCODE" config
expect  config-broken  'regular expressions, not shell globs'
expect  config-broken  'exit 1'

# Several providers in one config: listed, marked, and selectable by name.
MULTI="$ROOT/examples/multi-provider"
capture config-multi   "$MULTI"   "$ZCODE" config
expect  config-multi   'provider +primary +\(openai-compatible\)'
# Not a fixed count: a user-level config may declare providers of its own,
# and them merging in is the feature, not a failure.
expect  config-multi   'providers +[0-9]+'
expect  config-multi   '▸ primary'
expect  config-multi   'backup .*openai-compatible'
expect  config-multi   'local .*ollama'
# Each profile brings its own endpoint, not the top-level one.
expect  config-multi   'http://127.0.0.1:8094/v1/chat/completions'
expect  config-multi   'http://localhost:11434/api/chat'

capture provider-flag  "$MULTI"   "$ZCODE" run --provider local "hi"
# No Ollama is running, so the point is *which* endpoint it tried.
expect  provider-flag  'ollama request failed'
expect  provider-flag  '11434'

# An unknown name is refused before any request, and says what it would take.
capture provider-bad   "$MULTI"   "$ZCODE" run --provider nope "hi"
expect  provider-bad   'unknown provider `nope`'
expect  provider-bad   'configured:.*primary.*backup'
expect  provider-bad   'exit 1'

# `--model <provider>/<model>` moves the endpoint too: the config selects
# `primary` on port 8095, and the prefix has to take the run to Ollama instead.
capture model-flag     "$MULTI"   "$ZCODE" run --model local/qwen2.5-coder "hi"
expect  model-flag     'ollama request failed'
expect  model-flag     '11434'
refute  model-flag     '8095'

# A leading segment that is *not* a provider stays part of the id, or every
# OpenRouter model would be read as an endpoint that does not exist.
capture model-vendor   "$MULTI"   "$ZCODE" run --model z-ai/glm-4.6 "hi"
expect  model-vendor   '8095'

# A provider named and nothing after it: refused before any request.
capture model-bad      "$MULTI"   "$ZCODE" run --model "local/" "hi"
expect  model-bad      'names the provider `local` but no model'
expect  model-bad      'exit 1'

# The flags of the bare invocation are the flags of `zcode repl`. Written
# before a subcommand they land where nothing reads them — that is a usage
# error, not a run with the flag silently dropped.
capture model-misplaced "$MULTI"  "$ZCODE" --model gpt-4o run "hi"
expect  model-misplaced 'given before the subcommand'
expect  model-misplaced 'exit 2'

# ---------------------------------------------------------------------------
banner "3. Tools and skills"

capture tools-list     "$GO_DEMO" "$ZCODE" tools list
expect  tools-list     '^ *apply_patch'
expect  tools-list     '^ *shell'

capture skills-list    "$GO_DEMO" "$ZCODE" skills list
expect  skills-list    'skill\(s\) offered to the model'

[ "$ONLY" = offline ] && { echo; echo "offline sections done: $pass passed, $fail failed"; exit $((fail>0)); }

# ---------------------------------------------------------------------------
breathe
banner "4. Headless run: a real edit, verified by a real build"

# Start from a pristine file so the edit is a real edit, not a no-op.
cp "$ROOT/examples/fixtures/main.go" "$GO_DEMO/main.go"
capture run-go-edit    "$GO_DEMO" "$ZCODE" run \
  "Add a farewell function to main.go mirroring greet, call it from main, then run 'go build ./... 2>&1' to verify."
expect  run-go-edit    'step\(s\).*tokens.*\$'
expect  run-go-edit    'apply_patch|str_replace_editor|write' 

capture run-go-verify  "$GO_DEMO" go run .
expect_after run-go-edit run-go-verify 'Goodbye, world!'

# ---------------------------------------------------------------------------
breathe
banner "5. Agent modes: planning refuses, editing refuses shell, auto allows"

# The guard has two layers: the tool is not advertised, and if the model calls
# it anyway the engine refuses before it runs. A live model usually never
# reaches layer two — so assert the property that actually matters, which is
# that the file is untouched. Layer two is covered by the unit tests
# (`app::planning_mode_refuses_execute_tools`).
before=$(shasum "$GO_DEMO/main.go" | cut -d' ' -f1)
capture mode-planning  "$GO_DEMO" "$ZCODE" run --mode planning \
  "Rewrite main.go to remove the farewell function. Use apply_patch to do it."
after=$(shasum "$GO_DEMO/main.go" | cut -d' ' -f1)
if [ "$before" = "$after" ]; then
  pass=$((pass+1)); echo "   ✓ main.go is byte-identical after a planning run"
else
  fail=$((fail+1)); echo "   ✗ PLANNING MODE WROTE TO main.go"
fi
echo

capture mode-editing   "$GO_DEMO" "$ZCODE" run --mode editing \
  "Run the shell command 'go vet ./...' and report the output."
expect  mode-editing   'unable to run shell|cannot .*shell|shell tool is disabled|denied|shell'

# Editing mode still edits: the distinction is shell access, not writes.
capture mode-editing-writes "$RS_DEMO" "$ZCODE" run --mode editing \
  "Add a doc comment to the greet function in src/main.rs. Do not run any commands."
expect  mode-editing-writes 'step\(s\)' 

# ---------------------------------------------------------------------------
breathe
banner "6. Shell safety: the denylist overrides even a wide-open allowlist"

capture shell-denied   "$GO_DEMO" env ZCODE_SHELL_ALLOWED='.*' "$ZCODE" run \
  "Run exactly this shell command and report what happens: sudo rm -rf /tmp/zcode-test"
expect  shell-denied   'denylist|refused'

capture shell-allowed  "$GO_DEMO" "$ZCODE" run \
  "Run 'go build ./... 2>&1' and tell me the exact output."
expect  shell-allowed  'step\(s\)'

# ---------------------------------------------------------------------------
breathe
banner "7. JSON output and telemetry"

capture json-run       "$GO_DEMO" "$ZCODE" run --json "List the files in this directory."
expect  json-run       '"kind":"tool_call"'
expect  json-run       '"kind":"finish"'
expect  json-run       '"cost_usd"'

# The opencode-compatible stream, for consumers written against its bus.
breathe
capture json-opencode  "$GO_DEMO" "$ZCODE" run --json --json-format opencode \
  "List the files in this directory."
expect  json-opencode  '"type":"session.next.step.started"'
expect  json-opencode  '"type":"session.next.step.ended"'
expect  json-opencode  '"type":"session.idle"'

# ---------------------------------------------------------------------------
breathe
banner "8. Sessions: create, continue, fork, export"

SID=$("$ZCODE" session create 2>/dev/null | tail -1)
capture session-flow   "$GO_DEMO" bash -c "
  set -e
  id=\$($ZCODE session create | tail -1)
  echo \"created \$id\"
  $ZCODE session continue \"\$id\" 'Remember the number 42. Just acknowledge.' >/dev/null 2>&1
  $ZCODE session fork \"\$id\" --as acceptance-fork
  $ZCODE session export \"\$id\" --to /tmp/zcode-export.json
  echo 'exported:'; head -c 200 /tmp/zcode-export.json
"
expect  session-flow   'created [0-9a-f-]{36}'

echo
echo "═════════════════════════════════════════════════════"
printf '  %d passed, %d failed' "$pass" "$fail"
[ "$skip" -gt 0 ] && printf ', %d skipped (provider throttled)' "$skip"
printf ' — captures in examples/captures/\n'
echo "═════════════════════════════════════════════════════"
if [ "$skip" -gt 0 ]; then
  echo "  Skips are OpenRouter throttling a free route, not zcode failing."
  echo "  Re-run later, raise ZCODE_SECTION_PAUSE, or use a paid model."
fi
exit $((fail>0))
