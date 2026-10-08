"""Export only assigned G502 X settings from G HUB's local SQLite database.

No login tokens, analytics fields, unrelated cards, cloud APIs, or executable
actions are copied. The output is an inert migration inventory, not a claim
that every G HUB action can already be executed by OpenLogi.
"""

import argparse
import json
import os
from pathlib import Path
import sqlite3


def export_profiles(document):
    cards = {card["id"]: card for card in document.get("cards", {}).get("cards", [])}
    result = {"schema_version": 1, "device": "g502x-lightspeed", "profiles": []}
    for profile in document.get("profiles", {}).get("profiles", []):
        exported = {"name": profile.get("name", ""), "assignments": [], "mouse_settings": None}
        for assignment in profile.get("assignments", []):
            slot = assignment.get("slotId", "")
            if not slot.startswith("g502x-lightspeed_"):
                continue
            card_id = assignment.get("cardId", "")
            card = cards.get(card_id)
            entry = {"slot": slot}
            if card is None:
                entry.update(status="unresolved_builtin", builtin_id=card_id)
            elif card.get("attribute") == "MOUSE_SETTINGS":
                settings = card.get("mouseSettings", {})
                exported["mouse_settings"] = {
                    key: settings[key] for key in ("dpiTable", "reportRate") if key in settings
                }
                continue
            elif card.get("attribute") == "MACRO_PLAYBACK":
                macro = card.get("macro", {})
                kind = macro.get("type", "")
                entry.update(name=card.get("name", ""), macro_type=kind)
                if kind == "SEQUENCE":
                    sequence = macro.get("sequence", {})
                    entry.update(status="captured_not_imported", sequence={
                        key: sequence[key] for key in (
                            "defaultDelay", "useDefaultDelay", "useSimpleActions",
                            "simpleSequence", "pressSequence", "heldSequence",
                            "releaseSequence", "toggleSequence",
                        ) if key in sequence
                    })
                elif kind == "KEYSTROKE":
                    entry.update(status="captured_not_imported", keystroke=macro.get("keystroke", {}))
                else:
                    entry["status"] = "unsupported_action"
            else:
                entry.update(status="unsupported_card", attribute=card.get("attribute", ""))
            exported["assignments"].append(entry)
        if exported["assignments"] or exported["mouse_settings"]:
            result["profiles"].append(exported)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="New JSON file; never overwritten")
    parser.add_argument("--database", type=Path,
                        default=Path(os.environ.get("LOCALAPPDATA", ".")) / "LGHUB" / "settings.db")
    args = parser.parse_args()
    uri = args.database.resolve().as_uri() + "?mode=ro"
    with sqlite3.connect(uri, uri=True) as connection:
        connection.execute("PRAGMA query_only=ON")
        row = connection.execute("SELECT file FROM data ORDER BY _id DESC LIMIT 1").fetchone()
    if row is None:
        raise SystemExit("No G HUB settings record found")
    output = export_profiles(json.loads(row[0]))
    with args.output.open("x", encoding="utf-8") as file:
        json.dump(output, file, ensure_ascii=False, indent=2)
        file.flush()
        os.fsync(file.fileno())
    print(f"Exported {len(output['profiles'])} profiles to {args.output}; no settings applied")


if __name__ == "__main__":
    main()
