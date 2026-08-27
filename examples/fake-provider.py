#!/usr/bin/env python3
"""A stand-in OpenAI-compatible endpoint for testing paths a real provider
will not produce on demand.

Rate limits are the obvious one: you cannot ask OpenRouter for a 429 when you
want to see how the client behaves, so the retry path would otherwise ship
untested outside unit tests. This server answers the first N requests with 429
(and a `Retry-After`), then streams a normal SSE completion.

It can also emit a canned tool call, so the paths that depend on what the
*guard* does with a command — a refusal wrapped under its tool row — are
reproducible offline instead of hinging on a model choosing to run `go build`.

    ./examples/fake-provider.py --port 8099 --fail 2 [--status 429]
    ./examples/fake-provider.py --port 8098 --fail 0 \
        --tool shell --tool-args '{"command":"cd /workspace && go build ./..."}'
"""
import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

STATE = {"seen": 0}


class Handler(BaseHTTPRequestHandler):
    fail_times = 2
    fail_status = 429
    retry_after = "1"
    # When set, the first non-failing request answers with this tool call
    # instead of prose; the next one closes the turn with a sentence.
    tool_name = None
    tool_args = "{}"
    # When set, the answer is this many numbered lines — deterministic,
    # individually identifiable content for the scrolling checks.
    reply_lines = 0
    label = ""

    def log_message(self, *_):
        pass  # the test harness owns stdout

    def do_POST(self):
        STATE["seen"] += 1
        length = int(self.headers.get("content-length", 0))
        self.rfile.read(length)

        if STATE["seen"] <= self.fail_times:
            body = json.dumps(
                {"error": {"message": "rate limit exceeded (simulated)"}}
            ).encode()
            self.send_response(self.fail_status)
            self.send_header("content-type", "application/json")
            # Omitting the header is the interesting case: it is what makes
            # the client fall back to its own rate-limit backoff.
            if self.fail_status == 429 and self.retry_after is not None:
                self.send_header("retry-after", self.retry_after)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        if self.reply_lines:
            body = "\n".join(
                f"line {n:03d} of {self.reply_lines}{self.label}"
                for n in range(1, self.reply_lines + 1)
            )
            self.stream(
                [
                    {"choices": [{"delta": {"content": body}}]},
                    {"choices": [{"delta": {}, "finish_reason": "stop"}]},
                    {
                        "choices": [],
                        "usage": {"prompt_tokens": 40, "completion_tokens": self.reply_lines},
                    },
                ]
            )
            return

        if self.tool_name and STATE["seen"] == self.fail_times + 1:
            self.stream(
                [
                    {
                        "choices": [
                            {
                                "delta": {
                                    "content": "Running that for you now."
                                }
                            }
                        ]
                    },
                    {
                        "choices": [
                            {
                                "delta": {
                                    "tool_calls": [
                                        {
                                            "index": 0,
                                            "id": "call_1",
                                            "type": "function",
                                            "function": {
                                                "name": self.tool_name,
                                                "arguments": self.tool_args,
                                            },
                                        }
                                    ]
                                }
                            }
                        ]
                    },
                    {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]},
                    {"choices": [], "usage": {"prompt_tokens": 90, "completion_tokens": 20}},
                ]
            )
            return

        if self.tool_name:
            self.stream(
                [
                    {"choices": [{"delta": {"content": "That is what the shell had to say."}}]},
                    {"choices": [{"delta": {}, "finish_reason": "stop"}]},
                    {"choices": [], "usage": {"prompt_tokens": 140, "completion_tokens": 7}},
                ]
            )
            return

        chunks = [
            {"choices": [{"delta": {"content": "Recovered after "}}]},
            {"choices": [{"delta": {"content": f"{self.fail_times} rate limit(s)."}}]},
            {"choices": [{"delta": {}, "finish_reason": "stop"}]},
            {"choices": [], "usage": {"prompt_tokens": 120, "completion_tokens": 8}},
        ]
        self.stream(chunks)

    def stream(self, chunks):
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for chunk in chunks:
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.flush()
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


def serve(
    port: int,
    fail: int,
    status: int,
    retry_after: str | None = "1",
    tool: str | None = None,
    tool_args: str = "{}",
    lines: int = 0,
    label: str = "",
) -> HTTPServer:
    Handler.fail_times = fail
    Handler.fail_status = status
    Handler.retry_after = retry_after
    Handler.tool_name = tool
    Handler.tool_args = tool_args
    Handler.reply_lines = lines
    Handler.label = label
    STATE["seen"] = 0
    server = HTTPServer(("127.0.0.1", port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8099)
    ap.add_argument("--fail", type=int, default=2)
    ap.add_argument("--status", type=int, default=429)
    ap.add_argument(
        "--no-retry-after",
        action="store_true",
        help="omit Retry-After, so the client's own backoff is what is tested",
    )
    ap.add_argument("--tool", help="answer with a call to this tool instead of prose")
    ap.add_argument("--tool-args", default="{}", help="JSON arguments for --tool")
    ap.add_argument("--lines", type=int, default=0, help="answer with N numbered lines")
    ap.add_argument("--label", default="", help="suffix on each line, to tell servers apart")
    a = ap.parse_args()
    srv = serve(
        a.port,
        a.fail,
        a.status,
        None if a.no_retry_after else "1",
        a.tool,
        a.tool_args,
        a.lines,
        a.label,
    )
    print(f"fake provider on http://127.0.0.1:{a.port} "
          f"— first {a.fail} request(s) answer {a.status}")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        srv.shutdown()
