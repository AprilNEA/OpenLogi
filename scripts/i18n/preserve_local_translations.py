#!/usr/bin/env python3
"""Keep non-English locale values when Crowdin re-exports source text.

Crowdin fills untranslated strings with the English source. Locale catalogs in
git often already carry real translations (hand-seeded or from a prior sync)
that Crowdin never received because the workflow only uploaded sources. A bare
download then rewrites those values back to English.

This script walks each non-English locale file after a Crowdin download and,
for any key whose new value is identical to the English key while the
pre-download catalog had a different value, restores the pre-download value.
Real Crowdin updates (new or changed non-source text) still win.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
import unittest
from pathlib import Path

ENTRY_RE = re.compile(r'^"((?:\\.|[^"\\])*)":\s*"((?:\\.|[^"\\])*)"\s*$')


def unescape(value: str) -> str:
    out: list[str] = []
    i = 0
    while i < len(value):
        if value[i] == "\\" and i + 1 < len(value):
            out.append(value[i + 1])
            i += 2
            continue
        out.append(value[i])
        i += 1
    return "".join(out)


def escape(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"')


def parse_entries(path: Path) -> dict[str, str]:
    entries: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        match = ENTRY_RE.match(line)
        if match is None:
            continue
        entries[unescape(match.group(1))] = unescape(match.group(2))
    return entries


def preserve_file(before: Path, after: Path) -> int:
    if not before.is_file() or not after.is_file():
        return 0

    before_entries = parse_entries(before)
    restored = 0
    lines: list[str] = []
    original = after.read_text(encoding="utf-8")

    for line in original.splitlines():
        match = ENTRY_RE.match(line)
        if match is None:
            lines.append(line)
            continue

        key = unescape(match.group(1))
        value = unescape(match.group(2))
        previous = before_entries.get(key)
        if value == key and previous is not None and previous != key:
            # Keep Crowdin's key escaping; only replace the clobbered value.
            lines.append(f'"{match.group(1)}": "{escape(previous)}"')
            restored += 1
            continue
        lines.append(line)

    text = "\n".join(lines)
    if original.endswith("\n") or not text:
        text += "\n"
    after.write_text(text, encoding="utf-8")
    return restored


def preserve_locales(before_dir: Path, locales_dir: Path) -> int:
    total = 0
    for after in sorted(locales_dir.glob("*.yml")):
        if after.name == "en.yml":
            continue
        restored = preserve_file(before_dir / after.name, after)
        if restored:
            print(f"{after.name}: restored {restored} non-English value(s)")
        total += restored
    return total


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--before",
        type=Path,
        help="Directory of locale YAML files snapshotted before Crowdin download",
    )
    parser.add_argument(
        "--locales",
        type=Path,
        help="Locale directory Crowdin just wrote into",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run built-in regression checks and exit",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(PreserveTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    if args.before is None or args.locales is None:
        parser.error("--before and --locales are required unless --self-test")

    total = preserve_locales(args.before, args.locales)
    print(f"preserved {total} translation value(s) total")
    return 0


class PreserveTests(unittest.TestCase):
    def test_restores_english_clobber_keeps_crowdin_updates(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            (before / "de.yml").write_text(
                "\n".join(
                    [
                        "# header",
                        "_version: 1",
                        '"Camera": "Kamera"',
                        '"Sleep": "Ruhezustand"',
                        '"New feature": "New feature"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            (after / "de.yml").write_text(
                "\n".join(
                    [
                        "# header",
                        "_version: 1",
                        '"Camera": "Camera"',
                        '"Sleep": "Schlafen"',
                        '"New feature": "Neue Funktion"',
                        '"Brand new": "Brand new"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )

            total = preserve_locales(before, after)
            text = (after / "de.yml").read_text(encoding="utf-8")
            self.assertEqual(total, 1)
            self.assertIn('"Camera": "Kamera"', text)
            self.assertIn('"Sleep": "Schlafen"', text)
            self.assertIn('"New feature": "Neue Funktion"', text)
            self.assertIn('"Brand new": "Brand new"', text)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
