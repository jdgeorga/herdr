//! Jobs sidebar action dispatch (design doc: "Actions").
//!
//! Entry point is [`App::choose_list_action`], called when the user picks an
//! item off the Jobs row context menu. It resolves argv/cwd against the row
//! **once**, at menu-selection time, and either opens
//! `Mode::ConfirmListAction` (when the action's config has a `confirm`
//! template) or runs the action immediately. Confirmed actions run on Enter
//! via [`App::submit_list_action_confirm`]. Either way, execution itself goes
//! through [`App::spawn_list_action_command`] (background target) or
//! [`App::spawn_overlay_argv_command`] (overlay target, e.g. `tail`).
//!
//! `std::process::Command` only: tokio's `process` feature is deliberately
//! not enabled (`Cargo.toml`), matching `list_refresh.rs`'s poller.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use regex::Regex;

use super::{App, Mode};
use crate::app::state::{ListActionConfirmState, ToastKind, ToastNotification};
use crate::config::{ActionTarget, ListActionConfig};
use crate::events::AppEvent;
use crate::list_section::protocol::{ParsedGroup, ParsedRow};
use crate::list_section::substitute::{resolve_argv, validate_path, validate_token, RowContext, SubstError};

impl App {
    /// Resolves and either confirms or runs a Jobs sidebar action, chosen off
    /// the row context menu (design doc: "Actions"). `action_id` is the
    /// `ui.sidebar.list.actions[].id` the menu item maps to.
    ///
    /// Everything here is resolved against the row **as it exists right
    /// now** and frozen: this function is the only place that reads
    /// `self.state.jobs.groups` for an action. Nothing downstream
    /// (`submit_list_action_confirm`, the background thread) looks the row
    /// up again -- re-resolving after confirmation would be a
    /// time-of-check/time-of-use bug (the row may be gone, its id reused, or
    /// the config reloaded by then).
    pub(crate) fn choose_list_action(&mut self, row_id: &str, action_id: &str) {
        let Some(row) = find_job_row(&self.state.jobs.groups, row_id) else {
            self.refuse_list_action("job is no longer listed");
            return;
        };
        let Some(config) = self
            .state
            .sidebar_list
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .cloned()
        else {
            self.refuse_list_action("action is not configured");
            return;
        };

        let row_ctx = row_context(row);
        let mode = self.state.jobs.mode.clone();

        let argv = match resolve_argv(&config.command, &row_ctx, &mode) {
            Ok(argv) => argv,
            Err(err) => {
                self.refuse_list_action(&err.to_string());
                return;
            }
        };
        if let Err(err) = validate_list_action_fields(&config, &row_ctx, &mode) {
            self.refuse_list_action(&err.to_string());
            return;
        }
        // Design doc's Security section: "{log} and {dir} must be absolute,
        // canonicalized, and existing" -- checked unconditionally, not just
        // for actions that reference them, since "tail will happily display
        // any file the user can read".
        if let Err(err) = validate_list_row_paths(&row_ctx) {
            self.refuse_list_action(&err.to_string());
            return;
        }
        let cwd = match config
            .cwd
            .as_deref()
            .map(|template| resolve_single(template, &row_ctx, &mode))
            .transpose()
        {
            Ok(cwd) => cwd.map(PathBuf::from),
            Err(err) => {
                self.refuse_list_action(&err.to_string());
                return;
            }
        };

        if let Some(prompt_template) = &config.confirm {
            let prompt = match resolve_single(prompt_template, &row_ctx, &mode) {
                Ok(prompt) => prompt,
                Err(err) => {
                    self.refuse_list_action(&err.to_string());
                    return;
                }
            };
            self.next_list_action_generation += 1;
            self.state.list_action_confirm = Some(ListActionConfirmState {
                action_id: config.id.clone(),
                label: config.label.clone(),
                argv,
                cwd,
                prompt,
                generation: self.next_list_action_generation,
                in_progress: false,
                error: None,
            });
            self.state.mode = Mode::ConfirmListAction;
            return;
        }

        match config.target {
            ActionTarget::Background => {
                self.next_list_action_generation += 1;
                let generation = self.next_list_action_generation;
                self.spawn_list_action_command(generation, config.label.clone(), argv, cwd);
                self.leave_modal_after_list_action();
            }
            ActionTarget::Overlay => {
                self.launch_overlay_list_action(config.label.clone(), argv, cwd);
            }
        }
    }

