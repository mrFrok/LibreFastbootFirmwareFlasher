#!/usr/bin/env python3
"""Tests for release-notes.py."""

import importlib.util
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("rn", HERE / "release-notes.py")
rn = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rn)

TEXT = """# Changelog

## 3.0.0 — 2026-10-22

New interface.

### Fixes

- one

## 2.9.4

Old.
"""


class TestSection(unittest.TestCase):
    def test_takes_one_section_up_to_the_next(self):
        notes = rn.section(TEXT, "3.0.0")
        self.assertTrue(notes.startswith("New interface."))
        self.assertIn("### Fixes", notes)
        self.assertNotIn("Old.", notes)

    def test_accepts_a_tag_with_v(self):
        self.assertEqual(rn.section(TEXT, "v2.9.4"), "Old.")

    def test_a_missing_version_is_none(self):
        self.assertIsNone(rn.section(TEXT, "3.1.0"))

    def test_a_prefix_is_not_a_match(self):
        self.assertIsNone(rn.section(TEXT, "3.0"))

    def test_an_empty_section_is_none(self):
        self.assertIsNone(rn.section("## 4.0.0\n\n## 3.0.0\nx\n", "4.0.0"))

    def test_the_real_changelog_has_this_version(self):
        version = None
        for line in (HERE.parent / "cli" / "Cargo.toml").read_text().splitlines():
            if line.startswith("version = "):
                version = line.split('"')[1]
                break
        self.assertIsNotNone(rn.section(rn.CHANGELOG.read_text(), version))


if __name__ == "__main__":
    unittest.main()
