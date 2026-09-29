import unittest

from pylib.cache import LRUCache


class CacheTest(unittest.TestCase):
    def test_get_missing(self):
        self.assertIsNone(LRUCache(2).get("x"))

    def test_evicts_least_recently_used(self):
        c = LRUCache(2)
        c.put("a", 1)
        c.put("b", 2)
        c.get("a")
        c.put("c", 3)
        self.assertIsNone(c.get("b"))
        self.assertEqual(c.get("a"), 1)


if __name__ == "__main__":
    unittest.main()
