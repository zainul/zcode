# Acceptance harness

Everything here exists to prove the shipped binary behaves the way the guide
says it does — against a **live provider**, not a mock. Unit tests cover the
logic; this covers the parts only a real run can: what the terminal actually
draws, what a real model actually does with a withheld tool, whether a rate
limit is legible.

```
examples/
├── run-acceptance.sh     40 checks over the CLI surface
├── tui-screenshot.py     88 checks; drives the TUI on a pty
├── fake-provider.py      429s, a canned tool call, or N numbered lines
├── demo-go/              a Go project the agent edits and builds
├── demo-rust/            a Rust project
├── broken/               a config with an invalid allowlist pattern
├── broken-model/         a config naming a model that does not exist
├── rate-limited/         points at fake-provider.py
├── blocked-shell/        a narrow `shell_allowed`, so a command is refused
├── open-shell/           the same, with `"shell_allowed": [".*"]`
├── scrolling/            an answer taller than the pane, for the scroll checks
├── multi-provider/       three endpoints, for `/provider` and `--provider`
├── paid-usage/           a paid model no price table knows, for the usage checks
├── fixtures/             pristine copies, restored before each run
└── captures/             the output — real, regenerated, not hand-written
```

## Running it

```sh
cargo build --release -p zcode
export ZCODE_OPENROUTER_API_KEY=sk-or-v1-...

./examples/run-acceptance.sh            # everything
./examples/run-acceptance.sh offline    # only what needs no API key
```

`ratelimit`, `blocked`, `scrolling`, `providers` and `selection` need no API key: all four
run against `fake-provider.py`, because what they prove is what *zcode* does —
with a 429, with a given command, with more rows than fit, with two endpoints —
and a live model that answers differently every time proves nothing either way.
`scrolling` in particular needs content it can identify row by row: "is
`line 001` on screen?" has an answer, "did it scroll?" does not.

The live sections pause 20s between them (`ZCODE_SECTION_PAUSE`) because free
routes throttle per minute. On a paid model set `ZCODE_SECTION_PAUSE=0`. If a
run is throttled anyway, the affected checks report `⊘ SKIPPED` rather than
failing — someone else's 429 is not a zcode defect, and treating it as one
would make the suite useless as a regression signal.

Skips cascade. `go run .` cannot fail with a 429, but asserting on the output
of an edit that was throttled is asserting on nothing, so `expect_after` marks
the dependent check skipped too.

The live waits also allow for the client's own retry budget
(`max_retries` × `rate_limit_backoff_ms`, 90s at the defaults). A check that
gives up sooner than the agent does would report a defect that is not there.

The TUI harness needs `pyte` to interpret the escape-sequence stream:

```sh
python3 -m pip install pyte
./examples/tui-screenshot.py            # all scenarios
./examples/tui-screenshot.py --list     # names
./examples/tui-screenshot.py paste live # just these
```

`ZCODE_SCENARIO_PAUSE` does the same job for the scenarios that make a live
call.

Both write to `examples/captures/` and print a pass/fail tally. Non-zero exit
means something regressed.

## No timing bets

Scenarios wait for the thing they need rather than for a number of seconds.
`t.ready()` blocks until the opening frame is actually drawn; `wait_for` polls
for a pattern. A fixed `pump(2.0)` is a bet on how long startup takes, and a
cold binary — the first run after a build, with a language server to spawn —
loses it. That produced a suite that passed locally and failed in CI, which is
the least useful kind of test.

## Why a pty

`ratatui` output is only meaningful once a terminal has interpreted it.
Capturing stdout gives you escape codes; capturing a *rendered screen* gives you
the thing the user sees — which is what lets a check assert "the caret is
visible at row 29, column 3" or "the paste is all there".

`tui-screenshot.py` allocates a pty, runs the real binary on it, sends real
keystrokes (including a real bracketed-paste sequence), and feeds the output
through `pyte`. A capture in `captures/tui-*.txt` is a screenshot in text form,
with the cursor position recorded underneath. The screens in
[docs/guide/05-tui.md](../docs/guide/05-tui.md) come from these files.

## Why a fake provider

Some paths cannot be requested from a real provider on demand — you cannot ask
OpenRouter for a 429 when you want to watch the client back off. `fake-provider.py`
answers the first N requests with 429 and a `Retry-After`, then streams a normal
completion, so the retry path is exercised end to end rather than only in unit
tests.

```sh
./examples/fake-provider.py --port 8099 --fail 2
cd examples/rate-limited && ZCODE_API_KEY=dummy zcode run "hello"
```

It can also answer with a **canned tool call**, which is how the shell guard is
exercised on screen. What matters there is what the guard does with a given
command; asking a live model to produce that exact command is a coin flip, and
one that would make the check flaky rather than thorough:

```sh
./examples/fake-provider.py --port 8098 --fail 0 --tool shell \
    --tool-args '{"command":"cd /workspace && go build ./... 2>&1 | head"}'
cd examples/blocked-shell && ZCODE_API_KEY=dummy zcode run "build it"
```

And with `--lines N` it answers with N numbered lines, which is what gives the
scrolling checks something they can point at:

```sh
./examples/fake-provider.py --port 8093 --fail 0 --lines 60
```

Each scenario that needs it starts its own server, because a shared one that has
already spent its failure budget answers `200` and proves nothing.

## Costs

The demo configs point at `poolside/laguna-s-2.1:free`, an OpenRouter free
route, so a full pass costs **nothing**. It is tool-capable, which is what the
agent loop needs.

Free routes are rate limited, which turns out to be useful: a full pass usually
trips at least one real 429 and exercises the retry path against a real
provider rather than only against `fake-provider.py`.

Point the configs at a paid model for a stronger check of tool use — on
`anthropic/claude-haiku-4.5` a full pass costs about **$0.10**.

## When a check fails

The captures are the evidence. `examples/captures/<name>.txt` holds the exact
command, its full output, and its exit code — diff it against a known-good copy
to see what moved.
