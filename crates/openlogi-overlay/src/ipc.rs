//! The overlay's IPC client: connecting to the agent, receiving ring
//! invocations, and reporting what the user does with them.
//!
//! Reporting is not fire-and-forget. A hover that never lands leaves the ring
//! showing the wrong slot, and a dropped activation loses the user's click
//! outright — so terminal commands retry until the session's own deadline. A
//! newer command for the same ring supersedes a stalled one rather than
//! queueing behind it, which is what keeps the ring responsive when the agent
//! is briefly slow.
//!
//! The ring and the highlight each get their own delivery worker
//! ([`Family`]): a family's commands supersede each other, but never a
//! command of the other family, so a ring hover cannot drop a highlight
//! release nor a release a ring activation.

use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use openlogi_core::action_ring::DISPLAY_LIFETIME;
use openlogi_core::binding::ActionRingSlot;
use openlogi_ipc::client::{self, ConnectError, Observer, Stamped};
use openlogi_ipc::{
    ActionRingInvocation, AgentClient, BacklightObservation, ClientKind, HighlightObservation,
};
use succession::Standing;
use tarpc::context;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::session::allegiance;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OverlayCommand {
    Hover {
        session_id: u64,
        slot: ActionRingSlot,
    },
    Activate {
        session_id: u64,
        slot: ActionRingSlot,
    },
    Cancel {
        session_id: u64,
    },
    /// Highlight `session_id` ended with its dot at `(x, y)`, in global
    /// top-left-origin points; the agent moves the pointer there.
    HighlightReleased {
        session_id: u64,
        x: f64,
        y: f64,
    },
    /// Highlight `session_id` is at `(x, y)` right now, in the same space;
    /// the agent warps the pointer there while the session is active. Not
    /// terminal: a move is sent once and never retried.
    HighlightMoved {
        session_id: u64,
        x: f64,
        y: f64,
    },
}

/// The independent stream a command belongs to. Commands of one family
/// supersede each other; commands of different families never do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Ring,
    Highlight,
}

impl OverlayCommand {
    const fn family(self) -> Family {
        match self {
            Self::Hover { .. } | Self::Activate { .. } | Self::Cancel { .. } => Family::Ring,
            Self::HighlightReleased { .. } | Self::HighlightMoved { .. } => Family::Highlight,
        }
    }

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Activate { .. } | Self::Cancel { .. } | Self::HighlightReleased { .. }
        )
    }
}

/// What the view holds to talk to the agent.
pub(crate) struct Handle {
    /// The ring the agent says should be showing, each time that changes.
    /// `None` is no ring — including a dismissal, which is why there is no
    /// separate "close" message to recognise.
    pub(crate) invocations: mpsc::UnboundedReceiver<Option<ActionRingInvocation>>,
    /// Transient keyboard-backlight indicator state.
    pub(crate) backlights: mpsc::UnboundedReceiver<BacklightObservation>,
    /// Every highlight observation, plus a hidden one on disconnect.
    pub(crate) highlights: mpsc::UnboundedReceiver<HighlightUpdate>,
    /// Where the view reports hover, activation, and cancellation.
    pub(crate) commands: mpsc::UnboundedSender<OverlayCommand>,
}

/// Spawn the IPC client thread. Returns immediately; the thread connects (and
/// reconnects) on its own.
pub(crate) fn spawn() -> Handle {
    let (invocation_tx, invocations) = mpsc::unbounded_channel();
    let (backlight_tx, backlights) = mpsc::unbounded_channel();
    let (highlight_tx, highlights) = mpsc::unbounded_channel();
    let (commands, mut command_rx) = mpsc::unbounded_channel();
    let started = openlogi_core::worker::spawn("openlogi-overlay-ipc", move |runtime| {
        runtime.block_on(async {
            tokio::join!(
                observe_invocations(invocation_tx),
                observe_backlights(backlight_tx),
                observe_highlights(highlight_tx),
                send_commands(&mut command_rx)
            );
        });
    });
    if let Err(error) = started {
        warn!(%error, "overlay IPC client thread could not start");
    }
    Handle {
        invocations,
        backlights,
        highlights,
        commands,
    }
}

/// The observation that closes the indicator when the agent that was driving
/// it is gone. The generation is meaningless to the window, which only reads
/// `visible`.
const fn agent_lost() -> BacklightObservation {
    BacklightObservation {
        generation: 0,
        current_level: 0,
        levels: 0,
        visible: false,
    }
}

