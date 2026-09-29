# Baseline — PRD-CTX-EFF-003 §10.3

**Status: not yet captured.**

The baseline is a live corpus run of v0.6.0 behaviour with only the Phase 0
ledger applied (task-21), so that its only effect is measurement. It needs
provider API keys and spends real money (≈144 agent runs), neither of which
was available when this milestone was implemented. No numbers have been
estimated or filled in here, on purpose.

## How to capture it

```sh
git checkout 7e4e0a0   # task-21: v0.6.0 + the token ledger, no behaviour change
git checkout develop-release-context-efficiency -- evals Makefile   # harness from task-22
export ZCODE_ANTHROPIC_API_KEY=… ZCODE_OPENAI_API_KEY=…
make eval-tokens LIVE=1 LABEL=baseline-v0.6.0
```

Then run the same corpus on the release candidate and compare:

```sh
make eval-tokens LIVE=1 LABEL=v0.7.0
make eval-compare BASE=evals/results/baseline-v0.6.0.json NEW=evals/results/v0.7.0.json
```

Record both result files here with the comparison table, and for each PRD
§2.3 target note **keep** or **revise (with evidence)**, editing PRD §2.3 in
the same change if a target is revised.
