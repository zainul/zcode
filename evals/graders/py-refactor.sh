#!/bin/sh
set -eu
grep -q 'def word_count' pylib/counting.py
! grep -q 'def word_count' pylib/text.py
grep -q 'pylib.counting' tests/test_text.py
python3 -c 'from pylib.text import word_count; from pylib import word_count as w; assert word_count("a b") == 2 == w("a b")'
python3 -m unittest tests.test_text
