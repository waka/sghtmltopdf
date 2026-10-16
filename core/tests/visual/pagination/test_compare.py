import tempfile
import unittest
from pathlib import Path

import pymupdf

from compare import cases, cli_args, compare_page, write_report


class VisualComparisonTests(unittest.TestCase):
    def test_matrix_has_unique_names_and_existing_inputs(self):
        from compare import FIXTURES

        matrix = cases()
        self.assertEqual(len(matrix), 35)
        self.assertEqual(len({case["name"] for case in matrix}), 35)
        for case in matrix:
            self.assertTrue((FIXTURES / case["body"]).is_file())
            if case["cover"]:
                self.assertTrue((FIXTURES / case["cover"]).is_file())
            self.assertNotIn("--streaming", cli_args(case))

    def test_identical_pages_match_and_changed_pages_fail(self):
        with tempfile.TemporaryDirectory() as temp, pymupdf.open() as left, pymupdf.open() as right:
            directory = Path(temp)
            a = left.new_page(width=30, height=30)
            b = right.new_page(width=30, height=30)
            self.assertTrue(compare_page(a, b, directory, 1, 72, 0)["equal"])
            b.draw_rect(pymupdf.Rect(2, 2, 15, 15), color=(1, 0, 0), fill=(1, 0, 0))
            result = compare_page(a, b, directory, 1, 72, 0)
            self.assertFalse(result["equal"])
            self.assertGreater(result["changed_pixels"], 0)
            self.assertTrue((directory / "page-001-diff.png").is_file())
            self.assertTrue(compare_page(a, b, directory, 1, 72, 255)["equal"])

    def test_missing_page_and_dimensions_fail(self):
        with tempfile.TemporaryDirectory() as temp, pymupdf.open() as left, pymupdf.open() as right:
            directory = Path(temp)
            a = left.new_page(width=30, height=30)
            b = right.new_page(width=40, height=30)
            self.assertFalse(compare_page(a, None, directory, 1, 72, 0)["equal"])
            self.assertFalse(compare_page(a, b, directory, 1, 72, 0)["equal"])

    def test_report_handles_conversion_failures_and_escapes_text(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            write_report(directory, [dict(name="failed", equal=False, error="<broken>")], {})
            report = (directory / "index.html").read_text(encoding="utf-8")
            self.assertIn("&lt;broken&gt;", report)
            self.assertTrue((directory / "results.json").is_file())


if __name__ == "__main__":
    unittest.main()
