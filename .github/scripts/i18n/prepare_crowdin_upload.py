#!/usr/bin/env python3
"""Make the Crowdin project accept the TOML catalog before upload.

The CLI updates a source when the path matches with the extension removed, so
`en.toml` tries to rename the existing `en.yml`. That file is YAML, which
rejects `.toml`. Deleting it makes the next upload an add, and an add is what
sends `type: toml`. Target languages named in the config are added first.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import unittest
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

API_ROOT = "https://api.crowdin.com/api/v2"
STALE_YAML_SOURCE = "crates/openlogi-ui/locales/en.yml"
PAGE_LIMIT = 500


def export_language_ids(text: str) -> list[str]:
    """Return Crowdin language ids from the top-level export_languages list."""
    ids: list[str] = []
    collecting = False
    for raw in text.splitlines():
        if not collecting:
            if raw == "export_languages:":
                collecting = True
            continue
        if raw.startswith("  - "):
            value = raw[4:].strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in {'"', "'"}:
                value = value[1:-1]
            ids.append(value)
            continue
        if raw.strip() == "":
            continue
        break
    if not ids:
        raise ValueError("export_languages is missing from the Crowdin config")
    return ids


def directory_paths(directories: list[dict]) -> dict[int, str]:
    """Return each directory id mapped to its slash-separated project path."""
    by_id = {item["id"]: item for item in directories}
    cache: dict[int, str] = {}

    def build(directory_id: int, stack: set[int]) -> str:
        cached = cache.get(directory_id)
        if cached is not None:
            return cached
        if directory_id not in by_id:
            raise RuntimeError(f"Crowdin directory {directory_id} was not listed")
        if directory_id in stack:
            raise RuntimeError(f"Crowdin directory {directory_id} is its own parent")
        item = by_id[directory_id]
        stack.add(directory_id)
        parent = item.get("directoryId")
        path = item["name"] if not parent else f"{build(parent, stack)}/{item['name']}"
        stack.remove(directory_id)
        cache[directory_id] = path
        return path

    return {directory_id: build(directory_id, set()) for directory_id in by_id}


def source_path(file: dict, dir_paths: dict[int, str]) -> str:
    """Return the project path Crowdin shows for one source file."""
    raw = file.get("path")
    if isinstance(raw, str) and raw.strip("/"):
        return raw.strip("/")
    directory_id = file.get("directoryId")
    if directory_id is None:
        return str(file["name"])
    return f"{dir_paths[directory_id]}/{file['name']}"


def prepare(api: object, language_ids: list[str]) -> list[str]:
    """Add missing target languages, then delete the stale YAML source."""
    notes: list[str] = []
    current = list(api.target_language_ids())
    present = set(current)
    missing = [language_id for language_id in language_ids if language_id not in present]
    if missing:
        api.set_target_languages([*current, *missing])
        notes.extend(f"added target language {language_id}" for language_id in missing)
    for file_id, path in api.source_files():
        if path != STALE_YAML_SOURCE:
            continue
        api.delete_file(file_id)
        notes.append(f"deleted {path} ({file_id})")
    if not notes:
        notes.append("Crowdin project already accepts the TOML source")
    return notes


class CrowdinClient:
    """Crowdin API v2 calls this sync needs."""

    def __init__(self, token: str, project_id: str, api_root: str = API_ROOT) -> None:
        if not project_id.isdigit():
            raise ValueError("CROWDIN_PROJECT_ID must be numeric")
        self.token = token
        self.project_id = project_id
        self.api_root = api_root.rstrip("/")

    def target_language_ids(self) -> list[str]:
        """Return the project's current target language ids."""
        project = self._json("GET", f"/projects/{self.project_id}")
        return list(project["data"]["targetLanguageIds"])

    def set_target_languages(self, language_ids: list[str]) -> None:
        """Replace the project's target language ids, preserving ids already set."""
        self._json(
            "PATCH",
            f"/projects/{self.project_id}",
            [{"op": "replace", "path": "/targetLanguageIds", "value": language_ids}],
        )

    def source_files(self) -> list[tuple[int, str]]:
        """Return every source file id and its project path."""
        directories = self._walk_directories()
        paths = directory_paths(directories)
        files = self._collect("files", {})
        for directory in directories:
            files.extend(self._collect("files", {"directoryId": str(directory["id"])}))
        unique = {item["id"]: item for item in files}
        return [(item["id"], source_path(item, paths)) for item in unique.values()]

    def delete_file(self, file_id: int) -> None:
        """Delete one source file. Crowdin deletes its translations with it."""
        self._json("DELETE", f"/projects/{self.project_id}/files/{file_id}")

    def _walk_directories(self) -> list[dict]:
        directories = self._collect("directories", {})
        pending = [item["id"] for item in directories]
        seen = set(pending)
        while pending:
            children = self._collect("directories", {"directoryId": str(pending.pop())})
            for child in children:
                if child["id"] in seen:
                    continue
                seen.add(child["id"])
                directories.append(child)
                pending.append(child["id"])
        return directories

    def _collect(self, resource: str, query: dict[str, str]) -> list[dict]:
        items: list[dict] = []
        offset = 0
        while True:
            params = {"limit": str(PAGE_LIMIT), "offset": str(offset), **query}
            page = self._json("GET", f"/projects/{self.project_id}/{resource}?{urllib.parse.urlencode(params)}")
            batch = [row["data"] for row in page["data"]]
            items.extend(batch)
            if len(batch) < PAGE_LIMIT:
                return items
            offset += PAGE_LIMIT

    def _json(self, method: str, path: str, payload: object | None = None) -> object:
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(f"{self.api_root}{path}", data=data, method=method)
        request.add_header("Authorization", f"Bearer {self.token}")
        request.add_header("Accept", "application/json")
        if payload is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                body = response.read()
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", "replace")
            raise RuntimeError(f"Crowdin {method} {path} failed ({error.code}): {detail}") from None
        if not body:
            return None
        return json.loads(body)