async fn connect() -> Option<AgentClient> {
    // Declaring as an overlay is part of the handshake: an overlay
    // reconnecting on its own is an orphan of a previous run and must not wake
    // a dormant agent — an armed agent spawns its own overlay.
    let client = match client::connect_as(ClientKind::Overlay).await {
        Ok(client) => client,
        // A mismatch in either direction is not transient — this binary is
        // from a superseded install, and the agent will start the overlay
        // that matches it as soon as this one releases the role.
        Err(ConnectError::Skew(skew)) => stand_down(&skew.to_string()),
        Err(_) => return None,
    };
    let identity = client.identity(context::current()).await.ok()?;
    if let Standing::Superseded(because) = allegiance().observe(identity) {
        stand_down(&because.to_string());
    }
    Some(client)
}

/// Exit, releasing the overlay role.
///
/// Staying alive is worse than useless in every case that reaches here. A
/// superseded helper cannot serve a ring it can no longer be asked about, and
/// its claim on the role is exactly what stops the agent's supervisor from
/// starting the one that can; an orphaned helper has no agent to serve at all.
#[expect(
    clippy::exit,
    reason = "the IPC tasks run off the GPUI main thread and cannot return a status to `main`, which is parked in the application run loop; releasing the role by exiting is the point"
)]
pub(crate) fn stand_down(because: &str) -> ! {
    info!("{because} — exiting and releasing the Actions Ring overlay role");
    std::process::exit(0)
}

/// How long to keep reaching for an agent before giving up and exiting.
///
/// Nothing else bounds this process's life. The agent starts it detached and
/// never kills it — the menu-bar Quit is a `process::exit`, which runs no
/// destructors — and every other exit path here needs a *replacement* agent to
/// answer. So an agent that stops for good leaves its helper reconnecting
/// forever; one was found still at it three days on. Giving up costs almost
/// nothing: the helper only exists to keep process-start latency off the first
/// ring press, which is worth exactly zero while no agent is running, and the
/// supervisor starts a fresh one within its restart backoff once an agent is
/// back.
const GIVE_UP_AFTER: Duration = Duration::from_mins(1);

/// How long to wait between attempts to reach an agent.
const RETRY_PERIOD: Duration = Duration::from_secs(1);

/// One observe connection: its phase, and the state meaningful within each
/// phase.
///
/// A successful connection owns its [`Observer`], and with it the generation
/// cursor; a reconnect episode owns its give-up clock. Transitioning between
/// them resets the fact from the previous phase by construction.
enum ObserveLink<T: Stamped> {
    Reconnecting { unreachable_since: Option<Instant> },
    Observing(Observer<T>),
}

impl<T: Stamped> Default for ObserveLink<T> {
    fn default() -> Self {
        Self::Reconnecting {
            unreachable_since: None,
        }
    }
}

impl<T: Stamped> ObserveLink<T> {
    fn connected(&mut self, observer: Observer<T>) {
        *self = Self::Observing(observer);
    }

    fn connect_failed(&mut self, now: Instant) -> bool {
        let Self::Reconnecting { unreachable_since } = self else {
            return false;
        };
        let armed = *unreachable_since.get_or_insert(now);
        now.duration_since(armed) >= GIVE_UP_AFTER
    }

    fn disconnected(&mut self) {
        *self = Self::Reconnecting {
            unreachable_since: None,
        };
    }
}

/// What an observe loop hands its consumer.
enum Observed<T> {
    /// A fresh answer from the agent.
    Answer(T),
    /// An established connection dropped. The agent's own timers die with
    /// it, so a consumer holding something only the agent can end must let
    /// go of it.
    Lost,
}

/// Keep one observe connection alive, handing every answer and every lost
/// connection to `deliver`, which returns `false` once nobody listens any
/// more.
///
/// All observe loops share this, so they share the connect, stand-down and
/// give-up policy.
async fn observe<T: Stamped>(
    arm: fn(AgentClient) -> Observer<T>,
    mut deliver: impl FnMut(Observed<T>) -> bool,
) {
    let mut link = ObserveLink::<T>::default();
    loop {
        if matches!(&link, ObserveLink::Reconnecting { .. }) {
            if let Some(client) = connect().await {
                // Generation 0 says "I have seen nothing", so the first
                // answer is whatever is showing right now. A replacement
                // agent numbers its own generations, so every connection
                // starts there independently.
                link.connected(arm(client));
            } else {
                if link.connect_failed(Instant::now()) {
                    stand_down(&format!("no agent has answered for {GIVE_UP_AFTER:?}"));
                }
                tokio::time::sleep(RETRY_PERIOD).await;
                continue;
            }
        }
        let ObserveLink::Observing(observer) = &mut link else {
            continue;
        };
        match observer.next().await {
            // The hold elapsing with nothing new, or a stale reply: still
            // alive, nothing to show.
            Ok(None) => {}
            Ok(Some(answer)) => {
                if !deliver(Observed::Answer(answer)) {
                    return;
                }
            }
            Err(error) => {
                debug!(?error, "observe channel disconnected");
                link.disconnected();
                if !deliver(Observed::Lost) {
                    return;
                }
            }
        }
    }
}

