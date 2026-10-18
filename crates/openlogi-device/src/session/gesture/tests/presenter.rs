//! Presenter holds: live button edges and raw-XY motion, never gestures.

use super::*;

const TOUCH: u16 = reprog_controls::PRESENTER_TOUCH_CID;
const PRESS: u16 = reprog_controls::PRESENTER_PRESS_CID;

fn presenter(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    tx: &mpsc::UnboundedSender<CapturedInput>,
) {
    acc.on_presenter_event(event, &PRESENTER_SOURCE_BUTTONS, tx);
    acc.on_event(event, &[], &[], &[], &[], tx);
}

fn held(cids: [u16; 4]) -> RawControlEvent {
    RawControlEvent::DivertedButtons(cids)
}

fn xy(dx: i16, dy: i16) -> RawControlEvent {
    RawControlEvent::RawXy { dx, dy }
}

fn drain(rx: &mut mpsc::UnboundedReceiver<CapturedInput>) -> Vec<CapturedInput> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[test]
fn hold_emits_edges_and_motion_after_skipping_the_first_sample() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    presenter(&mut acc, held([TOUCH, 0, 0, 0]), &tx);
    presenter(&mut acc, xy(-5562, 3), &tx);
    presenter(&mut acc, xy(4, -2), &tx);
    presenter(&mut acc, xy(1, 1), &tx);
    presenter(&mut acc, held([0; 4]), &tx);
    assert_eq!(
        drain(&mut rx),
        [
            CapturedInput::ButtonDown(ButtonId::PresenterTouch),
            CapturedInput::Motion { dx: 4, dy: -2 },
            CapturedInput::Motion { dx: 1, dy: 1 },
            CapturedInput::ButtonUp(ButtonId::PresenterTouch),
        ]
    );
}

#[test]
fn raw_xy_outside_a_hold_is_dropped_and_a_new_hold_skips_again() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    presenter(&mut acc, xy(7, 7), &tx);
    assert!(drain(&mut rx).is_empty());
    for _ in 0..2 {
        presenter(&mut acc, held([TOUCH, 0, 0, 0]), &tx);
        presenter(&mut acc, xy(-4000, 0), &tx);
        presenter(&mut acc, xy(2, 0), &tx);
        presenter(&mut acc, held([0; 4]), &tx);
    }
    let motions = drain(&mut rx)
        .into_iter()
        .filter(|input| matches!(input, CapturedInput::Motion { .. }))
        .count();
    assert_eq!(motions, 2, "each hold drops exactly its own first sample");
}

#[test]
fn overlapping_holds_emit_each_buttons_own_edges() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    presenter(&mut acc, held([TOUCH, 0, 0, 0]), &tx);
    presenter(&mut acc, held([TOUCH, PRESS, 0, 0]), &tx);
    presenter(&mut acc, xy(-4000, 0), &tx);
    presenter(&mut acc, xy(3, 0), &tx);
    presenter(&mut acc, held([TOUCH, 0, 0, 0]), &tx);
    presenter(&mut acc, xy(5, 0), &tx);
    presenter(&mut acc, held([0; 4]), &tx);
    assert_eq!(
        drain(&mut rx),
        [
            CapturedInput::ButtonDown(ButtonId::PresenterTouch),
            CapturedInput::ButtonDown(ButtonId::PresenterPress),
            CapturedInput::Motion { dx: 3, dy: 0 },
            CapturedInput::ButtonUp(ButtonId::PresenterPress),
            CapturedInput::Motion { dx: 5, dy: 0 },
            CapturedInput::ButtonUp(ButtonId::PresenterTouch),
        ]
    );
}

#[test]
fn presenter_holds_never_emit_a_gesture() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    presenter(&mut acc, held([TOUCH, 0, 0, 0]), &tx);
    presenter(&mut acc, xy(-4000, 0), &tx);
    acc.backdate_hold_for_test();
    presenter(&mut acc, xy(300, 0), &tx);
    presenter(&mut acc, held([0; 4]), &tx);
    assert!(
        drain(&mut rx)
            .iter()
            .all(|input| !matches!(input, CapturedInput::Gesture(..)))
    );
}

