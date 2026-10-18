use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::slider::SliderEvent;
use openlogi_core::config::{Config, HighlightPointer};

use super::*;
use crate::services::assets::AssetResolver;
use crate::services::i18n::LOCALE_LOCK;
use crate::state::Sources;
use crate::state::tests::direct_inventory;

fn force(raw: u16) -> ForceThreshold {
    ForceThreshold::try_new(raw).expect("a threshold inside the device range")
}

/// A panel over a state that knows one device, so commits have somewhere to
/// land.
fn panel(cx: &mut TestAppContext) -> (Entity<PresenterPanel>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        theme::register_builtin_themes(cx);
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
        let state = cx.new(|_| {
            AppState::new(Sources {
                inventories: &[inventory],
                ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
            })
        });
        AppState::set_global(state, cx);
    });
    cx.add_window_view(PresenterPanel::new)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn stored(cx: &mut VisualTestContext) -> PresenterSettings {
    cx.update(|_, cx| {
        AppState::try_read(cx)
            .expect("state")
            .current_presenter_settings()
    })
}

#[test]
fn every_choice_round_trips_through_its_action() {
    for &choice in HighlightChoice::ALL {
        assert_eq!(HighlightChoice::of(&choice.action()), Some(choice));
    }
}

#[test]
fn an_action_this_panel_does_not_offer_reads_as_custom() {
    assert_eq!(
        Entry::<HighlightChoice>::of(&Action::PlayPause),
        Entry::Custom
    );
    assert_eq!(Entry::<ClickChoice>::of(&Action::PlayPause), Entry::Custom);
    assert_eq!(
        Entry::<ClickChoice>::of(&Action::None),
        Entry::Offered(ClickChoice::Off)
    );
}

#[test]
fn custom_is_listed_only_while_it_is_the_stored_action() {
    assert_eq!(
        Entry::<ClickChoice>::listing(),
        [
            Entry::Offered(ClickChoice::LeftClick),
            Entry::Offered(ClickChoice::Off)
        ]
    );
    assert_eq!(
        Entry::<ClickChoice>::listing_with_custom().first(),
        Some(&Entry::Custom)
    );
}

#[test]
fn the_click_choices_round_trip_and_left_click_is_the_default() {
    for &choice in ClickChoice::ALL {
        assert_eq!(ClickChoice::of(&choice.action()), Some(choice));
    }
    assert_eq!(
        ClickChoice::of(&openlogi_core::binding::default_binding(
            ButtonId::PresenterClick
        )),
        Some(ClickChoice::LeftClick)
    );
}

#[test]
fn hex_parsing_takes_six_digits_with_an_optional_hash() {
    let red = Some(Rgb::new(0xff, 0x2a, 0x2a));
    assert_eq!(parse_hex_color("ff2a2a"), red);
    assert_eq!(parse_hex_color("#FF2A2A"), red);
    assert_eq!(parse_hex_color("  #ff2A2a "), red);
    for bad in ["ff2a2", "gg0000", "#", "", "##ff2a2a", "ff2a2a0", "+f2a2a"] {
        assert_eq!(parse_hex_color(bad), None, "{bad:?} is rejected");
    }
}

#[gpui::test]
fn the_panel_renders_its_sections_without_a_device(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        theme::register_builtin_themes(cx);
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state =
            cx.new(|_| AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands)));
        AppState::set_global(state, cx);
    });
    let (_, cx) = cx.add_window_view(PresenterPanel::new);
    draw(cx);
    for selector in [
        "presenter-touch-effect",
        "presenter-press-effect",
        "presenter-click",
        "presenter-pointer",
        "presenter-laser-color",
        "presenter-laser-field",
        "presenter-laser-preview",
        "presenter-laser-size",
        "presenter-spotlight-size",
        "presenter-spotlight-dim",
        "presenter-touch-force",
        "presenter-press-force",
        "presenter-sensitivity",
        "presenter-precision",
        "presenter-steady-release",
        "presenter-haptics",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
    }
}

#[gpui::test]
fn a_released_touch_slider_stops_at_the_press_threshold(cx: &mut TestAppContext) {
    let (_, cx) = panel(cx);
    cx.update(|_, cx| PresenterPanel::commit_force(Force::Press, force(200), cx));

    cx.update(|_, cx| PresenterPanel::commit_force(Force::Touch, force(900), cx));

    let settings = stored(cx);
    assert_eq!(settings.effective_touch_force(), force(200));
    assert_eq!(settings.effective_press_force(), force(200));
    assert_eq!(settings.validate(), Ok(()));
}

