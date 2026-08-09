#!/usr/bin/env python3
"""Tests for scripts/release/augment_changelog.py (real shipped helpers)."""

from __future__ import annotations

import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

from augment_changelog import (  # noqa: E402
    augment_changelog,
    parse_commit_subject,
)


SAMPLE = """# Changelog

## [Unreleased]

## [0.6.24](https://example/compare/v0.6.23...v0.6.24) - 2026-08-09

### Fixed

- *(agent)* reuse inventory channels for input capture ([#522](https://github.com/AprilNEA/OpenLogi/pull/522))

## [0.6.23](https://example/compare/v0.6.22...v0.6.23) - 2026-08-02

### Fixed

- *(hook)* grab only relative pointer devices ([#401](https://github.com/AprilNEA/OpenLogi/pull/401))
"""


class AugmentChangelogTests(unittest.TestCase):
    def test_parse_fix_with_scope_and_pr(self) -> None:
        commit = parse_commit_subject(
            "fix(i18n): complete Crowdin synchronization (#508)"
        )
        assert commit is not None
        self.assertEqual(commit.type, "fix")
        self.assertEqual(commit.scope, "i18n")
        self.assertEqual(commit.pr, "508")

    def test_parse_ignores_chore(self) -> None:
        self.assertIsNone(parse_commit_subject("chore: release v0.6.24"))

    def test_merges_missing_gui_commit_into_top_section(self) -> None:
        commits = [
            parse_commit_subject(
                "fix(agent): reuse inventory channels for input capture (#522)"
            ),
            parse_commit_subject(
                "fix(i18n): complete Crowdin synchronization (#508)"
            ),
        ]
        assert all(c is not None for c in commits)
        updated = augment_changelog(
            SAMPLE,
            [c for c in commits if c is not None],
            "https://github.com/AprilNEA/OpenLogi",
        )
        # Top section must list both PRs; older 0.6.23 body stays untouched.
        top, _, older = updated.partition("## [0.6.23]")
        self.assertIn("#508", top)
        self.assertIn("#522", top)
        self.assertIn("Crowdin synchronization", top)
        self.assertIn("inventory channels", top)
        self.assertNotIn("#508", older)
        self.assertIn("#401", older)

    def test_idempotent_when_already_present(self) -> None:
        commits = [
            parse_commit_subject(
                "fix(agent): reuse inventory channels for input capture (#522)"
            ),
        ]
        assert commits[0] is not None
        once = augment_changelog(
            SAMPLE, [commits[0]], "https://github.com/AprilNEA/OpenLogi"
        )
        twice = augment_changelog(
            once, [commits[0]], "https://github.com/AprilNEA/OpenLogi"
        )
        self.assertEqual(once, twice)
        self.assertEqual(once.count("#522"), 1)


if __name__ == "__main__":
    unittest.main()
