#!/bin/sh
set -eu
! grep -rn 'truncate_words' pylib tests
grep -q 'def shorten_words' pylib/text.py
python3 -m unittest tests.test_text