#[test]
fn unlisted_presenter_cid_is_ignored() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    acc.on_presenter_event(held([TOUCH, 0, 0, 0]), &[], &tx);
    acc.on_event(held([TOUCH, 0, 0, 0]), &[], &[], &[], &[], &tx);
    acc.on_event(xy(1, 1), &[], &[], &[], &[], &tx);
    assert!(drain(&mut rx).is_empty());
}

/// A presenter control is armed with raw-XY when its table entry advertises
/// it and as a plain press otherwise, so press/release still arrive.
#[tokio::test]
async fn presenter_source_arms_with_raw_xy_or_falls_back_to_plain() {
    let cid = TOUCH;
    for (raw_xy, expected_flags) in [(true, 0x33), (false, 0x23)] {
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => response[4] = 2,
                (2, 0) => response[4] = 1,
                (2, 1) => {
                    response[4..6].copy_from_slice(&cid.to_be_bytes());
                    response[8] = 0x20;
                    response[12] = u8::from(raw_xy);
                }
                (2, 2) => response[4..6].copy_from_slice(&cid.to_be_bytes()),
                (2, 3) => return Some(request.to_vec()),
                _ => panic!("unexpected capture request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        let device = Device::new(channel.clone(), 0xff).await.unwrap();
        let spec = CaptureSpec {
            divert_presenter_sources: vec![(cid, ButtonId::PresenterTouch)],
            ..CaptureSpec::default()
        };
        let mut armed = ArmedControls::default();
        arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
            .await
            .unwrap();
        let writes: Vec<_> = handle
            .written_reports()
            .into_iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .map(|report| report[6])
            .collect();
        assert_eq!(writes, [expected_flags], "raw XY {raw_xy}");
        assert_eq!(
            armed.presenter_cids,
            [(cid, ButtonId::PresenterTouch)],
            "raw XY {raw_xy}"
        );
    }
}

/// Arm `spec` on a device exposing `cids` (all divertable) whose control
/// `0x0050` reports `click_diverted`; returns the `setCidReporting` writes as
/// `(cid, flags)`.
async fn arm_presenter_click(
    cids: [u16; 2],
    click_diverted: bool,
    spec: CaptureSpec,
) -> (Vec<(u16, u8)>, ArmedControls) {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0 | 2, 0) => response[4] = 2,
            (2, 1) => {
                let cid = cids[usize::from(request[4])];
                response[4..6].copy_from_slice(&cid.to_be_bytes());
                response[8] = 0x20;
            }
            (2, 2) => {
                response[4..6].copy_from_slice(&request[4..6]);
                response[6] = u8::from(click_diverted);
            }
            (2, 3) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    });
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .unwrap();
    let writes = handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
        .map(|report| (u16::from_be_bytes([report[4], report[5]]), report[6]))
        .collect();
    (writes, armed)
}

const CLICK: u16 = reprog_controls::PRESENTER_CLICK_CID;

fn native_click_spec() -> CaptureSpec {
    CaptureSpec {
        presenter_click_native: true,
        ..CaptureSpec::default()
    }
}

/// Logi Options+ leaves the short click diverted when it exits uncleanly; a
/// native-click plan hands it back once, as a plain undivert (valid bit set,
/// divert bit clear), without taking ownership of it.
#[tokio::test]
async fn a_stale_diverted_presenter_click_is_handed_back_to_native() {
    let (writes, armed) = arm_presenter_click([TOUCH, CLICK], true, native_click_spec()).await;
    assert_eq!(writes, [(CLICK, 0x02)]);
    assert!(armed.presenter_cids.is_empty() && armed.button_cids.is_empty());
}