#[gpui::test]
fn a_released_press_slider_stops_at_the_touch_threshold(cx: &mut TestAppContext) {
    let (_, cx) = panel(cx);
    cx.update(|_, cx| {
        PresenterPanel::commit_force(Force::Press, force(700), cx);
        PresenterPanel::commit_force(Force::Touch, force(400), cx);
    });

    cx.update(|_, cx| PresenterPanel::commit_force(Force::Press, force(150), cx));

    let settings = stored(cx);
    assert_eq!(settings.effective_touch_force(), force(400));
    assert_eq!(settings.effective_press_force(), force(400));
}

#[gpui::test]
fn a_threshold_edit_leaves_the_other_one_and_the_rest_alone(cx: &mut TestAppContext) {
    let (_, cx) = panel(cx);
    cx.update(|_, cx| {
        AppState::edit_presenter(cx, |it| it.pointer = HighlightPointer::Follow);
        PresenterPanel::commit_force(Force::Press, force(500), cx);
    });

    cx.update(|_, cx| PresenterPanel::commit_force(Force::Touch, force(120), cx));

    let settings = stored(cx);
    assert_eq!(settings.touch_force, Some(force(120)));
    assert_eq!(settings.press_force, Some(force(500)));
    assert_eq!(settings.pointer, HighlightPointer::Follow);

    // Back on the defaults, the table keeps no trace of the thresholds.
    cx.update(|_, cx| {
        PresenterPanel::commit_force(Force::Press, PresenterSettings::DEFAULT_PRESS_FORCE, cx);
        PresenterPanel::commit_force(Force::Touch, PresenterSettings::DEFAULT_TOUCH_FORCE, cx);
    });
    let settings = stored(cx);
    assert_eq!((settings.touch_force, settings.press_force), (None, None));
}

fn type_hex(panel: &Entity<PresenterPanel>, text: &str, cx: &mut VisualTestContext) {
    let input = panel.read_with(cx, |panel, _| panel.laser_hex.clone());
    cx.update(|window, cx| input.update(cx, |input, cx| input.set_value(text, window, cx)));
    cx.run_until_parked();
}

fn emit_hex(panel: &Entity<PresenterPanel>, event: InputEvent, cx: &mut VisualTestContext) {
    let input = panel.read_with(cx, |panel, _| panel.laser_hex.clone());
    input.update(cx, |_, cx| cx.emit(event));
    cx.run_until_parked();
}

const ENTER: InputEvent = InputEvent::PressEnter {
    secondary: false,
    shift: false,
};

#[gpui::test]
fn enter_commits_a_typed_hex_colour_and_normalises_the_field(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    draw(cx);

    type_hex(&panel, "#00E5FF", cx);
    emit_hex(&panel, ENTER, cx);

    assert_eq!(stored(cx).laser_color, Rgb::new(0x00, 0xe5, 0xff));
    let input = panel.read_with(cx, |panel, _| panel.laser_hex.clone());
    assert_eq!(input.read_with(cx, |input, _| input.value()), "00e5ff");
    assert!(!panel.read_with(cx, |panel, _| panel.laser_hex_invalid));
}

#[gpui::test]
fn an_invalid_hex_is_flagged_on_enter_and_never_written(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    draw(cx);

    type_hex(&panel, "12345", cx);
    emit_hex(&panel, ENTER, cx);

    assert_eq!(stored(cx), PresenterSettings::default());
    assert!(panel.read_with(cx, |panel, _| panel.laser_hex_invalid));
    draw(cx);
    assert!(cx.debug_bounds("presenter-laser-hex-error").is_some());

    // The next edit clears the flag again.
    emit_hex(&panel, InputEvent::Change, cx);
    assert!(!panel.read_with(cx, |panel, _| panel.laser_hex_invalid));
}

#[gpui::test]
fn blur_commits_a_valid_hex_and_restores_the_stored_colour_otherwise(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    draw(cx);

    type_hex(&panel, "00ff40", cx);
    emit_hex(&panel, InputEvent::Blur, cx);
    assert_eq!(stored(cx).laser_color, Rgb::new(0x00, 0xff, 0x40));

    type_hex(&panel, "zz", cx);
    emit_hex(&panel, InputEvent::Blur, cx);
    assert_eq!(stored(cx).laser_color, Rgb::new(0x00, 0xff, 0x40));
    assert!(!panel.read_with(cx, |panel, _| panel.laser_hex_invalid));
    draw(cx);
    let input = panel.read_with(cx, |panel, _| panel.laser_hex.clone());
    assert_eq!(input.read_with(cx, |input, _| input.value()), "00ff40");
}