async fn observe_invocations(tx: mpsc::UnboundedSender<Option<ActionRingInvocation>>) {
    observe(Observer::action_ring, |observed| match observed {
        Observed::Answer(ring) => tx.send(ring.invocation).is_ok(),
        // The ring's own session deadline closes it.
        Observed::Lost => true,
    })
    .await;
}

async fn observe_backlights(tx: mpsc::UnboundedSender<BacklightObservation>) {
    observe(Observer::backlight, |observed| {
        tx.send(match observed {
            Observed::Answer(observation) => observation,
            // The expiry timer lives in the agent, so a lost agent can never
            // send the hide that closes a visible indicator.
            Observed::Lost => agent_lost(),
        })
        .is_ok()
    })
    .await;
}

/// Every highlight observation, and [`HighlightUpdate::Disconnected`] when
/// the agent goes away.
async fn observe_highlights(tx: mpsc::UnboundedSender<HighlightUpdate>) {
    observe(Observer::highlight, |observed| {
        tx.send(match observed {
            Observed::Answer(highlight) => HighlightUpdate::Observed(highlight),
            Observed::Lost => HighlightUpdate::Disconnected,
        })
        .is_ok()
    })
    .await;
}

/// What the highlight loop hands the view: the agent's latest observation,
/// or the news that the agent is gone. The second is its own variant rather
/// than a sentinel observation, because whether an answer is new is the
/// [`Observer`]'s call alone — nothing here inspects a generation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HighlightUpdate {
    /// The agent's current highlight state.
    Observed(HighlightObservation),
    /// The agent went away: a held highlight must not stay on screen for an
    /// agent that can no longer say the button was released.
    Disconnected,
}

/// Fold a newly-produced command into the one still waiting to be sent. Both
/// are of the same [`Family`]: the families are delivered by separate workers
/// and never meet here.
///
/// A hover is dropped once its own session has already been activated or
/// cancelled — the buzz would be for a ring that is closing. It must be the
/// *same* session though: rings open back to back, and the view only emits a
/// hover when the hovered slot changes, so swallowing the new ring's first
/// hover loses it for as long as the pointer stays where it is.
///
/// A running highlight position report is superseded by any newer command but
/// never displaces the release still being delivered: the release is
/// terminal and retried, a move is not. A newer release does replace an older
/// one still waiting, even though a release is terminal: sessions end in
/// order and the pointer ends where the last one left its dot, so delivering
/// the older position first would only be overwritten.
fn coalesce_command(current: OverlayCommand, next: OverlayCommand) -> OverlayCommand {
    debug_assert_eq!(current.family(), next.family());
    match (next, current) {
        (OverlayCommand::HighlightMoved { .. }, OverlayCommand::HighlightReleased { .. }) => {
            current
        }
        (
            OverlayCommand::Hover { session_id, .. },
            OverlayCommand::Activate {
                session_id: closing,
                ..
            }
            | OverlayCommand::Cancel {
                session_id: closing,
            },
        ) if session_id == closing => current,
        _ => next,
    }
}

type CommandFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

async fn send_command(client: &AgentClient, command: OverlayCommand) -> bool {
    let ctx = context::current();
    match command {
        OverlayCommand::Hover { session_id, slot } => client
            .action_ring_hover(ctx, session_id, slot)
            .await
            .is_ok(),
        OverlayCommand::Activate { session_id, slot } => client
            .action_ring_activate(ctx, session_id, slot)
            .await
            .is_ok(),
        OverlayCommand::Cancel { session_id } => {
            client.action_ring_cancel(ctx, session_id).await.is_ok()
        }
        OverlayCommand::HighlightReleased { session_id, x, y } => client
            .highlight_released(ctx, session_id, x, y)
            .await
            .is_ok(),
        OverlayCommand::HighlightMoved { session_id, x, y } => {
            client.highlight_moved(ctx, session_id, x, y).await.is_ok()
        }
    }
}

async fn send_commands(rx: &mut mpsc::UnboundedReceiver<OverlayCommand>) {
    send_commands_by_family(
        rx,
        || Box::pin(connect()),
        |client, command| Box::pin(send_command(client, command)),
    )
    .await;
}

