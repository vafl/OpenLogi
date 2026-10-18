use std::time::Duration;

use super::*;
use crate::capture_plan::{PlanContext, plan_for_device, plan_for_device_on};
use crate::highlight::HighlightSession;
use crate::runtime::pointer_output::PointerOutput;
use crate::watchers::retry::wait_for_deadline;
use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection};
use openlogi_core::config::{Config, ThumbwheelSensitivity, VerticalScrollSensitivity};
use openlogi_core::device::Capabilities;
use openlogi_hid::{CaptureChannelSlot, DeviceRoute};

fn route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc548,
    }
}

fn physical_key() -> PhysicalDeviceKey {
    PhysicalDeviceKey::parse("unit:00000001").expect("fixture should be a physical key")
}

fn session_id(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("mouse-a", epoch)
}

/// A highlight session no pointer worker serves.
fn detached_highlight() -> Arc<HighlightSession> {
    Arc::new(HighlightSession::new(PointerOutput::detached()))
}

fn plan() -> DeviceCapturePlan {
    plan_for_device(
        &openlogi_core::config::Config::default(),
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    )
}

fn live_session_from_plan(epoch: u64, plan: DeviceCapturePlan) -> RunningSession {
    let (stop, _rx) = oneshot::channel();
    CaptureSession::active(session_id(epoch), plan.target, plan.dispatch, stop)
}

fn live_session_with_epoch(epoch: u64) -> RunningSession {
    live_session_from_plan(epoch, plan())
}

fn draining_session_with_epoch(epoch: u64) -> RunningSession {
    let mut session = live_session_with_epoch(epoch);
    assert_eq!(
        session.reconcile_with(None, stop_for_target_change),
        ReconcileAction::Retiring
    );
    session
}

#[tokio::test(start_paused = true)]
async fn planned_done_allows_an_immediate_successor_but_unexpected_done_is_paced() {
    let now = Instant::now();
    let mut planned = GestureSlot::running(draining_session_with_epoch(7));
    let (_, planned_unexpected) = planned
        .complete(&session_id(7), None, Some(now + RETRY_DELAY))
        .expect("the tracked planned completion should settle its session");
    assert!(
        !planned_unexpected
            && planned
                .recovery()
                .is_some_and(|recovery| recovery.restart_at.is_none()),
        "ordered Done after planned retirement must reconcile its successor immediately"
    );

    let mut unexpected = GestureSlot::running(live_session_with_epoch(8));
    let (_, failed) = unexpected
        .complete(&session_id(8), None, Some(now + RETRY_DELAY))
        .expect("the tracked unexpected completion should settle its session");
    assert!(failed);
    assert_eq!(
        unexpected
            .recovery()
            .and_then(|recovery| recovery.restart_at),
        Some(now + RETRY_DELAY)
    );
}

#[tokio::test(start_paused = true)]
async fn retry_deadline_is_not_postponed_by_ready_input() {
    let retry_at = Instant::now() + RETRY_DELAY;
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let waiter = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = wait_for_deadline(Some(retry_at)) => return Instant::now(),
                Some(()) = input_rx.recv() => {}
            }
        }
    });

    for _ in 0..10 {
        input_tx
            .send(())
            .expect("the deadline waiter should remain alive");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    tokio::task::yield_now().await;

    assert_eq!(
        waiter.await.expect("deadline waiter should finish"),
        retry_at,
        "recreating an absolute sleep after unrelated input must not postpone it"
    );
}

#[test]
fn suspended_device_io_disables_retry_deadlines() {
    let retry_at = Instant::now() + RETRY_DELAY;
    let slots = HashMap::from([(
        physical_key(),
        GestureSlot::recovering(None, Some(retry_at)),
    )]);

    assert_eq!(
        next_deadline(ReceiverRequestState::default(), true, &slots),
        Some(retry_at),
    );
    assert_eq!(
        next_deadline(ReceiverRequestState::default(), false, &slots),
        None,
        "capture retries must stay dormant until visible resume",
    );
}