#[gpui::test]
fn confirming_a_picker_option_commits_it(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    draw(cx);
    let (touch, click, pointer) = panel.read_with(cx, |panel, _| {
        (
            panel.touch_effect.select.clone(),
            panel.click.select.clone(),
            panel.pointer.clone(),
        )
    });

    cx.update(|window, cx| {
        touch.update(cx, |select, cx| {
            let spotlight = Entry::Offered(HighlightChoice::Spotlight);
            select.set_selected_value(&spotlight, window, cx);
            cx.emit(SelectEvent::Confirm(Some(spotlight)));
        });
        click.update(cx, |select, cx| {
            let off = Entry::Offered(ClickChoice::Off);
            select.set_selected_value(&off, window, cx);
            cx.emit(SelectEvent::Confirm(Some(off)));
        });
        pointer.update(cx, |select, cx| {
            let follow = PointerChoice(HighlightPointer::Follow);
            select.set_selected_value(&follow, window, cx);
            cx.emit(SelectEvent::Confirm(Some(follow)));
        });
    });
    cx.run_until_parked();

    let bindings = cx.update(|_, cx| AppState::try_read(cx).unwrap().current_presenter_bindings());
    assert_eq!(
        bindings.touch,
        Action::Highlight(HighlightEffect::Spotlight)
    );
    assert_eq!(bindings.click, Action::None);
    assert_eq!(stored(cx).pointer, HighlightPointer::Follow);
}

fn bindings(cx: &mut VisualTestContext) -> PresenterBindings {
    cx.update(|_, cx| AppState::try_read(cx).unwrap().current_presenter_bindings())
}

#[gpui::test]
fn the_pickers_follow_a_reloaded_binding_and_list_custom_only_while_it_is_stored(
    cx: &mut TestAppContext,
) {
    let (panel, cx) = panel(cx);
    draw(cx);
    let touch = panel.read_with(cx, |panel, _| panel.touch_effect.select.clone());
    let shown = |cx: &mut VisualTestContext| {
        touch.read_with(cx, |select, _| select.selected_value().copied())
    };
    assert_eq!(shown(cx), Some(Entry::Offered(HighlightChoice::LaserDot)));

    // A reload (or another view) rebinds the control behind the picker.
    cx.update(|_, cx| {
        PresenterPanel::commit_binding(
            ButtonId::PresenterTouch,
            Action::Highlight(HighlightEffect::Spotlight),
            cx,
        );
    });
    draw(cx);
    assert_eq!(shown(cx), Some(Entry::Offered(HighlightChoice::Spotlight)));

    // A hand-edited action the panel does not offer reads as read-only "Custom".
    cx.update(|_, cx| {
        PresenterPanel::commit_binding(ButtonId::PresenterTouch, Action::PlayPause, cx);
    });
    draw(cx);
    assert_eq!(shown(cx), Some(Entry::Custom));
    assert!(panel.read_with(cx, |panel, _| panel.touch_effect.listing_custom));
    assert_eq!(bindings(cx).touch, Action::PlayPause);

    // Confirming "Custom" rewrites nothing.
    touch.update(cx, |_, cx| {
        cx.emit(SelectEvent::Confirm(Some(Entry::Custom)));
    });
    cx.run_until_parked();
    assert_eq!(bindings(cx).touch, Action::PlayPause);

    // Back on an offered action, "Custom" leaves the list again.
    cx.update(|_, cx| {
        PresenterPanel::commit_binding(ButtonId::PresenterTouch, Action::None, cx);
    });
    draw(cx);
    assert_eq!(shown(cx), Some(Entry::Offered(HighlightChoice::Off)));
    assert!(!panel.read_with(cx, |panel, _| panel.touch_effect.listing_custom));
}

#[gpui::test]
fn releasing_a_slider_commits_its_setting(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    draw(cx);
    let (laser, touch) = panel.read_with(cx, |panel, _| {
        (
            panel.laser_size.slider().clone(),
            panel.touch_force.slider().clone(),
        )
    });

    laser.update(cx, |_, cx| cx.emit(SliderEvent::Release(30.0.into())));
    touch.update(cx, |_, cx| cx.emit(SliderEvent::Release(150.0.into())));
    cx.run_until_parked();

    let settings = stored(cx);
    assert_eq!(f32::from(u16::from(settings.laser_size)), 30.0);
    assert_eq!(settings.effective_touch_force(), force(150));
}

#[gpui::test]
fn the_switches_write_their_setting(cx: &mut TestAppContext) {
    let (_, cx) = panel(cx);
    // Tall enough that the last card is on screen to be clicked.
    cx.simulate_resize(gpui::size(px(1000.), px(3000.)));
    draw(cx);
    let defaults = PresenterSettings::default();

    for (selector, read) in [
        (
            "presenter-precision-control",
            (|settings: &PresenterSettings| settings.precision) as fn(&PresenterSettings) -> bool,
        ),
        ("presenter-steady-release-control", |settings| {
            settings.steady_release
        }),
        ("presenter-haptics-control", |settings| settings.haptics),
    ] {
        let center = cx.debug_bounds(selector).expect(selector).center();
        cx.simulate_click(center, Modifiers::default());
        assert_eq!(read(&stored(cx)), !read(&defaults), "{selector}");
        draw(cx);
    }
}
