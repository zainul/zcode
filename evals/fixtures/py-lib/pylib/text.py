"""Text helpers."""

import re

_NON_WORD = re.compile(r"[^a-z0-9]+")


def slugify(text: str) -> str:
    """Lower-case, ASCII-only, hyphen-separated slug."""
    return _NON_WORD.sub("-", text.lower()).strip("-")


def word_count(text: str) -> int:
    """Number of whitespace-separated words."""
    return len(text.split())


def truncate_words(text: str, limit: int, suffix: str = "…") -> str:
    """Keep at most `limit` words, appending `suffix` when anything was cut."""
    words = text.split()
    if len(words) <= limit:
        return text
    return " ".join(words[:limit]) + suffix
