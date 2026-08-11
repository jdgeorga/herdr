//! Background polling for the Jobs sidebar list section (design doc:
//! "Polling").
//!
//! The scheduling shape mirrors `git_refresh.rs`'s deadline/in-flight
//! pattern (`start_git_status_refresh_if_due`), but the design doc is
//! explicit that `git_refresh` is *not* a subprocess template: it provides no
//! timeout, no child handle, no cancellation, no output limits. Everything
//! below that pattern doesn't already cover -- hard timeout, process-group
//! kill, bounded concurrent stdout/stderr capture, a launch-time identity
//! check, and shutdown cancellation -- is this module's own responsibility.
//!
//! `std::process::Command` only: tokio's `process` feature is deliberately
//! not enabled (`Cargo.toml`), so there is no `.await`-able child exit here.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::App;
use crate::events::AppEvent;
use crate::list_section::protocol::{parse_provider_payload, ParseFailure, ParsedPayload};
use crate::list_section::substitute::{resolve_argv, RowContext};

/// Per-stream output cap, shared with the provider protocol's own total-size
/// cap (design doc: "Bounded output. Cap stdout and stderr (256 KiB each)
/// before parsing."). Reusing the same constant means a stream that hits this
/// cap is, by construction, already too big to ever parse.
const MAX_STREAM_BYTES: usize = crate::list_section::protocol::MAX_TOTAL_BYTES;

/// Granularity of the `try_wait` busy-loop while waiting for the child to
/// exit or the hard timeout to elapse. Not itself part of the design doc --
/// just how finely we check, since there is no async child-exit future to
/// `.await` without tokio's `process` feature.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Outcome of one poll attempt, delivered as `AppEvent::ListSectionPolled`.
/// `Failed` covers every non-provider-protocol failure mode the design doc's
/// "Result state" section calls out -- spawn failure, read failure, timeout,
/// oversized output and worker panic -- as well as a provider-protocol parse
/// failure. All of them collapse to the same "retain rows, mark stale"
/// handling, so there is no need to distinguish them in the type itself.
#[derive(Debug)]
pub(crate) enum ListPollOutcome {
    Success(ParsedPayload),
    Failed(String),
}

/// Shared between the poll worker thread and `App::shutdown_list_poll`
/// (design doc: "Shutdown cancellation"). Holds the in-flight poll's
/// process-group pid, if any, plus a flag that lets shutdown cancel a poll
/// that hasn't managed to register its pid yet.
#[derive(Debug, Default)]
pub(crate) struct ListPollShared {
    pid: Option<u32>,
    shutting_down: bool,
}

pub(crate) type ListPollChildSlot = Arc<Mutex<ListPollShared>>;

/// RAII guard around a registered process-group pid (finding 2: "use an RAII
/// guard for the child group so panics and early returns still clean up").
/// While held, `pid` stays registered in `shared` so `shutdown_list_poll` can
/// always find and kill it -- including while this worker is blocked joining
/// the pipe-reader threads, not just while waiting on the child itself.
/// Dropping the guard (on every return path, including panics unwinding
/// through `catch_worker_panic`) unconditionally kills the whole process
/// group one more time and clears the registration. Killing an
/// already-reaped group is a harmless no-op (`kill(2)` on a vanished pgid).
struct ChildGroupGuard {
    shared: ListPollChildSlot,
    pid: u32,
}

impl ChildGroupGuard {
    /// Registers `pid` as the in-flight poll's process group, unless
    /// shutdown was already requested -- covering "a similar race if
    /// shutdown happens before the worker records its PID": without this
    /// check, a poll spawned in the same instant as shutdown could register
    /// after `shutdown_list_poll` already ran and escape it entirely.
    fn register(shared: &ListPollChildSlot, pid: u32) -> Result<Self, ()> {
        let Ok(mut guard) = shared.lock() else {
            return Err(());
        };
        if guard.shutting_down {
            return Err(());
        }
        guard.pid = Some(pid);
        drop(guard);
        Ok(Self {
            shared: shared.clone(),
            pid,
        })
    }
}

impl Drop for ChildGroupGuard {
    fn drop(&mut self) {
        kill_process_group(self.pid);
        if let Ok(mut guard) = self.shared.lock() {
            if guard.pid == Some(self.pid) {
                guard.pid = None;
            }
        }
    }
}

