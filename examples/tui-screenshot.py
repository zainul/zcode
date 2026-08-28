#!/usr/bin/env python3
"""Drive the zcode TUI through a pty and save what the screen actually shows.

The TUI is the one surface unit tests cannot cover end to end: `TuiState` is
tested directly, but nothing proves the widgets, the cursor, and the key
handling agree once ratatui has drawn them. This script runs the real binary on
a real pseudo-terminal, feeds it real keystrokes, and renders the resulting
escape-sequence stream with `pyte` — so a capture is the screen, not a log.

Usage:  ./examples/tui-screenshot.py <scenario> [more...]
        ./examples/tui-screenshot.py --list
"""
import codecs
import json
import os
import pty
import re
import select
import signal
import subprocess
import sys
import time
from pathlib import Path

import pyte

ROOT = Path(__file__).resolve().parent.parent
ZCODE = ROOT / "target" / "release" / "zcode"
OUT = ROOT / "examples" / "captures"
COLS, ROWS = 100, 32

# Keystrokes, as raw bytes.
ENTER = b"\r"
ESC = b"\x1b"
CTRL_C = b"\x03"
SHIFT_TAB = b"\x1b[Z"
ALT_ENTER = b"\x1b\r"
LEFT = b"\x1b[D"
CTRL_W = b"\x17"
CTRL_A = b"\x01"
PAGE_UP = b"\x1b[5~"
PAGE_DOWN = b"\x1b[6~"
# SGR mouse reports, which is the encoding crossterm turns on: button 64 is
# wheel-up and 65 wheel-down, at an arbitrary cell inside the pane.
WHEEL_UP = b"\x1b[<64;20;10M"
WHEEL_DOWN = b"\x1b[<65;20;10M"
# SGR mouse: button 0 press/drag/release, for the selection checks.
def mouse(button: int, col: int, row: int, release: bool = False) -> bytes:
    return f"\x1b[<{button};{col};{row}{'m' if release else 'M'}".encode()


CTRL_HOME = b"\x1b[1;5H"
CTRL_END = b"\x1b[1;5F"


def bracketed(text: str) -> bytes:
    """A paste, exactly as a terminal delivers one."""
    return b"\x1b[200~" + text.encode() + b"\x1b[201~"


