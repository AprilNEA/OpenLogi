#!/usr/bin/env python3
"""Merge a sparse Crowdin TOML download into complete git catalogs.

Crowdin overwrites each non-English `locales/*.toml` file when it downloads
translations. This script keeps the pre-download catalog as the complete base
and only accepts exported values that differ from the English source. Keys
Crowdin omitted remain unchanged, and English fill-in never replaces an
existing translation.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

DEFAULT_HEADER = (
    "# OpenLogi GUI translations. Managed by Crowdin; "
    "edit source text there when possible.\n"
    "_version = 1\n"
)


def parse_entries(text: str) -> dict[str, str]:
    document = tomllib.loads(text)
    entries: dict[str, str] = {}

    def flatten(prefix: str, value: object) -> None:
        if isinstance(value, dict):
            for key, child in value.items():
                flatten(f"{prefix}.{key}" if prefix else key, child)
            return
        if not isinstance(value, str):
            raise ValueError(f"translation {prefix!r} must be a string")
        entries[prefix] = value

    for table, values in document.items():
        if table == "_version":
            continue
        if not isinstance(values, dict):
            raise ValueError(f"{table!r} must be a TOML table")
        flatten(table, values)
    return entries


def parse_entries_path(path: Path) -> dict[str, str]:
    return parse_entries(path.read_text(encoding="utf-8"))


def header_lines(text: str) -> list[str]:
    """Return comments and metadata before the first translation table."""
    lines: list[str] = []
    for line in text.splitlines():
        if line.strip().startswith("["):
            break
        lines.append(line)
    if lines:
        return lines
    return DEFAULT_HEADER.rstrip("\n").split("\n")


def toml_string(value: str) -> str:
    """JSON basic strings are also valid TOML basic strings."""
    return json.dumps(value, ensure_ascii=False)


def merge_catalog(
    en_entries: dict[str, str],
    en_order: list[str],
    before_text: str,
    after_text: str,
) -> str:
    before_entries = parse_entries(before_text)
    after_entries = parse_entries(after_text)

    merged: dict[str, str] = {}
    for key in en_order:
        source = en_entries[key]
        previous = before_entries.get(key, source)
        crowdin = after_entries.get(key)
        merged[key] = crowdin if crowdin is not None and crowdin != source else previous

    out_lines = header_lines(before_text)
    while out_lines and not out_lines[-1]:
        out_lines.pop()
    current_table = None
    for full_key in en_order:
        table, key = full_key.rsplit(".", 1)
        if table != current_table:
            out_lines.extend(["", f"[{table}]"])
            current_table = table
        out_lines.append(f"{key} = {toml_string(merged[full_key])}")
    return "\n".join(out_lines) + "\n"


def merge_locales(before_dir: Path, locales_dir: Path, en_path: Path) -> int:
    en_entries = parse_entries_path(en_path)
    en_order = list(en_entries)
    if not en_order:
        raise SystemExit(f"{en_path} has no translation entries")

    changed_files = 0
    for after_path in sorted(locales_dir.glob("*.toml")):
        if after_path.name == "en.toml":
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
        improvements = sum(
            1
            for key, source in en_entries.items()
            if (crowdin := after_entries.get(key)) is not None
            and crowdin != source
            and crowdin != before_entries.get(key, source)
        )

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
        help="Directory of locale TOML files snapshotted before Crowdin download",
    )
    parser.add_argument(
        "--locales",
        type=Path,
        help="Locale directory Crowdin just wrote into",
    )
    parser.add_argument(
        "--en",
        type=Path,
        help="Path to en.toml (English source of truth)",
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


def catalog(entries: dict[str, dict[str, str]], *, comment: bool = False) -> str:
    lines = []
    if comment:
        lines.append("# OpenLogi GUI translations.")
    lines.append("_version = 1")
    for table, values in entries.items():
        lines.extend(["", f"[{table}]"])
        lines.extend(f"{key} = {toml_string(value)}" for key, value in values.items())
    return "\n".join(lines) + "\n"


class MergeTests(unittest.TestCase):
    def test_sparse_download_keeps_parity_and_accepts_real_updates(self) -> None:
        """#552: skip_untranslated export must not delete keys or headers."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            en = root / "en.toml"
            en.write_text(
                catalog(
                    {
                        "camera": {"title": "Camera"},
                        "actions": {
                            "sleep": "Sleep",
                            "dpi": "DPI",
                            "back_forward": "Back / Forward",
                        },
                    }
                ),
                encoding="utf-8",
            )
            (before / "de.toml").write_text(
                catalog(
                    {
                        "camera": {"title": "Kamera"},
                        "actions": {
                            "sleep": "Ruhezustand",
                            "dpi": "DPI",
                            "back_forward": "Zurück / Vor",
                        },
                    },
                    comment=True,
                ),
                encoding="utf-8",
            )
            (after / "de.toml").write_text(
                "[actions]\nsleep = \"Schlafen\"\n",
                encoding="utf-8",
            )

            changed = merge_locales(before, after, en)
            text = (after / "de.toml").read_text(encoding="utf-8")
            self.assertEqual(changed, 1)
            self.assertIn("# OpenLogi GUI translations.", text)
            self.assertIn("_version = 1", text)
            self.assertEqual(
                parse_entries(text),
                {
                    "camera.title": "Kamera",
                    "actions.sleep": "Schlafen",
                    "actions.dpi": "DPI",
                    "actions.back_forward": "Zurück / Vor",
                },
            )

    def test_english_fill_in_does_not_clobber_real_translations(self) -> None:
        """#549: English export values must not wipe git translations."""
        en = catalog(
            {
                "camera": {"title": "Camera"},
                "actions": {"sleep": "Sleep", "new_feature": "New feature"},
            }
        )
        before = catalog(
            {
                "camera": {"title": "Kamera"},
                "actions": {
                    "sleep": "Ruhezustand",
                    "new_feature": "New feature",
                },
            }
        )
        after = catalog(
            {
                "camera": {"title": "Camera"},
                "actions": {
                    "sleep": "Schlafen",
                    "new_feature": "Neue Funktion",
                },
            }
        )
        en_entries = parse_entries(en)
        merged = parse_entries(merge_catalog(en_entries, list(en_entries), before, after))
        self.assertEqual(
            merged,
            {
                "camera.title": "Kamera",
                "actions.sleep": "Schlafen",
                "actions.new_feature": "Neue Funktion",
            },
        )

    def test_english_only_export_restores_git_values(self) -> None:
        en = catalog({"camera": {"title": "Camera"}, "actions": {"sleep": "Sleep"}})
        before = catalog(
            {"camera": {"title": "Kamera"}, "actions": {"sleep": "Ruhezustand"}}
        )
        after = catalog(
            {"camera": {"title": "Camera"}, "actions": {"sleep": "Sleep"}}
        )
        en_entries = parse_entries(en)
        merged = parse_entries(merge_catalog(en_entries, list(en_entries), before, after))
        self.assertEqual(
            merged, {"camera.title": "Kamera", "actions.sleep": "Ruhezustand"}
        )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
