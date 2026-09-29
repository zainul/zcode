"""Descriptive statistics over sequences of numbers."""

from typing import Sequence


def mean(values: Sequence[float]) -> float:
    """Arithmetic mean. Raises ValueError on an empty sequence."""
    if not values:
        raise ValueError("mean() of an empty sequence")
    return sum(values) / len(values)


def median(values: Sequence[float]) -> float:
    """Middle value of the sorted data; the mean of the two middle values
    when there is an even number of them."""
    if not values:
        raise ValueError("median() of an empty sequence")
    ordered = sorted(values)
    mid = len(ordered) // 2
    if len(ordered) % 2 == 1:
        return ordered[mid]
    return ordered[mid]


def variance(values: Sequence[float]) -> float:
    """Population variance."""
    m = mean(values)
    return sum((v - m) ** 2 for v in values) / len(values)


def percentile(values: Sequence[float], p: float) -> float:
    """Nearest-rank percentile, p in [0, 100]."""
    if not 0 <= p <= 100:
        raise ValueError("p must be within [0, 100]")
    if not values:
        raise ValueError("percentile() of an empty sequence")
    ordered = sorted(values)
    rank = max(1, round(p / 100 * len(ordered)))
    return ordered[rank - 1]
