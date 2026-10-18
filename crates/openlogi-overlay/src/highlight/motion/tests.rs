use openlogi_core::config::HighlightAppearance;

use super::*;

fn active(session_id: u64, travel: (i64, i64)) -> ActiveHighlight {
    ActiveHighlight {
        session_id,
        effect: HighlightEffect::LaserDot,
        origin: Some((10.0, 10.0)),
        travel_x: travel.0,
        travel_y: travel.1,
        pointer: openlogi_core::config::HighlightPointer::Hide,
        appearance: HighlightAppearance::default(),
        // Plain gain and no rewind unless a test asks for more.
        motion: HighlightMotion {
            sensitivity: 100,
            precision: false,
            steady_release: false,
        },
    }
}

#[test]
fn a_session_keeps_the_appearance_it_began_with() {
    let look = HighlightAppearance {
        laser_size: 20,
        spotlight_size: 250,
        ..HighlightAppearance::default()
    };
    let mut highlight = active(3, (0, 0));
    highlight.appearance = look;
    let size = (400.0, 300.0);
    let mut session = new_session(&highlight, None, size);
    assert_eq!(session.appearance, look);
    highlight.appearance = HighlightAppearance::default();
    session.advance(&highlight, size, Instant::now());
    assert_eq!(session.appearance, look, "fixed at the start");
}

fn new_session(highlight: &ActiveHighlight, anchor: Option<Anchor>, size: (f32, f32)) -> Session {
    Session::new(highlight, anchor, size, Instant::now())
}

fn anchor(local: (f32, f32)) -> Anchor {
    Anchor {
        local,
        display_origin: (0.0, 0.0),
    }
}

#[test]
fn motion_follows_travel_deltas_and_clamps_per_step() {
    let size = (100.0, 100.0);
    let mut session = new_session(&active(1, (0, 0)), Some(anchor((50.0, 50.0))), size);
    session.advance(&active(1, (10, -5)), size, Instant::now());
    assert_eq!(session.pos, (60.0, 45.0));
    // Push far past the right edge, then back: the excess is discarded.
    session.advance(&active(1, (1000, -5)), size, Instant::now());
    assert_eq!(session.pos.0, 100.0);
    session.advance(&active(1, (990, -5)), size, Instant::now());
    assert_eq!(session.pos.0, 90.0);
}

#[test]
fn advance_reports_whether_the_position_changed() {
    let size = (100.0, 100.0);
    let mut session = new_session(&active(1, (0, 0)), Some(anchor((50.0, 50.0))), size);
    assert!(
        !session.advance(&active(1, (0, 0)), size, Instant::now()),
        "no travel"
    );
    assert!(session.advance(&active(1, (4, 0)), size, Instant::now()));
    // Pinned against the right edge: the clamped step moves nothing.
    session.advance(&active(1, (1000, 0)), size, Instant::now());
    assert!(!session.advance(&active(1, (1010, 0)), size, Instant::now()));
}

#[test]
fn the_move_report_is_the_drawn_position_in_global_points() {
    let size = (100.0, 100.0);
    let mut session = new_session(
        &active(7, (0, 0)),
        Some(Anchor {
            local: (50.0, 50.0),
            display_origin: (-1920.0, -200.0),
        }),
        size,
    );
    session.advance(&active(7, (10, -5)), size, Instant::now());
    assert_eq!(
        session.move_command(),
        Some(OverlayCommand::HighlightMoved {
            session_id: 7,
            x: -1920.0 + 60.0,
            y: -200.0 + 45.0,
        })
    );
    let unanchored = new_session(&active(7, (0, 0)), None, size);
    assert_eq!(unanchored.move_command(), None);
}

fn move_to(x: f64) -> OverlayCommand {
    OverlayCommand::HighlightMoved {
        session_id: 1,
        x,
        y: 0.0,
    }
}

#[test]
fn the_first_move_goes_out_at_once_and_a_burst_keeps_only_the_newest() {
    let mut throttle = MoveThrottle::default();
    let t0 = Instant::now();
    assert_eq!(throttle.offer(t0, move_to(1.0)), Some(move_to(1.0)));
    // Inside the interval: held, and each newer one replaces the last.
    assert_eq!(
        throttle.offer(t0 + Duration::from_millis(4), move_to(2.0)),
        None
    );
    assert_eq!(
        throttle.offer(t0 + Duration::from_millis(8), move_to(3.0)),
        None
    );
    assert_eq!(
        throttle.wait(t0 + Duration::from_millis(8)),
        Some(Duration::from_millis(8))
    );
    assert_eq!(
        throttle.flush(t0 + Duration::from_millis(8)),
        None,
        "too early"
    );
    assert_eq!(throttle.flush(t0 + MOVE_INTERVAL), Some(move_to(3.0)));
    assert_eq!(throttle.flush(t0 + MOVE_INTERVAL), None, "sent once");
    assert_eq!(throttle.wait(t0 + MOVE_INTERVAL), None);
}

