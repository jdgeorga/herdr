mod tokens;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use self::tokens::{ResolvedToken, ResolvedTokenKind, SpaceTokenContext};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{state_icon, state_label, state_label_color};
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::{AgentPanelSort, JobRowHit, JobsHeaderHits, JobsSectionState, Palette, SidebarLayout};
use crate::app::{AppState, Mode};
use crate::config::{ColumnAlign, ColumnSpec, ColumnWidth, ListSectionConfig};
use crate::detect::AgentState;
use crate::list_section::protocol::{ParsedGroup, ParsedRow, RowStyle};
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: Option<String>,
    pub agent_kind_label: Option<String>,
    pub agent: Option<crate::detect::Agent>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub state_labels: std::collections::HashMap<String, String>,
    pub tokens: std::collections::HashMap<String, String>,
}

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let (ws_h, detail_h) = sidebar_section_heights(content.height, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(content.height, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

fn agent_panel_sort_label(sort: AgentPanelSort) -> &'static str {
    match sort {
        AgentPanelSort::Spaces => "grouped",
        AgentPanelSort::Priority => "priority",
    }
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, sort: AgentPanelSort) -> Rect {
    agent_panel_header_label_rect(area, agent_panel_sort_label(sort))
}

fn agent_panel_header_label_rect(area: Rect, label: &str) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let width = display_width_u16(label).min(area.width);
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

fn active_agent_view_label(app: &AppState) -> Option<&str> {
    app.agent_view_override
        .as_ref()
        .map(|view| view.label.as_deref().unwrap_or("filtered"))
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn all_agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    collect_agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let mut entries = collect_agent_panel_entries_with_runtimes(app, terminal_runtimes);
    crate::app::agent_view::apply_agent_view(app, &mut entries);
    entries
}

fn collect_agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    app.workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| {
            let multi_tab = ws.tabs.len() > 1;
            let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(move |detail| {
                    let show_tab = multi_tab
                        || ws
                            .tabs
                            .get(detail.tab_idx)
                            .is_some_and(|tab| !tab.is_auto_named());
                    AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        primary_tab_label: show_tab.then_some(detail.tab_label),
                        pane_label: detail.pane_label,
                        terminal_title: detail.terminal_title,
                        terminal_title_stripped: detail.terminal_title_stripped,
                        agent_label: Some(detail.agent_label),
                        agent_kind_label: detail.agent_kind_label,
                        agent: detail.agent,
                        state: detail.state,
                        seen: detail.seen,
                        last_agent_state_change_seq: detail.last_agent_state_change_seq,
                        state_labels: detail.state_labels,
                        tokens: detail.tokens,
                    }
                })
        })
        .collect()
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn workspace_row_height(app: &AppState, ws: &crate::workspace::Workspace, indented: bool) -> u16 {
    let (state, seen) = ws.aggregate_state(&app.terminals);
    let label = if indented {
        grouped_child_display_label(
            &ws.display_name_from_terminals(&app.terminals),
            ws.branch().as_deref(),
            ws.custom_name.is_some(),
        )
    } else {
        ws.display_name_from_terminals(&app.terminals)
    };
    let token_values = ws.metadata_tokens.values();
    tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: &label,
            branch: ws.branch().as_deref(),
            state_text: state_label(state, seen),
            ahead_behind: ws.git_ahead_behind(),
            tokens: &token_values,
            suppress_git_details: indented,
        },
    )
    .len()
    .max(1)
    .min(u16::MAX as usize) as u16
}

fn workspace_row_height_in_body(
    app: &AppState,
    workspace: &crate::workspace::Workspace,
    indented: bool,
    body_height: u16,
) -> u16 {
    workspace_row_height(app, workspace, indented).min(body_height)
}

fn workspace_entry_gap(app: &AppState, entries: &[WorkspaceListEntry], entry_idx: usize) -> u16 {
    if entry_idx + 1 < entries.len() && !next_entry_is_indented_workspace(entries, entry_idx) {
        app.sidebar_spaces.row_gap
    } else {
        0
    }
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    app.workspaces
        .iter()
        .filter(|ws| ws.worktree_space().is_some_and(|space| space.key == key))
        .map(|ws| ws.aggregate_state(&app.terminals))
        .max_by_key(|(state, seen)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    if space.is_linked_worktree {
        return None;
    }
    let member_count = app
        .workspaces
        .iter()
        .filter(|ws| {
            ws.worktree_space()
                .is_some_and(|member| member.key == space.key)
        })
        .count();
    (member_count >= 2).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

pub(crate) fn grouped_child_display_label(
    label: &str,
    branch: Option<&str>,
    has_custom_name: bool,
) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace { ws_idx: usize, indented: bool },
}

pub(crate) fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = compute_expanded_sidebar_layout(area, app.sidebar_section_split, jobs_section_want(app))
        .spaces;
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    if workspace_list_entries(app).is_empty() {
        0
    } else {
        requested.min(workspace_list_bottom_start(app, ws_area))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, false)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true)
}

fn workspace_list_entries_inner(app: &AppState, force_expanded: bool) -> Vec<WorkspaceListEntry> {
    let mut members_by_key = std::collections::HashMap::<String, Vec<usize>>::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        if let Some(space) = ws.worktree_space() {
            members_by_key
                .entry(space.key.clone())
                .or_default()
                .push(ws_idx);
        }
    }
    let grouped_keys = members_by_key
        .iter()
        .filter(|(_, members)| {
            members.len() >= 2
                && members.iter().any(|idx| {
                    app.workspaces
                        .get(*idx)
                        .and_then(|ws| ws.worktree_space())
                        .is_some_and(|space| !space.is_linked_worktree)
                })
        })
        .map(|(key, _)| key.clone())
        .collect::<std::collections::HashSet<_>>();

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx.and_then(|idx| {
        app.workspaces
            .get(idx)
            .and_then(|ws| ws.worktree_space())
            .map(|space| space.key.clone())
    });

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut entries = Vec::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let Some(space) = ws
            .worktree_space()
            .filter(|space| grouped_keys.contains(&space.key))
        else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };

        if !emitted_groups.insert(space.key.clone()) {
            continue;
        }

        let Some(members) = members_by_key.get(&space.key) else {
            continue;
        };
        let Some(parent_idx) = members.iter().copied().find(|idx| {
            app.workspaces
                .get(*idx)
                .and_then(|member| member.worktree_space())
                .is_some_and(|member_space| !member_space.is_linked_worktree)
        }) else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        let collapsed = !force_expanded && app.collapsed_space_keys.contains(&space.key);
        entries.push(WorkspaceListEntry::Workspace {
            ws_idx: parent_idx,
            indented: false,
        });

        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| *idx != parent_idx)
                .filter(|_| active_group.as_deref() == Some(space.key.as_str()))
            {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: active_idx,
                    indented: true,
                });
            }
        } else {
            for member_idx in members {
                if *member_idx == parent_idx {
                    continue;
                }
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: *member_idx,
                    indented: true,
                });
            }
        }
    }
    entries
}

// Only exercised by tests now: production call sites moved to
// `compute_sidebar_layout` so Jobs geometry is never recomputed with a
// different formula. Kept (unchanged) because pre-existing tests call it
// directly -- see the design doc's regression bar.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let (row_height, gap) = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                (
                    workspace_row_height_in_body(app, ws, *indented, body.height),
                    workspace_entry_gap(app, &entries, entry_idx),
                )
            }
        };
        if used_rows.saturating_add(row_height) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(row_height);
        visible += 1;
        used_rows = used_rows.saturating_add(gap).min(body.height);
    }
    visible
}

fn workspace_list_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = workspace_list_body_rect(area, false);
    let entries = workspace_list_entries(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (entry_idx, entry) in entries.iter().enumerate().rev() {
        let WorkspaceListEntry::Workspace { ws_idx, indented } = entry;
        let Some(workspace) = app.workspaces.get(*ws_idx) else {
            continue;
        };
        let gap = workspace_entry_gap(app, &entries, entry_idx);
        let needed = workspace_row_height_in_body(app, workspace, *indented, body.height)
            .saturating_add(gap);
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        start = entry_idx;
    }
    start.min(entries.len().saturating_sub(1))
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let max_scroll = workspace_list_bottom_start(app, area);
    let scroll = app.workspace_scroll.min(max_scroll);
    let viewport_rows = workspace_list_visible_count(app, area, scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn resolved_agent_rows(app: &AppState, entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let label = entry
        .state_labels
        .get(agent_panel_status_key(entry.state, entry.seen))
        .map(String::as_str)
        .unwrap_or_else(|| state_label(entry.state, entry.seen));
    tokens::agent_rows(&app.sidebar_agents, entry, label)
}

pub(crate) fn agent_entry_height_in_body(
    app: &AppState,
    entry: &AgentPanelEntry,
    body_height: u16,
) -> u16 {
    (resolved_agent_rows(app, entry)
        .len()
        .max(1)
        .min(u16::MAX as usize) as u16)
        .min(body_height)
}

pub(crate) fn agent_entry_gap(app: &AppState, entry_idx: usize, entry_count: usize) -> u16 {
    if entry_idx + 1 < entry_count {
        app.sidebar_agents.row_gap
    } else {
        0
    }
}

fn agent_panel_visible_count_from(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = agent_panel_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = agent_panel_entries(app);
    for (index, entry) in entries.iter().enumerate().skip(scroll) {
        let height = agent_entry_height_in_body(app, entry, body.height);
        if used_rows.saturating_add(height) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(height);
        visible += 1;
        used_rows = used_rows
            .saturating_add(agent_entry_gap(app, index, entries.len()))
            .min(body.height);
    }
    visible
}

fn agent_panel_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = agent_panel_body_rect(area, false);
    let entries = agent_panel_entries(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (index, entry) in entries.iter().enumerate().rev() {
        let gap = agent_entry_gap(app, index, entries.len());
        let needed = agent_entry_height_in_body(app, entry, body.height).saturating_add(gap);
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        start = index;
    }
    start.min(entries.len().saturating_sub(1))
}

pub(crate) fn agent_panel_scroll_for_target(
    app: &AppState,
    area: Rect,
    current_scroll: usize,
    target: usize,
) -> usize {
    let max_scroll = agent_panel_bottom_start(app, area);
    if target < current_scroll {
        return target.min(max_scroll);
    }
    let mut scroll = current_scroll.min(max_scroll);
    while scroll < target {
        let visible = agent_panel_visible_count_from(app, area, scroll);
        if visible > 0 && target < scroll.saturating_add(visible) {
            break;
        }
        scroll += 1;
    }
    scroll.min(max_scroll)
}

pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let max_scroll = agent_panel_bottom_start(app, area);
    let scroll = app.agent_panel_scroll.min(max_scroll);
    let viewport_rows = agent_panel_visible_count_from(app, area, scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    }
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (Vec<crate::app::state::WorkspaceCardArea>, Vec<()>) {
    let ws_area = compute_expanded_sidebar_layout(area, app.sidebar_section_split, jobs_section_want(app))
        .spaces;
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let headers = Vec::new();

    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = workspace_row_height_in_body(app, ws, *indented, body.height);
                let gap = workspace_entry_gap(app, &entries, entry_idx);
                if row_y.saturating_add(row_height) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                });
                row_y = row_y
                    .saturating_add(row_height)
                    .saturating_add(gap)
                    .min(body_bottom);
            }
        }
    }

    (cards, headers)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

pub(crate) fn workspace_group_chevron_rect(card: &crate::app::state::WorkspaceCardArea) -> Rect {
    if card.rect.width == 0 || card.rect.height == 0 {
        return Rect::default();
    }

    Rect::new(
        card.rect.x + card.rect.width.saturating_sub(1),
        card.rect.y,
        1,
        1,
    )
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

fn workspace_selection_background(p: &Palette, is_active: bool) -> Color {
    if is_active && p.selection_bg == Color::Reset {
        p.active_row_bg
    } else {
        p.selection_bg
    }
}

/// Content the Jobs section wants to draw, independent of geometry. Kept
/// separate from [`JobsSectionState`] (the actual polled content) so the
/// layout math below stays fully testable against the design doc's
/// degradation table with synthetic row counts, without needing a real
/// `AppState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JobsSectionWant {
    pub enabled: bool,
    /// Rows the section wants to draw -- section header, group headers and
    /// job rows alike (design doc: "`max_visible_rows` counts all rows the
    /// section draws").
    pub content_rows: u16,
    pub max_visible_rows: u16,
}

impl JobsSectionWant {
    pub(crate) const fn hidden() -> Self {
        Self {
            enabled: false,
            content_rows: 0,
            max_visible_rows: 0,
        }
    }
}

impl Default for JobsSectionWant {
    fn default() -> Self {
        Self::hidden()
    }
}