/// The stale-click repair is best-effort: when reading the click's reporting
/// fails, the session still arms its presenter sources.
#[tokio::test]
async fn a_failed_stale_click_repair_does_not_abort_arming() {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0 | 2, 0) => response[4] = 2,
            (2, 1) => {
                let cid = [TOUCH, CLICK][usize::from(request[4])];
                response[4..6].copy_from_slice(&cid.to_be_bytes());
                response[8] = 0x20;
            }
            (2, 2) if request[4..6] == CLICK.to_be_bytes() => {
                // A HID++ 2.0 error reply: the click cannot be read back.
                response[2] = 0xff;
                response[3] = request[2];
                response[4] = request[3];
                response[5] = 0x02;
            }
            (2, 2) => response[4..6].copy_from_slice(&request[4..6]),
            (2, 3) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    });
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let spec = CaptureSpec {
        presenter_click_native: true,
        divert_presenter_sources: vec![(TOUCH, ButtonId::PresenterTouch)],
        ..CaptureSpec::default()
    };
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .expect("the click repair is best-effort");
    assert_eq!(armed.presenter_cids, [(TOUCH, ButtonId::PresenterTouch)]);
    assert!(
        reporting_writes(&handle)
            .iter()
            .all(|&(cid, _)| cid == TOUCH),
        "the click was not written"
    );
}

#[tokio::test]
async fn a_native_presenter_click_is_left_alone_when_it_is_not_diverted() {
    let (writes, _) = arm_presenter_click([TOUCH, CLICK], false, native_click_spec()).await;
    assert!(writes.is_empty(), "{writes:?}");
}

#[tokio::test]
async fn the_stale_click_cleanup_needs_a_native_click_plan() {
    let (writes, _) = arm_presenter_click([TOUCH, CLICK], true, CaptureSpec::default()).await;
    assert!(
        writes.is_empty(),
        "a bound click keeps its own divert, and a plan without presenter controls never asks: {writes:?}"
    );
}

/// A bound (non-default) click is plain-diverted as the plan lists it. Only a
/// presenter's plan lists `0x0050`, so the session applies no device heuristic
/// of its own.
#[tokio::test]
async fn a_bound_presenter_click_is_plain_diverted() {
    let bound_click = CaptureSpec {
        divert_buttons: vec![(CLICK, ButtonId::PresenterClick)],
        ..CaptureSpec::default()
    };
    let (writes, armed) = arm_presenter_click([TOUCH, CLICK], false, bound_click).await;
    assert_eq!(writes, [(CLICK, 0x23)], "plain divert");
    assert_eq!(armed.button_cids, [(CLICK, ButtonId::PresenterClick)]);
}

#[test]
fn a_force_swap_in_one_report_emits_the_down_before_the_up() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    presenter(&mut acc, held([TOUCH, 0, 0, 0]), &tx);
    presenter(&mut acc, xy(-4000, 0), &tx);
    presenter(&mut acc, held([PRESS, 0, 0, 0]), &tx);
    presenter(&mut acc, xy(2, 0), &tx);
    presenter(&mut acc, held([TOUCH, 0, 0, 0]), &tx);
    presenter(&mut acc, xy(3, 0), &tx);
    presenter(&mut acc, held([0; 4]), &tx);
    assert_eq!(
        drain(&mut rx),
        [
            CapturedInput::ButtonDown(ButtonId::PresenterTouch),
            CapturedInput::ButtonDown(ButtonId::PresenterPress),
            CapturedInput::ButtonUp(ButtonId::PresenterTouch),
            CapturedInput::Motion { dx: 2, dy: 0 },
            CapturedInput::ButtonDown(ButtonId::PresenterTouch),
            CapturedInput::ButtonUp(ButtonId::PresenterPress),
            CapturedInput::Motion { dx: 3, dy: 0 },
            CapturedInput::ButtonUp(ButtonId::PresenterTouch),
        ],
        "motion streams through the swaps without a fresh skip"
    );
}

/// Feature index the presenter scripted by [`presenter_device`] answers for
/// `0x1b04`.
const REPROG: u8 = 2;

/// A presenter whose `0x1b04` table is its touch and press controls, both
/// divertable with raw-XY and reporting native. `setCidReporting` writes are
/// echoed.
fn presenter_device() -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
    ScriptedRawHidChannel::with_dynamic_responder(|request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => response[4] = REPROG,
            (2, 0) => response[4] = 2,
            (2, 1) => {
                let cid = [TOUCH, PRESS][usize::from(request[4])];
                response[4..6].copy_from_slice(&cid.to_be_bytes());
                response[8] = 0x20;
                response[12] = 0x01;
            }
            (2, 2) => response[4..6].copy_from_slice(&request[4..6]),
            (2, 3) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    })
}