#[test]
fn a_move_after_the_interval_passes_through_and_drops_the_held_one() {
    let mut throttle = MoveThrottle::default();
    let t0 = Instant::now();
    throttle.offer(t0, move_to(1.0));
    throttle.offer(t0 + Duration::from_millis(5), move_to(2.0));
    let later = t0 + MOVE_INTERVAL * 2;
    assert_eq!(throttle.offer(later, move_to(9.0)), Some(move_to(9.0)));
    assert_eq!(throttle.flush(later + MOVE_INTERVAL), None, "superseded");
}

#[test]
fn clearing_forgets_the_held_move() {
    let mut throttle = MoveThrottle::default();
    let t0 = Instant::now();
    throttle.offer(t0, move_to(1.0));
    throttle.offer(t0 + Duration::from_millis(1), move_to(2.0));
    throttle.clear();
    assert_eq!(throttle.flush(t0 + MOVE_INTERVAL), None);
    assert_eq!(throttle.wait(t0), None);
}

#[test]
fn a_missing_origin_starts_at_the_display_centre() {
    let session = new_session(&active(1, (0, 0)), None, (200.0, 100.0));
    assert_eq!(session.pos, (100.0, 50.0));
    assert_eq!(
        session.release_command(Instant::now()),
        None,
        "no global position to report"
    );
}

#[test]
fn the_release_is_the_last_drawn_position_in_global_points() {
    let size = (100.0, 100.0);
    // A second display left of and above the primary one.
    let mut session = new_session(
        &active(7, (0, 0)),
        Some(Anchor {
            local: (50.0, 50.0),
            display_origin: (-1920.0, -200.0),
        }),
        size,
    );
    session.advance(&active(7, (10, -5)), size, Instant::now());
    assert_eq!(
        session.release_command(Instant::now()),
        Some(OverlayCommand::HighlightReleased {
            session_id: 7,
            x: -1920.0 + 60.0,
            y: -200.0 + 45.0,
        })
    );
}

#[test]
fn local_positions_translate_by_the_display_origin() {
    assert_eq!(to_global((12.5, 3.0), (100.0, 50.0)), (112.5, 53.0));
    assert_eq!(to_global((0.0, 0.0), (0.0, 0.0)), (0.0, 0.0));
}

fn ms(base: Instant, millis: u64) -> Instant {
    base + Duration::from_millis(millis)
}

fn history(base: Instant, samples: &[(u64, (f32, f32))]) -> VecDeque<Sample> {
    samples.iter().map(|&(t, pos)| (ms(base, t), pos)).collect()
}

#[test]
fn the_precision_factor_is_bounded_monotonic_and_continuous() {
    assert!((precision_factor(0.0) - PRECISION_MIN_FACTOR).abs() < 1e-6);
    assert!((precision_factor(PRECISION_SLOW) - PRECISION_MIN_FACTOR).abs() < 1e-6);
    assert!((precision_factor(PRECISION_FAST) - 1.0).abs() < 1e-6);
    assert!((precision_factor(1e6) - 1.0).abs() < 1e-6);
    let mut previous = precision_factor(0.0);
    let mut speed = 0.0_f32;
    while speed < 2.0 * PRECISION_FAST {
        speed += 1.0;
        let factor = precision_factor(speed);
        assert!(
            (PRECISION_MIN_FACTOR..=1.0).contains(&factor),
            "{speed}: {factor}"
        );
        assert!(factor >= previous, "monotonic at {speed}");
        assert!(factor - previous < 0.01, "no jump at {speed}");
        previous = factor;
    }
}

#[test]
fn sensitivity_scales_the_gain_linearly() {
    for factor in [PRECISION_MIN_FACTOR, 0.7, 1.0] {
        let base = effective_gain(100, factor);
        assert!((base - factor).abs() < 1e-6);
        assert!((effective_gain(50, factor) - base / 2.0).abs() < 1e-6);
        assert!((effective_gain(200, factor) - base * 2.0).abs() < 1e-6);
    }
}