/// Reads config + polled content off `app` into the shape the geometry math
/// needs. The list-section poller itself is not wired into `AppState` yet
/// (see `crate::list_section`), so `app.jobs` is empty by construction until
/// either a poller or a test populates it; `app.sidebar_list.enabled`
/// defaults to `false` in `AppState::test_new` specifically so pre-existing
/// sidebar tests keep observing today's Jobs-less layout (the design doc's
/// regression bar).
fn jobs_section_want(app: &AppState) -> JobsSectionWant {
    if !app.sidebar_list.enabled || !jobs_has_content(&app.jobs) {
        return JobsSectionWant::hidden();
    }
    JobsSectionWant {
        enabled: true,
        content_rows: jobs_content_rows(&app.jobs),
        max_visible_rows: app.sidebar_list.max_visible_rows,
    }
}

/// Whether the poller has ever produced a result worth showing, distinct from
/// a valid successful poll that legitimately found zero jobs (which does set
/// `title`/`summary`, per the provider protocol's "a payload with no groups
/// and no notifications is a valid, empty result"). The list-section poller
/// itself is not wired into `AppState` yet, so `ListSectionConfig::enabled`
/// defaulting to `true` would otherwise put a permanently-empty "JOBS" header
/// into every sidebar today; gating on this too keeps that dormant until a
/// poller (or a test) actually populates `AppState::jobs`.
fn jobs_has_content(state: &JobsSectionState) -> bool {
    state.title.is_some() || state.summary.is_some() || !state.groups.is_empty()
}

/// One line the Jobs section body draws, in top-to-bottom order, independent
/// of scroll position or the height actually available. Shared by hit-rect
/// computation ([`jobs_content_layout`]) and rendering ([`render_jobs_section`])
/// so they cannot disagree about what's on screen.
enum JobsPlanRow<'a> {
    GroupHeader { group: &'a ParsedGroup },
    Row {
        group_id: &'a str,
        row: &'a ParsedRow,
    },
}

/// The Jobs section body, flattened: a group header for every group, and --
/// unless that group is individually collapsed -- its rows right after it.
/// Excludes the section's own header line, which is drawn separately and
/// never scrolls. Whether a given group is collapsed is re-derived from
/// `state.collapsed_group_ids` by id wherever it's needed (e.g.
/// `render_jobs_section`) rather than carried on `JobsPlanRow` itself, since
/// nothing here needs to remember it past deciding whether to emit rows.
fn jobs_body_plan(state: &JobsSectionState) -> Vec<JobsPlanRow<'_>> {
    let mut rows = Vec::new();
    for group in &state.groups {
        let collapsed = state.collapsed_group_ids.contains(&group.id);
        rows.push(JobsPlanRow::GroupHeader { group });
        if !collapsed {
            for row in &group.rows {
                rows.push(JobsPlanRow::Row {
                    group_id: &group.id,
                    row,
                });
            }
        }
    }
    rows
}

/// All rows the section draws -- section header, group headers, and job rows
/// alike (design doc: "`max_visible_rows` counts all rows the section
/// draws"). When the section's own header is collapsed there is nothing else
/// to count: the design doc's "collapsed by default" is a user choice to see
/// only the header line, regardless of how much content exists underneath.
fn jobs_content_rows(state: &JobsSectionState) -> u16 {
    if state.collapsed {
        return 1;
    }
    (1 + jobs_body_plan(state).len()).min(u16::MAX as usize) as u16
}

/// One outcome of the design doc's "Degradation policy" table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JobsAllocation {
    rows: u16,
    collapsed_only: bool,
}

impl JobsAllocation {
    const NONE: Self = Self {
        rows: 0,
        collapsed_only: false,
    };
}

/// Implements the design doc's degradation table. `content_height` is the
/// sidebar's content height *before* the toggle row is reserved (border
/// already excluded, since the border only trims width) -- this is the `H`
/// the table's last row ("`H < 7` => hidden entirely") gates on. The other
/// three rows key off usable height *after* the toggle row, i.e.
/// `content_height - 1`.
///
/// Below `content_height == 7` (so `content_height - 1 == 6`), giving Jobs
/// even its minimum one collapsed row would leave only 5 rows for
/// Spaces+Agents, one short of the stated 3-rows-each floor. The design doc
/// states both "Jobs never starves Spaces/Agents below 3 rows each" and this
/// exact one-row degradation row; they are not simultaneously satisfiable at
/// `content_height == 7`. This implements the table literally (row 3 fires,
/// minimum floor loses by one row at that single height) since the table is
/// the piece the design doc asks to be tested row-by-row; see the
/// `jobs_allocation` tests for the boundary.
fn jobs_allocation(want: JobsSectionWant, content_height: u16) -> JobsAllocation {
    if !want.enabled || content_height < 7 {
        return JobsAllocation::NONE;
    }
    let wanted = want.content_rows.min(want.max_visible_rows);
    if wanted == 0 {
        return JobsAllocation::NONE;
    }

    let usable = content_height - 1; // after reserving the toggle row
    if usable >= wanted.saturating_add(6) {
        JobsAllocation {
            rows: wanted,
            collapsed_only: false,
        }
    } else if usable >= 7 {
        JobsAllocation {
            rows: usable - 6,
            collapsed_only: false,
        }
    } else {
        JobsAllocation {
            rows: 1,
            collapsed_only: true,
        }
    }
}

/// Carves `rows` off the bottom of `area` (Jobs sits above the toggle row,
/// per the design doc's allocation order). Returns `(jobs_rect, remainder)`
/// where `remainder` is what's left for Spaces/Agents to split.
fn jobs_rect_from_bottom(area: Rect, rows: u16) -> (Rect, Rect) {
    if rows == 0 || area.height == 0 {
        return (Rect::default(), area);
    }
    let rows = rows.min(area.height);
    let remainder_h = area.height - rows;
    let jobs_rect = Rect::new(area.x, area.y + remainder_h, area.width, rows);
    let remainder = Rect::new(area.x, area.y, area.width, remainder_h);
    (jobs_rect, remainder)
}

fn sidebar_section_divider_row(area: Rect, split_ratio: f32) -> Option<u16> {
    let rect = sidebar_section_divider_rect(area, split_ratio);
    (rect != Rect::default()).then_some(rect.y)
}

/// Inverse of the Spaces/Agents split: converts a dragged divider row into a
/// ratio against `remainder` -- the same rect [`expanded_sidebar_sections`]
/// carves the split from (i.e. `SidebarLayout.spaces`/`.agents` stacked back
/// together). Must stay paired with that function: if a drag computed the
/// ratio against a different rect than rendering applies it to, the divider
/// jumps (design doc: "Layout — one source of truth").
pub(crate) fn sidebar_section_ratio_for_row(remainder: Rect, row: u16) -> Option<f32> {
    if remainder.height < 6 {
        return None;
    }
    let relative_y = row.saturating_sub(remainder.y);
    Some(((relative_y as f32) / (remainder.height as f32)).clamp(0.1, 0.9))
}

/// Expanded-sidebar carve: border, then toggle, then Jobs from the bottom of
/// what's left, then the existing Spaces/Agents split on the remainder. When
/// Jobs is hidden this is byte-for-byte `expanded_sidebar_sections` +
/// `expanded_sidebar_toggle_rect` on the untouched `area` -- the pre-Jobs
/// behaviour -- since carving a toggle row that nothing else needs would
/// itself be a behaviour change.
pub(crate) fn compute_expanded_sidebar_layout(
    area: Rect,
    split_ratio: f32,
    jobs: JobsSectionWant,
) -> SidebarLayout {
    let toggle = expanded_sidebar_toggle_rect(area);
    let content_width = area.width.saturating_sub(1);
    let content_height = if content_width == 0 { 0 } else { area.height };
    let allocation = jobs_allocation(jobs, content_height);

    if allocation.rows == 0 {
        let (spaces, agents) = expanded_sidebar_sections(area, split_ratio);
        return SidebarLayout {
            jobs: Rect::default(),
            jobs_collapsed: true,
            spaces,
            agents,
            section_divider_y: sidebar_section_divider_row(area, split_ratio),
            toggle,
            jobs_rows: Vec::new(),
            jobs_scrollbar: None,
            jobs_header_hits: JobsHeaderHits::default(),
        };
    }

    let above_toggle = Rect::new(area.x, area.y, area.width, area.height - 1);
    let (jobs_rect, remainder) = jobs_rect_from_bottom(above_toggle, allocation.rows);
    let (spaces, agents) = expanded_sidebar_sections(remainder, split_ratio);
    // `remainder` keeps the full (border-inclusive) width so
    // `expanded_sidebar_sections` can do its own border trim exactly as it
    // does when Jobs is hidden; `jobs_rect` doesn't go through that function,
    // so it needs its own trim here to match Spaces/Agents/the toggle in
    // never drawing into the reserved border column (allocation order step
    // 1: "reserve the rightmost column").
    let jobs_rect = Rect::new(
        jobs_rect.x,
        jobs_rect.y,
        jobs_rect.width.saturating_sub(1),
        jobs_rect.height,
    );
    SidebarLayout {
        jobs: jobs_rect,
        jobs_collapsed: allocation.collapsed_only,
        spaces,
        agents,
        section_divider_y: sidebar_section_divider_row(remainder, split_ratio),
        toggle,
        jobs_rows: Vec::new(),
        jobs_scrollbar: None,
        jobs_header_hits: JobsHeaderHits::default(),
    }
}

/// Collapsed-sidebar carve. Asymmetric with the expanded path by design (see
/// the design doc's "Collapsed mode is asymmetric" note):
/// `collapsed_sidebar_sections` ignores `split_ratio`, uses a fixed half
/// split, and drops Agents below seven rows -- this carves Jobs off first and
/// then defers to it unchanged for the remainder, same as the expanded path
/// defers to `expanded_sidebar_sections`.
pub(crate) fn compute_collapsed_sidebar_layout(area: Rect, jobs: JobsSectionWant) -> SidebarLayout {
    let toggle = collapsed_sidebar_toggle_rect(area);
    let content_width = area.width.saturating_sub(1);
    let content_height = if content_width == 0 { 0 } else { area.height };
    let allocation = jobs_allocation(jobs, content_height);

    if allocation.rows == 0 {
        let (spaces, section_divider_y, agents) = collapsed_sidebar_sections(area);
        return SidebarLayout {
            jobs: Rect::default(),
            jobs_collapsed: true,
            spaces,
            agents,
            section_divider_y,
            toggle,
            jobs_rows: Vec::new(),
            jobs_scrollbar: None,
            jobs_header_hits: JobsHeaderHits::default(),
        };
    }

    let above_toggle = Rect::new(area.x, area.y, area.width, area.height - 1);
    let (jobs_rect, remainder) = jobs_rect_from_bottom(above_toggle, allocation.rows);
    let (spaces, section_divider_y, agents) = collapsed_sidebar_sections(remainder);
    // See the matching comment in `compute_expanded_sidebar_layout`: `jobs_rect`
    // needs its own border-column trim since it doesn't pass through
    // `collapsed_sidebar_sections`'s internal one.
    let jobs_rect = Rect::new(
        jobs_rect.x,
        jobs_rect.y,
        jobs_rect.width.saturating_sub(1),
        jobs_rect.height,
    );
    SidebarLayout {
        jobs: jobs_rect,
        jobs_collapsed: allocation.collapsed_only,
        spaces,
        agents,
        section_divider_y,
        toggle,
        jobs_rows: Vec::new(),
        jobs_scrollbar: None,
        jobs_header_hits: JobsHeaderHits::default(),
    }
}

/// The single source of truth for sidebar geometry (design doc: "Layout —
/// one source of truth"). Computed once per frame in `compute_view` and
/// stored on `ViewState`; also called directly by the lower-level render/hit
/// -test helpers below since several of them are unit-tested with synthetic
/// areas that never go through `compute_view`. It is a pure function of
/// `(app, area)`, so every caller agrees regardless of which path calls it.
pub(crate) fn compute_sidebar_layout(app: &AppState, area: Rect) -> SidebarLayout {
    let jobs = jobs_section_want(app);
    let mut layout = if app.sidebar_collapsed {
        compute_collapsed_sidebar_layout(area, jobs)
    } else {
        compute_expanded_sidebar_layout(area, app.sidebar_section_split, jobs)
    };
    if layout.jobs != Rect::default() {
        let (jobs_rows, jobs_scrollbar, jobs_header_hits, _) =
            jobs_content_layout(&app.jobs, &app.sidebar_list, layout.jobs);
        layout.jobs_rows = jobs_rows;
        layout.jobs_scrollbar = jobs_scrollbar;
        layout.jobs_header_hits = jobs_header_hits;
    }
    layout
}

/// Clamps `scroll` against `total_rows`/`body_height` and derives the
/// `ScrollMetrics` the existing scrollbar helpers expect. Every Jobs body row
/// (group header or job row) is exactly one line tall, so this is simpler
/// than the Spaces/Agents equivalents, which must additionally sum variable
/// per-entry heights.
fn jobs_scroll_metrics(
    total_rows: usize,
    body_height: u16,
    scroll: usize,
) -> (usize, crate::pane::ScrollMetrics) {
    let body_height = body_height as usize;
    let max_scroll = total_rows.saturating_sub(body_height);
    let scroll = scroll.min(max_scroll);
    let viewport_rows = total_rows.saturating_sub(scroll).min(body_height);
    (
        scroll,
        crate::pane::ScrollMetrics {
            offset_from_bottom: max_scroll - scroll,
            max_offset_from_bottom: max_scroll,
            viewport_rows,
        },
    )
}