/// Deliver commands with one [`send_commands_with`] worker per [`Family`], so
/// the single pending command each worker holds can only be displaced by its
/// own family. Returns once `rx` is closed and both workers have stopped.
async fn send_commands_by_family<C>(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    connect_client: impl FnMut() -> CommandFuture<'static, Option<C>> + Clone,
    send: impl for<'a> FnMut(&'a C, OverlayCommand) -> CommandFuture<'a, bool> + Clone,
) {
    let (ring_tx, mut ring_rx) = mpsc::unbounded_channel();
    let (highlight_tx, mut highlight_rx) = mpsc::unbounded_channel();
    let route = async {
        while let Some(command) = rx.recv().await {
            let target = match command.family() {
                Family::Ring => &ring_tx,
                Family::Highlight => &highlight_tx,
            };
            // A worker only stops once its channel closes, so a failed send
            // cannot happen while the router runs.
            let _ = target.send(command);
        }
        // Closing both channels is what stops the workers.
        drop((ring_tx, highlight_tx));
    };
    tokio::join!(
        route,
        send_commands_with(&mut ring_rx, connect_client.clone(), send.clone()),
        send_commands_with(&mut highlight_rx, connect_client, send),
    );
}

async fn send_commands_with<C>(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    mut connect_client: impl FnMut() -> CommandFuture<'static, Option<C>>,
    mut send: impl for<'a> FnMut(&'a C, OverlayCommand) -> CommandFuture<'a, bool>,
) {
    let mut client = None;
    while let Some(mut command) = rx.recv().await {
        while let Ok(next) = rx.try_recv() {
            command = coalesce_command(command, next);
        }
        let mut deadline = command_deadline(command);
        loop {
            while let Ok(next) = rx.try_recv() {
                (command, deadline) = merge_pending(command, deadline, next);
            }
            if client.is_none() {
                match await_command_attempt(rx, command, deadline, connect_client()).await {
                    CommandAttempt::Completed(connected) => client = connected,
                    CommandAttempt::Superseded(next, next_deadline) => {
                        command = next;
                        deadline = next_deadline;
                        continue;
                    }
                    CommandAttempt::Expired => break,
                    CommandAttempt::Closed => return,
                }
            }
            let Some(active) = client.as_ref() else {
                let Some((next, next_deadline)) = wait_for_retry(rx, command, deadline).await
                else {
                    break;
                };
                command = next;
                deadline = next_deadline;
                continue;
            };
            match await_command_attempt(rx, command, deadline, send(active, command)).await {
                CommandAttempt::Completed(false) => client = None,
                CommandAttempt::Superseded(next, next_deadline) => {
                    command = next;
                    deadline = next_deadline;
                    continue;
                }
                CommandAttempt::Completed(true) | CommandAttempt::Expired => break,
                CommandAttempt::Closed => return,
            }
            let Some((next, next_deadline)) = wait_for_retry(rx, command, deadline).await else {
                break;
            };
            command = next;
            deadline = next_deadline;
        }
    }
}

#[derive(Debug)]
enum CommandAttempt<T> {
    Completed(T),
    Superseded(OverlayCommand, Option<Instant>),
    Expired,
    Closed,
}

async fn await_command_attempt<T>(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    command: OverlayCommand,
    deadline: Option<Instant>,
    attempt: impl Future<Output = T>,
) -> CommandAttempt<T> {
    tokio::pin!(attempt);
    loop {
        tokio::select! {
            result = &mut attempt => return CommandAttempt::Completed(result),
            next = rx.recv() => {
                let Some(next) = next else {
                    return CommandAttempt::Closed;
                };
                let mut pending = merge_pending(command, deadline, next);
                while let Ok(next) = rx.try_recv() {
                    pending = merge_pending(pending.0, pending.1, next);
                }
                if pending.0 != command {
                    return CommandAttempt::Superseded(pending.0, pending.1);
                }
            }
            () = deadline_elapsed(deadline) => return CommandAttempt::Expired,
        }
    }
}

async fn deadline_elapsed(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline.into()).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn command_deadline(command: OverlayCommand) -> Option<Instant> {
    command
        .is_terminal()
        .then(|| Instant::now() + DISPLAY_LIFETIME)
}

fn merge_pending(
    command: OverlayCommand,
    deadline: Option<Instant>,
    next: OverlayCommand,
) -> (OverlayCommand, Option<Instant>) {
    let pending = coalesce_command(command, next);
    let deadline = if pending == command {
        deadline
    } else {
        command_deadline(pending)
    };
    (pending, deadline)
}

async fn wait_for_retry(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    command: OverlayCommand,
    deadline: Option<Instant>,
) -> Option<(OverlayCommand, Option<Instant>)> {
    if !retry_before(deadline) {
        return None;
    }
    tokio::select! {
        () = tokio::time::sleep(Duration::from_millis(100)) => Some((command, deadline)),
        next = rx.recv() => {
            let mut pending = merge_pending(command, deadline, next?);
            while let Ok(next) = rx.try_recv() {
                pending = merge_pending(pending.0, pending.1, next);
            }
            Some(pending)
        }
    }
}

fn retry_before(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() < deadline)
}

#[cfg(test)]
mod tests;
