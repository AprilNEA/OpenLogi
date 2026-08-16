#!/usr/bin/env python3
"""Merge a Crowdin locale download into complete git catalogs.

Crowdin download overwrites each non-English `locales/*.yml` file. Two failure
modes that raw download creates:

1. Untranslated strings exported as English source (`"Key": "Key"`) — #549.
2. `skip_untranslated_strings` exports a sparse file and deletes every other
   key (including headers) — #552.

This script treats the pre-download git catalogs as the complete base and only
accepts Crowdin values that differ from the English source in `en.yml`. Keys
Crowdin omitted stay as they were. Result always has every `en.yml` key (parity)
and keeps the pre-download header / `_version` lines.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
import unittest
from pathlib import Path

ENTRY_RE = re.compile(r'^"((?:\\.|[^"\\])*)":\s*"((?:\\.|[^"\\])*)"\s*$')

DEFAULT_HEADER = (
    "# OpenLogi GUI translations. Managed by Crowdin; "
    "edit source text there when possible.\n"
    "_version: 1\n"
)


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


def parse_entries(text: str) -> dict[str, str]:
    entries: dict[str, str] = {}
    for line in text.splitlines():
        match = ENTRY_RE.match(line)
        if match is None:
            continue
        entries[unescape(match.group(1))] = unescape(match.group(2))
    return entries


def parse_entries_path(path: Path) -> dict[str, str]:
    return parse_entries(path.read_text(encoding="utf-8"))


def header_lines(text: str) -> list[str]:
    """Non-entry lines before the first translation entry."""
    lines: list[str] = []
    for line in text.splitlines():
        if ENTRY_RE.match(line):
            break
        lines.append(line)
    return lines


def en_key_order(en_text: str) -> list[str]:
    order: list[str] = []
    for line in en_text.splitlines():
        match = ENTRY_RE.match(line)
        if match is None:
            continue
        order.append(unescape(match.group(1)))
    return order


def merge_catalog(
    en_entries: dict[str, str],
    en_order: list[str],
    before_text: str,
    after_text: str,
) -> str:
    before_entries = parse_entries(before_text)
    after_entries = parse_entries(after_text)
    header = header_lines(before_text)
    if not any(line.strip() for line in header):
        header = DEFAULT_HEADER.rstrip("\n").split("\n")

    merged: dict[str, str] = {}
    for key in en_order:
        source = en_entries[key]
        previous = before_entries.get(key, source)
        crowdin = after_entries.get(key)
        if crowdin is not None and crowdin != source:
            merged[key] = crowdin
        else:
            merged[key] = previous

    out_lines = list(header)
    while out_lines and out_lines[-1] == "":
        out_lines.pop()
    for key in en_order:
        out_lines.append(f'"{escape(key)}": "{escape(merged[key])}"')
    return "\n".join(out_lines) + "\n"


def merge_locales(before_dir: Path, locales_dir: Path, en_path: Path) -> int:
    en_text = en_path.read_text(encoding="utf-8")
    en_entries = parse_entries(en_text)
    en_order = en_key_order(en_text)
    if not en_order:
        raise SystemExit(f"{en_path} has no translation entries")

    changed_files = 0
    for after_path in sorted(locales_dir.glob("*.yml")):
        if after_path.name == "en.yml":
            continue
        before_path = before_dir / after_path.name
        if not before_path.is_file():
            print(f"skip {after_path.name}: no pre-download snapshot", file=sys.stderr)
            continue
        before_text = before_path.read_text(encoding="utf-8")
        after_text = after_path.read_text(encoding="utf-8")
        merged = merge_catalog(en_entries, en_order, before_text, after_text)
        if merged == after_text:
            continue

        before_entries = parse_entries(before_text)
        after_entries = parse_entries(after_text)
        improvements = 0
        for key, source in en_entries.items():
            crowdin = after_entries.get(key)
            previous = before_entries.get(key, source)
            if crowdin is not None and crowdin != source and crowdin != previous:
                improvements += 1

        after_path.write_text(merged, encoding="utf-8")
        changed_files += 1
        if improvements:
            print(
                f"{after_path.name}: applied {improvements} Crowdin "
                f"improvement(s); kept {len(en_order)} keys for parity"
            )
        else:
            print(
                f"{after_path.name}: restored complete catalog "
                f"({len(en_order)} keys; no Crowdin improvements)"
            )
    return changed_files


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
        "--en",
        type=Path,
        help="Path to en.yml (English source of truth)",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run built-in regression checks and exit",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(MergeTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    if args.before is None or args.locales is None or args.en is None:
        parser.error("--before, --locales, and --en are required unless --self-test")

    changed = merge_locales(args.before, args.locales, args.en)
    print(f"updated {changed} locale file(s)")
    return 0


class MergeTests(unittest.TestCase):
    def test_sparse_download_keeps_parity_and_accepts_real_updates(self) -> None:
        """#552: skip_untranslated export must not delete keys or headers."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            en = root / "en.yml"
            en.write_text(
                "\n".join(
                    [
                        "_version: 1",
                        '"Camera": "Camera"',
                        '"Sleep": "Sleep"',
                        '"DPI": "DPI"',
                        '"Back / Forward": "Back / Forward"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            (before / "de.yml").write_text(
                "\n".join(
                    [
                        "# OpenLogi GUI translations.",
                        "_version: 1",
                        '"Camera": "Kamera"',
                        '"Sleep": "Ruhezustand"',
                        '"DPI": "DPI"',
                        '"Back / Forward": "Zurück / Vor"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            # Sparse Crowdin export: only one improved string, no header.
            (after / "de.yml").write_text(
                '"Sleep": "Schlafen"\n',
                encoding="utf-8",
            )

            changed = merge_locales(before, after, en)
            text = (after / "de.yml").read_text(encoding="utf-8")
            self.assertEqual(changed, 1)
            self.assertIn("# OpenLogi GUI translations.", text)
            self.assertIn("_version: 1", text)
            self.assertIn('"Camera": "Kamera"', text)
            self.assertIn('"Sleep": "Schlafen"', text)
            self.assertIn('"DPI": "DPI"', text)
            self.assertIn('"Back / Forward": "Zurück / Vor"', text)
            self.assertEqual(len(parse_entries(text)), 4)

    def test_english_fill_in_does_not_clobber_real_translations(self) -> None:
        """#549 / older clobber: English export values must not wipe git."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            en = root / "en.yml"
            en.write_text(
                "\n".join(
                    [
                        '"Camera": "Camera"',
                        '"Sleep": "Sleep"',
                        '"New feature": "New feature"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            (before / "de.yml").write_text(
                "\n".join(
                    [
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
                        "_version: 1",
                        '"Camera": "Camera"',
                        '"Sleep": "Schlafen"',
                        '"New feature": "Neue Funktion"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )

            merge_locales(before, after, en)
            text = (after / "de.yml").read_text(encoding="utf-8")
            self.assertIn('"Camera": "Kamera"', text)
            self.assertIn('"Sleep": "Schlafen"', text)
            self.assertIn('"New feature": "Neue Funktion"', text)

    def test_english_only_export_restores_git_values(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            en = root / "en.yml"
            en.write_text(
                "\n".join(['"Camera": "Camera"', '"Sleep": "Sleep"', ""]),
                encoding="utf-8",
            )
            (before / "de.yml").write_text(
                "\n".join(
                    [
                        "_version: 1",
                        '"Camera": "Kamera"',
                        '"Sleep": "Ruhezustand"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            (after / "de.yml").write_text(
                "\n".join(
                    [
                        "_version: 1",
                        '"Camera": "Camera"',
                        '"Sleep": "Sleep"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            merge_locales(before, after, en)
            text = (after / "de.yml").read_text(encoding="utf-8")
            self.assertIn('"Camera": "Kamera"', text)
            self.assertIn('"Sleep": "Ruhezustand"', text)
            self.assertNotIn('"Camera": "Camera"', text)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