    /// Enter on `Mode::ConfirmListAction` (design doc: "Confirmation is
    /// Enter to accept, Escape to cancel").
    pub(crate) fn handle_list_action_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self
                    .state
                    .list_action_confirm
                    .as_ref()
                    .is_some_and(|confirm| confirm.in_progress)
                {
                    return;
                }
                self.state.list_action_confirm = None;
                self.leave_modal_after_list_action();
            }
            KeyCode::Enter => self.submit_list_action_confirm(),
            _ => {}
        }
    }

    /// Runs the frozen `argv`/`cwd` from the open confirm dialog (design
    /// doc: "Background target: run via std::process in a thread, then
    /// force an immediate re-poll and toast the result"). The dialog's
    /// `target` is always background: a `confirm` prompt implies a
    /// fire-and-report command, not an interactive overlay pane.
    pub(crate) fn submit_list_action_confirm(&mut self) {
        let Some(confirm) = self.state.list_action_confirm.as_mut() else {
            return;
        };
        if confirm.in_progress {
            return;
        }
        confirm.in_progress = true;
        confirm.error = None;
        let generation = confirm.generation;
        let label = confirm.label.clone();
        let argv = confirm.argv.clone();
        let cwd = confirm.cwd.clone();
        self.spawn_list_action_command(generation, label, argv, cwd);
    }

    /// Applies a completed `AppEvent::ListActionFinished`. Closes the confirm
    /// dialog only if it's still the one that launched this command (design
    /// doc's `generation` field); either way, toasts the result and forces
    /// an immediate re-poll so a successful `cancel`/etc. is reflected
    /// without waiting out the refresh interval.
    pub(crate) fn handle_list_action_finished(
        &mut self,
        generation: u64,
        label: String,
        result: Result<(), String>,
    ) {
        if self
            .state
            .list_action_confirm
            .as_ref()
            .is_some_and(|confirm| confirm.generation == generation)
        {
            self.state.list_action_confirm = None;
            self.leave_modal_after_list_action();
        }

        let previous_toast = self.state.toast.clone();
        self.state.toast = Some(match result {
            Ok(()) => ToastNotification {
                kind: ToastKind::Finished,
                title: label,
                context: "done".to_string(),
                position: None,
                target: None,
            },
            Err(err) => ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: label,
                context: err,
                position: None,
                target: None,
            },
        });
        self.sync_toast_deadline(previous_toast);
        self.mark_list_poll_due(Instant::now());
    }

    fn spawn_list_action_command(
        &mut self,
        generation: u64,
        label: String,
        argv: Vec<String>,
        cwd: Option<PathBuf>,
    ) {
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = run_list_action_command(&argv, cwd.as_deref());
            let _ = event_tx.blocking_send(AppEvent::ListActionFinished {
                generation,
                label,
                result,
            });
        });
    }

    /// `target = "overlay"` (design doc: "Tail-log uses
    /// `spawn_overlay_argv_command`... the caller must integrate the
    /// returned `NewPane` as the scrollback caller does").
    fn launch_overlay_list_action(&mut self, label: String, argv: Vec<String>, cwd: Option<PathBuf>) {
        match self.spawn_overlay_argv_command(&argv, cwd, Vec::new(), Vec::new()) {
            Ok((_, new_pane)) => {
                let terminal_id = new_pane.terminal.id.clone();
                self.terminal_runtimes
                    .insert(terminal_id.clone(), new_pane.runtime);
                self.state
                    .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
                self.state.terminals.insert(terminal_id, new_pane.terminal);
            }
            Err(err) => {
                self.refuse_list_action(&format!("failed to open {label}: {err}"));
            }
        }
    }

    /// Toasts a refusal (design doc: "An action that fails validation is
    /// refused with a toast, never executed") and closes whatever modal
    /// (context menu or confirm dialog) triggered the attempt.
    fn refuse_list_action(&mut self, message: &str) {
        let previous_toast = self.state.toast.clone();
        self.state.toast = Some(ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: "action refused".to_string(),
            context: message.to_string(),
            position: None,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
        self.state.list_action_confirm = None;
        self.leave_modal_after_list_action();
    }

    /// `app::input::modal::leave_modal` is `pub(super)` to `app::input`;
    /// this module lives outside it (mirroring `worktrees.rs`, which
    /// inlines the same fallback rather than reaching into that module).
    fn leave_modal_after_list_action(&mut self) {
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }
}

fn find_job_row<'a>(groups: &'a [ParsedGroup], row_id: &str) -> Option<&'a ParsedRow> {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .find(|row| row.id == row_id)
}

fn row_context(row: &ParsedRow) -> RowContext {
    RowContext {
        id: row.id.clone(),
        cells: row.cells.clone(),
        vars: row
            .vars
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    }
}

/// Resolves a single (non-argv) template string, such as an action's `cwd`
/// or `confirm` prompt, by reusing `resolve_argv`'s tested expansion: a
/// throwaway `argv[0]` absorbs the "no substitution in position 0" rule
/// (irrelevant here, since it's discarded), so only `template` itself is
/// ever expanded.
fn resolve_single(template: &str, row: &RowContext, mode: &str) -> Result<String, SubstError> {
    let resolved = resolve_argv(&["_".to_string(), template.to_string()], row, mode)?;
    Ok(resolved.into_iter().nth(1).unwrap_or_default())
}

