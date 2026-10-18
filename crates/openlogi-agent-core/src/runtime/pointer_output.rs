//! OS pointer output of the presenter highlight, owned by one dedicated worker.
//!
//! The highlight session and the IPC handlers submit typed requests through
//! [`PointerOutput`] without blocking; the worker alone calls the OS. It
//! applies them in submission order, coalesces a backlog of moves to the
//! newest position, and decides "is this session still current" at execution
//! time, from the begin/end requests it has already applied. That ordering is
//! what lets a late release report or a stale move be dropped without a
//! check-then-act race against the session that ended or replaced it.
//!
//! What the pointer does is the session's [`HighlightPointer`]. `Hide` is pure
//! visualisation: the pointer is hidden on the first move the overlay reports
//! for the session, not when the session begins (the report proves a dot is on
//! screen, so a missing or failed overlay never leaves an invisible pointer
//! with nothing drawn in its place) and is never moved. When the session ends
//! it stays hidden until the physical mouse moves: the worker then polls the
//! cursor position and shows the pointer once it is `MOUSE_MOVE_THRESHOLD`
//! away from where it was at the end. `Follow` moves the pointer with every
//! reported position and parks it under the dot on release. Whatever the
//! worker has hidden is shown again when it stops, normally or by unwinding.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use openlogi_core::config::HighlightPointer;
use tracing::{debug, warn};

/// Bounds graceful process shutdown if platform injection stops returning.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// How often the worker reads the cursor position while it waits for the
/// mouse to move.
const MOUSE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How far, in points, the cursor must be from where it was when a `Hide`
/// session ended for the pointer to count as moved by the mouse.
const MOUSE_MOVE_THRESHOLD: f64 = 2.0;

/// A global cursor position, `(x, y)` in top-left-origin points.
type Position = (f64, f64);

/// One pointer request of a highlight session, in the order the session
/// published it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PointerRequest {
    /// Session `session_id` began; `pointer` is what happens to the visible
    /// pointer meanwhile, from the session's first reported move on.
    Begin {
        session_id: u64,
        pointer: HighlightPointer,
    },
    /// The active session ended; show the pointer. Ignored when no session is
    /// active.
    End,
    /// The overlay reports session `session_id` has its dot at `(x, y)`.
    Moved { session_id: u64, x: f64, y: f64 },
    /// The overlay reports session `session_id` ended with its dot at `(x, y)`.
    Released { session_id: u64, x: f64, y: f64 },
}

/// What the OS should do for one applied request.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PointerEffect {
    HidePointer,
    ShowPointer,
    MoveTo(f64, f64),
}

/// The effects of one applied request, in order.
type Effects = [Option<PointerEffect>; 2];

/// The worker's view of the highlight sessions, advanced one request at a
/// time. Pure, so the admission rules are testable without a thread.
#[derive(Default)]
struct PointerState {
    /// The session that is active after the requests applied so far, with
    /// what it does to the visible pointer.
    active: Option<(u64, HighlightPointer)>,
    /// Whether the worker has hidden the pointer and not shown it since.
    hidden: bool,
    /// The session most recently ended or replaced, with what it did to the
    /// visible pointer; `None` before any. Only its release report may move
    /// the pointer, and only when it was a `Follow` session.
    last_ended: Option<(u64, HighlightPointer)>,
    /// Where the cursor was when a `Hide` session ended with the pointer
    /// hidden: it stays hidden until the cursor leaves that spot. `hidden`
    /// stays set meanwhile.
    awaiting_mouse: Option<Position>,
}

impl PointerState {
    /// Whether the pointer is hidden until the mouse moves, so the worker
    /// should [`poll`](Self::poll).
    fn is_awaiting_mouse(&self) -> bool {
        self.awaiting_mouse.is_some()
    }