/// What a launched poll was fetching, captured at launch time and compared
/// against the live generation at completion (design doc: "A generation
/// counter; discard results whose mode or command changed since launch").
/// `App::list_poll_generation` is bumped on every mode toggle and every
/// config reload that touches `sidebar_list` -- a monotonic counter rather
/// than comparing mode/command *values* directly, since a live -> history ->
/// live round trip while a poll is in flight would otherwise land back on
/// the same value and let a stale result compare equal (design doc finding:
/// "the stale-result guard is an identity comparison, not a generation
/// counter").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListPollIdentity {
    pub generation: u64,
}

impl App {
    /// Schedules a poll if the interval has elapsed and none is in flight.
    /// Mirrors `start_git_status_refresh_if_due` (`git_refresh.rs:36`).
    pub(crate) fn start_list_poll_if_due(&mut self, now: Instant) {
        let Some(deadline) = self.list_poll_deadline() else {
            return;
        };
        if now < deadline {
            return;
        }
        self.start_list_poll(now);
    }

    /// `None` when polling shouldn't happen at all (section disabled or no
    /// command configured) or a poll is already in flight -- single-flight:
    /// an overrunning poll stretches the cadence rather than overlapping it,
    /// since there is simply no deadline to become due while in flight.
    pub(crate) fn list_poll_deadline(&self) -> Option<Instant> {
        (!self.list_poll_in_flight
            && self.state.sidebar_list.enabled
            && !self.state.sidebar_list.command.is_empty())
        .then_some(self.last_list_poll + list_poll_interval(&self.state.sidebar_list))
    }

    /// Forces the next `start_list_poll_if_due` to fire immediately, for
    /// callers that just changed something a poll result depends on (design
    /// doc: mode toggle is "immediate re-poll"). If a poll is already in
    /// flight -- whose result may belong to the mode/command about to change
    /// -- defers to right after it lands instead of overlapping it.
    pub(crate) fn mark_list_poll_due(&mut self, now: Instant) {
        if self.list_poll_in_flight {
            self.list_poll_due_after_in_flight = true;
            return;
        }
        self.last_list_poll = now
            .checked_sub(list_poll_interval(&self.state.sidebar_list))
            .unwrap_or(now);
        self.list_poll_due_after_in_flight = false;
    }

    fn start_list_poll(&mut self, now: Instant) {
        let identity = ListPollIdentity {
            generation: self.list_poll_generation,
        };
        let mode = self.state.jobs.mode.clone();
        let command = self.state.sidebar_list.command.clone();
        let row = RowContext {
            id: String::new(),
            cells: Vec::new(),
            vars: HashMap::new(),
        };
        // The section command only ever substitutes `{mode}` (design doc's
        // substitution table: "`{mode}` | ... command only"); an empty row
        // means `{id}`/`{cellN}`/vars all correctly fail as unresolved rather
        // than silently resolving against nothing.
        let argv = match resolve_argv(&command, &row, &mode) {
            Ok(argv) => argv,
            Err(err) => {
                // A bad command template is a config problem, not a
                // subprocess one: nothing was spawned, so it never goes in
                // flight and there's nothing to time out or reap.
                self.last_list_poll = now;
                self.apply_list_poll_outcome(
                    ListPollOutcome::Failed(format!("list command template: {err}")),
                    now,
                );
                return;
            }
        };
        let timeout = Duration::from_secs(self.state.sidebar_list.timeout_seconds.max(1));
        // Design doc: "`style` is a name resolved against config" -- the
        // parser is pure and has no config access, so the caller (here)
        // passes the configured style names through so a legitimate custom
        // style doesn't get flagged as unknown on every single poll
        // (`list_section::protocol::parse_provider_payload`'s `known_styles`
        // parameter).
        let known_styles: Vec<String> = self.state.sidebar_list.styles.keys().cloned().collect();

        self.list_poll_in_flight = true;
        self.last_list_poll = now;
        let event_tx = self.event_tx.clone();
        let child_slot = self.list_poll_child.clone();
        let event_identity = identity;
        std::thread::spawn(move || {
            let outcome =
                catch_worker_panic(|| run_list_poll(&argv, timeout, &child_slot, &known_styles));
            let _ = event_tx.blocking_send(AppEvent::ListSectionPolled {
                identity: event_identity,
                outcome,
            });
        });
    }

