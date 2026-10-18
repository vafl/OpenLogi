//! A presenter's `[presenter]` table: defaults, validated ranges, the
//! threshold ordering rule, and the published appearance and motion.

use super::*;
use crate::color::Rgb;

fn force(value: u16) -> ForceThreshold {
    ForceThreshold::try_new(value).expect("valid force threshold")
}

fn parse(body: &str) -> Result<DeviceConfig, toml::de::Error> {
    toml::from_str(body)
}

#[test]
fn presenter_table_parses_with_defaults_and_round_trips() {
    let mut cfg = Config::default();
    for (key, body) in [
        ("p1", "[presenter]\ntouch_force = 168\npress_force = 420\n"),
        ("p2", "[presenter]\n"),
    ] {
        cfg.devices
            .insert(key.to_owned(), parse(body).expect("parse"));
    }
    assert_eq!(
        cfg.presenter("p1"),
        Some(PresenterSettings {
            touch_force: Some(force(168)),
            press_force: Some(force(420)),
            ..PresenterSettings::default()
        })
    );
    assert_eq!(cfg.presenter("p2"), Some(PresenterSettings::default()));
    let defaults = PresenterSettings::default();
    assert_eq!(
        (
            u16::from(defaults.effective_touch_force()),
            u16::from(defaults.effective_press_force())
        ),
        (93, 300)
    );
    assert_eq!(cfg.presenter("absent"), None);
    assert_eq!(write_and_read(&cfg).presenter("p1"), cfg.presenter("p1"));
}

#[test]
fn default_presenter_values_stay_out_of_the_toml() {
    let mut cfg = Config::default();
    cfg.set_presenter("p1", PresenterSettings::default());
    let toml = toml::to_string(&cfg).expect("serialize");
    assert_eq!(toml.matches("[devices.p1.presenter]").count(), 1, "{toml}");
    for key in [
        "force",
        "haptics",
        "pointer",
        "laser",
        "spotlight",
        "sensitivity",
        "precision",
        "steady_release",
    ] {
        assert!(!toml.contains(key), "default {key} is written: {toml}");
    }

    cfg.set_presenter(
        "p1",
        PresenterSettings {
            haptics: false,
            pointer: HighlightPointer::Follow,
            laser_size: LaserSize::MAX,
            laser_color: Rgb::new(0, 0xff, 0),
            ..PresenterSettings::default()
        },
    );
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(toml.contains("haptics = false"), "{toml}");
    assert!(toml.contains("pointer = \"follow\""), "{toml}");
    assert!(toml.contains("laser_size = 40"), "{toml}");
    assert!(toml.contains("laser_color = \"00ff00\""), "{toml}");
    assert!(!toml.contains("spotlight"), "{toml}");
    assert!(!toml.contains("precision"), "{toml}");
}

#[test]
fn unset_presenter_is_omitted_from_toml() {
    let mut cfg = Config::default();
    cfg.set_dpi("2b042", Dpi::new(1600));
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(!toml.contains("presenter"), "{toml}");
}

#[test]
fn presenter_pointer_defaults_to_hide_and_round_trips() {
    let mut cfg = Config::default();
    for (key, body) in [
        ("p1", "[presenter]\npointer = \"follow\"\n"),
        ("p2", "[presenter]\npointer = \"hide\"\n"),
        ("p3", "[presenter]\n"),
    ] {
        cfg.devices
            .insert(key.to_owned(), parse(body).expect("parse"));
    }
    assert_eq!(cfg.presenter_pointer("p1"), HighlightPointer::Follow);
    assert_eq!(cfg.presenter_pointer("p2"), HighlightPointer::Hide);
    assert_eq!(cfg.presenter_pointer("p3"), HighlightPointer::Hide);
    assert_eq!(cfg.presenter_pointer("absent"), HighlightPointer::Hide);
    assert_eq!(write_and_read(&cfg).presenter("p1"), cfg.presenter("p1"));
    let rejected = parse("[presenter]\npointer = \"sideways\"\n");
    assert!(rejected.unwrap_err().to_string().contains("sideways"));
}

#[test]
fn presenter_motion_defaults_and_round_trips() {
    let defaults = PresenterSettings::default();
    assert_eq!(
        defaults.motion(),
        HighlightMotion {
            sensitivity: 100,
            precision: true,
            steady_release: true,
        }
    );
    let mut cfg = Config::default();
    cfg.devices.insert(
        "p1".to_owned(),
        parse("[presenter]\nsensitivity = 60\nprecision = false\nsteady_release = false\n")
            .expect("parse"),
    );
    assert_eq!(
        cfg.presenter_motion("p1"),
        HighlightMotion {
            sensitivity: 60,
            precision: false,
            steady_release: false,
        }
    );
    assert_eq!(cfg.presenter_motion("absent"), defaults.motion());
    assert_eq!(write_and_read(&cfg).presenter("p1"), cfg.presenter("p1"));
}

