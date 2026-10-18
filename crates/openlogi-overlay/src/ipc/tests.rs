use openlogi_ipc::RingObservation;

use super::*;

/// A client nobody serves: all a phase transition needs to stand for "an agent
/// answered", because an observe call is only sent once it is polled.
fn unserved_client() -> AgentClient {
    let (transport, _server) = tarpc::transport::channel::unbounded();
    AgentClient::new(tarpc::client::Config::default(), transport).spawn()
}

#[test]
fn the_first_failed_attempt_only_arms_the_give_up_clock() {
    let mut link = ObserveLink::<RingObservation>::default();
    let now = Instant::now();
    assert!(!link.connect_failed(now));
    assert!(matches!(
        link,
        ObserveLink::Reconnecting {
            unreachable_since: Some(armed)
        } if armed == now
    ));
}

#[test]
fn an_agent_that_stays_away_past_the_deadline_ends_the_overlay() {
    let start = Instant::now();
    let mut link = ObserveLink::<RingObservation>::default();
    assert!(!link.connect_failed(start));
    assert!(!link.connect_failed(start + GIVE_UP_AFTER / 2));
    assert!(link.connect_failed(start + GIVE_UP_AFTER));
}

#[tokio::test]
async fn an_agent_that_keeps_coming_back_never_accumulates_its_way_to_an_exit() {
    let start = Instant::now();
    let mut link = ObserveLink::<RingObservation>::default();
    // Each round the agent is gone for half the deadline, then answers —
    // which moves the clock out of the reconnecting phase. Five rounds is
    // two and a half deadlines in total, so a version that kept the clock
    // across transitions would have exited by the third.
    for round in 1..=5 {
        let gone = start + (GIVE_UP_AFTER / 2) * round;
        assert!(
            !link.connect_failed(gone),
            "a reachable agent must not inherit the previous outage's clock"
        );
        link.connected(Observer::action_ring(unserved_client()));
        link.disconnected();
    }
}