    /// Kills any in-flight poll's process group. The generic child tracker
    /// (`reap_finished_custom_commands`, `runtime.rs:12`) only `try_wait`s
    /// and never kills on exit, so this is this poller's own shutdown hook
    /// (design doc: "Shutdown cancellation"). Called from both run loops on
    /// their way out.
    pub(crate) fn shutdown_list_poll(&mut self) {
        let Ok(mut guard) = self.list_poll_child.lock() else {
            return;
        };
        // Set unconditionally, even if no pid is registered yet: covers the
        // spawn/register race where a worker thread is between
        // `Command::spawn` and `ChildGroupGuard::register` right now, so it
        // must see `shutting_down` and self-immolate instead of escaping.
        guard.shutting_down = true;
        if let Some(pid) = guard.pid.take() {
            kill_process_group(pid);
        }
    }

    /// Applies a completed `AppEvent::ListSectionPolled` (design doc: "Result
    /// state"). Discards the outcome if the mode or command has changed since
    /// the poll launched (`list_poll_generation` no longer matches).
    pub(crate) fn handle_list_section_polled(
        &mut self,
        identity: ListPollIdentity,
        outcome: ListPollOutcome,
    ) {
        self.list_poll_in_flight = false;
        if identity.generation == self.list_poll_generation {
            self.apply_list_poll_outcome(outcome, Instant::now());
        } else {
            tracing::debug!(
                "discarding list section poll result: mode or command changed since launch"
            );
        }
        if self.list_poll_due_after_in_flight {
            self.mark_list_poll_due(Instant::now());
            self.list_poll_due_after_in_flight = false;
        }
    }

    fn apply_list_poll_outcome(&mut self, outcome: ListPollOutcome, now: Instant) {
        match outcome {
            ListPollOutcome::Success(payload) => {
                for diagnostic in &payload.diagnostics {
                    tracing::warn!(
                        message = %diagnostic.message,
                        "list section poll: dropped invalid content"
                    );
                }
                // Full replace, not a merge: each poll is a complete
                // snapshot (design doc: "A valid empty result CLEARS rows").
                self.state.jobs.title = payload.title;
                self.state.jobs.summary = payload.summary;
                self.state.jobs.groups = payload.groups;
                if !self
                    .state
                    .jobs
                    .selected_row_id
                    .as_ref()
                    .is_some_and(|id| jobs_groups_contain_row(&self.state.jobs.groups, id))
                {
                    // Design doc: "Selection is identified by row id... A
                    // selection whose id vanishes is cleared."
                    self.state.jobs.selected_row_id = None;
                }
                self.state.jobs.is_stale = false;
                self.state.jobs.last_error = None;
                self.last_list_poll_success = Some(now);
                // Design doc: "Notifications" -- bridges provider `notify`
                // entries through the bounded toast FIFO, deduplicated by id
                // so a provider that keeps reporting the same event doesn't
                // re-toast every poll.
                self.enqueue_list_notifications(&payload.notify);
            }
            ListPollOutcome::Failed(message) => {
                tracing::warn!(err = %message, "list section poll failed");
                // Every failure mode retains prior rows and marks them stale
                // (design doc: "Result state") -- `groups`/`title`/`summary`
                // are simply left untouched.
                self.state.jobs.is_stale = true;
                self.state.jobs.last_error = Some(message);
            }
        }
        self.state.jobs.last_success_label =
            format_last_success_label(self.last_list_poll_success, now);
    }
}

fn list_poll_interval(config: &crate::config::ListSectionConfig) -> Duration {
    Duration::from_secs(config.refresh_seconds.max(1))
}

fn jobs_groups_contain_row(
    groups: &[crate::list_section::protocol::ParsedGroup],
    row_id: &str,
) -> bool {
    groups
        .iter()
        .any(|group| group.rows.iter().any(|row| row.id == row_id))
}

/// Runs `f`, converting a panic into `ListPollOutcome::Failed` rather than
/// letting the poll worker thread die silently and leave `list_poll_in_flight`
/// stuck forever (design doc: "worker panic" is one of the completion-event
/// cases that must still fire). Generic over the closure purely so this
/// wrapping is unit-testable without spawning a real subprocess.
fn catch_worker_panic<F>(f: F) -> ListPollOutcome
where
    F: FnOnce() -> ListPollOutcome,
{
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        ListPollOutcome::Failed("list section poll worker panicked".to_string())
    })
}

