#!/usr/bin/env python3
"""Audit and repair GUI locale YAML after a Crowdin download.

OpenLogi keys are English source text. Across **every** locale catalog, a value
equal to the English source (from `en.yml`) is treated as untranslated / wrong
when Crowdin fills missing translations with source.

Merge rules per key (all locale files under `locales/`):

1. Crowdin value is translated → use Crowdin (fixes English-on-master when
   Crowdin has a real string; also accepts Crowdin updates).
2. Else master/git value is translated → keep git (Crowdin source fill-in must
   not clobber hand-seeded or previously good strings).
3. Else keep Crowdin/export (both still English source fill-in).

The English catalog (`en.yml`) is the reference for “source text” and is still
merged like every other file (Crowdin/source updates apply). “Still English”
counts are for catalogs that should differ from source.

Writes repaired catalogs in place, prints a summary, and optionally writes a
Markdown report for the bot PR body.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
import unittest
from dataclasses import dataclass, field
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
    if not path.is_file():
        return entries
    for line in path.read_text(encoding="utf-8").splitlines():
        match = ENTRY_RE.match(line)
        if match is None:
            continue
        entries[unescape(match.group(1))] = unescape(match.group(2))
    return entries


def english_source(key: str, en: dict[str, str]) -> str:
    return en.get(key, key)


def is_translated(key: str, value: str, en: dict[str, str]) -> bool:
    return value != english_source(key, en)


@dataclass
class LocaleStats:
    name: str
    fixed: list[str] = field(default_factory=list)
    preserved: list[str] = field(default_factory=list)
    crowdin_updated: list[str] = field(default_factory=list)
    still_english: list[str] = field(default_factory=list)

    @property
    def changed(self) -> bool:
        return bool(self.fixed or self.preserved or self.crowdin_updated)


@dataclass
class RepairReport:
    locales: list[LocaleStats] = field(default_factory=list)

    @property
    def fixed_count(self) -> int:
        return sum(len(item.fixed) for item in self.locales)

    @property
    def preserved_count(self) -> int:
        return sum(len(item.preserved) for item in self.locales)

    @property
    def crowdin_updated_count(self) -> int:
        return sum(len(item.crowdin_updated) for item in self.locales)

    @property
    def still_english_count(self) -> int:
        return sum(len(item.still_english) for item in self.locales)

    def markdown(self) -> str:
        lines = [
            "## Locale repair report",
            "",
            "English source fill-in (`value == en.yml source`) is treated as untranslated.",
            "Every locale file under `locales/` is audited with the same rules.",
            "",
            f"- **Fixed** (were English source on master, now translated): **{self.fixed_count}**",
            f"- **Preserved** (blocked Crowdin English clobber): **{self.preserved_count}**",
            f"- **Crowdin updates** (translated text changed): **{self.crowdin_updated_count}**",
            f"- **Still English source fill-in** (needs translators): **{self.still_english_count}**",
            "",
        ]
        for locale in self.locales:
            if not locale.changed and not locale.still_english:
                continue
            lines.append(f"### `{locale.name}`")
            if locale.fixed:
                lines.append(
                    f"- Fixed {len(locale.fixed)}: "
                    + ", ".join(f"`{k}`" for k in locale.fixed[:30])
                )
                if len(locale.fixed) > 30:
                    lines.append(f"  - …and {len(locale.fixed) - 30} more")
            if locale.preserved:
                lines.append(
                    f"- Preserved {len(locale.preserved)} against Crowdin source fill-in"
                )
            if locale.crowdin_updated:
                lines.append(
                    f"- Applied {len(locale.crowdin_updated)} Crowdin translation update(s)"
                )
            if locale.still_english:
                lines.append(f"- Still English source fill-in: {len(locale.still_english)} key(s)")
            lines.append("")
        return "\n".join(lines).rstrip() + "\n"


def repair_file(
    before: Path,
    after: Path,
    en: dict[str, str],
    locale_name: str,
    *,
    track_still_english: bool,
) -> LocaleStats:
    stats = LocaleStats(name=locale_name)
    if not after.is_file():
        return stats

    before_entries = parse_entries(before)
    original = after.read_text(encoding="utf-8")
    lines: list[str] = []

    for line in original.splitlines():
        match = ENTRY_RE.match(line)
        if match is None:
            lines.append(line)
            continue

        key_raw = match.group(1)
        key = unescape(key_raw)
        crowdin_value = unescape(match.group(2))
        master_value = before_entries.get(key, crowdin_value)

        crowdin_ok = is_translated(key, crowdin_value, en)
        master_ok = is_translated(key, master_value, en)

        if crowdin_ok:
            chosen = crowdin_value
            if not master_ok:
                stats.fixed.append(key)
            elif crowdin_value != master_value:
                stats.crowdin_updated.append(key)
        elif master_ok:
            chosen = master_value
            stats.preserved.append(key)
        else:
            chosen = crowdin_value
            if track_still_english:
                stats.still_english.append(key)

        if chosen == crowdin_value:
            lines.append(line)
        else:
            lines.append(f'"{key_raw}": "{escape(chosen)}"')

    text = "\n".join(lines)
    if original.endswith("\n") or not text:
        text += "\n"
    after.write_text(text, encoding="utf-8")
    return stats


def repair_locales(before_dir: Path, locales_dir: Path, en_path: Path) -> RepairReport:
    en = parse_entries(en_path)
    en_name = en_path.name
    report = RepairReport()
    for after in sorted(locales_dir.glob("*.yml")):
        # Same rules for every locale file, including en.yml.
        stats = repair_file(
            before_dir / after.name,
            after,
            en,
            after.name,
            # en.yml is the English source catalog — values matching source are
            # expected, not "still needs translation".
            track_still_english=after.name != en_name,
        )
        report.locales.append(stats)
        bits: list[str] = []
        if stats.fixed:
            bits.append(f"fixed {len(stats.fixed)}")
        if stats.preserved:
            bits.append(f"preserved {len(stats.preserved)}")
        if stats.crowdin_updated:
            bits.append(f"crowdin {len(stats.crowdin_updated)}")
        if stats.still_english:
            bits.append(f"still-english {len(stats.still_english)}")
        if bits:
            print(f"{after.name}: " + ", ".join(bits))
    return report


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--before",
        type=Path,
        help="Locale directory snapshotted from master before Crowdin download",
    )
    parser.add_argument(
        "--locales",
        type=Path,
        help="Locale directory Crowdin wrote into (repaired in place)",
    )
    parser.add_argument(
        "--en",
        type=Path,
        help="Path to en.yml English source reference (defaults to <locales>/en.yml)",
    )
    parser.add_argument(
        "--report",
        type=Path,
        help="Optional Markdown report path for the bot PR body",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run built-in regression checks and exit",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(RepairTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    if args.before is None or args.locales is None:
        parser.error("--before and --locales are required unless --self-test")

    en_path = args.en if args.en is not None else args.locales / "en.yml"
    report = repair_locales(args.before, args.locales, en_path)
    print(
        "totals: "
        f"fixed={report.fixed_count} preserved={report.preserved_count} "
        f"crowdin_updated={report.crowdin_updated_count} "
        f"still_english={report.still_english_count}"
    )
    if args.report is not None:
        args.report.write_text(report.markdown(), encoding="utf-8")
        print(f"wrote report {args.report}")
    return 0


class RepairTests(unittest.TestCase):
    def test_fixes_english_on_master_from_crowdin(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            en = '"Camera": "Camera"\n"Sleep": "Sleep"\n"Ok": "Ok"\n'
            (after / "en.yml").write_text(en, encoding="utf-8")
            (before / "en.yml").write_text(en, encoding="utf-8")
            (before / "de.yml").write_text(
                "\n".join(
                    [
                        '"Camera": "Camera"',
                        '"Sleep": "Ruhezustand"',
                        '"Ok": "Ok"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            (after / "de.yml").write_text(
                "\n".join(
                    [
                        '"Camera": "Kamera"',
                        '"Sleep": "Sleep"',
                        '"Ok": "Ok"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )

            report = repair_locales(before, after, after / "en.yml")
            text = (after / "de.yml").read_text(encoding="utf-8")
            self.assertIn('"Camera": "Kamera"', text)
            self.assertIn('"Sleep": "Ruhezustand"', text)
            names = {item.name for item in report.locales}
            self.assertEqual(names, {"de.yml", "en.yml"})
            de = next(item for item in report.locales if item.name == "de.yml")
            self.assertEqual(de.fixed, ["Camera"])
            self.assertEqual(de.preserved, ["Sleep"])
            self.assertEqual(de.still_english, ["Ok"])
            en_stats = next(item for item in report.locales if item.name == "en.yml")
            self.assertEqual(en_stats.still_english, [])

    def test_crowdin_update_wins_when_both_translated(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            (after / "en.yml").write_text('"Sleep": "Sleep"\n', encoding="utf-8")
            (before / "en.yml").write_text('"Sleep": "Sleep"\n', encoding="utf-8")
            (before / "de.yml").write_text('"Sleep": "Ruhezustand"\n', encoding="utf-8")
            (after / "de.yml").write_text('"Sleep": "Schlafen"\n', encoding="utf-8")

            report = repair_locales(before, after, after / "en.yml")
            text = (after / "de.yml").read_text(encoding="utf-8")
            self.assertIn('"Sleep": "Schlafen"', text)
            de = next(item for item in report.locales if item.name == "de.yml")
            self.assertEqual(de.crowdin_updated, ["Sleep"])

    def test_processes_every_locale_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            before = root / "before"
            after = root / "after"
            before.mkdir()
            after.mkdir()
            (after / "en.yml").write_text('"A": "A"\n', encoding="utf-8")
            (before / "en.yml").write_text('"A": "A"\n', encoding="utf-8")
            for name, master, crowdin in (
                ("da.yml", '"A": "A"', '"A": "Dansk"'),
                ("ja.yml", '"A": "あ"', '"A": "A"'),
            ):
                (before / name).write_text(master + "\n", encoding="utf-8")
                (after / name).write_text(crowdin + "\n", encoding="utf-8")

            report = repair_locales(before, after, after / "en.yml")
            self.assertEqual(
                {item.name for item in report.locales},
                {"da.yml", "en.yml", "ja.yml"},
            )
            self.assertIn('"A": "Dansk"', (after / "da.yml").read_text(encoding="utf-8"))
            self.assertIn('"A": "あ"', (after / "ja.yml").read_text(encoding="utf-8"))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
