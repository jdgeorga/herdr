//! Bridges Jobs sidebar provider `notify` entries onto the single-slot
//! `AppState.toast` (design doc: "Notifications").
//!
//! `AppState.toast` is one `Option<ToastNotification>` and `ToastKind` has
//! three variants, none general-purpose (design doc, "Notifications"). A
//! provider can report several events per poll, so entries are queued into a
//! bounded FIFO and drained into the toast slot as it frees, deduplicated by
//! `notify[].id` so a provider that keeps reporting the same event doesn't
//! re-toast it.

use std::collections::{HashSet, VecDeque};

use super::App;
use crate::app::state::{ToastKind, ToastNotification};
use crate::list_section::protocol::{NotifyLevel, ParsedNotify};

/// Cap on queued-but-not-yet-shown notifications (design doc: "a bounded
/// FIFO, cap 8, oldest dropped").
const MAX_QUEUED_LIST_NOTIFICATIONS: usize = 8;

/// Cap on remembered `notify[].id`s (finding: an unbounded `HashSet` here
/// retains one string per id forever -- at a ~10s cadence that's roughly
/// 8,640 polls/day, and any provider that ever mints a fresh id per poll
/// (even just for its own steady-state jobs, not only completions) grows
/// this without bound for the life of the process). 512 comfortably covers
/// the realistic case (the protocol caps a single poll at 2000 rows, and a
/// provider's own 10-minute linger window means any one id is at most
/// repeated across ~60 polls at the default 10s cadence) while bounding
/// memory for a session that runs for days.
const MAX_REMEMBERED_LIST_NOTIFY_IDS: usize = 512;

/// Insertion-ordered, capacity-bounded id set (design doc finding: "a
/// bounded LRU or prune once an id is no longer present for longer than the
/// provider's linger window"). Evicts the oldest-inserted id once `cap` is
/// exceeded, which is safe here specifically because the property this set
/// protects -- "don't re-toast an id the provider keeps repeating" -- only
/// needs to hold across a provider's own linger window, and eviction only
/// discards ids old enough that hundreds of newer ones have been seen since.
#[derive(Debug, Clone)]
pub struct BoundedIdSet {
    order: VecDeque<String>,
    members: HashSet<String>,
    cap: usize,
}

impl BoundedIdSet {
    pub fn new(cap: usize) -> Self {
        Self {
            order: VecDeque::new(),
            members: HashSet::new(),
            cap: cap.max(1),
        }
    }

    /// Inserts `id`, evicting the oldest entry first if already at capacity.
    /// Returns `true` if `id` was newly inserted (i.e. not already present).
    pub fn insert(&mut self, id: String) -> bool {
        if self.members.contains(&id) {
            return false;
        }
        if self.order.len() >= self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.members.remove(&oldest);
            }
        }
        self.order.push_back(id.clone());
        self.members.insert(id);
        true
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    #[cfg(test)]
    pub fn contains(&self, id: &str) -> bool {
        self.members.contains(id)
    }
}

impl Default for BoundedIdSet {
    fn default() -> Self {
        Self::new(MAX_REMEMBERED_LIST_NOTIFY_IDS)
    }
}

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

    // -- BoundedIdSet ------------------------------------------------------

    #[test]
    fn bounded_id_set_reports_new_vs_already_seen() {
        let mut set = BoundedIdSet::new(4);
        assert!(set.insert("a".to_string()));
        assert!(!set.insert("a".to_string()));
        assert_eq!(set.len(), 1);
        assert!(set.contains("a"));
    }

    #[test]
    fn bounded_id_set_evicts_oldest_once_at_capacity() {
        let mut set = BoundedIdSet::new(3);
        for id in ["a", "b", "c"] {
            assert!(set.insert(id.to_string()));
        }
        assert_eq!(set.len(), 3);

        // Over capacity: "a" (oldest) is evicted to make room for "d".
        assert!(set.insert("d".to_string()));
        assert_eq!(set.len(), 3);
        assert!(!set.contains("a"));
        assert!(set.contains("b"));
        assert!(set.contains("c"));
        assert!(set.contains("d"));

        // Evicted ids are treated as new again -- a provider id that ages
        // out and is (implausibly) reused would toast once more, which is
        // an acceptable cosmetic cost for bounding memory.
        assert!(set.insert("a".to_string()));
    }

    #[test]
    fn list_notify_seen_stays_bounded_across_many_distinct_ids() {
        let mut app = test_app();
        let entries: Vec<ParsedNotify> = (0..MAX_REMEMBERED_LIST_NOTIFY_IDS + 200)
            .map(|i| notify(&format!("done-{i}"), NotifyLevel::Ok, "x"))
            .collect();
        app.enqueue_list_notifications(&entries);
        assert_eq!(
            app.state.list_notify_seen.len(),
            MAX_REMEMBERED_LIST_NOTIFY_IDS
        );
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