    /// Apply `request`. `cursor` reads the cursor position, and only when a
    /// hidden pointer's session ends; without a position the pointer is shown
    /// at once, as no movement could ever be noticed.
    fn apply(
        &mut self,
        request: PointerRequest,
        cursor: impl FnOnce() -> Option<Position>,
    ) -> Effects {
        match request {
            PointerRequest::Begin {
                session_id,
                pointer,
            } => {
                // A session is about to hold the pointer; the mouse is not
                // watched until it ends.
                self.awaiting_mouse = None;
                if let Some(replaced) = self.active.replace((session_id, pointer)) {
                    self.last_ended = Some(replaced);
                }
                // A replaced session's hidden pointer stays hidden for a
                // Hide successor and comes back for a Follow one.
                if self.hidden && pointer == HighlightPointer::Follow {
                    self.hidden = false;
                    [Some(PointerEffect::ShowPointer), None]
                } else {
                    [None, None]
                }
            }
            PointerRequest::End => {
                let Some(ended) = self.active.take() else {
                    return [None, None];
                };
                self.last_ended = Some(ended);
                if self.hidden
                    && let Some(at) = cursor()
                {
                    self.awaiting_mouse = Some(at);
                    return [None, None];
                }
                self.hidden = false;
                [Some(PointerEffect::ShowPointer), None]
            }
            // A move belongs to the session active right now; a late report
            // from one that ended is the release's business.
            PointerRequest::Moved { session_id, x, y } => match self.active {
                Some((active, HighlightPointer::Hide)) if active == session_id => {
                    let hide = !self.hidden;
                    self.hidden = true;
                    [hide.then_some(PointerEffect::HidePointer), None]
                }
                Some((active, HighlightPointer::Follow)) if active == session_id => {
                    [Some(PointerEffect::MoveTo(x, y)), None]
                }
                _ => Self::ignored(session_id),
            },
            // A release belongs to the session that ended last, and only while
            // no newer press is active, so it cannot yank a pointer the user
            // has since taken over.
            PointerRequest::Released { session_id, x, y } => match self.last_ended {
                Some((ended, pointer)) if self.active.is_none() && ended == session_id => {
                    match pointer {
                        HighlightPointer::Follow => [Some(PointerEffect::MoveTo(x, y)), None],
                        HighlightPointer::Hide => [None, None],
                    }
                }
                _ => Self::ignored(session_id),
            },
        }
    }

    /// Show the pointer if the mouse has moved since a `Hide` session ended,
    /// given the cursor position now (`None` when unknown, which counts as no
    /// movement).
    fn poll(&mut self, cursor: Option<Position>) -> Effects {
        let (Some(from), Some(now)) = (self.awaiting_mouse, cursor) else {
            return [None, None];
        };
        if (now.0 - from.0).hypot(now.1 - from.1) < MOUSE_MOVE_THRESHOLD {
            return [None, None];
        }
        self.awaiting_mouse = None;
        self.hidden = false;
        [Some(PointerEffect::ShowPointer), None]
    }

    fn ignored(session_id: u64) -> Effects {
        debug!(
            session_id,
            "pointer report ignored: its session is not current"
        );
        [None, None]
    }
}

/// What travels to the worker: a session request, or the order to stop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PointerCommand {
    Request(PointerRequest),
    Shutdown,
}

/// Cloneable, non-blocking capability to enqueue pointer requests.
#[derive(Clone)]
pub struct PointerOutput {
    /// `None` for a handle nobody serves.
    commands: Option<mpsc::Sender<PointerCommand>>,
    /// Whether a request found the worker gone, which is warned about once
    /// for all clones.
    gone_warned: Arc<AtomicBool>,
}

impl PointerOutput {
    /// A handle with no worker behind it: every request is dropped, so it
    /// never touches the OS pointer. For the mock agent and unit tests.
    #[must_use]
    pub fn detached() -> Self {
        Self {
            commands: None,
            gone_warned: Arc::default(),
        }
    }

    /// Queue `request`; dropped once the worker has shut down, with one
    /// warning in case it died instead of being asked to stop.
    pub(crate) fn submit(&self, request: PointerRequest) {
        if let Some(commands) = &self.commands
            && commands.send(PointerCommand::Request(request)).is_err()
            && !self.gone_warned.swap(true, Ordering::Relaxed)
        {
            warn!("pointer worker is gone; highlight pointer requests are dropped");
        }
    }

    /// A handle whose requests the test reads from the returned receiver.
    #[cfg(test)]
    pub(crate) fn recording() -> (Self, mpsc::Receiver<PointerCommand>) {
        let (commands, recorded) = mpsc::channel();
        (
            Self {
                commands: Some(commands),
                gone_warned: Arc::default(),
            },
            recorded,
        )
    }
}

/// The requests a [`recording`](PointerOutput::recording) handle has received.
#[cfg(test)]
pub(crate) fn recorded_requests(recorded: &mpsc::Receiver<PointerCommand>) -> Vec<PointerRequest> {
    recorded
        .try_iter()
        .filter_map(|command| match command {
            PointerCommand::Request(request) => Some(request),
            PointerCommand::Shutdown => None,
        })
        .collect()
}

