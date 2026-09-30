"""pylib: small statistics, text and caching helpers."""

from pylib.cache import LRUCache
from pylib.stats import mean, median, percentile, variance
from pylib.text import slugify, truncate_words, word_count

__all__ = [
    "LRUCache",
    "mean",
    "median",
    "percentile",
    "slugify",
    "truncate_words",
    "variance",
    "word_count",
]
