A paid model the local price table has never heard of (`z-ai/glm-5.3-flash`),
pointed at `fake-provider`-style stub that reports `usage.cost` the way
OpenRouter does.

It exists for the `usage` scenario, which holds the second provider call open
so the screen can be read *mid-turn*. That is the case that was broken: the
status bar showed `0 in / 0 out | n/a` for the whole of a multi-step turn,
because usage was only reported when the turn ended and the cost came from a
table with no entry for this model.