/// `ScrollMetrics` for the Jobs body against a given `jobs_rect`, mirroring
/// `workspace_list_scroll_metrics`/`agent_panel_scroll_metrics` -- used by
/// mouse hit-testing (wheel clamping, scrollbar track/thumb) so it never
/// needs to recompute `jobs_content_layout`'s row rects just to know how far
/// the body can scroll.
pub(crate) fn jobs_list_scroll_metrics(
    app: &AppState,
    jobs_rect: Rect,
) -> crate::pane::ScrollMetrics {
    if jobs_rect.height <= 1 {
        return crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: 0,
        };
    }
    let body_height = jobs_rect.height - 1;
    let total_rows = jobs_body_plan(&app.jobs).len();
    jobs_scroll_metrics(total_rows, body_height, app.jobs.scroll).1
}

/// Right-aligned label rect within `row_rect`, clipped to its width. Mirrors
/// `agent_panel_header_label_rect`'s shape for a header line that isn't
/// necessarily row 1 of its area.
fn jobs_right_label_rect(row_rect: Rect, label: &str) -> Rect {
    if row_rect.width == 0 {
        return Rect::default();
    }
    let width = display_width_u16(label).min(row_rect.width);
    Rect::new(
        row_rect.x + row_rect.width.saturating_sub(width),
        row_rect.y,
        width,
        1,
    )
}

fn jobs_mode_toggle_text(config: &ListSectionConfig) -> String {
    format!("[{}]", config.modes.join("|"))
}

fn jobs_stale_label(state: &JobsSectionState) -> Option<String> {
    if !state.is_stale {
        return None;
    }
    Some(match &state.last_success_label {
        Some(label) => format!("stale · {label}"),
        None => "stale".to_string(),
    })
}

/// The text drawn on the right side of the section header, and whether it's
/// the mode toggle (only the mode toggle gets a mouse hit target -- the stale
/// label is informational). Staleness takes priority: there's no room to show
/// both, and a stale result is the more actionable state to surface.
fn jobs_header_right_text(state: &JobsSectionState, config: &ListSectionConfig, collapsed_style: bool) -> Option<(String, bool)> {
    if let Some(stale) = jobs_stale_label(state) {
        return Some((stale, false));
    }
    if collapsed_style || config.modes.len() < 2 {
        return None;
    }
    Some((jobs_mode_toggle_text(config), true))
}

/// Computes the exact rects [`render_jobs_section`] will draw into, plus
/// header hit targets. This is the single source of truth both
/// `compute_sidebar_layout` (for `SidebarLayout::jobs_rows` et al, read by the
/// hit-tester) and `render_jobs_section` call -- given the same `jobs_rect`
/// and the same `AppState::jobs`/`AppState::sidebar_list`, a pure function
/// cannot produce different rects for the two callers (design doc: "Layout —
/// one source of truth").
fn jobs_content_layout(
    state: &JobsSectionState,
    config: &ListSectionConfig,
    jobs_rect: Rect,
) -> (
    Vec<JobRowHit>,
    Option<Rect>,
    JobsHeaderHits,
    crate::pane::ScrollMetrics,
) {
    let no_scroll = crate::pane::ScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 0,
        viewport_rows: 0,
    };
    if jobs_rect.width == 0 || jobs_rect.height == 0 {
        return (Vec::new(), None, JobsHeaderHits::default(), no_scroll);
    }

    let header_rect = Rect::new(jobs_rect.x, jobs_rect.y, jobs_rect.width, 1);
    let chevron_rect = Rect::new(header_rect.x, header_rect.y, 1, 1);
    // Whenever there's only room for one row -- whether because the user
    // collapsed the section or because the degradation table forced it down
    // to its minimum -- show the collapsed-style header rather than a mode
    // toggle with no list underneath it.
    let collapsed_style = state.collapsed || jobs_rect.height <= 1;
    let mode_toggle_rect = match jobs_header_right_text(state, config, collapsed_style) {
        Some((text, true)) => jobs_right_label_rect(header_rect, &text),
        _ => Rect::default(),
    };
    let header_hits = JobsHeaderHits {
        chevron: chevron_rect,
        mode_toggle: mode_toggle_rect,
    };

    if jobs_rect.height <= 1 {
        return (Vec::new(), None, header_hits, no_scroll);
    }

    let body_rect = Rect::new(jobs_rect.x, jobs_rect.y + 1, jobs_rect.width, jobs_rect.height - 1);
    let plan = jobs_body_plan(state);
    let (scroll, metrics) = jobs_scroll_metrics(plan.len(), body_rect.height, state.scroll);
    let has_scrollbar = should_show_scrollbar(metrics);
    let content_width = body_rect.width.saturating_sub(u16::from(has_scrollbar));

    let mut rows = Vec::new();
    for (offset, plan_row) in plan.iter().skip(scroll).take(metrics.viewport_rows).enumerate() {
        let rect = Rect::new(body_rect.x, body_rect.y + offset as u16, content_width, 1);
        // Group headers use their own id as both `row_id` and `group_id` --
        // the convention `render_jobs_section` and (eventually) the mouse
        // handler use to tell a group header hit apart from a job row hit,
        // since job row ids are guaranteed unique across the whole payload
        // and so never legitimately collide with a group id here.
        let (row_id, group_id) = match plan_row {
            JobsPlanRow::GroupHeader { group } => (group.id.clone(), group.id.clone()),
            JobsPlanRow::Row { group_id, row } => (row.id.clone(), (*group_id).to_string()),
        };
        rows.push(JobRowHit {
            rect,
            row_id,
            group_id,
        });
    }

    let scrollbar = has_scrollbar.then(|| {
        Rect::new(
            body_rect.x + body_rect.width.saturating_sub(1),
            body_rect.y,
            1,
            body_rect.height,
        )
    });

    (rows, scrollbar, header_hits, metrics)
}

fn row_style_name(style: RowStyle) -> &'static str {
    match style {
        RowStyle::Normal => "normal",
        RowStyle::Ok => "ok",
        RowStyle::Fail => "fail",
        RowStyle::Warn => "warn",
        RowStyle::Muted => "muted",
    }
}

/// Resolves a row's style *name* against the config styles map (design doc:
/// "`style` is a name resolved against config, never a color"), falling back
/// to the main text color if the name isn't present (e.g. a user removed it
/// from their `[ui.sidebar.list.styles]` table).
fn resolve_row_style(config: &ListSectionConfig, style: RowStyle, p: &Palette) -> Style {
    let color = config
        .styles
        .get(row_style_name(style))
        .map(|color| color.ratatui())
        .unwrap_or(p.text);
    Style::default().fg(color)
}

/// Splits `row_rect` into one rect per `columns` entry: fixed columns get
/// their configured width, fill columns split whatever's left evenly, and a
/// single-column gap separates adjacent columns. Independent of
/// `resolved_token_spans` -- see the design doc's note on why that helper
/// isn't reused here.
fn jobs_column_rects(row_rect: Rect, columns: &[ColumnSpec]) -> Vec<Rect> {
    if columns.is_empty() || row_rect.width == 0 {
        return Vec::new();
    }

    let gaps = (columns.len() - 1) as u16;
    let fixed_total: u16 = columns
        .iter()
        .map(|column| match column.width {
            ColumnWidth::Fixed(width) => width,
            ColumnWidth::Fill => 0,
        })
        .sum();
    let fill_count = columns
        .iter()
        .filter(|column| matches!(column.width, ColumnWidth::Fill))
        .count() as u16;
    let mut fill_budget = row_rect
        .width
        .saturating_sub(fixed_total)
        .saturating_sub(gaps);
    let mut fill_remaining = fill_count;

    let right_edge = row_rect.x + row_rect.width;
    let mut x = row_rect.x;
    let mut rects = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(1).min(right_edge);
        }
        let width = match column.width {
            ColumnWidth::Fixed(width) => width,
            ColumnWidth::Fill if fill_remaining > 0 => {
                let share = fill_budget / fill_remaining;
                fill_budget -= share;
                fill_remaining -= 1;
                share
            }
            ColumnWidth::Fill => 0,
        };
        let width = width.min(right_edge.saturating_sub(x));
        rects.push(Rect::new(x, row_rect.y, width, 1));
        x = x.saturating_add(width);
    }
    rects
}

fn jobs_mode_toggle_spans(config: &ListSectionConfig, current_mode: &str, p: &Palette) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled("[", Style::default().fg(p.overlay0))];
    for (index, mode) in config.modes.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("|", Style::default().fg(p.overlay0)));
        }
        let style = if mode == current_mode {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
        };
        spans.push(Span::styled(mode.clone(), style));
    }
    spans.push(Span::styled("]", Style::default().fg(p.overlay0)));
    spans
}

fn render_jobs_header(
    frame: &mut Frame,
    header_hits: &JobsHeaderHits,
    header_rect: Rect,
    state: &JobsSectionState,
    config: &ListSectionConfig,
    collapsed_style: bool,
    p: &Palette,
) {
    if header_rect.width == 0 {
        return;
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            if collapsed_style { "▸" } else { "▾" },
            Style::default().fg(p.accent),
        )),
        header_hits.chevron,
    );

    let right = jobs_header_right_text(state, config, collapsed_style);
    let right_width = right
        .as_ref()
        .map(|(text, _)| display_width_u16(text))
        .unwrap_or(0);
    let gap = u16::from(right.is_some());
    let left_x = header_rect.x.saturating_add(2);
    let left_width = header_rect
        .width
        .saturating_sub(2)
        .saturating_sub(right_width)
        .saturating_sub(gap);

    let title = state.title.as_deref().unwrap_or("JOBS");
    let mut left = title.to_string();
    if collapsed_style {
        if let Some(summary) = state.summary.as_deref().filter(|s| !s.is_empty()) {
            left = format!("{left}  {summary}");
        }
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_end(&left, left_width as usize),
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )),
        Rect::new(left_x, header_rect.y, left_width, 1),
    );

    match right {
        Some((_, true)) => {
            frame.render_widget(
                Paragraph::new(Line::from(jobs_mode_toggle_spans(config, &state.mode, p))),
                header_hits.mode_toggle,
            );
        }
        Some((text, false)) => {
            let rect = jobs_right_label_rect(header_rect, &text);
            frame.render_widget(
                Paragraph::new(Span::styled(text, Style::default().fg(p.peach))),
                rect,
            );
        }
        None => {}
    }
}

fn render_jobs_group_header(frame: &mut Frame, rect: Rect, group: &ParsedGroup, collapsed: bool, p: &Palette) {
    if rect.width == 0 {
        return;
    }
    let label_width = rect.width.saturating_sub(2);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if collapsed { "▸ " } else { "▾ " },
                Style::default().fg(p.accent),
            ),
            Span::styled(
                truncate_end(&group.label, label_width as usize),
                Style::default().fg(p.overlay1).add_modifier(Modifier::BOLD),
            ),
        ])),
        rect,
    );
}

fn render_jobs_row(
    frame: &mut Frame,
    rect: Rect,
    row: &ParsedRow,
    config: &ListSectionConfig,
    selected: bool,
    p: &Palette,
) {
    if rect.width == 0 {
        return;
    }
    if selected {
        let buf = frame.buffer_mut();
        for x in rect.x..rect.x + rect.width {
            buf[(x, rect.y)].set_style(Style::default().bg(p.surface0));
        }
    }

    let style = resolve_row_style(config, row.style, p);
    let column_rects = jobs_column_rects(rect, &config.columns);
    for (index, column_rect) in column_rects.iter().enumerate() {
        if column_rect.width == 0 {
            continue;
        }
        let column = config.columns[index];
        let text = row.cells.get(index).map(String::as_str).unwrap_or("");
        let alignment = match column.align {
            ColumnAlign::Left => Alignment::Left,
            ColumnAlign::Right => Alignment::Right,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(text, column_rect.width as usize),
                style,
            ))
            .alignment(alignment),
            *column_rect,
        );
    }
}

