#!/usr/bin/env python3
"""Count catalog values that still equal the English source.

`crowdin upload translations` skips a value equal to its source string
(`import_eq_suggestions` stays off), so these keys remain untranslated in
Crowdin. The Crowdin workflow writes the counts to its job summary as a
translation backlog metric; they never fail the job.
"""

from __future__ import annotations

import argparse
import sys
import tempfile
import unittest
from pathlib import Path

from merge_crowdin_download import catalog, parse_entries_path


def english_fill_in(en_entries: dict[str, str], entries: dict[str, str]) -> int:
    return sum(1 for key, source in en_entries.items() if entries.get(key) == source)


def report(locales_dir: Path, en_path: Path) -> str:
    en_entries = parse_entries_path(en_path)
    lines = [
        "### Catalog values equal to English",
        "",
        (
            "Crowdin does not import these as translations, so translators see "
            f"them as untranslated. {len(en_entries)} keys per catalog."
        ),
        "",
        "| Locale | Keys | Share |",
        "| --- | ---: | ---: |",
    ]
    for path in sorted(locales_dir.glob("*.toml")):
        if path.name == en_path.name:
            continue
        count = english_fill_in(en_entries, parse_entries_path(path))
        lines.append(f"| `{path.stem}` | {count} | {count / len(en_entries):.1%} |")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--locales", type=Path, help="Locale directory to count")
    parser.add_argument(
        "--en", type=Path, help="Path to en.toml (English source of truth)"
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run built-in regression checks and exit",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ReportTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    if args.locales is None or args.en is None:
        parser.error("--locales and --en are required unless --self-test")

    sys.stdout.write(report(args.locales, args.en))
    return 0


class ReportTests(unittest.TestCase):
    def test_counts_exact_matches_of_the_same_key(self) -> None:
        en = {"tray.title": "Title", "device.title": "Device", "actions.dpi": "DPI"}
        entries = {
            # Swapped values match English text, but not for their own keys.
            "tray.title": "Device",
            "device.title": "Title",
            # Exact match only: case and whitespace differences are translations.
            "actions.dpi": "dpi ",
        }
        self.assertEqual(english_fill_in(en, entries), 0)
        entries["actions.dpi"] = "DPI"
        self.assertEqual(english_fill_in(en, entries), 1)

    def test_report_lists_every_non_english_catalog(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            locales = Path(tmp)
            en = locales / "en.toml"
            en.write_text(
                catalog(
                    {
                        "camera": {"title": "Camera"},
                        "actions": {"sleep": "Sleep", "dpi": "DPI", "new": "New"},
                    }
                ),
                encoding="utf-8",
            )
            (locales / "de.toml").write_text(
                catalog(
                    {
                        "camera": {"title": "Kamera"},
                        "actions": {"sleep": "Ruhezustand", "dpi": "DPI", "new": "New"},
                    }
                ),
                encoding="utf-8",
            )
            (locales / "ja.toml").write_text(
                catalog(
                    {
                        "camera": {"title": "カメラ"},
                        "actions": {"sleep": "スリープ", "dpi": "DPI", "new": "新規"},
                    }
                ),
                encoding="utf-8",
            )

            rows = [
                line
                for line in report(locales, en).splitlines()
                if line.startswith("| `")
            ]
            self.assertEqual(rows, ["| `de` | 2 | 50.0% |", "| `ja` | 1 | 25.0% |"])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
