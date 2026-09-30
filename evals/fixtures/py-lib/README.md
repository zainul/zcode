# pylib

Small utility library used as an evaluation fixture for zcode. Run the tests
with `python3 -m unittest discover -s tests -t .`.

`tests/test_stats.py::test_median_even_length` fails on purpose: the
single-file-fix task asks the agent to find and fix the bug.