/// Sends `SIGKILL` to the whole process group `pid` leads (design doc:
/// "Process group. Spawn with `process_group(0)` and kill the whole group.
/// Killing only Python orphans `squeue`/`sacct`."). `pid` was spawned with
/// `process_group(0)`, so it is also its own process group id.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: `libc::kill` with a negative pid signals the whole process
    // group rather than a single process; this is its documented behavior.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {
    // No process-group concept on this platform; this design targets
    // Perlmutter (Linux) only (design doc: "Cluster | Perlmutter only").
}

/// Spawns `argv`, enforces `timeout`, and returns the parsed result or a
/// description of whatever went wrong. Runs entirely on the calling
/// (detached) thread; the caller is responsible for delivering the
/// `AppEvent::ListSectionPolled` completion event no matter what this
/// returns.
fn run_list_poll(
    argv: &[String],
    timeout: Duration,
    child_slot: &ListPollChildSlot,
    known_styles: &[String],
) -> ListPollOutcome {
    let Some((program, args)) = argv.split_first() else {
        return ListPollOutcome::Failed("list section command is empty".to_string());
    };

    let mut command = Command::new(program);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group, pgid == the child's own pid, so a timeout can
        // kill the whole group instead of orphaning grandchildren.
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return ListPollOutcome::Failed(format!("failed to spawn: {err}")),
    };
    let pid = child.id();
    let guard = match ChildGroupGuard::register(child_slot, pid) {
        Ok(guard) => guard,
        Err(()) => {
            // Shutdown was requested before we could register (design doc
            // finding 2's spawn/register race): kill immediately rather than
            // let this child escape shutdown's reach.
            kill_process_group(pid);
            let _ = child.wait();
            return ListPollOutcome::Failed("list section poll cancelled by shutdown".to_string());
        }
    };

    // Present: spawned with `Stdio::piped()` above.
    let stdout_reader = child
        .stdout
        .take()
        .map(|pipe| std::thread::spawn(move || read_capped(pipe, MAX_STREAM_BYTES)));
    let stderr_reader = child
        .stderr
        .take()
        .map(|pipe| std::thread::spawn(move || read_capped(pipe, MAX_STREAM_BYTES)));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(CHILD_POLL_INTERVAL);
            }
            Err(err) => {
                // `guard` drops here (kills the group, clears registration)
                // as this function returns.
                return ListPollOutcome::Failed(format!("failed to wait for child: {err}"));
            }
        }
    };

    // Whatever became of the leader -- exited cleanly or timed out -- kill
    // any remaining process-group members *now*, before joining the pipe
    // readers below. Without this, a leader that exits 0 while a forked
    // descendant still holds stdout/stderr open leaves the readers blocked
    // on that descendant's pipe end, possibly forever (design doc finding:
    // "a poll can wedge forever"). `guard` stays registered through the
    // joins that follow (not cleared here), so shutdown can still reach this
    // group for the whole time the readers might be blocked.
    kill_process_group(pid);
    // Always reap after killing, even though the exit status itself is moot
    // once timed out (design doc: "Reap. Always wait() after kill.").
    let _ = child.wait();

    // Joined after the child (and any stray group members) are gone, so the
    // pipes have hit EOF and these don't block. Both readers ran
    // concurrently while we waited above (design doc: "Drain both streams
    // concurrently. Reading stdout while stderr fills its pipe deadlocks.").
    let stdout_result = stdout_reader.map(|handle| handle.join());
    let stderr_result = stderr_reader.map(|handle| handle.join());
    // Only now is it safe to drop the registration: the readers can no
    // longer block on this group.
    drop(guard);

    if timed_out {
        return ListPollOutcome::Failed(format!("timed out after {timeout:?}"));
    }

    let Some(status) = status else {
        return ListPollOutcome::Failed(
            "child exited but its status could not be read".to_string(),
        );
    };

    let (stdout_bytes, stdout_truncated) = match stdout_result {
        Some(Ok(Ok(captured))) => captured,
        Some(Ok(Err(err))) => {
            return ListPollOutcome::Failed(format!("failed to read stdout: {err}"))
        }
        Some(Err(_)) => {
            return ListPollOutcome::Failed("stdout reader thread panicked".to_string())
        }
        None => (Vec::new(), false),
    };
    // A stderr *read* error is treated the same as a stdout one -- both mean
    // the poll's output can't be trusted (design doc finding: "read errors
    // become silent EOF"). A stderr *truncation* (>256 KiB) is caught below,
    // alongside stdout's, once `status.success()` has been checked.
    let stderr_truncated = match stderr_result {
        Some(Ok(Ok((_, truncated)))) => truncated,
        Some(Ok(Err(err))) => {
            return ListPollOutcome::Failed(format!("failed to read stderr: {err}"))
        }
        Some(Err(_)) => {
            return ListPollOutcome::Failed("stderr reader thread panicked".to_string())
        }
        None => false,
    };

    if !status.success() {
        return ListPollOutcome::Failed(format!("exited with {status}"));
    }
    if stdout_truncated {
        return ListPollOutcome::Failed(format!("stdout exceeded the {MAX_STREAM_BYTES} byte cap"));
    }
    if stderr_truncated {
        // Design doc finding: "Valid stdout plus >256 KiB stderr and exit 0
        // is ACCEPTED instead of marked stale" -- a provider that floods
        // stderr is misbehaving even if stdout happens to parse, and that
        // should surface as staleness, not be silently accepted.
        return ListPollOutcome::Failed(format!("stderr exceeded the {MAX_STREAM_BYTES} byte cap"));
    }

    let stdout_text = String::from_utf8_lossy(&stdout_bytes);
    let known_styles: Vec<&str> = known_styles.iter().map(String::as_str).collect();
    match parse_provider_payload(&stdout_text, &known_styles) {
        Ok(payload) => ListPollOutcome::Success(payload),
        Err(err) => ListPollOutcome::Failed(describe_parse_failure(&err)),
    }
}