/// Renders `layout.jobs` (design doc mockups: section header, group headers,
/// job rows, stale indicator, scrollbar). Recomputes
/// [`jobs_content_layout`] against the same `jobs_rect` its caller carved,
/// rather than reading a cached `SidebarLayout` -- see the matching comment on
/// `render_sidebar` for why, and note it is a pure function of
/// `(app.jobs, app.sidebar_list, jobs_rect)` so it cannot disagree with what
/// `compute_sidebar_layout` stores for hit-testing.
fn render_jobs_section(app: &AppState, frame: &mut Frame, jobs_rect: Rect) {
    if jobs_rect == Rect::default() {
        return;
    }
    let p = &app.palette;
    let state = &app.jobs;
    let config = &app.sidebar_list;
    let (rows, scrollbar, header_hits, metrics) = jobs_content_layout(state, config, jobs_rect);
    let collapsed_style = state.collapsed || jobs_rect.height <= 1;
    let header_rect = Rect::new(jobs_rect.x, jobs_rect.y, jobs_rect.width, 1);

    render_jobs_header(frame, &header_hits, header_rect, state, config, collapsed_style, p);

    for row_hit in &rows {
        let Some(group) = state.groups.iter().find(|group| group.id == row_hit.group_id) else {
            continue;
        };
        if row_hit.row_id == row_hit.group_id {
            let collapsed = state.collapsed_group_ids.contains(&group.id);
            render_jobs_group_header(frame, row_hit.rect, group, collapsed, p);
        } else if let Some(row) = group.rows.iter().find(|row| row.id == row_hit.row_id) {
            let selected = state.selected_row_id.as_deref() == Some(row.id.as_str());
            render_jobs_row(frame, row_hit.rect, row, config, selected, p);
        }
    }

    if let Some(track) = scrollbar {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(p.sidebar_bg));
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    // Always the collapsed carve: which render function runs is the caller's
    // decision (`render_navigation_chrome` branches on `app.sidebar_collapsed`
    // before choosing between this and `render_sidebar`), not this function's
    // -- tests draw this directly with `sidebar_collapsed` left at its
    // default, so reading `app.sidebar_collapsed` here would pick the wrong
    // carve for them.
    let layout = compute_collapsed_sidebar_layout(area, jobs_section_want(app));
    let (ws_area, divider_y, detail_area) = (layout.spaces, layout.section_divider_y, layout.agents);
    render_jobs_section(app, frame, layout.jobs);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, layout.toggle, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        let (icon, icon_style) = state_icon(agg_state, agg_seen, app.status_indicators, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let selection_bg = workspace_selection_background(p, is_active);
        let row_style = if is_selected {
            Style::default().bg(selection_bg)
        } else if is_active {
            Style::default().bg(p.active_row_bg)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(selection_bg)
        } else if is_active {
            Style::default().fg(p.text).bg(p.active_row_bg)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{:<2}", visible_idx + 1), num_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        let divider_color = if app.agent_view_override.is_some() {
            p.accent
        } else {
            p.surface_dim
        };
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(divider_color));
        }
    }

    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        for (detail_idx, detail) in agent_panel_entries(app).iter().enumerate() {
            let y = detail_content_area.y + detail_idx as u16;
            if y >= detail_content_area.y + detail_content_area.height {
                break;
            }
            let position = detail_idx + 1;
            let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
            let position_style = if is_active {
                Style::default().fg(p.text).bg(p.active_row_bg)
            } else {
                Style::default().fg(p.overlay0)
            };
            let (icon, icon_style) =
                state_icon(detail.state, detail.seen, app.status_indicators, p);

            if is_active {
                let buf = frame.buffer_mut();
                for x in detail_content_area.x..detail_content_area.x + detail_content_area.width {
                    buf[(x, y)].set_style(Style::default().bg(p.active_row_bg));
                }
            }

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{position:<2}"), position_style),
                    Span::styled(icon, icon_style),
                ])),
                Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
            );
        }
    }

    render_sidebar_toggle(app, frame, layout.toggle, true, p);
}

pub(crate) fn workspace_drop_slots(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
) -> Vec<(crate::app::state::WorkspaceDropTarget, u16)> {
    if area.height == 0 || cards.is_empty() {
        return Vec::new();
    }
    let list_bottom = area.y + area.height.saturating_sub(1);
    let entries = workspace_list_entries(app);
    let entry_position = |ws_idx| {
        entries.iter().position(|entry| {
            matches!(
                entry,
                WorkspaceListEntry::Workspace {
                    ws_idx: entry_ws_idx,
                    ..
                } if *entry_ws_idx == ws_idx
            )
        })
    };
    let block_root_at = |entry_idx: usize| {
        entries[..=entry_idx]
            .iter()
            .rev()
            .find_map(|entry| match entry {
                WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                } => Some(*ws_idx),
                WorkspaceListEntry::Workspace { .. } => None,
            })
    };

    let mut slots = Vec::new();
    let mut previous_root = None;
    for card in cards {
        let Some(entry_idx) = entry_position(card.ws_idx) else {
            continue;
        };
        let Some(root_idx) = block_root_at(entry_idx) else {
            continue;
        };
        if previous_root == Some(root_idx) {
            continue;
        }
        previous_root = Some(root_idx);
        if let Some(row) = card.rect.y.checked_sub(1).filter(|row| *row < list_bottom) {
            slots.push((
                crate::app::state::WorkspaceDropTarget::Before(root_idx),
                row,
            ));
        }
    }

    let Some(last) = cards.last() else {
        return slots;
    };
    let Some(last_entry_idx) = entry_position(last.ws_idx) else {
        return slots;
    };
    let next_entry = entries.get(last_entry_idx.saturating_add(1));
    if matches!(
        next_entry,
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    ) {
        return slots;
    }
    let target = match next_entry {
        Some(WorkspaceListEntry::Workspace { ws_idx, .. }) => {
            crate::app::state::WorkspaceDropTarget::Before(*ws_idx)
        }
        None => crate::app::state::WorkspaceDropTarget::End,
    };
    let row = last.rect.y.saturating_add(last.rect.height);
    if row < list_bottom
        && slots
            .last()
            .is_none_or(|(last_target, _)| *last_target != target)
    {
        slots.push((target, row));
    }
    slots
}

pub(crate) fn workspace_drop_indicator_row(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    target: crate::app::state::WorkspaceDropTarget,
) -> Option<u16> {
    workspace_drop_slots(app, cards, area)
        .into_iter()
        .find_map(|(candidate, row)| (candidate == target).then_some(row))
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(p.sidebar_bg));
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    // Always the expanded carve -- see the matching comment in
    // `render_sidebar_collapsed`.
    let layout = compute_expanded_sidebar_layout(area, app.sidebar_section_split, jobs_section_want(app));

    render_workspace_list(app, terminal_runtimes, frame, layout.spaces, is_navigating);
    render_agent_detail(app, terminal_runtimes, frame, layout.agents);
    render_jobs_section(app, frame, layout.jobs);
    render_sidebar_toggle(app, frame, layout.toggle, false, p);
}

fn resolved_token_spans(
    resolved: &[ResolvedToken],
    state_icon: (&str, Style),
    state_text_style: Style,
    workspace_style: Style,
    secondary_style: Style,
    custom_style: Style,
    p: &Palette,
    max_width: usize,
) -> Vec<Span<'static>> {
    let fixed_widths = resolved
        .iter()
        .map(|token| match &token.kind {
            ResolvedTokenKind::StateIcon => display_width(state_icon.0),
            ResolvedTokenKind::GitStatus { ahead, behind } => {
                usize::from(*ahead > 0) * display_width(&format!("↑{ahead}"))
                    + usize::from(*behind > 0) * display_width(&format!("↓{behind}"))
                    + usize::from(*ahead > 0 && *behind > 0)
            }
            _ => 0,
        })
        .collect::<Vec<_>>();
    let flexible_widths = resolved
        .iter()
        .map(|token| match &token.kind {
            ResolvedTokenKind::StateText(text)
            | ResolvedTokenKind::Workspace(text)
            | ResolvedTokenKind::Tab(text)
            | ResolvedTokenKind::Pane(text)
            | ResolvedTokenKind::Agent(text)
            | ResolvedTokenKind::TerminalTitle(text)
            | ResolvedTokenKind::Branch(text)
            | ResolvedTokenKind::Custom(text) => display_width(text),
            _ => 0,
        })
        .collect::<Vec<_>>();
    let minimum_width = |active: &[bool]| {
        let indices = active
            .iter()
            .enumerate()
            .filter_map(|(index, active)| active.then_some(index))
            .collect::<Vec<_>>();
        let content = indices
            .iter()
            .map(|index| fixed_widths[*index] + usize::from(flexible_widths[*index] > 0))
            .sum::<usize>();
        let separators = indices
            .windows(2)
            .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
            .sum::<usize>();
        content + separators
    };
    let mut active = resolved.iter().map(|_| true).collect::<Vec<_>>();
    if minimum_width(&active) > max_width {
        for (index, width) in flexible_widths.iter().enumerate() {
            if *width > 0 {
                active[index] = false;
            }
        }
        for index in (0..resolved.len()).rev() {
            if flexible_widths[index] == 0 {
                continue;
            }
            active[index] = true;
            if minimum_width(&active) > max_width {
                active[index] = false;
            }
        }
    }
    let visible_indices = active
        .iter()
        .enumerate()
        .filter_map(|(index, active)| active.then_some(index))
        .collect::<Vec<_>>();
    let separator_width = visible_indices
        .windows(2)
        .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
        .sum::<usize>();
    let fixed_width = visible_indices
        .iter()
        .map(|index| fixed_widths[*index])
        .sum::<usize>();
    let mut budgets = flexible_widths
        .iter()
        .enumerate()
        .map(|(index, width)| usize::from(active[index] && *width > 0))
        .collect::<Vec<_>>();
    let minimum = budgets.iter().sum::<usize>();
    let mut remaining = max_width
        .saturating_sub(separator_width + fixed_width)
        .saturating_sub(minimum);
    while remaining > 0 {
        let mut grew = false;
        for (budget, width) in budgets.iter_mut().zip(&flexible_widths) {
            if *budget > 0 && *budget < *width {
                *budget += 1;
                remaining -= 1;
                grew = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut spans = Vec::new();
    for (position, index) in visible_indices.iter().copied().enumerate() {
        let token = &resolved[index];
        if position > 0 {
            let previous = &resolved[visible_indices[position - 1]];
            spans.push(Span::styled(
                tokens::separator(previous, token),
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ));
        }
        match &token.kind {
            ResolvedTokenKind::StateIcon => {
                spans.push(Span::styled(
                    state_icon.0.to_string(),
                    apply_token_style(state_icon.1, token.style),
                ));
            }
            ResolvedTokenKind::StateText(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(state_text_style, token.style),
                ));
            }
            ResolvedTokenKind::Workspace(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(workspace_style, token.style),
                ));
            }
            ResolvedTokenKind::Tab(text)
            | ResolvedTokenKind::Pane(text)
            | ResolvedTokenKind::Agent(text)
            | ResolvedTokenKind::Branch(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(secondary_style, token.style),
                ));
            }
            ResolvedTokenKind::GitStatus { ahead, behind } => {
                if *ahead > 0 {
                    spans.push(Span::styled(
                        format!("↑{ahead}"),
                        apply_token_style(Style::default().fg(p.green), token.style),
                    ));
                }
                if *ahead > 0 && *behind > 0 {
                    spans.push(Span::styled(
                        " ",
                        apply_token_style(Style::default(), token.style),
                    ));
                }
                if *behind > 0 {
                    spans.push(Span::styled(
                        format!("↓{behind}"),
                        apply_token_style(Style::default().fg(p.red), token.style),
                    ));
                }
            }
            ResolvedTokenKind::TerminalTitle(text) | ResolvedTokenKind::Custom(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(custom_style, token.style),
                ));
            }
        }
    }
    spans
}

fn apply_token_style(mut style: Style, patch: crate::config::SidebarTokenStyle) -> Style {
    if let Some(fg) = patch.fg {
        style = style.fg(fg.ratatui());
    }
    if let Some(bold) = patch.bold {
        style = if bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style.remove_modifier(Modifier::BOLD)
        };
    }
    if let Some(dim) = patch.dim {
        style = if dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style.remove_modifier(Modifier::DIM)
        };
    }
    style
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            drop_target: Some(drop_target),
            ..
        }) => workspace_drop_indicator_row(app, &app.view.workspace_card_areas, area, *drop_target),
        _ => None,
    };

    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;
    let entries = workspace_list_entries(app);

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                workspace_selection_background(p, is_active)
            } else if is_dragged {
                p.surface1
            } else {
                p.active_row_bg
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let display_label = if card.indented {
            grouped_child_display_label(&label, ws.branch().as_deref(), ws.custom_name.is_some())
        } else {
            label
        };
        let parent_group = (!card.indented)
            .then(|| workspace_parent_group_state(app, i))
            .flatten();
        let is_last_child = card.indented
            && entries
                .iter()
                .position(|entry| {
                    matches!(
                        entry,
                        WorkspaceListEntry::Workspace { ws_idx, .. } if *ws_idx == i
                    )
                })
                .is_none_or(|entry_idx| !next_entry_is_indented_workspace(&entries, entry_idx));
        let (display_state, display_seen) = parent_group
            .as_ref()
            .filter(|(_, collapsed)| *collapsed)
            .map(|(key, _)| space_aggregate_state(app, key))
            .unwrap_or((agg_state, agg_seen));
        let state_icon = state_icon(display_state, display_seen, app.status_indicators, p);
        let state_text_style = Style::default()
            .fg(state_label_color(display_state, display_seen, p))
            .add_modifier(Modifier::DIM);
        let branch_style = Style::default().fg(if selected || is_active {
            p.mauve
        } else {
            p.overlay0
        });
        let token_values = ws.metadata_tokens.values();
        let rows = tokens::space_rows(
            &app.sidebar_spaces,
            SpaceTokenContext {
                workspace: &display_label,
                branch: ws.branch().as_deref(),
                state_text: state_label(display_state, display_seen),
                ahead_behind: ws.git_ahead_behind(),
                tokens: &token_values,
                suppress_git_details: card.indented,
            },
        );

        for (row_index, resolved) in rows.iter().enumerate() {
            if row_index as u16 >= row_height || row_y + row_index as u16 >= list_bottom {
                break;
            }
            let mut spans = Vec::new();
            let prefix_width = if card.indented {
                spans.push(Span::raw("   "));
                if row_index == 0 {
                    spans.push(Span::styled(
                        if is_last_child { "└─ " } else { "├─ " },
                        Style::default().fg(p.overlay0),
                    ));
                    6
                } else if is_last_child {
                    spans.push(Span::raw("     "));
                    8
                } else {
                    spans.push(Span::styled("│", Style::default().fg(p.overlay0)));
                    spans.push(Span::raw("    "));
                    8
                }
            } else if row_index == 0 {
                spans.push(Span::raw(" "));
                1
            } else {
                spans.push(Span::raw("   "));
                3
            };
            let trailing_width = if row_index == 0 && parent_group.is_some() {
                2
            } else {
                0
            };
            spans.extend(resolved_token_spans(
                resolved,
                state_icon,
                state_text_style,
                name_style,
                branch_style,
                branch_style,
                p,
                card.rect
                    .width
                    .saturating_sub(prefix_width + trailing_width) as usize,
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(card.rect.x, row_y + row_index as u16, card.rect.width, 1),
            );
        }

        if let Some((_, collapsed)) = parent_group {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    if collapsed { "▸" } else { "▾" },
                    Style::default().fg(p.accent),
                )),
                workspace_group_chevron_rect(card),
            );
        }
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        let menu_line = if app.global_menu_attention_badge_visible() {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled("menu", Style::default().fg(p.overlay0)),
            ])
        } else {
            Line::from(vec![Span::styled("menu", Style::default().fg(p.overlay0))])
        };
        frame.render_widget(
            Paragraph::new(menu_line).alignment(Alignment::Right),
            menu_rect,
        );
    }
}