def main(argv: list[str]) -> int:
    """Add missing Crowdin languages and delete the stale YAML source."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--config",
        type=Path,
        default=Path(".config/crowdin.yml"),
        help="Crowdin config whose export_languages list is authoritative",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run built-in regression checks and exit",
    )
    args = parser.parse_args(argv)
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(PrepareTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    token = os.environ.get("CROWDIN_PERSONAL_TOKEN", "")
    project_id = os.environ.get("CROWDIN_PROJECT_ID", "")
    if not token or not project_id:
        parser.error("CROWDIN_PERSONAL_TOKEN and CROWDIN_PROJECT_ID are required")
    language_ids = export_language_ids(args.config.read_text(encoding="utf-8"))
    for note in prepare(CrowdinClient(token, project_id), language_ids):
        print(note)
    return 0


class PrepareTests(unittest.TestCase):
    """Regression checks for the pre-upload Crowdin cleanup."""

    def test_export_languages_keep_config_order_and_quotes(self) -> None:
        text = '\n'.join(
            [
                "export_languages:",
                "  - be",
                "  - cs",
                '  - "no"',
                "",
                "files:",
            ]
        )
        self.assertEqual(export_language_ids(text), ["be", "cs", "no"])

    def test_missing_export_languages_fail(self) -> None:
        with self.assertRaises(ValueError):
            export_language_ids("files:\n")

    def test_directory_paths_nest(self) -> None:
        directories = [
            {"id": 1, "name": "crates", "directoryId": None},
            {"id": 2, "name": "openlogi-ui", "directoryId": 1},
            {"id": 3, "name": "locales", "directoryId": 2},
        ]
        self.assertEqual(
            directory_paths(directories),
            {1: "crates", 2: "crates/openlogi-ui", 3: "crates/openlogi-ui/locales"},
        )

    def test_source_path_prefers_api_path(self) -> None:
        file = {"name": "en.yml", "directoryId": 3, "path": "/crates/openlogi-ui/locales/en.yml"}
        self.assertEqual(source_path(file, {}), STALE_YAML_SOURCE)

    def test_prepare_adds_languages_before_deleting_yaml(self) -> None:
        api = _FakeApi()
        notes = prepare(api, ["de", "be", "cs"])
        self.assertEqual(api.calls, [("languages", ["de", "be", "cs"]), ("delete", 7)])
        self.assertEqual(
            notes,
            [
                "added target language be",
                "added target language cs",
                f"deleted {STALE_YAML_SOURCE} (7)",
            ],
        )
        self.assertEqual(api.files, [(8, "crates/openlogi-ui/locales/en.toml")])

    def test_prepare_is_quiet_when_nothing_is_stale(self) -> None:
        api = _FakeApi()
        api.languages = ["be", "cs", "de"]
        api.files = [(8, "crates/openlogi-ui/locales/en.toml")]
        self.assertEqual(
            prepare(api, ["be", "cs"]),
            ["Crowdin project already accepts the TOML source"],
        )
        self.assertEqual(api.calls, [])


class _FakeApi:
    def __init__(self) -> None:
        self.languages = ["de"]
        self.files = [
            (7, STALE_YAML_SOURCE),
            (8, "crates/openlogi-ui/locales/en.toml"),
        ]
        self.calls: list[tuple[str, object]] = []

    def target_language_ids(self) -> list[str]:
        return list(self.languages)

    def set_target_languages(self, language_ids: list[str]) -> None:
        self.calls.append(("languages", list(language_ids)))
        self.languages = list(language_ids)

    def source_files(self) -> list[tuple[int, str]]:
        return list(self.files)

    def delete_file(self, file_id: int) -> None:
        self.calls.append(("delete", file_id))
        self.files = [item for item in self.files if item[0] != file_id]


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