#[test]
fn the_speed_average_follows_a_change_with_the_time_it_covers() {
    let short = smooth_speed(0.0, 1000.0, 0.01);
    let long = smooth_speed(0.0, 1000.0, 0.2);
    assert!(0.0 < short && short < long && long < 1000.0);
    assert!(
        (smooth_speed(500.0, 500.0, 0.05) - 500.0).abs() < 1e-3,
        "a steady speed stays put"
    );
}

#[test]
fn a_session_starts_fully_damped_so_one_fast_report_cannot_undamp_it() {
    let t0 = Instant::now();
    let mut pace = Pace::new(t0);
    // One 30-unit report 10 ms in is 3000 units/s, but alone it is only a
    // sample of the average, not a sweep.
    let first = pace.factor(ms(t0, 10), (30.0, 0.0));
    assert!(first < 0.8, "{first}");
    assert!(first > PRECISION_MIN_FACTOR);
    let mut factor = first;
    for n in 2..=40 {
        factor = pace.factor(ms(t0, 10 * n), (30.0, 0.0));
    }
    assert!((factor - 1.0).abs() < 1e-3, "a sustained sweep undamps");
}

/// Drive a session with `units` device units per observation on the x
/// axis, one observation every 10 ms (the device's ~100 Hz), and return
/// the distance the dot moved in the last step.
fn last_step(motion: HighlightMotion, units: i64) -> f32 {
    let size = (100_000.0, 100.0);
    let t0 = Instant::now();
    let mut highlight = active(1, (0, 0));
    highlight.motion = motion;
    let mut session = Session::new(&highlight, Some(anchor((50_000.0, 50.0))), size, t0);
    let mut step = 0.0;
    for n in 1..=40 {
        let before = session.pos.0;
        highlight.travel_x = n * units;
        session.advance(&highlight, size, ms(t0, 10 * n.unsigned_abs()));
        step = session.pos.0 - before;
    }
    step
}

#[test]
fn slow_steady_motion_steps_less_per_unit_than_fast_motion() {
    let motion = HighlightMotion {
        sensitivity: 100,
        precision: true,
        steady_release: false,
    };
    let slow = last_step(motion, 1);
    let fast = last_step(motion, 30) / 30.0;
    assert!(
        (slow - PRECISION_MIN_FACTOR).abs() < 1e-3,
        "slow aiming is fully damped: {slow}"
    );
    assert!((fast - 1.0).abs() < 1e-3, "a sweep is undamped: {fast}");
    let plain = HighlightMotion {
        precision: false,
        ..motion
    };
    assert!(
        (last_step(plain, 1) - 1.0).abs() < 1e-3,
        "precision off is plain gain"
    );
    let half = HighlightMotion {
        sensitivity: 50,
        precision: false,
        steady_release: false,
    };
    assert!((last_step(half, 10) - 5.0).abs() < 1e-3);
}

#[test]
fn a_dwell_followed_by_a_flick_settles_on_the_dwell_point() {
    let t0 = Instant::now();
    // Arrives at (50, 50), rests 300 ms, then a 60 ms flick of ~40 pt.
    let samples = history(
        t0,
        &[
            (0, (10.0, 50.0)),
            (100, (50.0, 50.0)),
            (400, (60.0, 50.0)),
            (420, (70.0, 50.0)),
            (440, (80.0, 50.0)),
            (460, (90.0, 50.0)),
        ],
    );
    // The dwell was left at 400 ms, inside the 100 ms flick window.
    assert_eq!(
        settled_position(&samples, ms(t0, 460), (90.0, 50.0)),
        (50.0, 50.0)
    );
}

#[test]
fn a_still_hand_keeps_its_last_position() {
    let t0 = Instant::now();
    let samples = history(t0, &[(0, (10.0, 10.0)), (100, (50.0, 50.0))]);
    assert_eq!(
        settled_position(&samples, ms(t0, 300), (50.0, 50.0)),
        (50.0, 50.0)
    );
}

#[test]
fn a_dwell_that_ended_before_the_flick_window_is_not_a_flick() {
    let t0 = Instant::now();
    // Dwelled 100..400, then kept moving for 170 ms: the dwell ended
    // before the last 100 ms, so this was a deliberate move.
    let samples = history(
        t0,
        &[
            (0, (10.0, 0.0)),
            (100, (50.0, 0.0)),
            (400, (70.0, 0.0)),
            (410, (90.0, 0.0)),
            (420, (110.0, 0.0)),
            (430, (130.0, 0.0)),
            (440, (150.0, 0.0)),
            (450, (170.0, 0.0)),
            (560, (190.0, 0.0)),
        ],
    );
    assert_eq!(
        settled_position(&samples, ms(t0, 570), (190.0, 0.0)),
        (190.0, 0.0)
    );
}