#[tokio::test(start_paused = true)]
async fn exclusive_receiver_access_disables_expired_recovery_deadlines() {
    let access = ReceiverAccess::default();
    let requests = access.subscribe_requests();
    let exclusive = access
        .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
        .await;
    let restart_at = Instant::now();
    let slots = HashMap::from([(
        physical_key(),
        GestureSlot::recovering(None, Some(restart_at)),
    )]);

    assert_eq!(next_deadline(*requests.borrow(), true, &slots), None);
    drop(exclusive);
    assert_eq!(
        next_deadline(*requests.borrow(), true, &slots),
        Some(restart_at),
        "release makes the retry actionable without adding another backoff"
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_manager_waits_for_control_events_and_shutdown_between_retries() {
    use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};
    use futures_lite::future::poll_once;

    for shutdown_requested in [false, true] {
        let (plans_tx, capture_plans) =
            watch::channel(Arc::new(CapturePublication::from_plans(vec![plan()])));
        let capture = CaptureChannelSlot::default();
        // A missing inventory channel makes the real session task fail without
        // opening hardware; ordered Done then drives normal restart recovery.
        let registry = openlogi_hid::ChannelRegistry::default();
        let access = ReceiverAccess::default();
        let (_signal, device_io) = openlogi_hid::device_io_channel();
        let (ring, _ring_rx) = mpsc::unbounded_channel();
        let device_access = DeviceAccess {
            channel: capture,
            registry,
            receiver_access: access.clone(),
            device_io,
            transport_turns: crate::hardware::TransportTurns::default(),
        };
        let mut actions =
            crate::runtime::ActionRuntime::new(Arc::default(), device_access.clone(), ring)
                .unwrap();
        let mut scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
            false,
            VerticalScrollSensitivity::default(),
        )))
        .unwrap();
        let (shutdown_tx, shutdown) = oneshot::channel();
        let mut manager = std::pin::pin!(manage(GestureManagerContext {
            capture_plans,
            receiver_requests: access.subscribe_requests(),
            access: device_access,
            outputs: GestureOutputs::new(
                actions.dispatcher(),
                scroll.input(),
                Arc::default(),
                detached_highlight(),
            ),
            shutdown,
        }));

        for _ in 0..3 {
            // Poll the actual manager and its detached forwarder/session tasks
            // through repeated failures. Each poll must return to event wait.
            for _ in 0..4 {
                assert!(poll_once(&mut manager).await.is_none());
                tokio::task::yield_now().await;
            }
            tokio::time::advance(RETRY_DELAY).await;
        }
        let before_event = Instant::now();
        if shutdown_requested {
            shutdown_tx.send(()).unwrap();
            assert!(matches!(manager.await, ManagerCompletion::Graceful));
        } else {
            drop(plans_tx);
            assert!(matches!(manager.await, ManagerCompletion::Unexpected));
        }
        assert_eq!(Instant::now(), before_event);
        scroll.shutdown();
        actions.shutdown();
    }
}

#[tokio::test]
async fn input_accepted_during_restoration_precedes_session_done() {
    let id = session_id(7);
    let key = physical_key();
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let (session_tx, session_rx) = mpsc::unbounded_channel();
    let listener_sink = session_tx.clone();
    drop(session_tx);
    let forward_task = spawn_input_forwarder(key.clone(), id.clone(), session_rx, events.clone());
    let completion = tokio::spawn(report_done_after_inputs(
        forward_task,
        events,
        SessionDone {
            physical_key: key.clone(),
            session: id.clone(),
            pending_restore: None,
        },
    ));
    let (restored_tx, restored_rx) = oneshot::channel();
    let listener = tokio::spawn(async move {
        listener_sink
            .send(CapturedInput::ButtonPulse(ButtonId::DpiToggle))
            .expect("the capture forwarder should remain open");
        let _ = restored_rx.await;
        drop(listener_sink);
    });

    match event_rx.recv().await {
        Some(SessionEvent::Input(input)) => {
            assert_eq!(input.physical_key, key);
            assert_eq!(input.session, id);
            assert_eq!(input.input, CapturedInput::ButtonPulse(ButtonId::DpiToggle));
        }
        _ => panic!("captured input must be forwarded first"),
    }
    assert!(
        event_rx.try_recv().is_err(),
        "Done must wait while restoration still holds the listener"
    );

    restored_tx.send(()).expect("restore signal should be open");
    listener.await.expect("listener task should finish");
    completion.await.expect("completion task should finish");
    match event_rx.recv().await {
        Some(SessionEvent::Done(done)) => assert_eq!(done.session, id),
        _ => panic!("Done must follow the last accepted input"),
    }
}

