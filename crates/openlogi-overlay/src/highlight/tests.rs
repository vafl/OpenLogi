use openlogi_core::config::{HighlightMotion, SpotlightDim};

use super::*;

fn active(session_id: u64) -> ActiveHighlight {
    ActiveHighlight {
        session_id,
        effect: HighlightEffect::LaserDot,
        origin: Some((10.0, 10.0)),
        travel_x: 0,
        travel_y: 0,
        pointer: openlogi_core::config::HighlightPointer::Hide,
        appearance: HighlightAppearance::default(),
        motion: HighlightMotion::default(),
    }
}

fn observed(active: Option<ActiveHighlight>) -> HighlightUpdate {
    HighlightUpdate::Observed(HighlightObservation {
        generation: 1,
        active,
    })
}

#[test]
fn the_feather_layers_compose_to_the_configured_dimming() {
    for percent in [0_u8, 25, u8::from(SpotlightDim::DEFAULT), 100] {
        let alpha = layer_alpha(percent);
        let composed = 1.0 - (1.0 - alpha).powi(4);
        assert!(
            (composed - f32::from(percent) / 100.0).abs() < 1e-5,
            "{percent} %: {composed}"
        );
    }
    assert!(
        (layer_alpha(200) - 1.0).abs() < f32::EPSILON,
        "over 100 is clamped"
    );
}

fn viewport(width: f32, height: f32) -> Size<Pixels> {
    Size::new(px(width), px(height))
}

#[test]
fn the_window_is_reused_only_on_the_same_known_display_at_the_same_size() {
    let main = Some(gpui::DisplayId::new(1));
    let side = Some(gpui::DisplayId::new(2));
    let target = located(None, (0.0, 0.0), None);
    let full = viewport(1920.0, 1080.0);
    assert!(covers(main, full, &target));
    assert!(!covers(side, full, &target), "press on another display");
    assert!(!covers(None, full, &target), "no window display recorded");
    assert!(
        !covers(main, viewport(1280.0, 720.0), &target),
        "the display changed resolution under the same id"
    );
    let unknown = Located {
        id: None,
        ..located(None, (0.0, 0.0), None)
    };
    assert!(!covers(main, full, &unknown), "press display unknown");
    assert!(!covers(None, full, &unknown), "unknown never vouches");
    let unbounded = Located {
        bounds: None,
        ..located(None, (0.0, 0.0), None)
    };
    assert!(!covers(main, full, &unbounded), "no bounds to compare");
}

#[test]
fn a_continuation_of_the_shown_session_advances_it() {
    assert_eq!(
        plan(Some(3), observed(Some(active(3)))),
        Step::Advance(active(3))
    );
}

#[test]
fn a_new_session_replaces_the_shown_one_and_reports_it() {
    // `Begin` ends whatever is shown as a real release, not a disconnect.
    assert_eq!(
        plan(Some(3), observed(Some(active(4)))),
        Step::Begin(active(4))
    );
}

#[test]
fn a_session_with_nothing_shown_begins() {
    assert_eq!(
        plan(None, observed(Some(active(1)))),
        Step::Begin(active(1))
    );
}

#[test]
fn a_hidden_observation_ends_the_shown_session_for_real() {
    assert_eq!(plan(Some(3), observed(None)), Step::Release);
}

#[test]
fn losing_the_agent_ends_the_session_without_a_release() {
    assert_eq!(
        plan(Some(3), HighlightUpdate::Disconnected),
        Step::Disconnect
    );
    assert_eq!(
        plan(None, HighlightUpdate::Disconnected),
        Step::Disconnect,
        "also with nothing shown"
    );
}

fn located(
    point: Option<(f32, f32)>,
    bounds_origin: (f32, f32),
    origin: Option<(f64, f64)>,
) -> Located {
    Located {
        id: Some(gpui::DisplayId::new(1)),
        bounds: Some(Bounds::new(
            self::point(px(bounds_origin.0), px(bounds_origin.1)),
            Size::new(px(1920.0), px(1080.0)),
        )),
        point: point.map(|(x, y)| self::point(px(x), px(y))),
        origin,
    }
}

#[test]
fn the_anchor_is_local_to_the_display_whichever_space_the_bounds_use() {
    // Display-relative bounds (macOS): the point is already local.
    let native = anchor(&located(
        Some((100.0, 50.0)),
        (0.0, 0.0),
        Some((-1920.0, 0.0)),
    ))
    .expect("on a display");
    assert_eq!(native.local, (100.0, 50.0));
    assert_eq!(native.display_origin, (-1920.0, 0.0));
    // Global bounds (Linux): the offset of the display is removed.
    let global = anchor(&located(
        Some((2020.0, 50.0)),
        (1920.0, 0.0),
        Some((1920.0, 0.0)),
    ))
    .expect("on a display");
    assert_eq!(global.local, (100.0, 50.0));
    assert_eq!(global.display_origin, (1920.0, 0.0));
}

#[test]
fn a_press_on_no_known_display_has_no_anchor() {
    assert!(anchor(&located(Some((5.0, 5.0)), (0.0, 0.0), None)).is_none());
    assert!(anchor(&located(None, (0.0, 0.0), None)).is_none());
}

fn moved(session_id: u64, travel_x: i64) -> HighlightUpdate {
    observed(Some(ActiveHighlight {
        travel_x,
        ..active(session_id)
    }))
}

#[test]
fn a_burst_keeps_only_its_newest_update() {
    assert_eq!(
        coalesce([moved(1, 1), moved(1, 2), moved(1, 3)]),
        vec![moved(1, 3)]
    );
    assert!(coalesce([]).is_empty());
}

#[test]
fn a_burst_ending_the_session_keeps_the_last_motion_before_it() {
    assert_eq!(
        coalesce([moved(1, 1), moved(1, 2), moved(1, 3), observed(None)]),
        vec![moved(1, 3), observed(None)]
    );
    assert_eq!(
        coalesce([observed(None)]),
        vec![observed(None)],
        "nothing was skipped"
    );
}

#[test]
fn a_disconnect_in_a_burst_drops_the_motion_before_it() {
    assert_eq!(
        coalesce([moved(1, 1), HighlightUpdate::Disconnected, observed(None)]),
        vec![observed(None)]
    );
    assert_eq!(
        coalesce([moved(1, 1), observed(None), HighlightUpdate::Disconnected]),
        vec![HighlightUpdate::Disconnected]
    );
}
