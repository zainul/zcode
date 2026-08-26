# 10. Multimodal input

← [MCP & LSP](09-mcp-and-lsp.md) · [Index](README.md) · Next: [JSON output & telemetry](11-json-and-telemetry.md)

Vision-capable models can be given images alongside the prompt — screenshots of
a failing UI, a diagram of the architecture you want, a photo of a whiteboard.

## Sending an image

```sh
$ zcode run --image screenshot.png "the layout breaks at this width — what's wrong in src/app.css?"
```

`--image` is repeatable:

```sh
$ zcode run --image before.png --image after.png "why did the spacing change?"
```

The file is read, base64-encoded, and attached to the first message of the run.
Supported types are chosen by extension: `.png`, `.gif`, `.webp`, and anything
else is sent as `image/jpeg`.

## Which providers accept images

| Provider | Vision |
|----------|--------|
| `openrouter` | Yes, for vision-capable models |
| `openai` | Yes (`gpt-4o`, `gpt-4o-mini`, …) |
| `anthropic` | Yes (Claude 3 and later) |
| `deepseek` | Depends on the model |
| `ollama` | **No** — a warning is prepended to the reply |
| `vllm` / `openai-compatible` | Depends on the served model |

Each provider gets the image in its own native shape — `image_url` blocks for
the OpenAI wire format, `source`/base64 blocks for Anthropic — so the same
command works across vendors.

With Ollama you will see:

```
(warning: ollama does not support vision)
```

and the run continues text-only rather than failing.

## Cost

Images are expensive in tokens, so they are attached to the **first** turn
only. A ten-step run does not re-bill the image ten times. The cost shows up in
`input_tokens` in the run summary and report:

```
[3 step(s) · 4820 in / 210 out / 0 cached tokens · session 01a03bd4-...]
```

## Practical notes

- Crop before sending. A screenshot of one broken component costs far fewer
  tokens than a 4K desktop capture, and the model reads it better.
- Say what to look at. "The button overlaps the label at the top right" beats
  "what's wrong here?".
- Combine with planning mode for a review that cannot touch anything:
  `zcode run --mode planning --image mock.png "how close is our implementation?"`

---

Next: [JSON output & telemetry](11-json-and-telemetry.md)