fn render_agent_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let control_label = active_agent_view_label(app)
        .unwrap_or_else(|| agent_panel_sort_label(app.agent_panel_sort));
    let toggle_rect = agent_panel_header_label_rect(area, control_label);
    if toggle_rect != Rect::default() {
        let color = if app.agent_view_override.is_some() {
            p.accent
        } else {
            p.overlay0
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                control_label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }

    let details = agent_panel_entries_from(app, terminal_runtimes);
    let metrics = agent_panel_scroll_metrics(app, area);
    let scrollbar_rect = agent_panel_scrollbar_rect(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    if body == Rect::default() {
        return;
    }
    if details.is_empty() && app.agent_view_override.is_some() {
        frame.render_widget(
            Paragraph::new(" no matching agents")
                .style(Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let scroll = app.agent_panel_scroll.min(metrics.max_offset_from_bottom);
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    for (index, detail) in details.iter().enumerate().skip(scroll) {
        let label_color = state_label_color(detail.state, detail.seen, p);
        let rows = resolved_agent_rows(app, detail);
        let height = (rows.len().max(1) as u16).min(body.height);
        if row_y.saturating_add(height) > body_bottom {
            break;
        }

        let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
        let row_style = if is_active {
            Style::default().bg(p.active_row_bg)
        } else {
            Style::default()
        };
        let name_style = if is_active {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
        };
        let status_style = if is_active {
            Style::default().fg(label_color)
        } else {
            Style::default().fg(label_color).add_modifier(Modifier::DIM)
        };
        let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
        let state_icon = state_icon(detail.state, detail.seen, app.status_indicators, p);

        for (row_index, resolved) in rows.iter().take(height as usize).enumerate() {
            let mut spans = vec![Span::raw(if row_index == 0 { " " } else { "   " })];
            spans.extend(resolved_token_spans(
                resolved,
                state_icon,
                status_style,
                name_style,
                agent_style,
                agent_style,
                p,
                body.width
                    .saturating_sub(if row_index == 0 { 1 } else { 3 }) as usize,
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(row_style),
                Rect::new(body.x, row_y + row_index as u16, body.width, 1),
            );
        }
        row_y = row_y
            .saturating_add(height)
            .saturating_add(agent_entry_gap(app, index, details.len()))
            .min(body_bottom);
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x + area.width.saturating_sub(2),
        area.y + area.height.saturating_sub(1),
        1,
        1,
    )
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    toggle_area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, layout::PaneId, workspace::Workspace};
    use ratatui::{backend::TestBackend, layout::Direction, Terminal};

    fn row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        (0..width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn find_symbol_x(buffer: &ratatui::buffer::Buffer, row: u16, width: u16, symbol: &str) -> u16 {
        (0..width)
            .find(|x| buffer[(*x, row)].symbol() == symbol)
            .unwrap_or_else(|| {
                panic!(
                    "missing symbol {symbol:?} in row {}",
                    row_text(buffer, row, width)
                )
            })
    }

    #[test]
    fn expanded_and_collapsed_sidebars_use_custom_background() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        app.active = None;
        app.palette.sidebar_bg = ratatui::style::Color::Rgb(12, 34, 56);
        let area = Rect::new(0, 0, 26, 20);

        let mut expanded = Terminal::new(TestBackend::new(26, 20)).unwrap();
        expanded
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert!(expanded
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == app.palette.sidebar_bg));

        let mut collapsed = Terminal::new(TestBackend::new(26, 20)).unwrap();
        collapsed
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        assert!(collapsed
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == app.palette.sidebar_bg));
    }

    #[test]
    fn default_agent_rows_remove_redundant_state_text() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.detected_agent = Some(Agent::Pi);
        terminal_state.state = AgentState::Working;

        let area = Rect::new(0, 0, 26, 20);
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);

        let first = row_text(buffer, body.y, 25);
        let second = row_text(buffer, body.y + 1, 25);
        assert!(first.contains("one"));
        assert_eq!(second, "   pi");
        assert!(!first.contains("working"));
        assert!(!second.contains("working"));

        let workspace_x = find_symbol_x(buffer, body.y, body.width, "o");
        let workspace_style = buffer[(workspace_x, body.y)].style();
        assert_eq!(workspace_style.fg, Some(app.palette.text));
        assert!(workspace_style.add_modifier.contains(Modifier::BOLD));
        assert!(!workspace_style.add_modifier.contains(Modifier::DIM));
        assert_eq!(workspace_style.bg, Some(app.palette.active_row_bg));

        let agent_x = find_symbol_x(buffer, body.y + 1, body.width, "p");
        let agent_style = buffer[(agent_x, body.y + 1)].style();
        assert_eq!(agent_style.fg, Some(app.palette.overlay0));
        assert!(agent_style.add_modifier.contains(Modifier::DIM));
        assert!(!agent_style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(agent_style.bg, Some(app.palette.active_row_bg));
    }

    #[test]
    fn occurrence_false_removes_default_workspace_bold_and_agent_dim() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.agents]
rows = [[{ token = "workspace", bold = false }, { token = "agent", dim = false }]]
"##,
        )
        .unwrap();
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_agents = config.ui.sidebar.agents;
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);

        let area = Rect::new(0, 0, 26, 20);
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);
        let buffer = terminal.backend().buffer();
        let workspace = buffer[(find_symbol_x(buffer, body.y, body.width, "o"), body.y)].style();
        let agent = buffer[(find_symbol_x(buffer, body.y, body.width, "p"), body.y)].style();

        assert_eq!(workspace.fg, Some(app.palette.text));
        assert!(!workspace.add_modifier.contains(Modifier::BOLD));
        assert_eq!(agent.fg, Some(app.palette.overlay0));
        assert!(!agent.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn default_space_workspace_style_tracks_active_state() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let first_row = app.view.workspace_card_areas[0].rect.y;
        let second_row = app.view.workspace_card_areas[1].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let active = buffer[(find_symbol_x(buffer, first_row, 25, "o"), first_row)].style();
        assert_eq!(active.fg, Some(app.palette.text));
        assert!(active.add_modifier.contains(Modifier::BOLD));
        assert!(!active.add_modifier.contains(Modifier::DIM));
        assert_eq!(active.bg, Some(app.palette.active_row_bg));

        let inactive = buffer[(find_symbol_x(buffer, second_row, 25, "t"), second_row)].style();
        assert_eq!(inactive.fg, Some(app.palette.subtext0));
        assert!(!inactive
            .add_modifier
            .intersects(Modifier::BOLD | Modifier::DIM));
        assert_eq!(inactive.bg, Some(ratatui::style::Color::Reset));
    }

    #[test]
    fn navigate_selection_keeps_its_existing_background_beside_active_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.selected = 1;
        app.mode = Mode::Navigate;
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let active_row = app.view.workspace_card_areas[0].rect.y;
        let selected_row = app.view.workspace_card_areas[1].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(
            buffer[(0, active_row)].bg,
            app.palette.active_row_bg,
            "active workspace should keep its dedicated background"
        );
        assert_eq!(
            buffer[(0, selected_row)].bg,
            app.palette.selection_bg,
            "navigate selection should use its dedicated cursor background"
        );
    }

    #[test]
    fn selected_active_workspace_resolves_expanded_background() {
        let mut app = crate::app::state::AppState::test_new();
        app.palette = crate::app::state::Palette::terminal();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Navigate;
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let active_row = app.view.workspace_card_areas[0].rect.y;
        let inactive_row = app.view.workspace_card_areas[1].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        assert_eq!(
            terminal.backend().buffer()[(0, active_row)].bg,
            app.palette.active_row_bg
        );

        app.selected = 1;
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, active_row)].bg,
            app.palette.active_row_bg
        );
        assert_eq!(
            terminal.backend().buffer()[(0, inactive_row)].bg,
            app.palette.selection_bg
        );

        app.palette = crate::app::state::Palette::catppuccin();
        app.selected = 0;
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, active_row)].bg,
            app.palette.selection_bg
        );
    }

    #[test]
    fn selected_active_workspace_resolves_collapsed_background() {
        let mut app = crate::app::state::AppState::test_new();
        app.palette = crate::app::state::Palette::terminal();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Navigate;
        let area = Rect::new(0, 0, 5, 8);
        let mut terminal = Terminal::new(TestBackend::new(5, 8)).unwrap();
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();

        let (workspace_area, _, _) = collapsed_sidebar_sections(area);
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y)].bg,
            app.palette.active_row_bg
        );

        app.selected = 1;
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y)].bg,
            app.palette.active_row_bg
        );
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y + 1)].bg,
            app.palette.selection_bg
        );

        app.palette = crate::app::state::Palette::catppuccin();
        app.selected = 0;
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y)].bg,
            app.palette.selection_bg
        );
    }

    #[test]
    fn space_occurrence_style_applies_without_styling_separator() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.spaces]