/// Per-action `validate` regexes (design doc: "Per-action validation: `{id}`
/// for `cancel` must match... Not one generic regex for everything"). Each
/// key names a token (`id`, `cellN`, or a `vars` name); the value is a regex
/// the resolved value must match.
fn validate_list_action_fields(
    config: &ListActionConfig,
    row: &RowContext,
    mode: &str,
) -> Result<(), SubstError> {
    for (field, pattern) in &config.validate {
        let value = resolve_single(&format!("{{{field}}}"), row, mode)?;
        let regex = Regex::new(pattern).map_err(|_| SubstError::ValidationFailed {
            value: value.clone(),
        })?;
        validate_token(&value, &regex)?;
    }
    Ok(())
}

/// Design doc's Security section: "{log} and {dir} must be absolute,
/// canonicalized, and existing."
fn validate_list_row_paths(row: &RowContext) -> Result<(), SubstError> {
    for name in ["log", "dir"] {
        if let Some(value) = row.vars.get(name) {
            validate_path(value)?;
        }
    }
    Ok(())
}

/// Runs a resolved action command to completion, blocking the calling
/// (detached) thread. No timeout or process-group handling: the design doc
/// only requires those for the polling subprocess ("Subprocess
/// requirements", under "Polling"), not for one-shot actions like `scancel`.
fn run_list_action_command(argv: &[String], cwd: Option<&std::path::Path>) -> Result<(), String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("action command is empty".to_string());
    };
    let mut command = Command::new(program);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    let output = match command.output() {
        Ok(output) => output,
        Err(err) => return Err(format!("failed to run: {err}")),
    };
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        Err(format!("exited with {}", output.status))
    } else {
        Err(stderr.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list_section::protocol::RowStyle;
    use std::collections::BTreeMap;

    fn row(id: &str, cells: &[&str], vars: &[(&str, &str)]) -> ParsedRow {
        ParsedRow {
            id: id.to_string(),
            cells: cells.iter().map(|c| c.to_string()).collect(),
            style: RowStyle::Normal,
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            actions: vec![],
        }
    }

    #[test]
    fn find_job_row_searches_across_groups() {
        let groups = vec![
            ParsedGroup {
                id: "running".to_string(),
                label: "Running".to_string(),
                rows: vec![row("1", &[], &[])],
            },
            ParsedGroup {
                id: "queued".to_string(),
                label: "Queued".to_string(),
                rows: vec![row("2", &[], &[])],
            },
        ];
        assert_eq!(find_job_row(&groups, "2").map(|r| r.id.as_str()), Some("2"));
        assert!(find_job_row(&groups, "missing").is_none());
    }

    #[test]
    fn resolve_single_expands_a_scalar_template() {
        let ctx = RowContext {
            id: "55241874".to_string(),
            cells: vec!["ued".to_string()],
            vars: [("dir".to_string(), "/pscratch/sd/j/jdgeorga/ued".to_string())]
                .into_iter()
                .collect(),
        };
        assert_eq!(
            resolve_single("{dir}", &ctx, "live").unwrap(),
            "/pscratch/sd/j/jdgeorga/ued"
        );
        assert_eq!(
            resolve_single("cancel {id} ({cell0})?", &ctx, "live").unwrap(),
            "cancel 55241874 (ued)?"
        );
    }

    #[test]
    fn validate_list_action_fields_accepts_matching_job_id() {
        let config = ListActionConfig {
            id: "cancel".to_string(),
            validate: [(
                "id".to_string(),
                r"^[0-9]+(_[0-9]+)?(\+[0-9]+)?$".to_string(),
            )]
            .into_iter()
            .collect(),
            ..ListActionConfig::default()
        };
        let ctx = RowContext {
            id: "55241874".to_string(),
            cells: vec![],
            vars: Default::default(),
        };
        assert!(validate_list_action_fields(&config, &ctx, "live").is_ok());
    }

    #[test]
    fn validate_list_action_fields_rejects_adversarial_job_id() {
        let config = ListActionConfig {
            id: "cancel".to_string(),
            validate: [(
                "id".to_string(),
                r"^[0-9]+(_[0-9]+)?(\+[0-9]+)?$".to_string(),
            )]
            .into_iter()
            .collect(),
            ..ListActionConfig::default()
        };
        let ctx = RowContext {
            id: "-A".to_string(),
            cells: vec![],
            vars: Default::default(),
        };
        assert!(matches!(
            validate_list_action_fields(&config, &ctx, "live"),
            Err(SubstError::ValidationFailed { .. })
        ));
    }

    #[test]
    fn validate_list_row_paths_rejects_relative_log() {
        let ctx = RowContext {
            id: "1".to_string(),
            cells: vec![],
            vars: [("log".to_string(), "slurm-1.out".to_string())]
                .into_iter()
                .collect(),
        };
        assert!(matches!(
            validate_list_row_paths(&ctx),
            Err(SubstError::PathNotAbsolute { .. })
        ));
    }

    #[test]
    fn validate_list_row_paths_accepts_absent_vars() {
        let ctx = RowContext {
            id: "1".to_string(),
            cells: vec![],
            vars: Default::default(),
        };
        assert!(validate_list_row_paths(&ctx).is_ok());
    }

    #[test]
    fn run_list_action_command_reports_nonzero_exit() {
        let argv = vec!["false".to_string()];
        let err = run_list_action_command(&argv, None).unwrap_err();
        assert!(err.contains("exited"));
    }

    #[test]
    fn run_list_action_command_reports_success() {
        let argv = vec!["true".to_string()];
        assert!(run_list_action_command(&argv, None).is_ok());
    }

    #[test]
    fn run_list_action_command_surfaces_stderr_on_failure() {
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo boom >&2; exit 1".to_string(),
        ];
        let err = run_list_action_command(&argv, None).unwrap_err();
        assert_eq!(err, "boom");
    }

    // -- App::choose_list_action end-to-end -----------------------------

    fn test_app_with_job(action: ListActionConfig, job_row: ParsedRow) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.sidebar_list.actions = vec![action];
        app.state.jobs.groups = vec![ParsedGroup {
            id: "running".to_string(),
            label: "Running".to_string(),
            rows: vec![job_row],
        }];
        // Simulates the precondition when a context-menu item is chosen.
        app.state.mode = Mode::ContextMenu;
        app
    }

    fn cancel_action_config() -> ListActionConfig {
        ListActionConfig {
            id: "cancel".to_string(),
            label: "Cancel job".to_string(),
            command: vec!["scancel".to_string(), "--".to_string(), "{id}".to_string()],
            confirm: Some("Cancel job {id} ({cell0})?".to_string()),
            validate: [(
                "id".to_string(),
                r"^[0-9]+(_[0-9]+)?(\+[0-9]+)?$".to_string(),
            )]
            .into_iter()
            .collect(),
            ..ListActionConfig::default()
        }
    }

    #[test]
    fn choose_list_action_opens_confirm_dialog_with_frozen_argv() {
        let mut app = test_app_with_job(cancel_action_config(), row("55241874", &["ued"], &[]));

        app.choose_list_action("55241874", "cancel");

        assert_eq!(app.state.mode, Mode::ConfirmListAction);
        let confirm = app
            .state
            .list_action_confirm
            .as_ref()
            .expect("confirm dialog opened");
        assert_eq!(confirm.argv, vec!["scancel", "--", "55241874"]);
        assert_eq!(confirm.prompt, "Cancel job 55241874 (ued)?");
        assert!(!confirm.in_progress);
        assert!(confirm.error.is_none());
    }

    #[test]
    fn choose_list_action_refuses_a_row_that_has_vanished() {
        let mut app = test_app_with_job(cancel_action_config(), row("1", &[], &[]));

        app.choose_list_action("missing", "cancel");

        assert!(app.state.list_action_confirm.is_none());
        assert_ne!(app.state.mode, Mode::ContextMenu);
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("job is no longer listed")
        );
    }

    #[test]
    fn choose_list_action_refuses_an_unconfigured_action_id() {
        let mut app = test_app_with_job(cancel_action_config(), row("1", &[], &[]));

        app.choose_list_action("1", "tail");

        assert!(app.state.list_action_confirm.is_none());
        assert_ne!(app.state.mode, Mode::ContextMenu);
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.context.as_str()),
            Some("action is not configured")
        );
    }

    #[test]
    fn choose_list_action_refuses_an_adversarial_job_id_without_executing() {
        let mut app = test_app_with_job(cancel_action_config(), row("-A", &[], &[]));

        app.choose_list_action("-A", "cancel");

        assert!(app.state.list_action_confirm.is_none());
        assert_ne!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.toast.is_some());
    }

    #[test]
    fn choose_list_action_refuses_a_non_absolute_log_path() {
        let action = ListActionConfig {
            id: "tail".to_string(),
            label: "Tail log".to_string(),
            command: vec![
                "tail".to_string(),
                "-f".to_string(),
                "--".to_string(),
                "{log}".to_string(),
            ],
            target: ActionTarget::Overlay,
            ..ListActionConfig::default()
        };
        let mut app =
            test_app_with_job(action, row("1", &[], &[("log", "relative/slurm-1.out")]));

        app.choose_list_action("1", "tail");

        assert_ne!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.toast.is_some());
    }
}