class Tui:
    """A live zcode TUI on a pty, with a screen we can read back."""

    def __init__(self, cwd: Path, args=(), env=None):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.Stream(self.screen)
        # A read can split a multi-byte character; decode across chunks.
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.master, slave = pty.openpty()
        import fcntl, struct, termios

        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        # Language servers installed by `go install` land in ~/go/bin, which a
        # non-login shell often lacks; without it the LSP default cannot resolve
        # and the capture would understate what a normal terminal sees.
        path = os.environ.get("PATH", "")
        gobin = os.path.expanduser("~/go/bin")
        if gobin not in path.split(os.pathsep):
            path = gobin + os.pathsep + path
        environ = dict(
            os.environ,
            PATH=path,
            TERM="xterm-256color",
            COLUMNS=str(COLS),
            LINES=str(ROWS),
            **(env or {}),
        )
        self.proc = subprocess.Popen(
            [str(ZCODE), *args],
            cwd=str(cwd),
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=environ,
            preexec_fn=os.setsid,
        )
        os.close(slave)

    def pump(self, seconds=0.6):
        """Read whatever the app has drawn, for up to `seconds`."""
        deadline = time.time() + seconds
        while time.time() < deadline:
            ready, _, _ = select.select([self.master], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(self.master, 65536)
            except OSError:
                break
            if not data:
                break
            self.stream.feed(self.decoder.decode(data))

    def send(self, data: bytes, settle=0.6):
        os.write(self.master, data)
        self.pump(settle)

    def type(self, text: str, settle=0.5):
        # One byte at a time: this is what a person does, and it exercises the
        # per-keystroke path rather than the paste path.
        for ch in text.encode():
            os.write(self.master, bytes([ch]))
            time.sleep(0.004)
        self.pump(settle)

    def wait_for(self, pattern: str, timeout=90.0, poll=0.4) -> bool:
        """Pump until the screen matches, or give up."""
        deadline = time.time() + timeout
        rx = re.compile(pattern)
        while time.time() < deadline:
            self.pump(poll)
            if rx.search(self.text()):
                return True
        return False

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def ready(self, timeout=30.0):
        """Block until the opening frame is drawn.

        A fixed `pump(2.0)` was a bet on how long startup takes, and a cold
        binary — the first run after a build, with a language server to spawn —
        loses it. Waiting for the thing itself is not slower on a warm machine
        and does not fail on a cold one.
        """
        if not self.wait_for(r"Ready when you are", timeout=timeout, poll=0.1):
            raise AssertionError(
                f"the TUI drew no opening frame within {timeout}s:\n{self.text()}"
            )

    def status_line(self) -> str:
        """The bottom bar: state, mode, provider/model, tokens, cost."""
        rows = [r for r in self.screen.display if r.strip()]
        return rows[-1] if rows else ""

    def cursor(self):
        return (self.screen.cursor.y, self.screen.cursor.x, self.screen.cursor.hidden)

    def shot(self, name: str, note: str = "") -> str:
        """Save the screen with a box around it, the way a user sees it."""
        y, x, hidden = self.cursor()
        body = "\n".join(f"│{line:<{COLS}}│" for line in self.screen.display)
        frame = (
            f"┌{'─' * COLS}┐\n{body}\n└{'─' * COLS}┘\n"
            f"cursor: row {y}, col {x}, visible={not hidden}\n"
        )
        header = f"### {name}\n{note}\n\n" if note else f"### {name}\n\n"
        path = OUT / f"tui-{name}.txt"
        path.write_text(header + frame)
        print(f"── saved {path.name}   (cursor row {y} col {x}, visible={not hidden})")
        return frame

    def close(self):
        try:
            os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
        except ProcessLookupError:
            pass
        self.proc.wait(timeout=5)
        os.close(self.master)


# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------

# Free OpenRouter routes are rate limited per minute; scenarios that each make
# a live call exhaust the quota when run back to back. Paid models can set
# ZCODE_SCENARIO_PAUSE=0.
SCENARIO_PAUSE = float(os.environ.get("ZCODE_SCENARIO_PAUSE", "15"))

CHECKS = []


def check(condition: bool, label: str, screen: str = ""):
    """Record a check. A provider throttle is a skip, not a failure.

    Free OpenRouter routes rate limit hard; reporting someone else's 429 as a
    zcode defect would make the suite useless as a regression signal.
    """
    # A visible retry marker counts too: while the client sits in its
    # rate-limit backoff there is no error on screen yet, only "↻ rate
    # limited …" — the check has run out of patience, not found a defect.
    if not condition and re.search(
        r"429 Too Many Requests|rate limited or out|↻ rate limited", screen
    ):
        CHECKS.append((None, label))
        print(f"   ⊘ {label} — provider rate limited this run (429)")
        return
    CHECKS.append((condition, label))
    print(f"   {'✓' if condition else '✗'} {label}")


def scenario_startup():
    """Opening screen: three panes, a status bar, and a visible caret."""
    t = Tui(ROOT / "examples" / "demo-go")
    t.ready()
    t.shot("01-startup", "zcode with no subcommand, nothing typed yet")
    text = t.text()
    check("conversation" in text, "conversation pane is drawn")
    check("mode auto" in text, "status bar shows the mode")
    check("openrouter" in text, "status bar shows provider/model")
    check("ready" in text, "status bar shows the idle state")
    _, _, hidden = t.cursor()
    check(not hidden, "the caret is visible")
    t.close()


def scenario_help_and_modes():
    """/help, /mode, and Shift-Tab cycling."""
    t = Tui(ROOT / "examples" / "demo-go")
    t.ready()
    t.type("/help")
    t.send(ENTER, 1.0)
    t.shot("02-help", "/help — the pane follows the tail, so the keys are shown")
    check("Alt-Enter" in t.text(), "/help documents the newline key")

    # The list is longer than the pane: PageUp reaches the top of it.
    t.send(PAGE_UP, 0.8)
    t.shot("02b-help-scrolled", "PageUp scrolls back to the start of /help")
    text = t.text()
    check("/exit" in text, "/help documents /exit")
    check("scrolled" in text, "the pane says it is no longer following the tail")
    t.send(PAGE_DOWN, 0.6)
    check("scrolled" not in t.text(), "PageDown returns to following the tail")

    t.type("/mode")
    t.send(ENTER, 1.0)
    t.shot("03-mode-list", "/mode with no argument lists the three modes")
    text = t.text()
    check("planning" in text and "editing" in text, "all three modes are listed")
    check("▸" in text, "the active mode is marked")

    t.type("/mode planning")
    t.send(ENTER, 1.0)
    t.shot("04-mode-planning", "after /mode planning — the status bar follows")
    check("mode planning" in t.text(), "the status bar shows planning")

    t.send(SHIFT_TAB, 1.0)
    t.shot("05-mode-cycled", "Shift-Tab cycles planning → editing")
    check("mode editing" in t.text(), "Shift-Tab advanced the mode")
    t.close()


def scenario_paste_and_wrap():
    """A large multi-line paste must land whole, wrapped, with the caret right."""
    t = Tui(ROOT / "examples" / "demo-go")
    t.ready()
    payload = (
        "Here is a long paste that must arrive whole. "
        "func handler(w http.ResponseWriter, r *http.Request) {\n"
        "    ctx := r.Context()\n"
        "    if err := svc.Do(ctx); err != nil {\n"
        "        http.Error(w, err.Error(), 500)\n"
        "        return\n"
        "    }\n"
        "}\n"
        "The final line proves nothing was truncated: SENTINEL-END"
    )
    t.send(bracketed(payload), 1.2)
    t.shot("06-paste", f"a {len(payload)}-byte, {payload.count(chr(10)) + 1}-line paste")
    text = t.text()
    check("SENTINEL-END" in text, "the last line of the paste survived")
    check("ctx := r.Context()" in text, "interior lines survived")
    check("conversation" in text, "the paste did not send the prompt")

    # Ctrl-A, then a long single line, to prove wrapping.
    t.send(CTRL_A + b"\x0b", 0.4)  # Ctrl-K clears to end
    t.type("x" * 250)
    t.shot("07-wrap", "a 250-character line wraps inside the prompt box")
    check("x" * 60 in t.text(), "the long line is displayed")
    t.close()


def scenario_editing_keys():
    """Word motion and word deletion at the caret."""
    t = Tui(ROOT / "examples" / "demo-go")
    t.ready()
    t.type("cargo test --workspace --all-features")
    t.send(CTRL_W, 0.4)
    t.send(CTRL_W, 0.4)
    t.shot("08-editing", "two Ctrl-W presses delete the last two words")
    text = t.text()
    check("cargo test" in text, "the head of the line is intact")
    check("--all-features" not in text, "the last word was deleted")
    check("--workspace" not in text, "the second-to-last word was deleted")
    t.close()


def scenario_live_turn():
    """A real turn: spinner while working, tools inline, cost when done.

    The waits allow for the client's full rate-limit budget
    (`max_retries` x `rate_limit_backoff_ms`, 90s at the defaults) plus the
    turn itself — otherwise a throttled free route reads as a failure.
    """
    t = Tui(ROOT / "examples" / "demo-go")
    t.ready()
    t.type("Read main.go, then list this directory, then stop. Use the tools.")
    t.send(ENTER, 1.5)
    t.shot("09-working", "mid-turn: the spinner, the step counter, the elapsed clock")
    text = t.text()
    check("working" in text or "step" in text, "the status bar shows progress")

    # The tools-pane marker specifically: the model's own prose often mentions
    # the tool name, so a bare "list_dir" match would prove nothing.
    got = t.wait_for(r"tools used", timeout=210)
    t.shot("10-tools", "the tool call, inline under the message that made it")
    text = t.text()
    check(got, "the tool group is labelled", text)
    check("list_dir" in text, "the tool is named", text)
    check(re.search(r"[├└]", text) is not None, "the timeline is bracketed", text)
    check(re.search(r"\d\d:\d\d:\d\d", text) is not None, "rows carry a timestamp", text)

    got = t.wait_for(r"●\s+ready", timeout=210)
    t.shot("11-finished", "back to ready, with tokens and cost on the status bar")
    text = t.text()
    check(got, "the run finished and the status bar says ready", text)
    check(re.search(r"\d+ in / \d+ out", text) is not None, "token totals are shown", text)
    check("$" in text, "an estimated cost is shown", text)

    t.type("/cost")
    t.send(ENTER, 1.0)
    t.shot("12-cost", "/cost breaks the estimate down")
    check("tokens:" in t.text(), "/cost reports the token breakdown", t.text())
    t.close()


def scenario_exit():
    """/exit leaves cleanly and restores the terminal."""
    t = Tui(ROOT / "examples" / "demo-go")
    t.ready()
    t.type("/exit")
    t.shot("13-exit-typed", "/exit typed, about to be sent")
    t.send(ENTER, 1.5)
    code = t.proc.poll()
    check(code == 0, f"/exit terminated the process cleanly (exit {code})")
    try:
        t.close()
    except Exception:
        pass


def scenario_unknown_command():
    """A typo is reported, not sent to the model as a prompt."""
    t = Tui(ROOT / "examples" / "demo-go")
    t.ready()
    t.type("/exitt")
    t.send(ENTER, 1.0)
    t.shot("14-unknown-command", "an unrecognised /command is caught locally")
    check("unknown command" in t.text(), "the typo was reported")
    check("●  ready" in t.text() or "ready" in t.text(), "no turn was started")
    t.close()


def scenario_provider_error():
    """A provider failure is shown in red on the status bar, not swallowed."""
    t = Tui(ROOT / "examples" / "broken-model")
    t.ready()
    t.type("hello")
    t.send(ENTER, 1.0)
    got = t.wait_for(r"error", timeout=60)
    t.shot("15-provider-error", "a 404 from the provider, surfaced verbatim")
    check(got, "the provider error reached the screen")
    check("✖" in t.text() or "error" in t.text(), "the status bar shows the failure")
    t.close()


def scenario_rate_limit():
    """A 429 must look like a rate limit, not like a hang.

    Owns its fake provider so the failure budget is fresh: a shared server
    that had already spent its 429s would answer 200 and prove nothing.
    """
    sys.path.insert(0, str(ROOT / "examples"))
    import importlib

    fake = importlib.import_module("fake-provider".replace("-", "_")) if False else None
    # The module name has a dash; load it by path instead.
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "fake_provider", ROOT / "examples" / "fake-provider.py"
    )
    fake = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fake)
    # Long enough to be observable at a 0.4s poll, short enough not to stall.
    server = fake.serve(port=8099, fail=2, status=429)

    t = Tui(ROOT / "examples" / "rate-limited")
    t.ready()
    t.type("hello")
    t.send(ENTER, 1.0)
    got = t.wait_for(r"rate limited", timeout=20, poll=0.1)
    t.shot("16-rate-limited", "mid-429: the status bar explains the pause")
    text = t.text()
    check(got, "the rate limit is named on screen")
    check("429" in text, "the status code is shown")
    check("retrying in" in text, "the wait is quantified")
    check("↻" in text, "the timeline records the retry")

    got = t.wait_for(r"Recovered after", timeout=30)
    t.shot("17-rate-limit-recovered", "the same turn, after the retries succeeded")
    check(got, "the turn completed once the provider recovered")
    check("↻" in t.text(), "the retry stays in the timeline afterwards")
    t.close()
    server.shutdown()