#[test]
fn accepts_inputs_from_the_current_session_until_teardown_finishes() {
    assert!(dispatch_context_for(&session_id(7), Some(&live_session_with_epoch(7))).is_some());
    assert!(
        dispatch_context_for(&session_id(6), Some(&live_session_with_epoch(7))).is_none(),
        "a superseded session's queued input is stale"
    );
    assert!(
        dispatch_context_for(&session_id(7), Some(&draining_session_with_epoch(7))).is_some(),
        "a draining session still owns diverted input until restoration completes"
    );
    assert!(dispatch_context_for(&session_id(7), None).is_none());
}

#[tokio::test]
async fn exclusive_request_retires_capture_without_rejecting_owned_input() {
    let access = ReceiverAccess::default();
    let session_lease = access
        .try_acquire_for_session()
        .expect("capture should acquire the receiver before pairing");
    let pairing = tokio::spawn({
        let access = access.clone();
        async move {
            access
                .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !access.exclusive_requested() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pairing should announce its exclusive request");

    let (plans, _) =
        tokio::sync::watch::channel(Arc::new(CapturePublication::from_plans(vec![plan()])));
    let plans = plans.subscribe();
    let requests = access.subscribe_requests();
    assert!(wanted_sessions(*requests.borrow(), &plans).is_empty());

    let mut session = live_session_with_epoch(7);
    assert_eq!(
        session.reconcile_with(None, stop_for_target_change),
        ReconcileAction::Retiring
    );
    assert!(!session.is_active());
    assert!(
        dispatch_context_for(&session_id(7), Some(&session)).is_some(),
        "an exclusive request retires capture but must not reject input while firmware remains diverted"
    );

    pairing.abort();
    let _ = pairing.await;
    drop(session_lease);
}

#[test]
fn an_active_session_refreshes_bindings_without_rearming_hardware() {
    let mut old_plan = plan();
    old_plan.dispatch.side_gesture_bindings.insert(
        ButtonId::Forward,
        [(GestureDirection::Click, Action::MissionControl)].into(),
    );
    old_plan
        .target
        .spec
        .divert_gesture_buttons
        .push((0x0056, ButtonId::Forward));
    let mut session = live_session_from_plan(7, old_plan.clone());

    let mut new_plan = old_plan;
    new_plan.dispatch.side_gesture_bindings.insert(
        ButtonId::Forward,
        [(GestureDirection::Click, Action::ShowDesktop)].into(),
    );
    assert_eq!(session.target(), &new_plan.target);

    assert_eq!(
        session.reconcile_with(
            Some((&new_plan.target, &new_plan.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged,
        "a hot plan refresh must cancel input lifecycles admitted under the old action map"
    );

    assert!(session.is_active());
    assert_eq!(
        session.dispatch().side_gesture_bindings[&ButtonId::Forward][&GestureDirection::Click],
        Action::ShowDesktop,
        "an unchanged capture target must still adopt the new app/profile action"
    );
}

#[test]
fn side_gesture_transition_keeps_the_retiring_plan_until_native_restore() {
    let mut old_plan = plan();
    old_plan.dispatch.side_gesture_bindings.insert(
        ButtonId::Forward,
        [(GestureDirection::Click, Action::MissionControl)].into(),
    );
    old_plan
        .target
        .spec
        .divert_gesture_buttons
        .push((0x0056, ButtonId::Forward));
    let mut session = live_session_from_plan(7, old_plan.clone());

    let mut published_without_hook = old_plan;
    published_without_hook
        .dispatch
        .side_gesture_bindings
        .clear();
    published_without_hook
        .target
        .spec
        .divert_gesture_buttons
        .clear();
    assert_ne!(session.target(), &published_without_hook.target);
    assert_eq!(
        session.reconcile_with(
            Some((
                &published_without_hook.target,
                &published_without_hook.dispatch,
            )),
            stop_for_target_change,
        ),
        ReconcileAction::Retiring
    );
    assert!(!session.is_active());

    let (_, retained) = dispatch_context_for(session.id(), Some(&session))
        .expect("the draining session must remain an admissible input owner");
    assert!(
        retained
            .side_gesture_bindings
            .contains_key(&ButtonId::Forward)
    );
    assert!(
        session
            .target()
            .spec
            .divert_gesture_buttons
            .iter()
            .any(|&(_, button)| button == ButtonId::Forward),
        "the retiring target must stay frozen while firmware diversion remains active"
    );
}

#[test]
fn capture_target_changes_schedule_the_old_session_for_retirement() {
    let session = live_session_with_epoch(7);
    let mut plan = plan_for_device(
        &openlogi_core::config::Config::default(),
        physical_key(),
        "mouse-a",
        session.target().route.clone(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    assert_eq!(session.target(), &plan.target);

    plan.target.rearm_generation = 1;
    assert_ne!(
        session.target(),
        &plan.target,
        "a capture-target epoch change must retire the old session"
    );
}

#[test]
fn receiver_route_change_requests_a_firmware_handoff() {
    let old_plan = plan();
    let mut new_plan = old_plan.clone();
    let successor_route = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    new_plan.target.route.clone_from(&successor_route);
    let (stop, mut stopped) = oneshot::channel();
    let mut session =
        CaptureSession::active(session_id(7), old_plan.target, old_plan.dispatch, stop);

    assert_eq!(
        session.reconcile_with(
            Some((&new_plan.target, &new_plan.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::Retiring
    );
    assert_eq!(
        stopped
            .try_recv()
            .expect("route change should stop the active session"),
        CaptureSessionStop::Handoff(successor_route),
        "teardown must restore through the receiver the mouse moved to"
    );
}

#[test]
fn same_route_rearm_shuts_down_instead_of_handing_off() {
    let old_plan = plan();
    let mut rearmed = old_plan.clone();
    rearmed.target.rearm_generation = old_plan.target.rearm_generation + 1;
    let (stop, mut stopped) = oneshot::channel();
    let mut session =
        CaptureSession::active(session_id(7), old_plan.target, old_plan.dispatch, stop);

    assert_eq!(
        session.reconcile_with(
            Some((&rearmed.target, &rearmed.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::Retiring
    );
    assert_eq!(
        stopped
            .try_recv()
            .expect("a reconnect rearm should stop the session"),
        CaptureSessionStop::Shutdown,
        "the same route restores through its current publication"
    );
}

#[test]
fn config_key_adoption_hot_refreshes_the_same_physical_capture_slot() {
    let old_plan = plan();
    let physical_key = old_plan.target.physical_key.clone();
    let mut adopted_plan = old_plan.clone();
    adopted_plan.dispatch.config_key = "unit:00000001".to_owned();
    assert_eq!(old_plan.target, adopted_plan.target);

    let mut sessions = HashMap::from([(physical_key.clone(), live_session_from_plan(7, old_plan))]);
    let wanted = HashMap::from([(physical_key.clone(), adopted_plan)]);
    let running = sessions
        .get_mut(&physical_key)
        .expect("the physical slot should already be occupied");
    let desired = wanted
        .get(&physical_key)
        .map(|plan| (&plan.target, &plan.dispatch));

    assert_eq!(
        running.reconcile_with(desired, stop_for_target_change),
        ReconcileAction::DispatchChanged
    );
    running.rekey(&wanted[&physical_key].dispatch.config_key);
    assert!(running.is_active());
    assert_eq!(running.id().device_key(), "unit:00000001");
    assert!(
        sessions.contains_key(&physical_key),
        "the old session must keep the one physical slot until ordered Done"
    );
}

#[test]
fn active_session_adopts_action_only_plan_changes_without_rearming() {
    let mut config = openlogi_core::config::Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Copy),
    );
    let first = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    let mut session = live_session_from_plan(7, first.clone());

    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Paste),
    );
    let rebound = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    assert_eq!(first.target, rebound.target);
    assert_eq!(
        session.reconcile_with(
            Some((&rebound.target, &rebound.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged
    );
    assert_eq!(
        session.dispatch().bindings.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn active_session_adopts_gesture_and_per_app_dispatch_changes() {
    let mut config = openlogi_core::config::Config::default();
    config.set_gesture_mode("mouse-a", ButtonId::GestureButton, true);
    let first = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    let mut session = live_session_from_plan(7, first.clone());

    config.set_gesture_direction(
        "mouse-a",
        ButtonId::GestureButton,
        GestureDirection::Right,
        Action::MissionControl,
    );
    let gestured = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    assert_eq!(first.target, gestured.target);
    assert_eq!(
        session.reconcile_with(
            Some((&gestured.target, &gestured.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged
    );
    assert_eq!(
        session
            .dispatch()
            .gesture_bindings
            .get(&ButtonId::GestureButton)
            .and_then(|map| map.get(&GestureDirection::Right)),
        Some(&Action::MissionControl)
    );

    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Copy),
    );
    let base = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    let mut session = live_session_from_plan(8, base.clone());
    config.set_per_app_binding(
        "mouse-a",
        "com.example.Editor",
        ButtonId::DpiToggle,
        Some(Action::Paste),
    );
    let per_app = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            app: Some("com.example.Editor"),
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    assert_eq!(base.target, per_app.target);
    assert_eq!(
        session.reconcile_with(
            Some((&per_app.target, &per_app.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged
    );
    assert_eq!(
        session.dispatch().bindings.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn wheel_configuration_changes_refresh_without_rearming_hardware() {
    let mut config = openlogi_core::config::Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::NextTab),
    );
    let first = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    let mut session = live_session_from_plan(7, first.clone());

    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::VolumeUp),
    );
    let rebound = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    assert_eq!(
        first.target, rebound.target,
        "both custom bindings require the same HID++ diversion"
    );
    assert_eq!(
        session.reconcile_with(
            Some((&rebound.target, &rebound.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged,
        "dispatch-only binding changes must not cycle firmware diversion"
    );
    assert!(session.is_active());

    config.set_device_thumbwheel_sensitivity("mouse-a", Some(ThumbwheelSensitivity::MIN));
    let rescaled = plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            ..Default::default()
        },
    );
    assert_eq!(rebound.target, rescaled.target);
    assert_eq!(
        session.reconcile_with(
            Some((&rescaled.target, &rescaled.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged,
        "an already-diverted wheel needs a state reset, not a hardware restart"
    );
    assert!(session.is_active());
}

#[tokio::test]
async fn pending_restore_follows_a_disabled_devices_latest_route() {
    use std::collections::BTreeMap;

    let stale = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    let latest = DeviceRoute::Bolt {
        receiver_uid: "receiver-c".to_owned(),
        slot: 1,
    };
    let registry = openlogi_hid::ChannelRegistry::default();
    let key = physical_key();
    let recovering = |route: DeviceRoute| {
        HashMap::from([(
            key.clone(),
            GestureSlot::recovering(
                Some(PendingRestore {
                    token: PendingCaptureRestore::detached(route),
                    retry_at: Instant::now(),
                }),
                None,
            ),
        )])
    };
    let route_of = |slots: &HashMap<PhysicalDeviceKey, GestureSlot>| {
        slots
            .get(&key)
            .and_then(GestureSlot::recovery)
            .and_then(|recovery| recovery.pending_restore.as_ref())
            .map(|pending| pending.token.route().clone())
    };

    let mut stuck = recovering(stale.clone());
    retry_pending_restores(&mut stuck, &registry, None, Instant::now()).await;
    assert_eq!(
        route_of(&stuck),
        Some(stale.clone()),
        "without a published route the owed restore stays on its last target"
    );

    let mut followed = recovering(stale);
    let publication =
        CapturePublication::new(Vec::new(), BTreeMap::from([(key.clone(), latest.clone())]));
    retry_pending_restores(&mut followed, &registry, Some(&publication), Instant::now()).await;
    assert!(
        publication.plans().is_empty(),
        "capture stays disarmed while the device is disabled"
    );
    assert_eq!(
        route_of(&followed),
        Some(latest),
        "restore must follow the mouse onto the receiver inventory names now"
    );
}

/// The action and scroll workers a dispatcher under test submits to; the
/// caller shuts them down.
fn input_runtimes() -> (
    crate::runtime::ActionRuntime,
    crate::runtime::scroll::ScrollRuntime,
) {
    use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};

    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (ring, _ring_rx) = mpsc::unbounded_channel();
    let device_access = DeviceAccess {
        channel: CaptureChannelSlot::default(),
        registry: openlogi_hid::ChannelRegistry::default(),
        receiver_access: ReceiverAccess::default(),
        device_io,
        transport_turns: crate::hardware::TransportTurns::default(),
    };
    let actions = crate::runtime::ActionRuntime::new(Arc::default(), device_access, ring).unwrap();
    let scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
        false,
        VerticalScrollSensitivity::default(),
    )))
    .unwrap();
    (actions, scroll)
}

#[tokio::test]
async fn a_pointer_only_plan_refresh_keeps_a_held_gesture() {
    let (mut actions, mut scroll) = input_runtimes();
    let mut dispatcher = InputDispatcher::new(GestureOutputs::new(
        actions.dispatcher(),
        scroll.input(),
        Arc::default(),
        detached_highlight(),
    ));

    let mut held = plan();
    held.dispatch.gesture_bindings.insert(
        ButtonId::GestureButton,
        [(GestureDirection::Left, Action::PreviousDesktop)].into(),
    );
    let mut session = live_session_from_plan(7, held.clone());
    let id = session.id().clone();
    dispatcher.dispatch(
        &id,
        session.dispatch(),
        CapturedInput::ButtonDown(ButtonId::GestureButton),
    );
    assert!(dispatcher.holds_gesture_press(&id, ButtonId::GestureButton));

    // The swipe switched desktops, leaving the pointer over another window
    // (#1634): the republished plan differs only in its pointer target.
    let mut moved = held;
    moved.dispatch.pointer_target = Some(openlogi_hook::PointerTarget::Desktop);
    reconcile_session(
        &mut session,
        Some((&moved.target, &moved.dispatch)),
        &mut dispatcher,
    );
    assert!(
        dispatcher.holds_gesture_press(&id, ButtonId::GestureButton),
        "a pointer-only refresh must not end the hold that caused it"
    );

    // A real binding change still ends every admitted press.
    let mut rebound = moved;
    rebound.dispatch.gesture_bindings.insert(
        ButtonId::GestureButton,
        [(GestureDirection::Left, Action::Copy)].into(),
    );
    reconcile_session(
        &mut session,
        Some((&rebound.target, &rebound.dispatch)),
        &mut dispatcher,
    );
    assert!(!dispatcher.holds_gesture_press(&id, ButtonId::GestureButton));

    scroll.shutdown();
    actions.shutdown();
}

#[tokio::test]
async fn a_plan_republish_with_other_bindings_keeps_a_held_highlight() {
    let mut rig = PresenterRig::new();
    let mut held = plan_with_presenter(&Config::default());
    let mut session = live_session_from_plan(1, held.clone());
    let id = session.id().clone();
    rig.dispatcher.dispatch(
        &id,
        session.dispatch(),
        CapturedInput::ButtonDown(ButtonId::PresenterTouch),
    );
    let began = rig.highlight.current().expect("touch begins a highlight");

    // The overlay's position reports moved the pointer onto an app with its
    // own bindings: the republished plan differs in more than its target.
    held.dispatch
        .bindings
        .insert(ButtonId::Back, Binding::Single(Action::Copy));
    reconcile_session(
        &mut session,
        Some((&held.target, &held.dispatch)),
        &mut rig.dispatcher,
    );
    assert_eq!(
        rig.highlight.current().map(|active| active.session_id),
        Some(began.session_id),
        "the republish must not end the held highlight"
    );

    rig.dispatcher.dispatch(
        &id,
        session.dispatch(),
        CapturedInput::ButtonUp(ButtonId::PresenterTouch),
    );
    assert_eq!(rig.highlight.current(), None);
    rig.shutdown();
}

/// The dispatch plan of a presenter under `config`: the device reports the
/// highlight controls, so its highlight-bound buttons are resolved.
fn presenter_dispatch(config: &Config) -> DispatchPlan {
    plan_with_presenter(config).dispatch
}

/// The capture plan of a presenter under `config`.
fn plan_with_presenter(config: &Config) -> DeviceCapturePlan {
    let capabilities = Capabilities {
        presenter_highlight: true,
        ..Capabilities::default()
    };
    plan_for_device_on(
        config,
        physical_key(),
        "mouse-a",
        route(),
        PlanContext {
            os_mouse_hook_available: true,
            capabilities: Some(&capabilities),
            ..Default::default()
        },
        true,
    )
}

struct PresenterRig {
    dispatcher: InputDispatcher,
    highlight: Arc<HighlightSession>,
    actions: crate::runtime::ActionRuntime,
    scroll: crate::runtime::scroll::ScrollRuntime,
}

impl PresenterRig {
    fn new() -> Self {
        let (actions, scroll) = input_runtimes();
        let highlight = detached_highlight();
        let outputs = GestureOutputs::new(
            actions.dispatcher(),
            scroll.input(),
            Arc::default(),
            Arc::clone(&highlight),
        );
        Self {
            dispatcher: InputDispatcher::new(outputs),
            highlight,
            actions,
            scroll,
        }
    }

    fn feed(&mut self, plan: &DispatchPlan, input: CapturedInput) {
        self.dispatcher.dispatch(&session_id(1), plan, input);
    }

    fn shutdown(mut self) {
        self.scroll.shutdown();
        self.actions.shutdown();
    }
}

#[tokio::test]
async fn presenter_holds_drive_the_highlight_session() {
    use openlogi_core::binding::HighlightEffect;

    let mut rig = PresenterRig::new();
    let plan = presenter_dispatch(&Config::default());
    let effect = |rig: &PresenterRig| rig.highlight.current().map(|active| active.effect);

    rig.feed(&plan, CapturedInput::Motion { dx: 5, dy: 5 });
    assert_eq!(rig.highlight.current(), None, "stray motion starts nothing");

    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterTouch));
    let began = rig.highlight.current().expect("touch begins a highlight");
    assert_eq!(began.effect, HighlightEffect::LaserDot);

    rig.feed(&plan, CapturedInput::Motion { dx: 3, dy: -4 });
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterPress));
    let pressed = rig.highlight.current().expect("still active");
    assert_eq!(pressed.effect, HighlightEffect::Spotlight);
    assert_eq!(pressed.session_id, began.session_id);
    assert_eq!((pressed.travel_x, pressed.travel_y), (3, -4));

    rig.feed(&plan, CapturedInput::ButtonUp(ButtonId::PresenterPress));
    assert_eq!(effect(&rig), Some(HighlightEffect::LaserDot));
    rig.feed(&plan, CapturedInput::ButtonUp(ButtonId::PresenterTouch));
    assert_eq!(rig.highlight.current(), None);

    rig.feed(&plan, CapturedInput::Motion { dx: 9, dy: 9 });
    assert_eq!(
        rig.highlight.current(),
        None,
        "motion after release is dropped"
    );
    rig.shutdown();
}

#[tokio::test]
async fn a_cancelled_session_ends_its_highlight() {
    let mut rig = PresenterRig::new();
    let plan = presenter_dispatch(&Config::default());
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterPress));
    assert!(rig.highlight.current().is_some());
    rig.dispatcher.cancel_session(&session_id(2));
    assert!(
        rig.highlight.current().is_some(),
        "another session's cancellation leaves it alone"
    );
    rig.dispatcher.cancel_session(&session_id(1));
    assert_eq!(rig.highlight.current(), None);
    rig.shutdown();
}

#[tokio::test]
async fn a_presenter_button_without_a_highlight_binding_takes_the_ordinary_path() {
    let mut rig = PresenterRig::new();
    let mut config = Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::PresenterPress,
        Binding::Single(Action::LeftClick),
    );
    let plan = presenter_dispatch(&config);
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterPress));
    rig.feed(&plan, CapturedInput::Motion { dx: 1, dy: 1 });
    assert_eq!(rig.highlight.current(), None);
    rig.feed(&plan, CapturedInput::ButtonUp(ButtonId::PresenterPress));
    assert_eq!(rig.highlight.current(), None);
    rig.shutdown();
}

#[tokio::test]
async fn a_force_swap_retargets_without_ending_the_session() {
    use openlogi_core::binding::HighlightEffect;

    let mut rig = PresenterRig::new();
    let plan = presenter_dispatch(&Config::default());
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterTouch));
    rig.feed(&plan, CapturedInput::Motion { dx: 2, dy: 3 });
    let began = rig.highlight.current().expect("active");
    // One report swapping the CID: down first, then up.
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterPress));
    rig.feed(&plan, CapturedInput::ButtonUp(ButtonId::PresenterTouch));
    let swapped = rig.highlight.current().expect("the swap keeps the session");
    assert_eq!(swapped.effect, HighlightEffect::Spotlight);
    assert_eq!(swapped.session_id, began.session_id);
    assert_eq!(swapped.origin, began.origin);
    assert_eq!((swapped.travel_x, swapped.travel_y), (2, 3));
    // And back.
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterTouch));
    rig.feed(&plan, CapturedInput::ButtonUp(ButtonId::PresenterPress));
    let back = rig.highlight.current().expect("still active");
    assert_eq!(back.effect, HighlightEffect::LaserDot);
    assert_eq!(back.session_id, began.session_id);
    rig.feed(&plan, CapturedInput::ButtonUp(ButtonId::PresenterTouch));
    assert_eq!(rig.highlight.current(), None);
    rig.shutdown();
}

/// The accumulator delivers a force swap as Down then Up; an Up first would
/// leave no hold, so the dispatcher ends the highlight and the Down starts a
/// new one rather than guessing at a report boundary it cannot see.
#[tokio::test]
async fn a_swap_delivered_up_first_restarts_the_highlight() {
    let mut rig = PresenterRig::new();
    let plan = presenter_dispatch(&Config::default());
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterTouch));
    let began = rig.highlight.current().expect("active");
    rig.feed(&plan, CapturedInput::ButtonUp(ButtonId::PresenterTouch));
    assert_eq!(rig.highlight.current(), None);
    rig.feed(&plan, CapturedInput::ButtonDown(ButtonId::PresenterPress));
    let restarted = rig.highlight.current().expect("a new highlight");
    assert_ne!(restarted.session_id, began.session_id);
    assert_eq!((restarted.travel_x, restarted.travel_y), (0, 0));
    rig.shutdown();
}
