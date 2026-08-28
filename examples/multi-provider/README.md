Three endpoints in one config, so `/provider` has something to switch between
and `--provider` has something to select. `primary` and `backup` both point at
`fake-provider.py` on different ports — the switch is only interesting if the
two providers give visibly different answers.

`local` is there to show a profile whose kind supplies its own endpoint: it
states no `base_url`, so it inherits Ollama's.
