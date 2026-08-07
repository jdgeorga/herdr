//! Bridges Jobs sidebar provider `notify` entries onto the single-slot
//! `AppState.toast` (design doc: "Notifications").
//!
//! `AppState.toast` is one `Option<ToastNotification>` and `ToastKind` has
//! three variants, none general-purpose (design doc, "Notifications"). A
//! provider can report several events per poll, so entries are queued into a
//! bounded FIFO and drained into the toast slot as it frees, deduplicated by
//! `notify[].id` so a provider that keeps reporting the same event doesn't
//! re-toast it.

use super::App;
use crate::app::state::{ToastKind, ToastNotification};
use crate::list_section::protocol::{NotifyLevel, ParsedNotify};

/// Cap on queued-but-not-yet-shown notifications (design doc: "a bounded
/// FIFO, cap 8, oldest dropped").
const MAX_QUEUED_LIST_NOTIFICATIONS: usize = 8;

impl App {
    /// Queues newly-seen `notify` entries from a completed poll, then
    /// immediately tries to fill the toast slot if it's free. Entries whose
    /// id has already been queued or shown are skipped.
    pub(crate) fn enqueue_list_notifications(&mut self, notify: &[ParsedNotify]) {
        for entry in notify {
            if !self.state.list_notify_seen.insert(entry.id.clone()) {
                continue;
            }
            if self.state.list_notify_queue.len() >= MAX_QUEUED_LIST_NOTIFICATIONS {
                self.state.list_notify_queue.pop_front();
            }
            self.state.list_notify_queue.push_back(entry.clone());
        }
        self.drain_list_notify_queue();
    }

    /// Pops the next queued notification into `state.toast` if the slot is
    /// currently free. Returns whether it did. Called right after enqueueing
    /// and on every scheduled-tasks tick, so a toast that expires on its own
    /// deadline picks up whatever is queued next (design doc: "drained into
    /// that slot as it frees").
    pub(crate) fn drain_list_notify_queue(&mut self) -> bool {
        if self.state.toast.is_some() {
            return false;
        }
        let Some(entry) = self.state.list_notify_queue.pop_front() else {
            return false;
        };
        let previous_toast = self.state.toast.clone();
        self.state.toast = Some(ToastNotification {
            kind: toast_kind_for_notify_level(entry.level),
            title: notify_level_title(entry.level).to_string(),
            context: entry.text,
            position: None,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
        true
    }
}

/// Maps a provider notify level onto one of the three existing toast kinds
/// (design doc: "Levels map onto the existing kinds; a new kind is added
/// only if none fits" -- none of `ok`/`warn`/`fail`/`info` needed one).
fn toast_kind_for_notify_level(level: NotifyLevel) -> ToastKind {
    match level {
        NotifyLevel::Fail | NotifyLevel::Warn => ToastKind::NeedsAttention,
        NotifyLevel::Ok | NotifyLevel::Info => ToastKind::Finished,
    }
}

fn notify_level_title(level: NotifyLevel) -> &'static str {
    match level {
        NotifyLevel::Fail => "job failed",
        NotifyLevel::Warn => "job warning",
        NotifyLevel::Ok => "job finished",
        NotifyLevel::Info => "jobs",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn notify(id: &str, level: NotifyLevel, text: &str) -> ParsedNotify {
        ParsedNotify {
            id: id.to_string(),
            level,
            text: text.to_string(),
        }
    }

    #[test]
    fn enqueue_fills_free_toast_slot_immediately() {
        let mut app = test_app();
        app.enqueue_list_notifications(&[notify("done-1", NotifyLevel::Ok, "1 finished")]);
        let toast = app.state.toast.as_ref().expect("toast shown");
        assert_eq!(toast.kind, ToastKind::Finished);
        assert_eq!(toast.context, "1 finished");
        assert!(app.state.list_notify_queue.is_empty());
    }

    #[test]
    fn enqueue_queues_behind_an_occupied_toast_slot() {
        let mut app = test_app();
        app.state.toast = Some(ToastNotification {
            kind: ToastKind::Finished,
            title: "unrelated".to_string(),
            context: "unrelated".to_string(),
            position: None,
            target: None,
        });
        app.enqueue_list_notifications(&[notify("done-1", NotifyLevel::Fail, "1 failed")]);
        assert_eq!(app.state.toast.as_ref().unwrap().title, "unrelated");
        assert_eq!(app.state.list_notify_queue.len(), 1);

        app.state.toast = None;
        assert!(app.drain_list_notify_queue());
        let toast = app.state.toast.as_ref().expect("drained toast");
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.context, "1 failed");
    }

    #[test]
    fn duplicate_notify_id_does_not_retoast() {
        let mut app = test_app();
        app.enqueue_list_notifications(&[notify("done-1", NotifyLevel::Ok, "first")]);
        app.state.toast = None;
        app.enqueue_list_notifications(&[notify("done-1", NotifyLevel::Ok, "first again")]);
        assert!(app.state.toast.is_none());
        assert!(app.state.list_notify_queue.is_empty());
    }

    #[test]
    fn queue_drops_oldest_past_cap() {
        let mut app = test_app();
        app.state.toast = Some(ToastNotification {
            kind: ToastKind::Finished,
            title: "occupied".to_string(),
            context: String::new(),
            position: None,
            target: None,
        });
        let entries: Vec<ParsedNotify> = (0..MAX_QUEUED_LIST_NOTIFICATIONS + 3)
            .map(|i| notify(&format!("done-{i}"), NotifyLevel::Ok, "x"))
            .collect();
        app.enqueue_list_notifications(&entries);
        assert_eq!(
            app.state.list_notify_queue.len(),
            MAX_QUEUED_LIST_NOTIFICATIONS
        );
        assert_eq!(app.state.list_notify_queue.front().unwrap().id, "done-3");
    }
}