/// Unique owner of the pointer worker and its graceful shutdown.
pub struct PointerRuntime {
    output: PointerOutput,
    done: mpsc::Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl PointerRuntime {
    /// Start the worker driving the real OS pointer.
    pub fn spawn() -> io::Result<Self> {
        Self::spawn_with(
            openlogi_inject::move_pointer_to,
            openlogi_inject::hide_pointer,
            openlogi_inject::show_pointer,
            || openlogi_hook::cursor_position().map(|cursor| (cursor.x, cursor.y)),
        )
    }

    fn spawn_with(
        mut move_to: impl FnMut(f64, f64) + Send + 'static,
        mut hide: impl FnMut() + Send + 'static,
        show: impl FnMut() + Send + 'static,
        mut cursor: impl FnMut() -> Option<Position> + Send + 'static,
    ) -> io::Result<Self> {
        let (commands, command_rx) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("openlogi-pointer".into())
            .spawn(move || {
                let mut show = ShowIfUnwound(show);
                run_worker(
                    &command_rx,
                    &mut move_to,
                    &mut hide,
                    &mut show.0,
                    &mut cursor,
                );
                let _ = finished.send(());
            })?;
        Ok(Self {
            output: PointerOutput {
                commands: Some(commands),
                gone_warned: Arc::default(),
            },
            done,
            worker: Some(worker),
        })
    }

    /// The capability that feeds this worker.
    #[must_use]
    pub fn output(&self) -> PointerOutput {
        self.output.clone()
    }

    /// Apply the requests already queued, show the pointer, and join the
    /// worker. Requests submitted afterwards are dropped.
    pub fn shutdown(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        if let Some(commands) = &self.output.commands {
            let _ = commands.send(PointerCommand::Shutdown);
        }
        if self.done.recv_timeout(SHUTDOWN_TIMEOUT).is_err() {
            warn!("pointer worker did not shut down before the deadline");
        } else if worker.join().is_err() {
            warn!("pointer worker panicked during shutdown");
        }
    }
}

impl Drop for PointerRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Shows the pointer when the worker unwinds, so a panic in an OS call cannot
/// leave it hidden for the rest of the process. A normal exit shows it itself.
struct ShowIfUnwound<F: FnMut()>(F);

impl<F: FnMut()> Drop for ShowIfUnwound<F> {
    fn drop(&mut self) {
        if thread::panicking() {
            (self.0)();
        }
    }
}

/// Collapse each run of consecutive moves of one session to the newest, so a
/// backlog never reorders anything around a begin, end or release.
fn coalesce(commands: impl IntoIterator<Item = PointerCommand>) -> Vec<PointerCommand> {
    let mut kept: Vec<PointerCommand> = Vec::new();
    for command in commands {
        if let (
            Some(PointerCommand::Request(PointerRequest::Moved {
                session_id: queued, ..
            })),
            PointerCommand::Request(PointerRequest::Moved { session_id, .. }),
        ) = (kept.last(), command)
            && *queued == session_id
        {
            kept.pop();
        }
        kept.push(command);
    }
    kept
}

fn run_effects(
    effects: Effects,
    move_to: &mut impl FnMut(f64, f64),
    hide: &mut impl FnMut(),
    show: &mut impl FnMut(),
) {
    for effect in effects.into_iter().flatten() {
        match effect {
            PointerEffect::HidePointer => hide(),
            PointerEffect::ShowPointer => show(),
            PointerEffect::MoveTo(x, y) => move_to(x, y),
        }
    }
}

fn run_worker(
    commands: &mpsc::Receiver<PointerCommand>,
    move_to: &mut impl FnMut(f64, f64),
    hide: &mut impl FnMut(),
    show: &mut impl FnMut(),
    cursor: &mut impl FnMut() -> Option<Position>,
) {
    let mut state = PointerState::default();
    loop {
        // Block while idle; wake up to poll the cursor only while the pointer
        // waits for the mouse.
        let received = if state.is_awaiting_mouse() {
            commands.recv_timeout(MOUSE_POLL_INTERVAL)
        } else {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        let first = match received {
            Ok(first) => first,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                run_effects(state.poll(cursor()), move_to, hide, show);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        for command in coalesce(std::iter::once(first).chain(commands.try_iter())) {
            let PointerCommand::Request(request) = command else {
                // A pointer still hidden at shutdown must not stay hidden after us.
                show();
                return;
            };
            run_effects(state.apply(request, &mut *cursor), move_to, hide, show);
        }
    }
    show();
}

#[cfg(test)]
mod tests {
    use super::*;

    use PointerEffect::{HidePointer, MoveTo, ShowPointer};

    fn begin(session_id: u64, pointer: HighlightPointer) -> PointerRequest {
        PointerRequest::Begin {
            session_id,
            pointer,
        }
    }

    fn moved(session_id: u64, x: f64) -> PointerRequest {
        PointerRequest::Moved {
            session_id,
            x,
            y: 0.0,
        }
    }

    fn released(session_id: u64, x: f64) -> PointerRequest {
        PointerRequest::Released {
            session_id,
            x,
            y: 0.0,
        }
    }

    const HIDE: HighlightPointer = HighlightPointer::Hide;
    const FOLLOW: HighlightPointer = HighlightPointer::Follow;
    const END: PointerRequest = PointerRequest::End;

    fn effects(requests: &[PointerRequest]) -> Vec<PointerEffect> {
        run(requests).1
    }

    const AT_REST: Option<Position> = Some((100.0, 100.0));

    /// Run `requests` and return the state with the effects they had.
    fn run(requests: &[PointerRequest]) -> (PointerState, Vec<PointerEffect>) {
        let mut state = PointerState::default();
        let effects = requests
            .iter()
            .flat_map(|&request| state.apply(request, || AT_REST).into_iter().flatten())
            .collect();
        (state, effects)
    }

    #[test]
    fn the_pointer_is_hidden_by_the_first_move_not_the_begin() {
        assert_eq!(
            effects(&[begin(1, HIDE)]),
            [],
            "no dot has been reported, so nothing is hidden"
        );
        assert_eq!(
            effects(&[begin(1, HIDE), moved(1, 1.0), moved(1, 2.0)]),
            [HidePointer],
            "hidden once"
        );
        assert_eq!(
            effects(&[begin(1, FOLLOW), moved(1, 1.0)]),
            [MoveTo(1.0, 0.0)],
            "a Follow session never hides"
        );
    }

    #[test]
    fn a_hide_session_never_moves_the_pointer() {
        assert_eq!(
            effects(&[
                begin(1, HIDE),
                moved(1, 1.0),
                moved(1, 2.0),
                END,
                released(1, 3.0)
            ]),
            [HidePointer],
            "reports are ignored and the pointer stays hidden until the mouse moves"
        );
    }

    #[test]
    fn hide_and_show_follow_the_session_in_order() {
        assert_eq!(
            effects(&[begin(1, HIDE), moved(1, 1.0), END, END]),
            [HidePointer],
            "the pointer waits for the mouse, and a second end has nothing to end"
        );
        assert_eq!(
            effects(&[begin(1, HIDE), moved(1, 1.0), begin(2, FOLLOW)]),
            [HidePointer, ShowPointer],
            "a replacing Follow session shows the pointer"
        );
        assert_eq!(
            effects(&[begin(1, HIDE), moved(1, 1.0), begin(2, HIDE), moved(2, 2.0)]),
            [HidePointer],
            "a replacing Hide session keeps it hidden"
        );
        assert_eq!(
            effects(&[begin(1, HIDE), END]),
            [ShowPointer],
            "ending a session that never reported shows harmlessly"
        );
    }

    #[test]
    fn a_hidden_pointer_waits_for_the_mouse_to_move() {
        let (mut state, effects) = run(&[begin(1, HIDE), moved(1, 1.0), END]);
        assert_eq!(effects, [HidePointer]);
        assert!(state.is_awaiting_mouse());
        assert_eq!(
            state.poll(None),
            [None, None],
            "an unknown position is no movement"
        );
        assert_eq!(state.poll(AT_REST), [None, None]);
        assert_eq!(
            state.poll(Some((101.0, 101.0))),
            [None, None],
            "under 2 points"
        );
        assert!(state.is_awaiting_mouse());
        assert_eq!(state.poll(Some((102.0, 100.0))), [Some(ShowPointer), None]);
        assert!(!state.is_awaiting_mouse());
        assert_eq!(state.poll(Some((500.0, 500.0))), [None, None], "idle again");
    }

    #[test]
    fn a_hide_session_without_a_readable_cursor_shows_at_its_end() {
        let mut state = PointerState::default();
        state.apply(begin(1, HIDE), || None);
        state.apply(moved(1, 1.0), || None);
        assert_eq!(state.apply(END, || None), [Some(ShowPointer), None]);
        assert!(!state.is_awaiting_mouse());
    }

    #[test]
    fn a_session_that_begins_while_awaiting_the_mouse() {
        let (mut state, _) = run(&[begin(1, HIDE), moved(1, 1.0), END, begin(2, HIDE)]);
        assert!(
            !state.is_awaiting_mouse(),
            "a session holds the pointer now"
        );
        assert_eq!(state.poll(Some((900.0, 900.0))), [None, None]);
        assert_eq!(
            state.apply(moved(2, 1.0), || AT_REST),
            [None, None],
            "still hidden: no second hide"
        );
        assert_eq!(state.apply(END, || AT_REST), [None, None]);
        assert!(state.is_awaiting_mouse(), "and waits again at its end");

        assert_eq!(
            effects(&[begin(1, HIDE), moved(1, 1.0), END, begin(2, FOLLOW)]),
            [HidePointer, ShowPointer],
            "a Follow session shows the pointer it is about to move"
        );
        let (state, _) = run(&[begin(1, HIDE), moved(1, 1.0), END, begin(2, FOLLOW)]);
        assert!(!state.is_awaiting_mouse());
    }

    #[test]
    fn a_move_applies_only_to_the_active_session() {
        assert_eq!(
            effects(&[
                moved(1, 1.0),
                begin(1, FOLLOW),
                moved(1, 2.0),
                moved(2, 3.0),
                END,
                moved(1, 4.0),
                begin(2, FOLLOW),
                moved(1, 5.0),
                moved(2, 6.0),
            ]),
            [MoveTo(2.0, 0.0), ShowPointer, MoveTo(6.0, 0.0)],
            "before the begin, for another id, after the end and after a replacement: dropped"
        );
    }

    #[test]
    fn only_the_follow_session_that_ended_last_may_report_its_release() {
        assert_eq!(
            effects(&[released(0, 1.0), released(1, 1.0)]),
            [],
            "nothing has ended yet"
        );
        assert_eq!(
            effects(&[begin(1, FOLLOW), released(1, 1.0), END, released(1, 2.0)]),
            [ShowPointer, MoveTo(2.0, 0.0)],
            "a held session has not released"
        );
        assert_eq!(
            effects(&[begin(1, FOLLOW), END, begin(2, FOLLOW), released(1, 3.0)]),
            [ShowPointer],
            "a new press is active"
        );
        assert_eq!(
            effects(&[
                begin(1, FOLLOW),
                END,
                begin(2, FOLLOW),
                END,
                released(1, 3.0)
            ]),
            [ShowPointer, ShowPointer],
            "a stale id"
        );
        assert_eq!(
            effects(&[begin(1, FOLLOW), begin(2, FOLLOW), END, released(1, 3.0)]),
            [ShowPointer],
            "a replaced session is not the one that ended last"
        );
        assert_eq!(
            effects(&[begin(1, FOLLOW), begin(2, FOLLOW), END, released(2, 3.0)]),
            [ShowPointer, MoveTo(3.0, 0.0)]
        );
        assert_eq!(
            effects(&[begin(1, FOLLOW), begin(2, HIDE), END, released(2, 3.0)]),
            [ShowPointer],
            "the replacing session was a Hide one"
        );
    }

    #[test]
    fn a_backlog_of_moves_collapses_to_the_newest_without_crossing_a_boundary() {
        let commands = [
            begin(1, HIDE),
            moved(1, 1.0),
            moved(1, 2.0),
            moved(1, 3.0),
            END,
            moved(1, 4.0),
            moved(2, 5.0),
            moved(2, 6.0),
        ]
        .map(PointerCommand::Request);
        assert_eq!(
            coalesce(commands),
            [
                begin(1, HIDE),
                moved(1, 3.0),
                END,
                moved(1, 4.0),
                moved(2, 6.0)
            ]
            .map(PointerCommand::Request)
        );
    }

    #[test]
    fn a_detached_handle_drops_requests() {
        let (recording, recorded) = PointerOutput::recording();
        recording.submit(begin(1, HIDE));
        assert_eq!(recorded_requests(&recorded), [begin(1, HIDE)]);
        PointerOutput::detached().submit(begin(1, HIDE));
    }

    #[test]
    fn the_worker_applies_requests_and_shows_the_pointer_at_shutdown() {
        let (calls, observed) = mpsc::channel();
        let (move_calls, hide_calls, show_calls) = (calls.clone(), calls.clone(), calls);
        let mut runtime = PointerRuntime::spawn_with(
            move |x, _| {
                move_calls.send(format!("move {x}")).expect("test receiver");
            },
            move || hide_calls.send("hide".into()).expect("test receiver"),
            move || show_calls.send("show".into()).expect("test receiver"),
            || None,
        )
        .expect("worker spawns");
        let output = runtime.output();
        output.submit(begin(1, HIDE));
        output.submit(moved(1, 7.0));
        output.submit(begin(2, FOLLOW));
        output.submit(moved(2, 9.0));
        output.submit(moved(3, 8.0));
        runtime.shutdown();
        let calls: Vec<String> = observed.try_iter().collect();
        assert_eq!(
            calls,
            ["hide", "show", "move 9", "show"],
            "Hide only hides, the stale move was dropped and a held hide is undone"
        );
        output.submit(begin(2, HIDE));
        assert!(
            observed.try_recv().is_err(),
            "a stopped runtime accepts nothing"
        );
    }

    #[test]
    fn a_worker_that_unwinds_shows_the_pointer() {
        let (calls, observed) = mpsc::channel();
        let show_calls = calls.clone();
        let mut runtime = PointerRuntime::spawn_with(
            |_, _| {},
            move || {
                calls.send("hide").expect("test receiver");
                panic!("the OS call failed");
            },
            move || show_calls.send("show").expect("test receiver"),
            || None,
        )
        .expect("worker spawns");
        let output = runtime.output();
        output.submit(begin(1, HIDE));
        output.submit(moved(1, 7.0));
        // The unwound worker drops its receiver, closing the channel.
        while output.commands.as_ref().is_some_and(|commands| {
            commands
                .send(PointerCommand::Request(begin(2, FOLLOW)))
                .is_ok()
        }) {
            thread::yield_now();
        }
        let calls: Vec<&str> = observed.try_iter().collect();
        assert_eq!(calls, ["hide", "show"], "the hidden pointer came back");
        output.submit(begin(3, HIDE));
        assert!(output.gone_warned.load(Ordering::Relaxed));
        runtime.shutdown();
    }

    #[test]
    fn the_worker_shows_the_pointer_once_the_mouse_moves() {
        let (calls, observed) = mpsc::channel();
        let (hide_calls, show_calls) = (calls.clone(), calls);
        let cursor = Arc::new(std::sync::Mutex::new((10.0, 10.0)));
        let reading = Arc::clone(&cursor);
        let mut runtime = PointerRuntime::spawn_with(
            |_, _| {},
            move || hide_calls.send("hide").expect("test receiver"),
            move || show_calls.send("show").expect("test receiver"),
            move || Some(*reading.lock().expect("test cursor")),
        )
        .expect("worker spawns");
        let output = runtime.output();
        output.submit(begin(1, HIDE));
        output.submit(moved(1, 7.0));
        output.submit(END);
        let wait = Duration::from_secs(5);
        assert_eq!(observed.recv_timeout(wait), Ok("hide"));
        assert!(
            observed.recv_timeout(3 * MOUSE_POLL_INTERVAL).is_err(),
            "a still mouse leaves the pointer hidden"
        );
        *cursor.lock().expect("test cursor") = (50.0, 10.0);
        assert_eq!(observed.recv_timeout(wait), Ok("show"));
        runtime.shutdown();
    }

    #[test]
    fn shutdown_while_awaiting_the_mouse_shows_the_pointer() {
        let (calls, observed) = mpsc::channel();
        let (hide_calls, show_calls) = (calls.clone(), calls);
        let mut runtime = PointerRuntime::spawn_with(
            |_, _| {},
            move || hide_calls.send("hide").expect("test receiver"),
            move || show_calls.send("show").expect("test receiver"),
            || Some((10.0, 10.0)),
        )
        .expect("worker spawns");
        let output = runtime.output();
        output.submit(begin(1, HIDE));
        output.submit(moved(1, 7.0));
        output.submit(END);
        runtime.shutdown();
        let calls: Vec<&str> = observed.try_iter().collect();
        assert_eq!(calls, ["hide", "show"]);
    }
}