#[test]
fn presenter_appearance_defaults_and_round_trips() {
    let defaults = PresenterSettings::default();
    assert_eq!(
        defaults.appearance(),
        HighlightAppearance {
            laser_size: 12,
            laser_color: Rgb::new(0xff, 0x2a, 0x2a),
            spotlight_size: 170,
            spotlight_dim: 50,
        }
    );
    let mut cfg = Config::default();
    cfg.devices.insert(
        "p1".to_owned(),
        parse(
            "[presenter]\nlaser_size = 20\nlaser_color = \"00ff00\"\nspotlight_size = 250\nspotlight_dim = 80\n",
        )
        .expect("parse"),
    );
    assert_eq!(
        cfg.presenter_appearance("p1"),
        HighlightAppearance {
            laser_size: 20,
            laser_color: Rgb::new(0, 0xff, 0),
            spotlight_size: 250,
            spotlight_dim: 80,
        }
    );
    assert_eq!(cfg.presenter_appearance("absent"), defaults.appearance());
    assert_eq!(write_and_read(&cfg).presenter("p1"), cfg.presenter("p1"));
    let rejected = parse("[presenter]\nlaser_color = \"red\"\n");
    assert!(rejected.is_err(), "a non-hex colour is rejected");
}

#[test]
fn out_of_range_presenter_values_are_rejected_on_load() {
    for (key, accepted, rejected) in [
        ("touch_force", [93, 933], [92, 934]),
        ("press_force", [93, 933], [92, 934]),
        ("laser_size", [4, 40], [3, 41]),
        ("spotlight_size", [60, 400], [59, 401]),
        ("spotlight_dim", [0, 100], [101, 255]),
        ("sensitivity", [25, 200], [24, 201]),
    ] {
        // The widest press threshold keeps a firm touch threshold ordered.
        let ordering = if key == "press_force" {
            ""
        } else {
            "press_force = 933\n"
        };
        let table = |value: u16| format!("[presenter]\n{key} = {value}\n{ordering}");
        for value in accepted {
            let body = table(value);
            assert!(parse(&body).is_ok(), "{body} is rejected");
        }
        for value in rejected {
            let body = table(value);
            assert!(parse(&body).is_err(), "{body} is accepted");
        }
    }
}

#[test]
fn unknown_presenter_fields_are_rejected() {
    let rejected = parse("[presenter]\nlaser_colour = 12\n");
    assert!(rejected.unwrap_err().to_string().contains("laser_colour"));
}

#[test]
fn inverted_force_thresholds_are_rejected_not_reordered() {
    // An unset side counts as its default: 93 / 300.
    for (body, loads) in [
        ("[presenter]\ntouch_force = 300\npress_force = 300\n", true),
        ("[presenter]\ntouch_force = 200\n", true),
        ("[presenter]\ntouch_force = 301\n", false),
        ("[presenter]\npress_force = 93\n", true),
        ("[presenter]\ntouch_force = 420\npress_force = 168\n", false),
    ] {
        assert_eq!(parse(body).is_ok(), loads, "{body}");
    }
    let error = parse("[presenter]\ntouch_force = 420\npress_force = 168\n").unwrap_err();
    assert!(error.to_string().contains("touch_force 420"), "{error}");

    let inverted = PresenterSettings {
        touch_force: Some(force(420)),
        press_force: Some(force(168)),
        ..PresenterSettings::default()
    };
    assert_eq!(
        inverted.validate(),
        Err(InvertedForceThresholds {
            touch: force(420),
            press: force(168),
        })
    );
    assert_eq!(PresenterSettings::default().validate(), Ok(()));
}

#[test]
fn normalizing_folds_default_thresholds_into_unset() {
    let settings = PresenterSettings {
        touch_force: Some(PresenterSettings::DEFAULT_TOUCH_FORCE),
        press_force: Some(force(420)),
        ..PresenterSettings::default()
    };
    assert_eq!(
        settings.normalized(),
        PresenterSettings {
            touch_force: None,
            press_force: Some(force(420)),
            ..PresenterSettings::default()
        }
    );
    let explicit_defaults = PresenterSettings {
        touch_force: Some(PresenterSettings::DEFAULT_TOUCH_FORCE),
        press_force: Some(PresenterSettings::DEFAULT_PRESS_FORCE),
        ..PresenterSettings::default()
    };
    assert_eq!(explicit_defaults.normalized(), PresenterSettings::default());
}
