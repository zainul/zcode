import unittest

from pylib.text import slugify, truncate_words, word_count


class TextTest(unittest.TestCase):
    def test_slugify(self):
        self.assertEqual(slugify("Hello, World!"), "hello-world")

    def test_word_count(self):
        self.assertEqual(word_count("a b  c"), 3)

    def test_truncate_words(self):
        self.assertEqual(truncate_words("one two three", 2), "one two…")
        self.assertEqual(truncate_words("one two", 2), "one two")


if __name__ == "__main__":
    unittest.main()