/// The `setCidReporting` writes `handle` saw, as `(cid, flags)`.
fn reporting_writes(handle: &crate::replay::ReplayChannelHandle) -> Vec<(u16, u8)> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == REPROG && report[3] >> 4 == 3)
        .map(|report| (u16::from_be_bytes([report[4], report[5]]), report[6]))
        .collect()
}

fn reprog_event(event: u8, payload_head: &[u8]) -> v20::Message {
    let mut payload = [0; 16];
    payload[..payload_head.len()].copy_from_slice(payload_head);
    v20::Message::Long(
        v20::MessageHeader {
            device_index: 0xff,
            feature_index: REPROG,
            function_id: hidpp::nibble::U4::from_lo(event),
            software_id: hidpp::nibble::U4::from_lo(0),
        },
        payload,
    )
}

fn buttons_event(cids: [u16; 2]) -> v20::Message {
    let [a, b] = cids.map(u16::to_be_bytes);
    reprog_event(0, &[a[0], a[1], b[0], b[1]])
}

fn raw_xy_event(dx: i16, dy: i16) -> v20::Message {
    let (dx, dy) = (dx.to_be_bytes(), dy.to_be_bytes());
    reprog_event(1, &[dx[0], dx[1], dy[0], dy[1]])
}

/// Through the session's own handler: a force swap reported in one
/// `divertedButtonsEvent` delivers the new level's down before the old
/// level's up, and motion streams through the swap.
#[tokio::test]
async fn a_force_swap_through_the_session_handler_goes_down_before_up() {
    let (raw, _handle) = presenter_device();
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let spec = CaptureSpec {
        divert_presenter_sources: PRESENTER_SOURCE_BUTTONS.to_vec(),
        ..CaptureSpec::default()
    };
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .unwrap();
    let capture = GestureCapture::new(armed);
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let handler = capture.report_handler(0xff, sink);
    handler(&buttons_event([TOUCH, 0]));
    handler(&raw_xy_event(-4000, 0));
    handler(&raw_xy_event(2, 0));
    // The firmware swaps the level in one report: [touch] -> [press].
    handler(&buttons_event([PRESS, 0]));
    handler(&raw_xy_event(3, 0));
    handler(&buttons_event([0, 0]));
    assert_eq!(
        drain(&mut inputs),
        [
            CapturedInput::ButtonDown(ButtonId::PresenterTouch),
            CapturedInput::Motion { dx: 2, dy: 0 },
            CapturedInput::ButtonDown(ButtonId::PresenterPress),
            CapturedInput::ButtonUp(ButtonId::PresenterTouch),
            CapturedInput::Motion { dx: 3, dy: 0 },
            CapturedInput::ButtonUp(ButtonId::PresenterPress),
        ]
    );
}

/// Ending the session hands both presenter controls back to native
/// reporting.
#[tokio::test]
async fn ending_the_session_restores_the_presenter_controls() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb042,
    };
    let (raw, handle) = presenter_device();
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let spec = CaptureSpec {
        divert_presenter_sources: PRESENTER_SOURCE_BUTTONS.to_vec(),
        ..CaptureSpec::default()
    };
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .unwrap();
    assert_eq!(reporting_writes(&handle), [(TOUCH, 0x33), (PRESS, 0x33)]);
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("presenter-node".to_owned()),
        [route.clone()],
        channel.clone(),
    );
    let shared = SharedChannel::new(channel, route);
    let pending = GestureCapture::new(armed)
        .into_pending(&shared)
        .expect("diverted presenter controls are owed a restore");
    assert!(matches!(
        pending.allow_current_channel().retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    let mut restores = reporting_writes(&handle).split_off(2);
    restores.sort_unstable();
    assert_eq!(restores, [(TOUCH, 0x22), (PRESS, 0x22)]);
}