/// Reads from `reader` into a buffer capped at `cap` bytes, returning
/// `(bytes, true)` if the stream had more data than that. Keeps draining
/// past the cap rather than stopping there, so a chatty child never blocks on
/// a full pipe (design doc: "Drain both streams concurrently"). A genuine
/// I/O error is propagated rather than treated as a clean EOF (design doc
/// finding: "read errors become silent EOF") -- the caller must not trust a
/// poll whose read failed partway through.
fn read_capped<R: Read>(mut reader: R, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::with_capacity(cap.min(64 * 1024));
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < cap {
                    let take = (cap - buf.len()).min(n);
                    buf.extend_from_slice(&chunk[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok((buf, truncated))
}

fn describe_parse_failure(err: &ParseFailure) -> String {
    match err {
        ParseFailure::Malformed(message) => format!("malformed provider output: {message}"),
        ParseFailure::VersionMismatch { expected, found } => {
            format!("provider protocol version mismatch: expected {expected}, found {found:?}")
        }
        ParseFailure::Oversized(message) => format!("provider output oversized: {message}"),
    }
}

/// Pre-formats how long ago the last successful poll landed, since rendering
/// never does its own time math (design doc: "Rendering never does its own
/// time math, mirroring how the SLURM provider pre-formats its own time
/// fields."). Recomputed on every poll completion (success or failure), so it
/// can go slightly stale between polls -- acceptable at a ~10s cadence.
fn format_last_success_label(last_success: Option<Instant>, now: Instant) -> Option<String> {
    let last_success = last_success?;
    let secs = now.saturating_duration_since(last_success).as_secs();
    Some(if secs < 1 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list_section::protocol::{ParsedGroup, ParsedRow, RowStyle};

    fn test_app(config: &crate::config::Config) -> super::super::App {
        super::super::App::new(
            config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    fn sample_row(id: &str) -> ParsedRow {
        ParsedRow {
            id: id.to_string(),
            cells: vec!["x".to_string()],
            style: RowStyle::Normal,
            vars: Default::default(),
            actions: Vec::new(),
        }
    }

    // -- scheduling --------------------------------------------------------

    #[test]
    fn deadline_is_none_when_section_disabled() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.list.enabled = false;
        let app = test_app(&config);
        assert_eq!(app.list_poll_deadline(), None);
    }

    #[test]
    fn deadline_is_none_when_command_empty() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.list.command = Vec::new();
        let app = test_app(&config);
        assert_eq!(app.list_poll_deadline(), None);
    }

    #[test]
    fn deadline_is_none_while_in_flight() {
        let mut app = test_app(&crate::config::Config::default());
        app.list_poll_in_flight = true;
        assert_eq!(app.list_poll_deadline(), None);
    }

    #[test]
    fn mark_list_poll_due_forces_an_immediate_deadline_when_idle() {
        let mut app = test_app(&crate::config::Config::default());
        let now = Instant::now();
        app.mark_list_poll_due(now);
        assert!(app
            .list_poll_deadline()
            .is_some_and(|deadline| deadline <= now));
    }

    #[test]
    fn mark_list_poll_due_defers_while_in_flight_instead_of_overlapping() {
        let mut app = test_app(&crate::config::Config::default());
        app.list_poll_in_flight = true;
        app.mark_list_poll_due(Instant::now());
        assert!(app.list_poll_due_after_in_flight);
    }

    #[test]
    fn due_after_in_flight_fires_once_the_in_flight_poll_completes() {
        let mut app = test_app(&crate::config::Config::default());
        app.list_poll_in_flight = true;
        app.mark_list_poll_due(Instant::now());

        app.handle_list_section_polled(
            ListPollIdentity {
                generation: app.list_poll_generation,
            },
            ListPollOutcome::Success(ParsedPayload::default()),
        );

        assert!(!app.list_poll_in_flight);
        assert!(!app.list_poll_due_after_in_flight);
        assert!(app
            .list_poll_deadline()
            .is_some_and(|deadline| deadline <= Instant::now()));
    }

    // -- identity / generation ----------------------------------------------

    #[test]
    fn stale_identity_result_is_discarded() {
        let mut app = test_app(&crate::config::Config::default());
        app.list_poll_in_flight = true;
        let launched = ListPollIdentity {
            generation: app.list_poll_generation,
        };
        // Something (mode toggle, config reload) bumped the generation
        // underneath the in-flight poll.
        app.list_poll_generation = app.list_poll_generation.wrapping_add(1);

        app.handle_list_section_polled(
            launched,
            ListPollOutcome::Success(ParsedPayload {
                title: Some("stale".to_string()),
                ..Default::default()
            }),
        );

        assert!(!app.list_poll_in_flight);
        assert_eq!(app.state.jobs.title, None);
    }

    /// Finding 12: toggling live -> history -> live while a poll is in
    /// flight must NOT let the stale result compare equal just because the
    /// mode string ended up back where it started -- the generation must
    /// have moved on regardless.
    #[test]
    fn a_mode_round_trip_back_to_the_original_value_still_discards_the_stale_result() {
        let mut app = test_app(&crate::config::Config::default());
        app.list_poll_in_flight = true;
        let launched = ListPollIdentity {
            generation: app.list_poll_generation,
        };
        // live -> history -> live: two generation bumps, ending on the same
        // mode string the poll originally launched under.
        app.list_poll_generation = app.list_poll_generation.wrapping_add(1);
        app.list_poll_generation = app.list_poll_generation.wrapping_add(1);

        app.handle_list_section_polled(
            launched,
            ListPollOutcome::Success(ParsedPayload {
                title: Some("stale".to_string()),
                ..Default::default()
            }),
        );

        assert_eq!(
            app.state.jobs.title, None,
            "a result launched two generations ago must be discarded even if mode round-tripped"
        );
    }

    #[test]
    fn matching_identity_result_is_applied() {
        let mut app = test_app(&crate::config::Config::default());
        let identity = ListPollIdentity {
            generation: app.list_poll_generation,
        };

        app.handle_list_section_polled(
            identity,
            ListPollOutcome::Success(ParsedPayload {
                title: Some("JOBS".to_string()),
                ..Default::default()
            }),
        );

        assert_eq!(app.state.jobs.title.as_deref(), Some("JOBS"));
    }

    // -- result state ---------------------------------------------------------

    #[test]
    fn valid_empty_result_clears_rows() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.jobs.groups = vec![ParsedGroup {
            id: "running".to_string(),
            label: "Running".to_string(),
            rows: vec![sample_row("1")],
        }];
        app.state.jobs.is_stale = true;

        app.apply_list_poll_outcome(
            ListPollOutcome::Success(ParsedPayload::default()),
            Instant::now(),
        );

        assert!(app.state.jobs.groups.is_empty());
        assert!(!app.state.jobs.is_stale);
    }

    #[test]
    fn failure_retains_prior_rows_and_marks_stale() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.jobs.groups = vec![ParsedGroup {
            id: "running".to_string(),
            label: "Running".to_string(),
            rows: vec![sample_row("1")],
        }];

        app.apply_list_poll_outcome(
            ListPollOutcome::Failed("timed out after 5s".to_string()),
            Instant::now(),
        );

        assert_eq!(app.state.jobs.groups.len(), 1);
        assert!(app.state.jobs.is_stale);
        assert_eq!(
            app.state.jobs.last_error.as_deref(),
            Some("timed out after 5s")
        );
    }

    #[test]
    fn success_clears_a_previous_error_and_staleness() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.jobs.is_stale = true;
        app.state.jobs.last_error = Some("boom".to_string());

        app.apply_list_poll_outcome(
            ListPollOutcome::Success(ParsedPayload::default()),
            Instant::now(),
        );

        assert!(!app.state.jobs.is_stale);
        assert_eq!(app.state.jobs.last_error, None);
    }

    #[test]
    fn a_vanished_selection_is_cleared_when_new_rows_land() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.jobs.selected_row_id = Some("gone".to_string());

        app.apply_list_poll_outcome(
            ListPollOutcome::Success(ParsedPayload {
                groups: vec![ParsedGroup {
                    id: "running".to_string(),
                    label: "Running".to_string(),
                    rows: vec![sample_row("still-here")],
                }],
                ..Default::default()
            }),
            Instant::now(),
        );

        assert_eq!(app.state.jobs.selected_row_id, None);
    }

    #[test]
    fn a_surviving_selection_is_kept_when_new_rows_land() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.jobs.selected_row_id = Some("still-here".to_string());

        app.apply_list_poll_outcome(
            ListPollOutcome::Success(ParsedPayload {
                groups: vec![ParsedGroup {
                    id: "running".to_string(),
                    label: "Running".to_string(),
                    rows: vec![sample_row("still-here")],
                }],
                ..Default::default()
            }),
            Instant::now(),
        );

        assert_eq!(
            app.state.jobs.selected_row_id.as_deref(),
            Some("still-here")
        );
    }

    // -- last_success_label ---------------------------------------------------

    #[test]
    fn label_is_none_before_any_success() {
        assert_eq!(format_last_success_label(None, Instant::now()), None);
    }

    #[test]
    fn label_formats_seconds_minutes_hours() {
        let now = Instant::now();
        assert_eq!(
            format_last_success_label(Some(now), now),
            Some("just now".to_string())
        );
        assert_eq!(
            format_last_success_label(Some(now - Duration::from_secs(5)), now),
            Some("5s ago".to_string())
        );
        assert_eq!(
            format_last_success_label(Some(now - Duration::from_secs(125)), now),
            Some("2m ago".to_string())
        );
        assert_eq!(
            format_last_success_label(Some(now - Duration::from_secs(7300)), now),
            Some("2h ago".to_string())
        );
    }

    // -- panic safety -----------------------------------------------------------

    #[test]
    fn worker_panic_is_converted_to_a_failed_outcome() {
        let outcome = catch_worker_panic(|| panic!("boom"));
        assert!(matches!(outcome, ListPollOutcome::Failed(_)));
    }

    // -- read_capped ------------------------------------------------------------

    #[test]
    fn read_capped_reads_everything_under_the_cap() {
        let data = b"hello world".to_vec();
        let (bytes, truncated) =
            read_capped(std::io::Cursor::new(data.clone()), 1024).expect("read should succeed");
        assert_eq!(bytes, data);
        assert!(!truncated);
    }

    #[test]
    fn read_capped_truncates_and_flags_when_over_the_cap() {
        let data = vec![b'x'; 100];
        let (bytes, truncated) =
            read_capped(std::io::Cursor::new(data), 10).expect("read should succeed");
        assert_eq!(bytes.len(), 10);
        assert!(truncated);
    }

    /// Finding 10: a genuine I/O error must not be swallowed into a clean
    /// EOF -- the caller has to be able to tell "stream ended" from "stream
    /// broke partway through".
    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("simulated read failure"))
        }
    }

    #[test]
    fn read_capped_propagates_io_errors_instead_of_treating_them_as_eof() {
        assert!(read_capped(FailingReader, 1024).is_err());
    }

    // -- ChildGroupGuard / shutdown race --------------------------------------

    fn empty_child_slot() -> ListPollChildSlot {
        Arc::new(Mutex::new(ListPollShared::default()))
    }

    /// Finding 2: "a similar race if shutdown happens before the worker
    /// records its PID" -- once shutdown is requested, a worker that hasn't
    /// registered yet must not be allowed to register afterward.
    #[test]
    fn child_group_guard_register_fails_once_shutdown_is_requested() {
        let shared = empty_child_slot();
        {
            let mut guard = shared.lock().expect("lock");
            guard.shutting_down = true;
        }
        assert!(ChildGroupGuard::register(&shared, 999_999).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn child_group_guard_drop_kills_the_group_and_clears_registration() {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let shared = empty_child_slot();
        let guard = ChildGroupGuard::register(&shared, pid).expect("register");
        assert_eq!(shared.lock().expect("lock").pid, Some(pid));

        drop(guard);

        assert_eq!(shared.lock().expect("lock").pid, None);
        // Reap so a killed-but-unreaped zombie doesn't make the liveness
        // check below see it as still alive (`kill(pid, 0)` succeeds against
        // a zombie until it's waited on).
        let _ = child.wait();
        let still_alive = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
        assert!(!still_alive, "dropping the guard should kill the group");
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_list_poll_prevents_a_racing_registration_afterward() {
        let mut app = test_app(&crate::config::Config::default());
        app.shutdown_list_poll();
        assert!(ChildGroupGuard::register(&app.list_poll_child, 999_999).is_err());
    }

    // -- subprocess integration (Unix-only: process groups, /bin/sh) ------------

    #[cfg(unix)]
    #[test]
    fn run_list_poll_parses_valid_provider_output() {
        let json = r#"{"version":1,"title":"JOBS","groups":[]}"#;
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo '{json}'"),
        ];
        let outcome = run_list_poll(&argv, Duration::from_secs(5), &empty_child_slot(), &[]);
        match outcome {
            ListPollOutcome::Success(payload) => {
                assert_eq!(payload.title.as_deref(), Some("JOBS"));
            }
            ListPollOutcome::Failed(err) => panic!("expected success, got {err}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_list_poll_fails_on_non_zero_exit() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 7".to_string(),
        ];
        let outcome = run_list_poll(&argv, Duration::from_secs(5), &empty_child_slot(), &[]);
        assert!(matches!(outcome, ListPollOutcome::Failed(_)));
    }

    #[cfg(unix)]
    #[test]
    fn run_list_poll_fails_on_spawn_error() {
        let argv = vec!["/this/does/not/exist-herdr-poll-test".to_string()];
        let outcome = run_list_poll(&argv, Duration::from_secs(5), &empty_child_slot(), &[]);
        assert!(matches!(outcome, ListPollOutcome::Failed(_)));
    }

    /// Finding 2's headline scenario: the leader exits 0 immediately but
    /// forks a descendant that inherits stdout and outlives it. Without
    /// killing the remaining process group before joining the pipe readers,
    /// this poll would block until the descendant exits (here, 30s) instead
    /// of completing promptly.
    #[cfg(unix)]
    #[test]
    fn a_leaked_descendant_holding_stdout_open_does_not_wedge_the_poll() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo done; (sleep 30) &".to_string(),
        ];
        let start = Instant::now();
        let outcome = run_list_poll(&argv, Duration::from_secs(5), &empty_child_slot(), &[]);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "poll should not block on a leaked descendant's inherited stdout, took {:?}",
            start.elapsed()
        );
        // "done" isn't valid JSON, so parsing fails -- what matters here is
        // that the poll returned promptly at all rather than wedging.
        assert!(matches!(outcome, ListPollOutcome::Failed(_)));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_whole_process_group() {
        let pid_file = std::env::temp_dir().join(format!(
            "herdr-list-poll-pgid-test-{}-{}",
            std::process::id(),
            "timeout_kills_the_whole_process_group"
        ));
        let _ = std::fs::remove_file(&pid_file);
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("sleep 30 & echo $! > {} ; wait", pid_file.display()),
        ];

        let outcome = run_list_poll(&argv, Duration::from_millis(200), &empty_child_slot(), &[]);
        assert!(matches!(outcome, ListPollOutcome::Failed(ref msg) if msg.contains("timed out")));

        // The grandchild `sleep 30`'s pid was written before the timeout;
        // read it (briefly retrying for scheduler jitter) and confirm the
        // whole group -- not just the immediate `sh` -- was killed.
        let mut grandchild_pid = None;
        for _ in 0..50 {
            if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    grandchild_pid = Some(pid);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_file(&pid_file);
        let grandchild_pid = grandchild_pid.expect("child should have recorded the grandchild pid");

        // Give the kill a moment to actually reap the grandchild.
        let mut still_alive = true;
        for _ in 0..50 {
            // kill(pid, 0) checks liveness without sending a real signal.
            let alive = unsafe { libc::kill(grandchild_pid, 0) == 0 };
            if !alive {
                still_alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !still_alive,
            "grandchild sleep should have been killed along with the process group"
        );
    }
}
