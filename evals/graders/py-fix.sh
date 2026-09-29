#!/bin/sh
set -eu
grep -q 'self.assertEqual(median(\[4, 1, 3, 2\]), 2.5)' tests/test_stats.py
python3 -m unittest discover -s tests -t .
