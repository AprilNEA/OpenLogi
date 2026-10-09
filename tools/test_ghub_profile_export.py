import unittest
from ghub_profile_export import export_profiles


class ExportTests(unittest.TestCase):
    def test_only_target_assignments_are_exported_without_account_fields(self):
        source = {
            "account": {"token": "must-not-leak"}, "analytics": {"id": "must-not-leak"},
            "cards": {"cards": [
                {"id": "mouse", "attribute": "MOUSE_SETTINGS", "mouseSettings": {"dpiTable": {"levels": [800]}}},
                {"id": "macro", "attribute": "MACRO_PLAYBACK", "name": "Page Up", "macro": {
                    "type": "SEQUENCE", "sequence": {"simpleSequence": {"components": [{"keyboard": {"hidUsage": "75", "isDown": True}}]}}
                }},
                {"id": "unassigned", "attribute": "MACRO_PLAYBACK", "macro": {"type": "SEQUENCE", "sequence": {"secret": "must-not-leak"}}},
            ]},
            "profiles": {"profiles": [{"name": "Desktop", "assignments": [
                {"slotId": "g502x-lightspeed_mouse_settings", "cardId": "mouse"},
                {"slotId": "g502x-lightspeed_g11_m1", "cardId": "macro"},
                {"slotId": "other-device_g1", "cardId": "unassigned"},
                {"slotId": "g502x-lightspeed_g1_m1", "cardId": "unknown"},
            ]}]},
        }
        result = export_profiles(source)
        self.assertNotIn("must-not-leak", str(result))
        profile = result["profiles"][0]
        self.assertEqual(profile["mouse_settings"]["dpiTable"]["levels"], [800])
        self.assertEqual(len(profile["assignments"]), 2)
        self.assertEqual(profile["assignments"][0]["status"], "captured_not_imported")
        self.assertEqual(profile["assignments"][1]["status"], "unresolved_builtin")


if __name__ == "__main__":
    unittest.main()