#[test]
fn activation_takes_priority_over_queued_hover_updates() {
    let hover = OverlayCommand::Hover {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let activation = OverlayCommand::Activate {
        session_id: 1,
        slot: ActionRingSlot::Right,
    };
    assert!(matches!(
        coalesce_command(hover, activation),
        OverlayCommand::Activate {
            slot: ActionRingSlot::Right,
            ..
        }
    ));
    assert!(matches!(
        coalesce_command(activation, hover),
        OverlayCommand::Activate { .. }
    ));
}

/// Rings open back to back — dismiss one, open the next — and the view
/// only emits a hover when the hovered slot *changes*. Swallowing the new
/// ring's first hover therefore loses it entirely for as long as the
/// pointer stays put: no hover buzz, and the agent believing nothing is
/// hovered.
#[test]
fn a_new_sessions_hover_survives_the_previous_sessions_dismissal() {
    let closing = OverlayCommand::Cancel { session_id: 1 };
    let hover = OverlayCommand::Hover {
        session_id: 2,
        slot: ActionRingSlot::Top,
    };

    assert!(matches!(
        coalesce_command(closing, hover),
        OverlayCommand::Hover { session_id: 2, .. }
    ));
}

#[tokio::test]
async fn newer_activation_supersedes_a_stale_retry_immediately() {
    let stale = OverlayCommand::Cancel { session_id: 1 };
    let replacement = OverlayCommand::Activate {
        session_id: 2,
        slot: ActionRingSlot::Right,
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(replacement).unwrap();

    let (pending, _) = tokio::time::timeout(
        Duration::from_millis(20),
        wait_for_retry(&mut rx, stale, Some(Instant::now() + DISPLAY_LIFETIME)),
    )
    .await
    .expect("queued replacement should interrupt the retry delay")
    .expect("replacement command should remain pending");

    assert_eq!(pending, replacement);
}

#[tokio::test]
async fn newer_activation_supersedes_a_stalled_terminal_request() {
    let stale = OverlayCommand::Cancel { session_id: 1 };
    let replacement = OverlayCommand::Activate {
        session_id: 2,
        slot: ActionRingSlot::Right,
    };
    let stale_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let replacement_sent = std::sync::Arc::new(tokio::sync::Notify::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let worker = tokio::spawn({
        let stale_started = std::sync::Arc::clone(&stale_started);
        let replacement_sent = std::sync::Arc::clone(&replacement_sent);
        async move {
            send_commands_with(
                &mut rx,
                || Box::pin(async { Some(()) }),
                move |(), command| {
                    let stale_started = std::sync::Arc::clone(&stale_started);
                    let replacement_sent = std::sync::Arc::clone(&replacement_sent);
                    Box::pin(async move {
                        if command == stale {
                            stale_started.notify_one();
                            std::future::pending().await
                        } else {
                            replacement_sent.notify_one();
                            true
                        }
                    })
                },
            )
            .await;
        }
    });

    tx.send(stale).unwrap();
    tokio::time::timeout(Duration::from_millis(100), stale_started.notified())
        .await
        .expect("stale request should start");
    tx.send(replacement).unwrap();
    tokio::time::timeout(Duration::from_millis(100), replacement_sent.notified())
        .await
        .expect("replacement should cancel the stalled request");
    drop(tx);
    tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .expect("command worker should stop")
        .expect("command worker should not panic");
}

#[tokio::test]
async fn stalled_hover_stops_when_the_command_channel_closes() {
    let hover = OverlayCommand::Hover {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let request_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let worker = tokio::spawn({
        let request_started = std::sync::Arc::clone(&request_started);
        async move {
            send_commands_with(
                &mut rx,
                || Box::pin(async { Some(()) }),
                move |(), _| {
                    let request_started = std::sync::Arc::clone(&request_started);
                    Box::pin(async move {
                        request_started.notify_one();
                        std::future::pending().await
                    })
                },
            )
            .await;
        }
    });

    tx.send(hover).unwrap();
    tokio::time::timeout(Duration::from_millis(100), request_started.notified())
        .await
        .expect("hover request should start");
    drop(tx);
    tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .expect("closing the channel should stop the command worker")
        .expect("command worker should not panic");
}

#[test]
fn only_terminal_commands_are_retryable() {
    let hover = OverlayCommand::Hover {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let activation = OverlayCommand::Activate {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let cancellation = OverlayCommand::Cancel { session_id: 1 };
    let moved = OverlayCommand::HighlightMoved {
        session_id: 1,
        x: 0.0,
        y: 0.0,
    };
    assert!(!hover.is_terminal());
    assert!(!moved.is_terminal(), "a move is never retried");
    assert!(activation.is_terminal());
    assert!(cancellation.is_terminal());
}

#[test]
fn terminal_retries_last_only_until_the_session_deadline() {
    assert!(retry_before(Some(Instant::now() + Duration::from_secs(1))));
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    assert!(!retry_before(Some(past)));
    assert!(!retry_before(None));
}

#[test]
fn losing_the_agent_hides_the_backlight_indicator() {
    assert!(!agent_lost().visible);
}

#[test]
fn a_newer_move_supersedes_an_older_one_but_never_a_release() {
    let moved = |x| OverlayCommand::HighlightMoved {
        session_id: 4,
        x,
        y: 0.0,
    };
    assert_eq!(coalesce_command(moved(1.0), moved(2.0)), moved(2.0));
    let release = OverlayCommand::HighlightReleased {
        session_id: 4,
        x: 1.0,
        y: 0.0,
    };
    assert_eq!(
        coalesce_command(release, moved(2.0)),
        release,
        "a late move must not displace the release still being delivered"
    );
    assert_eq!(coalesce_command(moved(1.0), release), release);
}

#[test]
fn ring_and_highlight_commands_belong_to_separate_families() {
    let hover = OverlayCommand::Hover {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let release = OverlayCommand::HighlightReleased {
        session_id: 1,
        x: 0.0,
        y: 0.0,
    };
    let moved = OverlayCommand::HighlightMoved {
        session_id: 1,
        x: 0.0,
        y: 0.0,
    };
    assert_eq!(hover.family(), Family::Ring);
    assert_eq!(
        OverlayCommand::Cancel { session_id: 1 }.family(),
        Family::Ring
    );
    assert_eq!(release.family(), Family::Highlight);
    assert_eq!(moved.family(), Family::Highlight);
}

/// Deliver `commands` through a worker whose requests are recorded; a
/// request for a command in `stalled` never completes.
async fn deliver_by_family(
    commands: &[OverlayCommand],
    stalled: &[OverlayCommand],
) -> Vec<OverlayCommand> {
    let started = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let worker = tokio::spawn({
        let started = std::sync::Arc::clone(&started);
        let stalled = stalled.to_vec();
        async move {
            send_commands_by_family(
                &mut rx,
                || Box::pin(async { Some(()) }),
                move |(), command| {
                    let started = std::sync::Arc::clone(&started);
                    let stall = stalled.contains(&command);
                    Box::pin(async move {
                        started.lock().unwrap().push(command);
                        if stall {
                            std::future::pending::<()>().await;
                        }
                        true
                    })
                },
            )
            .await;
        }
    });
    for command in commands {
        tx.send(*command).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(tx);
    tokio::time::timeout(Duration::from_millis(200), worker)
        .await
        .expect("closing the channel should stop both workers")
        .expect("command worker should not panic");
    started.lock().unwrap().clone()
}

#[tokio::test]
async fn a_ring_command_never_displaces_a_highlight_release_being_delivered() {
    let release = OverlayCommand::HighlightReleased {
        session_id: 1,
        x: 1.0,
        y: 2.0,
    };
    let hover = OverlayCommand::Hover {
        session_id: 2,
        slot: ActionRingSlot::Top,
    };
    // The release stalls (an agent that is briefly slow); a ring hover sent
    // meanwhile must neither replace it nor wait behind it.
    let started = deliver_by_family(&[release, hover], &[release]).await;
    assert_eq!(started, [release, hover]);
}

#[tokio::test]
async fn a_highlight_release_never_displaces_a_ring_activation_being_delivered() {
    let activation = OverlayCommand::Activate {
        session_id: 1,
        slot: ActionRingSlot::Right,
    };
    let release = OverlayCommand::HighlightReleased {
        session_id: 2,
        x: 1.0,
        y: 2.0,
    };
    let started = deliver_by_family(&[activation, release], &[activation]).await;
    assert_eq!(
        started,
        [activation, release],
        "the activation was neither replaced nor retried away"
    );
}

#[test]
fn a_move_never_displaces_a_pending_release_but_a_release_displaces_a_move() {
    let moved = OverlayCommand::HighlightMoved {
        session_id: 4,
        x: 1.0,
        y: 0.0,
    };
    let release = OverlayCommand::HighlightReleased {
        session_id: 4,
        x: 1.0,
        y: 0.0,
    };
    assert_eq!(coalesce_command(release, moved), release);
    assert_eq!(coalesce_command(moved, release), release);
    assert!(!moved.is_terminal(), "so a move is dropped, never retried");
    assert!(release.is_terminal());
}