rows = [[{ token = "$hype", fg = "#abcdef", bold = true, dim = false }, "workspace"]]
"##,
        )
        .unwrap();
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        app.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([("hype".into(), Some("HI".into()))]),
            None,
            std::time::Instant::now(),
        );

        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let row = app.view.workspace_card_areas[0].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let h = buffer[(find_symbol_x(buffer, row, 25, "H"), row)].style();
        let i = buffer[(find_symbol_x(buffer, row, 25, "I"), row)].style();
        let separator = buffer[(find_symbol_x(buffer, row, 25, "·"), row)].style();

        for style in [h, i] {
            assert_eq!(style.fg, Some(ratatui::style::Color::Rgb(0xab, 0xcd, 0xef)));
            assert!(style.add_modifier.contains(Modifier::BOLD));
            assert!(!style.add_modifier.contains(Modifier::DIM));
            assert_eq!(style.bg, Some(app.palette.active_row_bg));
        }
        assert_eq!(separator.fg, Some(app.palette.overlay0));
        assert!(separator.add_modifier.contains(Modifier::DIM));
        assert!(!separator.add_modifier.contains(Modifier::BOLD));
        assert_eq!(separator.bg, Some(app.palette.active_row_bg));
    }

    #[test]
    fn occurrence_foreground_flattens_composite_git_status_colors() {
        let config: crate::config::Config = toml::from_str(
            r##"[ui.sidebar.spaces]
rows = [[{ token = "git_status", fg = "#123456" }]]
"##,
        )
        .unwrap();
        let spans = resolved_token_spans(
            &[ResolvedToken {
                kind: ResolvedTokenKind::GitStatus {
                    ahead: 2,
                    behind: 1,
                },
                style: config.ui.sidebar.spaces.rows[0][0].parts().1,
            }],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &crate::app::state::AppState::test_new().palette,
            20,
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "↑2 ↓1"
        );
        assert!(spans
            .iter()
            .all(|span| { span.style.fg == Some(ratatui::style::Color::Rgb(0x12, 0x34, 0x56)) }));
    }

    #[test]
    fn default_agent_row_gap_packs_rendering_and_scroll_geometry() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();
        for (workspace, agent) in app.workspaces.iter().zip([Agent::Pi, Agent::Claude]) {
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(agent);
        }
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        assert_eq!(app.sidebar_agents.row_gap, 0);

        let area = Rect::new(0, 0, 20, 5);
        let metrics = agent_panel_scroll_metrics(&app, area);
        let body = agent_panel_body_rect(area, false);
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|frame| render_agent_detail(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(metrics.viewport_rows, 2);
        assert_eq!(metrics.max_offset_from_bottom, 0);
        assert_eq!(row_text(buffer, body.y, body.width), " pi");
        assert_eq!(row_text(buffer, body.y + 1, body.width), " claude");
    }

    #[test]
    fn narrow_agent_rows_preserve_later_tab_tokens() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("very-long-workspace-name");
        let tab_idx = workspace.test_add_tab(Some("logs"));
        let pane_id = workspace.tabs[tab_idx].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[tab_idx].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);

        let area = Rect::new(0, 0, 18, 20);
        let mut terminal = Terminal::new(TestBackend::new(18, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);
        let first = row_text(buffer, body.y, 17);

        assert!(first.contains("logs"), "rendered row: {first:?}");
        assert!(first.contains('·'), "rendered row: {first:?}");
    }

    #[test]
    fn stripped_terminal_title_renders_with_unicode_width_truncation() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.set_terminal_title(Some("⠋ 修复🙂标题很长".into()));
        app.sidebar_agents.rows = vec![vec![
            crate::config::AgentSidebarToken::TerminalTitleStripped,
        ]];

        let area = Rect::new(0, 0, 10, 12);
        let mut renderer = Terminal::new(TestBackend::new(10, 12)).unwrap();
        renderer
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);
        let rendered = row_text(renderer.backend().buffer(), body.y, 9);

        assert!(!rendered.contains('⠋'));
        assert!(rendered.contains('修') && rendered.contains('复'));

        let spans = resolved_token_spans(
            &[ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle(
                "修复🙂标题很长".into(),
            ))],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &app.palette,
            8,
        );
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(display_width(&text) <= 8, "resolved title: {text:?}");
    }

    #[test]
    fn variable_agent_heights_pack_the_bottom_and_reveal_targets() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.ensure_test_terminals();
        for workspace in &app.workspaces {
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);
        }
        let first_pane = app.workspaces[0].tabs[0].root_pane;
        let first_terminal = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal)
            .unwrap()
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([
                    ("a".into(), Some("a".into())),
                    ("b".into(), Some("b".into())),
                ]),
                None,
                std::time::Instant::now(),
            );
        app.sidebar_agents.rows = vec![
            vec![crate::config::AgentSidebarToken::Agent],
            vec![crate::config::AgentSidebarToken::Custom("a".into())],
            vec![crate::config::AgentSidebarToken::Custom("b".into())],
        ];
        let area = Rect::new(0, 0, 20, 6);

        let metrics = agent_panel_scroll_metrics(&app, area);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(agent_panel_scroll_for_target(&app, area, 0, 2), 1);
    }

    #[test]
    fn oversized_space_layout_is_clipped_to_the_section_body() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]; 6];
        let area = Rect::new(0, 0, 20, 10);
        let workspace_area = workspace_list_rect(area, app.sidebar_section_split);
        let body = workspace_list_body_rect(workspace_area, false);

        let metrics = workspace_list_scroll_metrics(&app, workspace_area);
        let (cards, _) = compute_workspace_list_areas(&app, area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 0);
        assert_eq!(cards[0].rect.height, body.height);
    }

    #[test]
    fn oversized_agent_override_is_clipped_to_the_panel_body() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        app.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![vec![crate::config::AgentSidebarToken::Agent]; 6],
        );
        let panel = Rect::new(0, 0, 20, 5);

        let metrics = agent_panel_scroll_metrics(&app, panel);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 0);
        let entry = agent_panel_entries(&app).pop().unwrap();
        assert_eq!(
            agent_entry_height_in_body(&app, &entry, agent_panel_body_rect(panel, false).height),
            agent_panel_body_rect(panel, false).height
        );
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        let toggle = expanded_sidebar_toggle_rect(area);
        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, toggle, false, &app.palette))
            .expect("sidebar toggle should render");

        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x + area.width - 2);
        assert_eq!(toggle.y, area.y + area.height - 1);
    }

    #[test]
    fn agent_panel_tab_label_visibility_tracks_tab_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let single_auto = Workspace::test_new("auto");
        let mut single_custom = Workspace::test_new("custom");
        single_custom.tabs[0].set_custom_name("focus".into());
        let mut multi = Workspace::test_new("multi");
        multi.test_add_tab(Some("logs"));

        app.workspaces = vec![single_auto, single_custom, multi];
        app.ensure_test_terminals();
        for (ws_idx, tab_idx, agent) in [
            (0, 0, Agent::Pi),
            (1, 0, Agent::Claude),
            (2, 0, Agent::Codex),
            (2, 1, Agent::Pi),
        ] {
            let pane_id = app.workspaces[ws_idx].tabs[tab_idx].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(agent);
        }

        let entries = agent_panel_entries(&app);
        let labels: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.primary_label.as_str(),
                    entry.primary_tab_label.as_deref(),
                )
            })
            .collect();

        assert_eq!(
            labels,
            [
                ("auto", None),
                ("custom", Some("focus")),
                ("multi", Some("1")),
                ("multi", Some("logs")),
            ]
        );
    }

    #[test]
    fn priority_agent_panel_sort_uses_attention_then_space_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
            Workspace::test_new("four"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, state| {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, AgentState::Working);
        set_state(&mut app, 1, AgentState::Idle);
        set_state(&mut app, 2, AgentState::Working);
        set_state(&mut app, 3, AgentState::Blocked);

        let done_pane = app.workspaces[1].tabs[0].root_pane;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&done_pane)
            .unwrap()
            .seen = false;

        let labels: Vec<String> = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect();

        assert_eq!(labels, ["four", "two", "one", "three"]);
    }

    #[test]
    fn collapsed_sidebar_numbers_grouped_agents_by_list_position() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 4, 12);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, detail_area.y)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 1)].symbol(), "2");
    }

    /// Two agent panes in one workspace plus a second workspace, so the
    /// assertions can tell pane-level highlighting apart from workspace-level.
    fn collapsed_agent_app() -> (crate::app::state::AppState, PaneId, PaneId) {
        let mut app = crate::app::state::AppState::test_new();
        let mut first = Workspace::test_new("one");
        let second_pane = first.test_split(Direction::Horizontal);
        let first_pane = first.tabs[0].root_pane;
        app.workspaces = vec![first, Workspace::test_new("two")];
        app.ensure_test_terminals();

        let terminal_ids: Vec<_> = app
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect();
        for terminal_id in terminal_ids {
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        (app, first_pane, second_pane)
    }

    fn collapsed_agent_row_styles(
        app: &crate::app::state::AppState,
        area: Rect,
        detail_area: Rect,
        rows: u16,
    ) -> Vec<Vec<ratatui::style::Style>> {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar_collapsed(app, frame, area))
            .expect("collapsed sidebar should render");
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|row| {
                (detail_area.x..detail_area.x + detail_area.width)
                    .map(|x| buffer[(x, detail_area.y + row)].style())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn collapsed_sidebar_highlights_only_the_focused_agent_pane() {
        let (mut app, first_pane, second_pane) = collapsed_agent_app();
        app.active = Some(0);
        app.workspaces[0].tabs[0].layout.focus_pane(second_pane);
        assert!(app.is_active_pane(0, 0, second_pane));
        assert!(!app.is_active_pane(0, 0, first_pane));

        let area = Rect::new(0, 0, 4, 14);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let rows = collapsed_agent_row_styles(&app, area, detail_area, 3);

        let highlighted: Vec<_> = rows
            .iter()
            .filter(|cells| {
                cells
                    .iter()
                    .all(|style| style.bg == Some(app.palette.active_row_bg))
            })
            .collect();
        assert_eq!(
            highlighted.len(),
            1,
            "only the focused agent pane should be highlighted, across the whole row"
        );
        assert_eq!(highlighted[0][0].fg, Some(app.palette.text));

        let muted = rows
            .iter()
            .filter(|cells| cells[0].fg == Some(app.palette.overlay0))
            .count();
        assert_eq!(
            muted, 2,
            "the sibling pane in the active workspace and the other workspace stay muted"
        );
    }

    #[test]
    fn collapsed_sidebar_does_not_highlight_agents_without_active_workspace() {
        let (mut app, _, _) = collapsed_agent_app();
        app.active = None;

        let area = Rect::new(0, 0, 4, 14);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let rows = collapsed_agent_row_styles(&app, area, detail_area, 3);

        for cells in rows {
            assert_eq!(cells[0].fg, Some(app.palette.overlay0));
            for style in cells {
                assert_ne!(style.bg, Some(app.palette.active_row_bg));
            }
        }
    }

    #[test]
    fn collapsed_sidebar_keeps_workspace_status_visible_for_two_digit_positions() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = (1..=10)
            .map(|idx| Workspace::test_new(&format!("workspace-{idx}")))
            .collect();
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 4, 25);
        let (workspace_area, _, _) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let tenth_row = workspace_area.y + 9;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(workspace_area.x, workspace_area.y)].symbol(), "1");
        assert_eq!(
            buffer[(workspace_area.x + 1, workspace_area.y)].symbol(),
            " "
        );
        assert_eq!(
            buffer[(workspace_area.x + 2, workspace_area.y)].symbol(),
            "·"
        );
        assert_eq!(buffer[(workspace_area.x, tenth_row)].symbol(), "1");
        assert_eq!(buffer[(workspace_area.x + 1, tenth_row)].symbol(), "0");
        assert_eq!(buffer[(workspace_area.x + 2, tenth_row)].symbol(), "·");
    }

    #[test]
    fn collapsed_sidebar_keeps_status_visible_for_two_digit_positions() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = (1..=10)
            .map(|idx| Workspace::test_new(&format!("workspace-{idx}")))
            .collect();
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 4, 25);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let tenth_row = detail_area.y + 9;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, tenth_row)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x + 1, tenth_row)].symbol(), "0");
        assert_eq!(buffer[(detail_area.x + 2, tenth_row)].symbol(), "·");
    }

    #[test]
    fn collapsed_sidebar_numbers_priority_agents_by_list_position() {
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        let urgent_pane = second.test_split(ratatui::layout::Direction::Horizontal);

        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;
        app.status_indicators = crate::config::StatusIndicatorStyle::Symbols;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, pane_id, state| {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, first_pane, AgentState::Idle);
        set_state(&mut app, 1, second_pane, AgentState::Working);
        set_state(&mut app, 1, urgent_pane, AgentState::Blocked);
        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&first_pane)
            .unwrap()
            .seen = false;

        assert_eq!(app.workspaces[1].public_pane_number(urgent_pane), Some(2));
        assert_eq!(agent_panel_entries(&app)[0].pane_id, urgent_pane);

        let area = Rect::new(0, 0, 4, 16);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, detail_area.y)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 1)].symbol(), "2");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 2)].symbol(), "3");
        assert_eq!(buffer[(detail_area.x + 2, detail_area.y)].symbol(), "×");
        assert_eq!(
            buffer[(detail_area.x + 2, detail_area.y)].style().fg,
            Some(app.palette.red)
        );
        assert_eq!(buffer[(detail_area.x + 2, detail_area.y + 1)].symbol(), "✓");
        assert_eq!(
            buffer[(detail_area.x + 2, detail_area.y + 1)].style().fg,
            Some(app.palette.teal)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5);

        assert_eq!(divider, Rect::default());
    }

    // --- SidebarLayout: degradation table -----------------------------

    fn jobs_want(content_rows: u16, max_visible_rows: u16) -> JobsSectionWant {
        JobsSectionWant {
            enabled: true,
            content_rows,
            max_visible_rows,
        }
    }

    #[test]
    fn jobs_disabled_never_allocates_rows() {
        let allocation = jobs_allocation(JobsSectionWant::hidden(), 30);
        assert_eq!(allocation, JobsAllocation::NONE);
    }

    #[test]
    fn jobs_with_no_content_never_allocates_rows() {
        let allocation = jobs_allocation(jobs_want(0, 12), 30);
        assert_eq!(allocation, JobsAllocation::NONE);
    }

    #[test]
    fn degradation_table_row_1_exact_fit_gets_full_wanted_rows() {
        // H - 6 >= wanted => Jobs gets `wanted`, no scrollbar.
        // content_height = 20 => usable = 19 => usable - 6 = 13 >= wanted(5).
        let allocation = jobs_allocation(jobs_want(5, 12), 20);
        assert_eq!(
            allocation,
            JobsAllocation {
                rows: 5,
                collapsed_only: false,
            }
        );
    }

    #[test]
    fn degradation_table_row_1_caps_at_max_visible_rows() {
        let allocation = jobs_allocation(jobs_want(50, 5), 20);
        assert_eq!(
            allocation,
            JobsAllocation {
                rows: 5,
                collapsed_only: false,
            }
        );
    }

    #[test]
    fn degradation_table_row_2_squeezes_with_scrollbar() {
        // 1 <= H - 6 < wanted => Jobs gets H - 6, with a scrollbar.
        // content_height = 12 => usable = 11 => usable - 6 = 5, wanted = 12.
        let allocation = jobs_allocation(jobs_want(12, 12), 12);
        assert_eq!(
            allocation,
            JobsAllocation {
                rows: 5,
                collapsed_only: false,
            }
        );
    }

    #[test]
    fn degradation_table_row_3_collapses_to_one_header_row() {
        // H - 6 < 1 (but H >= 7, so Jobs isn't fully hidden): one row,
        // collapsed header only. content_height = 7 => usable = 6.
        let allocation = jobs_allocation(jobs_want(4, 12), 7);
        assert_eq!(
            allocation,
            JobsAllocation {
                rows: 1,
                collapsed_only: true,
            }
        );
    }

    #[test]
    fn degradation_table_row_4_hides_entirely_below_seven_rows() {
        let allocation = jobs_allocation(jobs_want(4, 12), 6);
        assert_eq!(allocation, JobsAllocation::NONE);

        // Falls through to today's two-way split -- no toggle row reserved,
        // no Jobs rect, identical to `expanded_sidebar_sections` directly.
        let area = Rect::new(0, 0, 26, 6);
        let layout = compute_expanded_sidebar_layout(area, 0.5, jobs_want(4, 12));
        let (spaces, agents) = expanded_sidebar_sections(area, 0.5);
        assert_eq!(layout.jobs, Rect::default());
        assert_eq!(layout.spaces, spaces);
        assert_eq!(layout.agents, agents);
        assert_eq!(layout.toggle, expanded_sidebar_toggle_rect(area));
    }

    #[test]
    fn jobs_never_starves_spaces_or_agents_below_three_rows_each_outside_the_row_3_boundary() {
        // Away from the single contradictory boundary height (content_height
        // == 7, see `jobs_allocation`'s doc comment), Spaces+Agents always
        // keep at least 3 rows each once Jobs is showing full rows or a
        // scrollbar-squeezed count.
        for content_height in 8u16..=40 {
            for wanted in 1u16..=20 {
                let allocation = jobs_allocation(jobs_want(wanted, wanted), content_height);
                if allocation.rows == 0 {
                    continue;
                }
                let remainder = content_height - 1 - allocation.rows;
                assert!(
                    remainder >= 6,
                    "content_height={content_height} wanted={wanted} allocation={allocation:?} \
                     left only {remainder} rows for Spaces+Agents"
                );
            }
        }
    }

    #[test]
    fn expanded_layout_toggle_row_always_survives_regardless_of_jobs() {
        for height in 0u16..=30 {
            let area = Rect::new(0, 0, 26, height);
            for jobs in [JobsSectionWant::hidden(), jobs_want(4, 12), jobs_want(20, 20)] {
                let layout = compute_expanded_sidebar_layout(area, 0.5, jobs);
                assert_eq!(
                    layout.toggle,
                    expanded_sidebar_toggle_rect(area),
                    "toggle rect must never move, height={height} jobs={jobs:?}"
                );
                if layout.toggle != Rect::default() {
                    assert!(
                        !rects_overlap(layout.toggle, layout.jobs),
                        "jobs must never draw under the toggle, height={height}"
                    );
                }
                if layout.jobs != Rect::default() {
                    // The toggle row is only actually *reserved* out of
                    // Spaces/Agents once Jobs is genuinely showing; when
                    // hidden, the toggle stays the pre-existing overlay drawn
                    // on top of Agents' last row (see `compute_expanded_sidebar_layout`
                    // doc comment) -- that overlap is intentional there.
                    assert!(
                        !rects_overlap(layout.toggle, layout.agents),
                        "agents must never draw under the toggle once Jobs is reserving it, \
                         height={height}"
                    );
                }
            }
        }
    }

    #[test]
    fn collapsed_layout_toggle_row_always_survives_regardless_of_jobs() {
        for height in 0u16..=30 {
            let area = Rect::new(0, 0, 26, height);
            for jobs in [JobsSectionWant::hidden(), jobs_want(4, 12), jobs_want(20, 20)] {
                let layout = compute_collapsed_sidebar_layout(area, jobs);
                assert_eq!(
                    layout.toggle,
                    collapsed_sidebar_toggle_rect(area),
                    "toggle rect must never move, height={height} jobs={jobs:?}"
                );
            }
        }
    }

    fn rects_overlap(a: Rect, b: Rect) -> bool {
        if a == Rect::default() || b == Rect::default() {
            return false;
        }
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    #[test]
    fn expanded_layout_handles_tiny_heights_without_panicking() {
        for height in 0u16..=10 {
            for width in 0u16..=4 {
                let area = Rect::new(0, 0, width, height);
                for jobs in [JobsSectionWant::hidden(), jobs_want(4, 12)] {
                    // Must not panic (saturating arithmetic throughout).
                    let _ = compute_expanded_sidebar_layout(area, 0.5, jobs);
                    let _ = compute_collapsed_sidebar_layout(area, jobs);
                }
            }
        }
    }

    #[test]
    fn expanded_layout_never_allocates_jobs_in_a_bordered_out_sidebar() {
        // width <= 1 leaves no content column at all (matches
        // `expanded_sidebar_sections`'s own guard), so Jobs must not show
        // even when it has plenty of height and content to draw.
        for width in 0u16..=1 {
            let area = Rect::new(0, 0, width, 30);
            let layout = compute_expanded_sidebar_layout(area, 0.5, jobs_want(4, 12));
            assert_eq!(layout.jobs, Rect::default());
        }
    }

    #[test]
    fn expanded_and_collapsed_carves_are_not_symmetric() {
        // Same area, same Jobs want: the two carves must not coincidentally
        // agree, since collapsed ignores split_ratio and uses a fixed half
        // split while expanded honours it -- this pins that they really are
        // two independent code paths rather than one reusing the other.
        let area = Rect::new(0, 0, 26, 20);
        let jobs = jobs_want(4, 12);
        let expanded = compute_expanded_sidebar_layout(area, 0.9, jobs);
        let collapsed = compute_collapsed_sidebar_layout(area, jobs);
        assert_ne!(expanded.spaces, collapsed.spaces);
    }

    // --- SidebarLayout: divider drag round trip -----------------------

    #[test]
    fn divider_drag_round_trip_matches_render_with_jobs_allocated() {
        let area = Rect::new(0, 0, 26, 30);
        let jobs = jobs_want(4, 12);
        let target_ratio = 0.4_f32;
        let layout = compute_expanded_sidebar_layout(area, target_ratio, jobs);
        assert!(
            layout.jobs.height > 0,
            "jobs should be allocated for this test to be meaningful"
        );
        let divider_row = layout
            .section_divider_y
            .expect("divider should exist at this size");
        let remainder = Rect::new(
            layout.spaces.x,
            layout.spaces.y,
            layout.spaces.width,
            layout.spaces.height + layout.agents.height,
        );

        let ratio = sidebar_section_ratio_for_row(remainder, divider_row)
            .expect("remainder is tall enough to convert a row back into a ratio");
        let re_layout = compute_expanded_sidebar_layout(area, ratio, jobs);

        assert_eq!(re_layout.section_divider_y, Some(divider_row));
        assert_eq!(
            re_layout.jobs, layout.jobs,
            "the ratio round trip must not disturb Jobs' allocation"
        );
    }

    // --- SidebarLayout: render/hit-test agreement ---------------------
    //
    // The regression this refactor exists to prevent: rendering and
    // hit-testing carving the sidebar with two different formulas so a click
    // lands on the wrong row. Every consumer in this codebase (render_sidebar,
    // render_sidebar_collapsed and every `AppState` hit-test helper in
    // `app::input::sidebar`) is wired to call `compute_expanded_sidebar_layout`
    // / `compute_collapsed_sidebar_layout` / `compute_sidebar_layout` --
    // never to recompute the allocation order some other way. This test
    // exercises that shared entry point across a range of sizes and asserts
    // the pieces it hands back always tile the sidebar with no gaps and no
    // overlaps, for both carves and across the whole Jobs degradation table.
    #[test]
    fn render_and_hit_test_geometry_tile_the_sidebar_with_no_gaps_or_overlaps() {
        for width in [0u16, 1, 2, 4, 10, 26] {
            for height in 0u16..=40 {
                let area = Rect::new(3, 5, width, height);
                for jobs in [
                    JobsSectionWant::hidden(),
                    jobs_want(1, 12),
                    jobs_want(4, 12),
                    jobs_want(12, 12),
                    jobs_want(40, 12),
                ] {
                    for (layout, label) in [
                        (
                            compute_expanded_sidebar_layout(area, 0.5, jobs),
                            "expanded",
                        ),
                        (compute_collapsed_sidebar_layout(area, jobs), "collapsed"),
                    ] {
                        assert!(
                            !rects_overlap(layout.spaces, layout.agents),
                            "{label}: spaces/agents overlap at {area:?} jobs={jobs:?}"
                        );
                        assert!(
                            !rects_overlap(layout.spaces, layout.jobs),
                            "{label}: spaces/jobs overlap at {area:?} jobs={jobs:?}"
                        );
                        assert!(
                            !rects_overlap(layout.agents, layout.jobs),
                            "{label}: agents/jobs overlap at {area:?} jobs={jobs:?}"
                        );
                        assert!(
                            !rects_overlap(layout.jobs, layout.toggle),
                            "{label}: jobs/toggle overlap at {area:?} jobs={jobs:?}"
                        );
                        // The toggle row is only actually carved out of
                        // Spaces/Agents once Jobs is genuinely showing (see
                        // `compute_expanded_sidebar_layout`'s doc comment):
                        // when Jobs is hidden the toggle is the pre-existing
                        // overlay drawn on top of whatever's underneath,
                        // which can be Spaces or Agents at very small sizes.
                        // That overlap is intentional/unchanged there.
                        if layout.jobs != Rect::default() {
                            assert!(
                                !rects_overlap(layout.spaces, layout.toggle),
                                "{label}: spaces/toggle overlap at {area:?} jobs={jobs:?}"
                            );
                            assert!(
                                !rects_overlap(layout.agents, layout.toggle),
                                "{label}: agents/toggle overlap at {area:?} jobs={jobs:?}"
                            );
                        }
                        // Expanded stacks Spaces directly above Agents (the
                        // divider is drawn as an overlay on Agents' first
                        // row); collapsed reserves a separate divider row
                        // between them (`collapsed_sidebar_sections`), so
                        // only expanded is adjacent with no gap.
                        if label == "expanded"
                            && layout.spaces != Rect::default()
                            && layout.agents != Rect::default()
                        {
                            assert_eq!(
                                layout.spaces.y + layout.spaces.height,
                                layout.agents.y,
                                "{label}: spaces/agents must be adjacent at {area:?} jobs={jobs:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn regression_with_jobs_disabled_matches_pre_jobs_geometry_at_every_size() {
        // The regression bar: with Jobs hidden, `compute_sidebar_layout`
        // reproduces the exact pre-Jobs two-way geometry at every size, for
        // both the expanded and collapsed carve.
        for width in 0u16..=30 {
            for height in 0u16..=30 {
                let area = Rect::new(0, 0, width, height);
                let expanded = compute_expanded_sidebar_layout(area, 0.5, JobsSectionWant::hidden());
                let (spaces, agents) = expanded_sidebar_sections(area, 0.5);
                assert_eq!(expanded.spaces, spaces);
                assert_eq!(expanded.agents, agents);
                assert_eq!(expanded.toggle, expanded_sidebar_toggle_rect(area));
                assert_eq!(expanded.jobs, Rect::default());

                let collapsed = compute_collapsed_sidebar_layout(area, JobsSectionWant::hidden());
                let (c_spaces, c_divider, c_agents) = collapsed_sidebar_sections(area);
                assert_eq!(collapsed.spaces, c_spaces);
                assert_eq!(collapsed.agents, c_agents);
                assert_eq!(collapsed.section_divider_y, c_divider);
                assert_eq!(collapsed.toggle, collapsed_sidebar_toggle_rect(area));
                assert_eq!(collapsed.jobs, Rect::default());
            }
        }
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    #[test]
    fn workspace_list_truncates_cjk_branch_without_panic() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("feature/中文-分支-644".into());
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 1, 15, 2),
            indented: false,
        }];

        let mut terminal = Terminal::new(TestBackend::new(15, 6)).expect("test terminal");
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        terminal
            .draw(|frame| {
                render_workspace_list(&app, &runtimes, frame, Rect::new(0, 0, 15, 6), false)
            })
            .expect("workspace list should render");
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            repo_name: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn desktop_worktree_tree_aligns_parents_and_marks_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
            Workspace::test_new("notes"),
        ];
        app.sidebar_spaces.rows = vec![vec![
            crate::config::SpaceSidebarToken::StateIcon,
            crate::config::SpaceSidebarToken::Workspace,
        ]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area, app.sidebar_section_split);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cards = &app.view.workspace_card_areas;
        let parent_name_x = find_symbol_x(buffer, cards[0].rect.y, cards[0].rect.width, "m");
        let plain_name_x = find_symbol_x(buffer, cards[3].rect.y, cards[3].rect.width, "n");
        assert_eq!(parent_name_x, plain_name_x);
        assert_eq!(buffer[(cards[1].rect.x + 3, cards[1].rect.y)].symbol(), "├");
        assert_eq!(buffer[(cards[2].rect.x + 3, cards[2].rect.y)].symbol(), "└");
        assert_eq!(
            buffer[(cards[0].rect.x + cards[0].rect.width - 1, cards[0].rect.y)].symbol(),
            "▾"
        );
    }

    #[test]
    fn desktop_worktree_connector_uses_full_list_at_viewport_boundary() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 10);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        assert_eq!(app.view.workspace_card_areas.len(), 2);
        let list_area = workspace_list_rect(area, app.sidebar_section_split);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let child = app.view.workspace_card_areas[1];
        assert_eq!(
            terminal.backend().buffer()[(child.rect.x + 3, child.rect.y)].symbol(),
            "├"
        );
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.sidebar_spaces.row_gap = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height);
    }

    #[test]
    fn space_row_gap_preserves_compact_worktree_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
            Workspace::test_new("notes"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 2;

        let (spacious, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert_eq!(
            spacious[1].rect.y,
            spacious[0].rect.y + spacious[0].rect.height
        );
        assert_eq!(
            spacious[2].rect.y,
            spacious[1].rect.y + spacious[1].rect.height
        );
        assert_eq!(
            spacious[3].rect.y,
            spacious[2].rect.y + spacious[2].rect.height + 2
        );
        let spacious_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 7));
        assert_eq!(spacious_metrics.viewport_rows, 3);
        assert_eq!(spacious_metrics.max_offset_from_bottom, 2);

        app.sidebar_spaces.row_gap = 0;
        let (packed, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert!(packed
            .windows(2)
            .all(|pair| pair[1].rect.y == pair[0].rect.y + pair[0].rect.height));
        let packed_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 7));
        assert_eq!(packed_metrics.viewport_rows, 4);
        assert_eq!(packed_metrics.max_offset_from_bottom, 0);
    }

    #[test]
    fn packed_workspace_drag_indicator_overlays_an_internal_boundary() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area, app.sidebar_section_split);
        let indicator_row = workspace_drop_indicator_row(
            &app,
            &app.view.workspace_card_areas,
            list_area,
            crate::app::state::WorkspaceDropTarget::Before(2),
        )
        .unwrap();
        assert_eq!(indicator_row, app.view.workspace_card_areas[1].rect.y);
        app.drag = Some(crate::app::state::DragState {
            target: crate::app::state::DragTarget::WorkspaceReorder {
                source_id: 0,
                source_ws_idx: 0,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::Before(2)),
            },
        });

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        assert_eq!(
            terminal.backend().buffer()[(list_area.x, indicator_row)].symbol(),
            "─"
        );
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let entries = workspace_list_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_clamps_when_all_entries_fit() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(app.workspace_scroll, 0);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[2].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = Some("main".into());
        }
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    // --- Jobs section rendering ---------------------------------------

    fn sample_row(id: &str, cells: &[&str], style: RowStyle) -> ParsedRow {
        ParsedRow {
            id: id.to_string(),
            cells: cells.iter().map(|cell| cell.to_string()).collect(),
            style,
            vars: std::collections::BTreeMap::new(),
            actions: Vec::new(),
        }
    }

    fn jobs_app(groups: Vec<ParsedGroup>) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces.clear();
        app.active = None;
        app.sidebar_list = crate::config::ListSectionConfig {
            enabled: true,
            ..crate::config::ListSectionConfig::default()
        };
        app.jobs = JobsSectionState {
            title: Some("JOBS".to_string()),
            summary: Some("2R 1Q".to_string()),
            groups,
            mode: "live".to_string(),
            collapsed: false,
            ..JobsSectionState::default()
        };
        app
    }

    #[test]
    fn jobs_disabled_by_default_in_test_new_matches_the_regression_bar() {
        // `AppState::test_new` deliberately disables Jobs (see its comment) so
        // every pre-existing sidebar test built on it keeps observing today's
        // Jobs-less layout -- the design doc's literal regression bar.
        let app = AppState::test_new();
        assert!(!app.sidebar_list.enabled);
        assert_eq!(jobs_section_want(&app), JobsSectionWant::hidden());
    }

    #[test]
    fn jobs_stays_hidden_until_the_poller_produces_a_result_even_if_enabled() {
        // A fresh `App::new` with default config has `sidebar_list.enabled ==
        // true` (the config's shipped default) but no poller yet to populate
        // `AppState::jobs` -- this must not put a permanently-empty header
        // into every sidebar (and, concretely, must not shift the geometry
        // pre-existing mouse/layout tests hardcode coordinates against).
        let mut app = AppState::test_new();
        app.sidebar_list.enabled = true;
        assert_eq!(jobs_section_want(&app), JobsSectionWant::hidden());
    }

    #[test]
    fn jobs_content_rows_counts_header_group_headers_and_visible_rows() {
        let state = JobsSectionState {
            collapsed: false,
            groups: vec![
                ParsedGroup {
                    id: "running".into(),
                    label: "Running".into(),
                    rows: vec![sample_row("1", &["a"], RowStyle::Normal)],
                },
                ParsedGroup {
                    id: "queued".into(),
                    label: "Queued".into(),
                    rows: vec![],
                },
            ],
            ..JobsSectionState::default()
        };
        // header + running-header + running-row + queued-header
        assert_eq!(jobs_content_rows(&state), 4);
    }

    #[test]
    fn jobs_content_rows_is_one_when_the_section_is_collapsed() {
        let state = JobsSectionState {
            collapsed: true,
            groups: vec![ParsedGroup {
                id: "running".into(),
                label: "Running".into(),
                rows: vec![sample_row("1", &["a"], RowStyle::Normal)],
            }],
            ..JobsSectionState::default()
        };
        assert_eq!(jobs_content_rows(&state), 1);
    }

    #[test]
    fn jobs_content_rows_skips_rows_of_an_individually_collapsed_group() {
        let mut state = JobsSectionState {
            collapsed: false,
            groups: vec![ParsedGroup {
                id: "running".into(),
                label: "Running".into(),
                rows: vec![
                    sample_row("1", &["a"], RowStyle::Normal),
                    sample_row("2", &["b"], RowStyle::Normal),
                ],
            }],
            ..JobsSectionState::default()
        };
        state.collapsed_group_ids.insert("running".to_string());
        // header + the group's own header, rows excluded.
        assert_eq!(jobs_content_rows(&state), 2);
    }

    #[test]
    fn jobs_column_rects_splits_fill_and_fixed_columns_with_single_column_gaps() {
        let columns = crate::config::ListSectionConfig::default().columns;
        let rects = jobs_column_rects(Rect::new(0, 0, 24, 1), &columns);
        assert_eq!(
            rects,
            vec![
                Rect::new(0, 0, 12, 1),
                Rect::new(13, 0, 3, 1),
                Rect::new(17, 0, 7, 1),
            ]
        );
    }

    #[test]
    fn jobs_column_rects_shrinks_gracefully_when_too_narrow() {
        let columns = crate::config::ListSectionConfig::default().columns;
        let rects = jobs_column_rects(Rect::new(0, 0, 4, 1), &columns);
        assert_eq!(rects.len(), 3);
        for rect in rects {
            assert!(rect.x + rect.width <= 4);
        }
    }

    #[test]
    fn jobs_rect_respects_the_reserved_border_column_like_spaces_and_agents() {
        let area = Rect::new(0, 0, 26, 20);
        let layout = compute_expanded_sidebar_layout(area, 0.5, jobs_want(4, 12));
        assert!(layout.jobs.height > 0, "jobs should be allocated");
        let border_x = area.x + area.width - 1;
        assert!(layout.jobs.x + layout.jobs.width <= border_x);
        assert_eq!(layout.jobs.width, layout.spaces.width);
    }

    #[test]
    fn jobs_header_shows_chevron_title_and_summary_when_collapsed() {
        let mut app = jobs_app(vec![]);
        app.jobs.collapsed = true;
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let layout = compute_expanded_sidebar_layout(area, app.sidebar_section_split, jobs_section_want(&app));
        assert_eq!(layout.jobs.height, 1);
        let text = row_text(buffer, layout.jobs.y, layout.jobs.width);
        assert!(text.contains("JOBS"), "header text was {text:?}");
        assert!(text.contains("2R 1Q"), "header text was {text:?}");
        assert_eq!(buffer[(layout.jobs.x, layout.jobs.y)].symbol(), "▸");
    }

    #[test]
    fn jobs_header_shows_mode_toggle_when_expanded_with_room_for_a_body() {
        let group = ParsedGroup {
            id: "running".into(),
            label: "Running".into(),
            rows: vec![sample_row("1", &["ued", "4N", "1:23:45"], RowStyle::Normal)],
        };
        let app = jobs_app(vec![group]);
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let layout = compute_expanded_sidebar_layout(area, app.sidebar_section_split, jobs_section_want(&app));
        assert!(layout.jobs.height > 1, "expected room for a body below the header");
        let text = row_text(buffer, layout.jobs.y, layout.jobs.width);
        assert!(text.contains("[live|history]"), "header text was {text:?}");
        assert_eq!(buffer[(layout.jobs.x, layout.jobs.y)].symbol(), "▾");
    }

    #[test]
    fn jobs_header_shows_stale_indicator_and_last_success_label_over_the_mode_toggle() {
        let mut app = jobs_app(vec![ParsedGroup {
            id: "running".into(),
            label: "Running".into(),
            rows: vec![sample_row("1", &["ued", "4N", "1:23:45"], RowStyle::Normal)],
        }]);
        app.jobs.is_stale = true;
        app.jobs.last_success_label = Some("5m ago".to_string());
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let layout = compute_expanded_sidebar_layout(area, app.sidebar_section_split, jobs_section_want(&app));
        let text = row_text(buffer, layout.jobs.y, layout.jobs.width);
        assert!(text.contains("stale"), "header text was {text:?}");
        assert!(text.contains("5m ago"), "header text was {text:?}");
        assert!(!text.contains("[live|history]"), "header text was {text:?}");
    }

    #[test]
    fn jobs_group_header_and_row_render_at_their_own_hit_rects() {
        let rows: Vec<ParsedRow> = (0..10)
            .map(|index| {
                sample_row(
                    &index.to_string(),
                    &[&format!("job{index}"), "1N", "0:01:00"],
                    RowStyle::Ok,
                )
            })
            .collect();
        let group = ParsedGroup {
            id: "running".into(),
            label: "Running".into(),
            rows,
        };
        let mut app = jobs_app(vec![group]);
        app.sidebar_list.max_visible_rows = 6;
        let area = Rect::new(0, 0, 26, 20);

        let layout = compute_sidebar_layout(&app, area);
        assert!(
            layout.jobs_scrollbar.is_some(),
            "ten rows under a cap of six should need a scrollbar"
        );
        assert!(layout.jobs_rows.len() >= 2);

        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let header_hit = &layout.jobs_rows[0];
        assert_eq!(
            header_hit.row_id, header_hit.group_id,
            "a group header's row_id equals its group_id by convention"
        );
        assert!(row_text(buffer, header_hit.rect.y, area.width).contains("Running"));

        for hit in &layout.jobs_rows[1..] {
            assert_ne!(hit.row_id, hit.group_id);
            let expected = format!("job{}", hit.row_id);
            assert!(
                row_text(buffer, hit.rect.y, area.width).contains(&expected),
                "row {} rect {:?} did not draw its own content",
                hit.row_id,
                hit.rect
            );
        }

        let scrollbar = layout.jobs_scrollbar.unwrap();
        for y in scrollbar.y..scrollbar.y + scrollbar.height {
            assert_eq!(buffer[(scrollbar.x, y)].symbol(), "▕");
        }
    }

    #[test]
    fn jobs_row_style_is_resolved_from_config_styles_by_name() {
        let group = ParsedGroup {
            id: "running".into(),
            label: "Running".into(),
            rows: vec![sample_row("1", &["ued", "4N", "1:23"], RowStyle::Fail)],
        };
        let app = jobs_app(vec![group]);
        let area = Rect::new(0, 0, 26, 20);
        let layout = compute_sidebar_layout(&app, area);
        let row_hit = layout
            .jobs_rows
            .iter()
            .find(|hit| hit.row_id == "1")
            .expect("row should be visible");

        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let expected_fg = crate::config::ListSectionConfig::default().styles["fail"].ratatui();
        let cell = &buffer[(row_hit.rect.x, row_hit.rect.y)];
        assert_eq!(cell.style().fg, Some(expected_fg));
    }

    #[test]
    fn jobs_row_alignment_matches_column_spec() {
        let group = ParsedGroup {
            id: "running".into(),
            label: "Running".into(),
            rows: vec![sample_row("1", &["ued", "4N", "1:23"], RowStyle::Normal)],
        };
        let app = jobs_app(vec![group]);
        let area = Rect::new(0, 0, 26, 20);
        let layout = compute_sidebar_layout(&app, area);
        let row_hit = layout
            .jobs_rows
            .iter()
            .find(|hit| hit.row_id == "1")
            .expect("row should be visible");

        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let columns = jobs_column_rects(row_hit.rect, &crate::config::ListSectionConfig::default().columns);
        // Right-aligned "4N" in a 3-wide column is padded on the left.
        let nodes_col = columns[1];
        let text = row_text(buffer, nodes_col.y, nodes_col.x + nodes_col.width);
        assert!(text.ends_with("4N"));
    }
}