def _fake_provider():
    """Load `fake-provider.py`, whose dashed name is not importable."""
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "fake_provider", ROOT / "examples" / "fake-provider.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def scenario_tool_rows():
    """A tool row must say what ran, and a broken pipe must not panic."""
    fake = _fake_provider()
    server = fake.serve(
        # The port `examples/open-shell` is configured for.
        port=8097, fail=0, status=429, tool="shell",
        tool_args=json.dumps({"command": "ls -lah"}),
    )
    t = Tui(ROOT / "examples" / "open-shell")
    t.ready()
    t.type("list the files")
    t.send(ENTER, 1.0)
    check(t.wait_for(r"tools used", timeout=30), "the shell call settled")
    # A successful run folds itself; open it to read the row.
    t.send(b"\x14", 0.6)
    t.shot("28-tool-row-command", "the row names the command, not the output")
    row = next((l for l in t.text().splitlines() if "✔" in l and "shell" in l), "")
    check("ls -lah" in row, f"the command is shown: {row.strip()!r}")
    t.close()
    server.shutdown()


def scenario_folding():
    """A settled run of tool calls folds; the one in flight stays open."""
    fake = _fake_provider()
    server = fake.serve(
        port=8097, fail=0, status=429, tool="shell",
        tool_args=json.dumps({"command": "ls -lah"}),
    )
    t = Tui(ROOT / "examples" / "open-shell")
    t.ready()
    t.type("list the files")
    t.send(ENTER, 1.0)
    check(t.wait_for(r"▸ tools used", timeout=30), "the settled run folded itself")

    folded = t.text()
    t.shot("29-run-folded", "a settled run, folded to one line")
    check("1 call" in folded, f"the header counts the calls: {folded!r}"[:120])
    check("ls -lah" not in folded, "the detail is folded away")

    # Click the header to open it.
    rows = t.screen.display
    header = next(i for i, r in enumerate(rows) if "▸ tools used" in r)
    col = rows[header].index("▸") + 1
    t.send(mouse(0, col, header + 1), 0.2)
    t.send(mouse(0, col, header + 1, release=True), 0.6)
    opened = t.text()
    t.shot("30-run-expanded", "the same run after clicking its header")
    check("ls -lah" in opened, f"clicking the header opened it: {opened!r}"[:120])
    check("▾ tools used" in opened, "and the marker turned")

    # Ctrl-T folds everything again.
    t.send(b"\x14", 0.6)
    check("ls -lah" not in t.text(), "Ctrl-T folds every run")

    t.close()
    server.shutdown()


