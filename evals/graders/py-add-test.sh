#!/bin/sh
set -eu
[ "$(grep -c 'def test_' tests/test_cache.py)" -gt 2 ]
grep -q 'ValueError' tests/test_cache.py
python3 -m unittest tests.test_cache
