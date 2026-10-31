//! Per-application thumb-wheel rules and the delivery they resolve to.

use super::*;
use crate::scroll::ScrollDelivery;

fn parse(body: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(&format!("schema_version = {SCHEMA_VERSION}\n{body}"))
}

fn rule(delivery: ScrollDelivery) -> ThumbwheelAppRule {
    ThumbwheelAppRule {
        delivery: Some(delivery),
    }
}

#[test]
fn every_delivery_round_trips_under_its_snake_case_name() {
    let names = [
        "native",
        "shift_wheel",
        "ctrl_shift_wheel",
        "alt_wheel",
        "vertical_wheel",
        "ctrl_wheel",
    ];
    for (delivery, name) in ScrollDelivery::ALL.into_iter().zip(names) {
        let config = parse(&format!(
            "[thumbwheel_apps.\"exe:app.exe\"]\ndelivery = \"{name}\"\n"
        ))
        .expect("parse delivery");
        assert_eq!(
            config.thumbwheel_apps["exe:app.exe"],
            rule(delivery),
            "{name}"
        );

        let text = toml::to_string(&config).expect("serialize");
        assert!(text.contains(&format!("delivery = \"{name}\"")), "{text}");
        assert_eq!(
            write_and_read(&config).thumbwheel_apps,
            config.thumbwheel_apps,
            "{name}"
        );
    }
}

#[test]
fn unknown_deliveries_and_rule_keys_are_rejected() {
    for body in [
        "[thumbwheel_apps.\"exe:app.exe\"]\ndelivery = \"shift\"\n",
        "[thumbwheel_apps.\"exe:app.exe\"]\ndelivery = \"ShiftWheel\"\n",
        "[thumbwheel_apps.\"exe:app.exe\"]\nsensitivity = 5\n",
    ] {
        assert!(parse(body).is_err(), "accepted: {body}");
    }
}

#[test]
fn a_config_without_rules_writes_no_table_and_stays_native() {
    let config = Config::default();
    let text = toml::to_string(&config).expect("serialize");
    assert!(!text.contains("thumbwheel_apps"), "{text}");
    assert!(write_and_read(&config).thumbwheel_apps.is_empty());
    assert_eq!(
        config.scroll_delivery(Some(r"c:\apps\app.exe")),
        ScrollDelivery::Native
    );

    let unset = parse("[thumbwheel_apps.\"exe:app.exe\"]\n").expect("a rule may set nothing");
    assert_eq!(unset.thumbwheel_apps["exe:app.exe"].delivery, None);
    assert_eq!(
        unset.scroll_delivery(Some(r"c:\apps\app.exe")),
        ScrollDelivery::Native
    );
}

#[test]
fn delivery_resolves_through_the_per_app_selector_rules() {
    let mut config = Config::default();
    config.thumbwheel_apps.insert(
        "exe:excel.exe".to_string(),
        rule(ScrollDelivery::CtrlShiftWheel),
    );
    config.thumbwheel_apps.insert(
        r"c:\tools\excel.exe".to_string(),
        rule(ScrollDelivery::ShiftWheel),
    );
    config.thumbwheel_apps.insert(
        "com.example.Editor".to_string(),
        rule(ScrollDelivery::AltWheel),
    );

    assert_eq!(
        config.scroll_delivery(Some(
            r"c:\program files\microsoft office\root\office16\excel.exe"
        )),
        ScrollDelivery::CtrlShiftWheel
    );
    assert_eq!(
        config.scroll_delivery(Some(r"c:\tools\excel.exe")),
        ScrollDelivery::ShiftWheel,
        "the exact path wins over the executable fallback"
    );
    assert_eq!(
        config.scroll_delivery(Some("com.example.Editor")),
        ScrollDelivery::AltWheel
    );
    assert_eq!(
        config.scroll_delivery(Some(
            r"c:\program files\google\chrome\application\chrome.exe"
        )),
        ScrollDelivery::Native
    );
    assert_eq!(config.scroll_delivery(None), ScrollDelivery::Native);
}

#[test]
fn delivery_uses_app_aliases_after_platform_specific_rules() {
    let config = parse(
        r#"
        [apps.tool]
        macos = "exe:tool.exe"
        linux = "exe:tool.exe"
        windows = "exe:tool.exe"
        [thumbwheel_apps."app:tool"]
        delivery = "ctrl_shift_wheel"
        "#,
    )
    .expect("parse alias and thumb-wheel rule");
    let mut config = write_and_read(&config);
    let path = r"c:\tools\tool.exe";
    assert_eq!(
        config.scroll_delivery(Some(path)),
        ScrollDelivery::CtrlShiftWheel,
        "the saved alias applies on each platform that lists the executable"
    );

    config
        .thumbwheel_apps
        .insert("exe:tool.exe".to_string(), rule(ScrollDelivery::ShiftWheel));
    assert_eq!(
        config.scroll_delivery(Some(path)),
        ScrollDelivery::ShiftWheel,
        "an executable selector wins over its alias"
    );

    config
        .thumbwheel_apps
        .insert(path.to_string(), rule(ScrollDelivery::AltWheel));
    assert_eq!(
        config.scroll_delivery(Some(path)),
        ScrollDelivery::AltWheel,
        "an exact path wins over the executable and alias selectors"
    );

    config
        .thumbwheel_apps
        .insert(path.to_string(), ThumbwheelAppRule::default());
    assert_eq!(
        config.scroll_delivery(Some(path)),
        ScrollDelivery::Native,
        "an unset exact rule keeps native delivery rather than trying a broader rule"
    );
}
