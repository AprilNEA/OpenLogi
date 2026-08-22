//! Unit tests for the macOS `LaunchAgent` plist reconcile.

use super::*;

#[test]
fn rendered_plist_targets_the_agent_and_keeps_alive() {
    let body = render_plist(
        "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent",
    )
    .expect("render plist");
    assert!(body.contains(LABEL));
    assert!(body.contains("openlogi-agent"));
    assert!(body.contains("RunAtLoad"));
    // KeepAlive uses SuccessfulExit:false so a crash respawns but the tray's
    // Quit (a clean exit(0)) is NOT relaunched; no --minimized (always headless).
    let parsed = plist::Value::from_reader_xml(body.as_bytes()).expect("parse plist");
    let keep_alive = parsed
        .as_dictionary()
        .and_then(|root| root.get("KeepAlive"))
        .and_then(plist::Value::as_dictionary)
        .expect("KeepAlive dictionary");
    assert_eq!(
        keep_alive
            .get("SuccessfulExit")
            .and_then(plist::Value::as_boolean),
        Some(false)
    );
    assert!(!body.contains("--minimized"));
}

#[test]
fn render_plist_serializes_xml_metacharacters_in_the_path() {
    // A home/app path with XML metacharacters (all legal APFS filename chars)
    // must not produce a malformed plist launchd would reject.
    let path = "/Users/R&D/Apps/<OpenLogi>/openlogi-agent";
    let body = render_plist(path).expect("render plist");
    let parsed = plist::Value::from_reader_xml(body.as_bytes()).expect("parse plist");
    let args = parsed
        .as_dictionary()
        .and_then(|root| root.get("ProgramArguments"))
        .and_then(plist::Value::as_array)
        .expect("ProgramArguments array");
    assert_eq!(args.first().and_then(plist::Value::as_string), Some(path));
}