def scenario_usage():
    """The status bar must move while a multi-step turn is spending money.

    The reported bug: mid-turn the bar read `0 in / 0 out | n/a` for minutes,
    because usage was only reported when the whole turn ended and the cost came
    from a local table that had never heard of the model.
    """
    import importlib.util
    import json as _json
    import threading
    from http.server import BaseHTTPRequestHandler, HTTPServer

    calls = {"n": 0}
    gate = threading.Event()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            self.rfile.read(int(self.headers["content-length"]))
            calls["n"] += 1
            if calls["n"] == 1:
                # A tool call, so the turn continues — and a cost for a model
                # no local price table knows.
                body = [
                    {
                        "choices": [
                            {
                                "delta": {
                                    "tool_calls": [
                                        {
                                            "index": 0,
                                            "id": "c1",
                                            "type": "function",
                                            "function": {
                                                "name": "list_dir",
                                                "arguments": '{"path":"."}',
                                            },
                                        }
                                    ]
                                }
                            }
                        ]
                    },
                    {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]},
                    {
                        "choices": [],
                        "usage": {
                            "prompt_tokens": 2280,
                            "completion_tokens": 71,
                            "cost": 0.000189,
                        },
                    },
                ]
            else:
                # Hold the second call open so the screen can be read mid-turn.
                gate.wait(timeout=30)
                body = [
                    {"choices": [{"delta": {"content": "done"}}]},
                    {"choices": [{"delta": {}, "finish_reason": "stop"}]},
                    {
                        "choices": [],
                        "usage": {
                            "prompt_tokens": 9393,
                            "completion_tokens": 167,
                            "cost": 0.000508,
                        },
                    },
                ]
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            for chunk in body:
                self.wfile.write(f"data: {_json.dumps(chunk)}\n\n".encode())
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

    server = HTTPServer(("127.0.0.1", 8162), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    # openrouter requires a key; the stub does not check it.
    t = Tui(ROOT / "examples" / "paid-usage", env={"ZCODE_API_KEY": "stub"})
    t.ready()
    t.type("do something")
    t.send(ENTER, 1.0)

    # Mid-turn: step 2 is blocked on the gate, so step 1's usage is all there is.
    check(t.wait_for(r"2,280 in|2280 in", timeout=30), "tokens appear mid-turn")
    bar = t.status_line()
    t.shot("26-usage-midturn", "mid-turn: tokens and cost, not 0 in / 0 out")
    check("0 in / 0 out" not in bar, f"the bar is not stuck at zero: {bar.strip()!r}")
    check("n/a" not in bar, f"cost is known mid-turn: {bar.strip()!r}")

    gate.set()
    check(t.wait_for(r"11,673 in|11673 in", timeout=30), "totals settle at the end")
    final = t.status_line()
    t.shot("27-usage-settled", "after the turn: provider-reported totals")
    check("$0.0007" in final or "0.0007" in final, f"the reported cost is shown: {final.strip()!r}")

    t.close()
    server.shutdown()


def scenario_selection():
    """Drag must highlight, and releasing must copy.

    The clipboard itself cannot be read back from here — it is write-only from
    the app's side — so the check is what the screen shows and what zcode
    reports it did, which is also all the user gets to see.
    """
    fake = _fake_provider()
    # The port `examples/scrolling` is configured for.
    server = fake.serve(port=8093, fail=0, status=429, lines=6)

    t = Tui(ROOT / "examples" / "scrolling")
    t.ready()
    t.type("say something")
    t.send(ENTER, 1.0)
    check(t.wait_for(r"line 006", timeout=30), "there is something to select")

    # Find a row with text on it, and drag across it.
    rows = t.screen.display
    target = next(i for i, r in enumerate(rows) if "line 003" in r)
    start_col = rows[target].index("line 003") + 1  # 1-based columns
    t.send(mouse(0, start_col, target + 1), 0.2)
    t.send(mouse(32, start_col + 10, target + 1), 0.3)  # 32 = drag
    t.shot("24-selecting", "mid-drag: the selection is highlighted")
    check("line 003" in t.text(), "the row is still legible while selected")

    t.send(mouse(0, start_col + 10, target + 1, release=True), 0.8)
    after = t.text()
    t.shot("25-selection-copied", "released: zcode says what it copied")
    check(
        re.search(r"copied \(", after) is not None,
        f"the copy was reported: {[l for l in after.splitlines() if 'copi' in l]}",
    )
    check("could not copy" not in after, "the copy did not fail")

    # Esc dismisses rather than cancelling anything.
    t.send(ESC, 0.4)
    check("line 003" in t.text(), "the transcript survived the dismiss")

    t.close()
    server.shutdown()


def scenario_scrolling():
    """The conversation must scroll — by wheel, by page, and back to the tail.

    Driven by the fake provider so the pane holds a known number of uniquely
    identifiable rows: "is line 001 on screen?" is a question with an answer,
    where "did it scroll?" is not.
    """
    fake = _fake_provider()
    server = fake.serve(port=8093, fail=0, status=429, lines=60)

    t = Tui(ROOT / "examples" / "scrolling")
    t.ready()
    t.type("say something long")
    t.send(ENTER, 1.0)
    check(t.wait_for(r"line 060", timeout=30), "the answer arrived in full")

    # The tail is what you see first, and 60 lines do not fit in 32 rows.
    check("line 001 " not in t.text(), "the pane is following the tail")

    # 1. The wheel.
    for _ in range(12):
        t.send(WHEEL_UP, 0.05)
    t.pump(0.4)
    scrolled = t.text()
    t.shot("20-scrolled-back", "scrolled back with the mouse wheel")
    check("line 060" not in scrolled, "the wheel moved the view off the tail")
    check(re.search(r"scrolled ↑\d+", scrolled) is not None, "the title says so")

    # 2. Back down, and the tail must be exactly where it was.
    for _ in range(40):
        t.send(WHEEL_DOWN, 0.02)
    t.pump(0.4)
    check("line 060" in t.text(), "scrolling down returns to the tail")
    check("scrolled ↑" not in t.text(), "and the title stops saying otherwise")

    # 3. Scrolling up past the top must not bank invisible scrollback: after
    #    100 notches up, one notch down has to move the view.
    for _ in range(100):
        t.send(WHEEL_UP, 0.01)
    t.pump(0.5)
    top = t.text()
    check("line 001" in top, "scrolling up reaches the first line")
    t.send(WHEEL_DOWN, 0.3)
    check(
        t.text() != top,
        "one notch back down moves the view (no banked scrollback)",
    )

    # 4. The keys.
    t.send(CTRL_END, 0.4)
    check("line 060" in t.text(), "Ctrl-End jumps to the newest line")
    t.send(PAGE_UP, 0.4)
    check("scrolled ↑" in t.text(), "PageUp scrolls back")
    t.send(PAGE_DOWN, 0.4)
    check("line 060" in t.text(), "PageDown follows the tail again")
    t.send(CTRL_HOME, 0.4)
    t.shot("21-scrolled-top", "Ctrl-Home, at the oldest line")
    check("line 001" in t.text(), "Ctrl-Home jumps to the oldest line")

    t.close()
    server.shutdown()


def scenario_providers():
    """Several providers in one config: list them, switch, and be refused."""
    fake = _fake_provider()
    primary = fake.serve(port=8095, fail=0, status=429, lines=2, label="  [primary]")
    t = Tui(ROOT / "examples" / "multi-provider")
    t.ready()

    t.type("/provider")
    t.send(ENTER, 1.0)
    listing = t.text()
    t.shot("22-providers", "the configured providers, active one marked")
    for name in ["primary", "backup", "local"]:
        check(name in listing, f"{name} is listed")
    check("▸ primary" in listing, "the active provider is marked")

    # A name that is not configured must be refused, and must leave the
    # working provider in place rather than stranding the session.
    t.type("/provider nope")
    t.send(ENTER, 1.5)
    check("unknown provider" in t.text(), "an unknown name is reported")
    check("primary" in t.status_line(), "the working provider survived it")

    # A real switch: a second server, so the answer itself proves which
    # endpoint replied.
    backup = fake.serve(port=8094, fail=0, status=429, lines=2, label="  [backup]")
    t.type("/provider backup")
    t.send(ENTER, 1.5)
    t.shot("23-provider-switched", "after /provider backup")
    check("backup" in t.status_line(), "the status bar shows the new provider")
    check("backup-model" in t.status_line(), "and its model")

    t.type("who are you")
    t.send(ENTER, 1.0)
    check(t.wait_for(r"\[backup\]", timeout=30), "the new endpoint is the one answering")

    t.close()
    primary.shutdown()
    backup.shutdown()


def scenario_blocked_shell():
    """A refusal must be readable, and `".*"` must actually allow everything.

    Both halves run against the fake provider rather than a live model: the
    point is what the *guard* does with a given command, and a model that
    decides to run something else proves nothing either way.
    """
    fake = _fake_provider()

    # 1. A narrow allowlist refuses the command the model chose.
    blocked = "cd /workspace && go build ./... 2>&1 | head"
    server = fake.serve(
        port=8098,
        fail=0,
        status=429,
        tool="shell",
        tool_args=json.dumps({"command": blocked}),
    )
    t = Tui(ROOT / "examples" / "blocked-shell")
    t.ready()
    t.type("build the project")
    t.send(ENTER, 1.0)
    got = t.wait_for(r"blocked by the shell allowlist", timeout=30)
    t.shot("18-shell-blocked", "a refusal wrapped in full under its tool row")
    text = t.text()
    server.shutdown()
    check(got, "the refusal reached the screen")
    check("✖" in text, "the tool row is marked failed")
    # The whole message survives, across the rows it wrapped onto.
    joined = " ".join(line.strip() for line in text.splitlines())
    for needle in ["shell_allowed", "zcode.json/zcode.toml", "go build", "hint:"]:
        check(needle in joined, f"the refusal still says {needle!r}")
    check("…" not in joined, "nothing about the refusal was truncated")
    t.close()

    # 2. The same shape of command, under `".*"`, runs.
    ran = "cd /tmp && printf 'a\\nb\\n' 2>&1 | tail -1"
    server = fake.serve(
        port=8097,
        fail=0,
        status=429,
        tool="shell",
        tool_args=json.dumps({"command": ran}),
    )
    t = Tui(ROOT / "examples" / "open-shell")
    t.ready()
    t.type("run the pipeline")
    t.send(ENTER, 1.0)
    got = t.wait_for(r"tools used", timeout=30)
    # A successful run folds itself once it settles; open it to read the row.
    t.send(b"\x14", 0.6)
    got = got and t.wait_for(r"✔.*shell", timeout=10)
    t.shot("19-shell-open", 'the same shape of command under "shell_allowed": [".*"]')
    text = t.text()
    server.shutdown()
    check(got, "the chained command ran under an unrestricted allowlist")
    check("blocked by the shell allowlist" not in text, "it was not refused")
    # The engine times the call; the TUI must not time the channel instead.
    # `text` carries the pane borders too, so anchor on the row, not the line.
    row = next((l for l in text.splitlines() if "✔" in l and "shell" in l), "")
    check(
        re.search(r"\b\d+(ms|\.\ds|m\d\ds|h\d\dm)\b", row) is not None,
        f"the row carries the duration the tool actually took: {row.strip()!r}",
    )
    t.close()


SCENARIOS = {
    "startup": scenario_startup,
    "help": scenario_help_and_modes,
    "paste": scenario_paste_and_wrap,
    "editing": scenario_editing_keys,
    "live": scenario_live_turn,
    "exit": scenario_exit,
    "unknown": scenario_unknown_command,
    "error": scenario_provider_error,
    "ratelimit": scenario_rate_limit,
    "blocked": scenario_blocked_shell,
    "scrolling": scenario_scrolling,
    "selection": scenario_selection,
    "usage": scenario_usage,
    "toolrows": scenario_tool_rows,
    "folding": scenario_folding,
    "providers": scenario_providers,
}


def main():
    args = sys.argv[1:] or list(SCENARIOS)
    if args == ["--list"]:
        print("\n".join(SCENARIOS))
        return 0
    OUT.mkdir(parents=True, exist_ok=True)
    live = {"live", "error"}
    for i, name in enumerate(args):
        if i and name in live and SCENARIO_PAUSE:
            time.sleep(SCENARIO_PAUSE)
        print(f"\n═══ {name} " + "═" * 40)
        SCENARIOS[name]()
    ok = sum(1 for c, _ in CHECKS if c is True)
    skipped = sum(1 for c, _ in CHECKS if c is None)
    bad = sum(1 for c, _ in CHECKS if c is False)
    tally = f"  TUI: {ok} passed, {bad} failed"
    if skipped:
        tally += f", {skipped} skipped (provider throttled)"
    print(f"\n{'═' * 55}\n{tally}\n{'═' * 55}")
    for cond, label in CHECKS:
        if cond is False:
            print(f"  ✗ {label}")
    if skipped:
        print("  Skips are OpenRouter throttling a free route, not zcode failing.")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
