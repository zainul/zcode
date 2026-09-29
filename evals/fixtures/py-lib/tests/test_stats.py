import unittest

from pylib.stats import mean, median, percentile, variance


class StatsTest(unittest.TestCase):
    def test_mean(self):
        self.assertEqual(mean([1, 2, 3, 4]), 2.5)

    def test_mean_empty_raises(self):
        with self.assertRaises(ValueError):
            mean([])

    def test_median_odd_length(self):
        self.assertEqual(median([3, 1, 2]), 2)

    def test_median_even_length(self):
        self.assertEqual(median([4, 1, 3, 2]), 2.5)

    def test_variance(self):
        self.assertEqual(variance([2, 4, 4, 4, 5, 5, 7, 9]), 4)

    def test_percentile(self):
        self.assertEqual(percentile([15, 20, 35, 40, 50], 40), 20)


if __name__ == "__main__":
    unittest.main()