#[test]
fn continuous_motion_keeps_its_last_position() {
    let t0 = Instant::now();
    // 5 pt every 10 ms for half a second: never rests.
    let samples: VecDeque<Sample> = (0..=50_u16)
        .map(|n| (ms(t0, 10 * u64::from(n)), (5.0 * f32::from(n), 0.0)))
        .collect();
    assert_eq!(
        settled_position(&samples, ms(t0, 500), (250.0, 0.0)),
        (250.0, 0.0)
    );
}

#[test]
fn a_rest_shorter_than_the_minimum_dwell_is_not_a_rest() {
    let t0 = Instant::now();
    // Paused 120 ms (< 150) mid-gesture, then moved on.
    let samples = history(
        t0,
        &[
            (0, (0.0, 0.0)),
            (100, (50.0, 50.0)),
            (220, (80.0, 50.0)),
            (240, (110.0, 50.0)),
        ],
    );
    assert_eq!(
        settled_position(&samples, ms(t0, 260), (110.0, 50.0)),
        (110.0, 50.0)
    );
}

#[test]
fn jittery_rest_counts_as_a_dwell() {
    let t0 = Instant::now();
    // Within 3 pt for 190 ms (>= 150), then a flick away.
    let samples = history(
        t0,
        &[
            (0, (0.0, 0.0)),
            (100, (50.0, 50.0)),
            (150, (51.0, 50.0)),
            (200, (50.0, 51.0)),
            (250, (51.0, 51.0)),
            (290, (80.0, 50.0)),
            (300, (95.0, 50.0)),
        ],
    );
    assert_eq!(
        settled_position(&samples, ms(t0, 320), (95.0, 50.0)),
        (51.0, 51.0)
    );
}

#[test]
fn no_history_keeps_the_last_position() {
    let t0 = Instant::now();
    assert_eq!(
        settled_position(&VecDeque::new(), ms(t0, 120), (7.0, 8.0)),
        (7.0, 8.0)
    );
}

#[test]
fn the_history_is_bounded_but_keeps_the_sample_covering_its_start() {
    let size = (100_000.0, 100.0);
    let t0 = Instant::now();
    let mut highlight = active(1, (0, 0));
    let mut session = Session::new(&highlight, Some(anchor((10.0, 50.0))), size, t0);
    for n in 1..=500_i64 {
        highlight.travel_x = n;
        session.advance(&highlight, size, ms(t0, 10 * n.unsigned_abs()));
    }
    // One sample per 10 ms over the 1 s span, plus the sample that covers its
    // start and the newest one.
    assert!(session.history.len() <= 102, "{}", session.history.len());
    let oldest = session.history.front().expect("history").0;
    // The last observation was at 5000 ms, so the retained span must start at
    // or before 5000 ms - HISTORY_SPAN.
    let last = ms(t0, 10 * 500);
    let horizon = last.checked_sub(HISTORY_SPAN).expect("an instant 5 s in");
    assert!(oldest <= horizon, "the history covers its full span");
}

#[test]
fn a_session_release_undoes_a_flick_only_with_steady_release_on() {
    let size = (1000.0, 100.0);
    let t0 = Instant::now();
    let mut highlight = active(4, (0, 0));
    highlight.motion.steady_release = true;
    let mut session = Session::new(&highlight, Some(anchor((100.0, 50.0))), size, t0);
    // Rest at the anchor for 300 ms, then a quick 40-pt jiggle.
    for (n, t) in [(1, 320), (2, 340), (3, 360), (4, 380)] {
        highlight.travel_x = n * 10;
        session.advance(&highlight, size, ms(t0, t));
    }
    assert_eq!(session.pos.0, 140.0);
    assert_eq!(
        session.release_command(ms(t0, 400)),
        Some(OverlayCommand::HighlightReleased {
            session_id: 4,
            x: 100.0,
            y: 50.0,
        }),
        "back at the resting point"
    );
    session.motion.steady_release = false;
    assert_eq!(
        session.release_command(ms(t0, 400)),
        Some(OverlayCommand::HighlightReleased {
            session_id: 4,
            x: 140.0,
            y: 50.0,
        }),
        "off keeps the last position"
    );
}
