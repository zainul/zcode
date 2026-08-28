Three endpoints in one config, so `/provider` has something to switch between
and `--provider` — and the `<provider>/<model>` form of `--model` — has
something to select. `primary` and `backup` both point at `fake-provider.py` on
different ports — the switch is only interesting if the two providers give
visibly different answers.

`local` is there to show a profile whose kind supplies its own endpoint: it
states no `base_url`, so it inherits Ollama's. That is also what makes it
useful for `-m local/qwen2.5-coder`: the acceptance run asserts the request went
to port 11434 and *not* to `primary`'s 8095, which is the whole claim the
prefix makes.

The same directory pins the rest of the `<provider>/<model>` format:
`-m primary/z-ai/glm-4.6` splits at the first slash only and must reach 8095,
`-m some-model` has no slash and must stay on the selected provider, and
`-m z-ai/glm-4.6` must be refused — `z-ai` names no provider here, and the
error has to suggest `primary/z-ai/glm-4.6`.
