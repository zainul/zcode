#!/bin/sh
set -eu
grep -q 'histogram' tests/test_stats.py
python3 - <<'PY'
from pylib import histogram
assert histogram([1, 2, 3, 4], 2) == [2, 2], histogram([1, 2, 3, 4], 2)
assert histogram([0, 10], 5) == [1, 0, 0, 0, 1]
assert histogram([3, 3, 3], 4) == [3, 0, 0, 0]
for bad in (([], 2), ([1], 0)):
    try:
        histogram(*bad)
    except ValueError:
        pass
    else:
        raise AssertionError(f"no ValueError for {bad}")
PY
python3 -m unittest tests.test_stats.StatsTest.test_mean tests.test_cache
