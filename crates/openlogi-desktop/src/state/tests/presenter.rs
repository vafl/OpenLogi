//! Presenter tab: effects are ordinary bindings, thresholds and haptics a
//! per-device `[presenter]` table; both save and reload the agent only.
use openlogi_core::binding::{Binding, HighlightEffect};
use openlogi_core::color::Rgb;
use openlogi_core::config::{
    ForceThreshold, HighlightPointer, HighlightSensitivity, LaserSize, PresenterSettings,
    SpotlightDim, SpotlightSize,
};

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::Command;
use crate::state::StateEvents;

/// The known device (a mouse fixture: the commits do not gate on the
/// capability, the tab does).
fn state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    drain(&mut receiver);
    (state, receiver)
}

fn force(raw: u16) -> ForceThreshold {
    ForceThreshold::try_new(raw).expect("a threshold inside the device range")
}

fn settings(touch: Option<u16>, press: Option<u16>, haptics: bool) -> PresenterSettings {
    PresenterSettings {
        touch_force: touch.map(force),
        press_force: press.map(force),
        haptics,
        ..PresenterSettings::default()
    }
}

#[test]
fn the_highlight_buttons_read_their_defaults_until_rebound() {
    let (mut state, mut receiver) = state();
    assert_eq!(
        state.current_presenter_bindings().touch,
        Action::Highlight(HighlightEffect::LaserDot)
    );
    assert_eq!(
        state.current_presenter_bindings().press,
        Action::Highlight(HighlightEffect::Spotlight)
    );

    let _ = state.commit_presenter_binding(ButtonId::PresenterPress, Action::None);

    assert_eq!(state.current_presenter_bindings().press, Action::None);
    assert_eq!(
        state
            .config
            .stored_bindings(KNOWN_MOUSE_KEY)
            .get(&ButtonId::PresenterPress),
        Some(&Binding::Single(Action::None))
    );
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));
}

#[test]
fn the_click_is_native_until_switched_off_and_can_be_switched_back() {
    let (mut state, mut receiver) = state();
    assert_eq!(state.current_presenter_bindings().click, Action::LeftClick);

    let _ = state.commit_presenter_binding(ButtonId::PresenterClick, Action::None);
    assert_eq!(state.current_presenter_bindings().click, Action::None);
    assert_eq!(
        state
            .config
            .stored_bindings(KNOWN_MOUSE_KEY)
            .get(&ButtonId::PresenterClick),
        Some(&Binding::Single(Action::None))
    );
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));

    let _ = state.commit_presenter_binding(ButtonId::PresenterClick, Action::LeftClick);
    assert_eq!(state.current_presenter_bindings().click, Action::LeftClick);
}

#[test]
fn a_mouse_button_is_not_a_presenter_binding() {
    let (mut state, mut receiver) = state();
    let events = state.commit_presenter_binding(ButtonId::Back, Action::Copy);
    assert_eq!(events, StateEvents::none());
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn thresholds_are_stored_sparsely() {
    let (mut state, mut receiver) = state();
    assert_eq!(
        state.current_presenter_settings(),
        PresenterSettings::default()
    );
    let key = state.current_record().expect("selected").device_key();

    let events = state.commit_presenter_settings(settings(Some(200), Some(700), true));
    assert_eq!(events, [StateEvent::DeviceConfigChanged(key)]);
    assert_eq!(
        state.current_presenter_settings(),
        settings(Some(200), Some(700), true)
    );
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));

    // Defaults are stored as unset.
    let _ = state.commit_presenter_settings(settings(Some(93), Some(300), false));
    assert_eq!(
        state.config.presenter(KNOWN_MOUSE_KEY),
        Some(settings(None, None, false))
    );
}

#[test]
fn an_inverted_threshold_pair_is_refused_not_reordered() {
    let (mut state, mut receiver) = state();
    let _ = state.commit_presenter_settings(settings(Some(200), Some(700), true));
    drain(&mut receiver);

    // Touch firmer than press, and touch firmer than the default press.
    for inverted in [
        settings(Some(800), Some(700), true),
        settings(Some(400), None, true),
    ] {
        let events = state.commit_presenter_settings(inverted);
        assert_ne!(events, StateEvents::none(), "the panel still repaints");
        assert_eq!(
            state.current_presenter_settings(),
            settings(Some(200), Some(700), true),
            "the stored pair is untouched"
        );
        assert!(
            drain(&mut receiver).is_empty(),
            "nothing is saved or reloaded"
        );
    }
}

#[test]
fn committing_unchanged_settings_reloads_nothing() {
    let (mut state, mut receiver) = state();
    let _ = state.commit_presenter_settings(PresenterSettings::default());
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn the_other_settings_survive_each_edit_and_defaults_leave_no_trace() {
    let (mut state, mut receiver) = state();
    let green = Rgb::new(0, 0xff, 0x40);
    let edited = PresenterSettings {
        pointer: HighlightPointer::Follow,
        laser_size: LaserSize::MAX,
        laser_color: green,
        spotlight_size: SpotlightSize::MIN,
        spotlight_dim: SpotlightDim::MIN,
        sensitivity: HighlightSensitivity::MAX,
        precision: false,
        steady_release: false,
        ..PresenterSettings::default()
    };
    let _ = state.commit_presenter_settings(edited);
    assert_eq!(state.config.presenter(KNOWN_MOUSE_KEY), Some(edited));
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));

    // A threshold edit carries every other value along.
    let current = state.current_presenter_settings();
    let _ = state.commit_presenter_settings(PresenterSettings {
        touch_force: Some(force(200)),
        ..current
    });
    assert_eq!(
        state.current_presenter_settings(),
        PresenterSettings {
            touch_force: Some(force(200)),
            ..edited
        }
    );

    // Back to the defaults: the table keeps no trace of them.
    let _ = state.commit_presenter_settings(PresenterSettings::default());
    assert_eq!(
        state.config.presenter(KNOWN_MOUSE_KEY),
        Some(PresenterSettings::default())
    );
}
