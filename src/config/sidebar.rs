use std::collections::{BTreeMap, HashMap};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::detect::Agent;

const MAX_SIDEBAR_ROWS: usize = 16;
const MAX_SIDEBAR_TOKENS_PER_ROW: usize = 16;
const DEFAULT_SIDEBAR_ROW_GAP: u16 = 0;

fn deserialize_sidebar_rows<'de, D, T>(deserializer: D) -> Result<Vec<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let rows = Vec::<Vec<T>>::deserialize(deserializer)?;
    validate_sidebar_rows(&rows).map_err(serde::de::Error::custom)?;
    Ok(rows)
}

fn validate_sidebar_rows<T>(rows: &[Vec<T>]) -> Result<(), String> {
    if rows.len() > MAX_SIDEBAR_ROWS {
        return Err(format!(
            "sidebar layouts may contain at most {MAX_SIDEBAR_ROWS} rows"
        ));
    }
    if rows
        .iter()
        .any(|row| row.len() > MAX_SIDEBAR_TOKENS_PER_ROW)
    {
        return Err(format!(
            "sidebar rows may contain at most {MAX_SIDEBAR_TOKENS_PER_ROW} tokens"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarTokenColor {
    r: u8,
    g: u8,
    b: u8,
}

impl SidebarTokenColor {
    pub(crate) fn ratatui(self) -> ratatui::style::Color {
        ratatui::style::Color::Rgb(self.r, self.g, self.b)
    }

    /// Parses a `#RGB` or `#RRGGBB` hex color literal. Used both by the
    /// `Deserialize` impl and by built-in defaults (e.g. the `list` section's
    /// default `styles` table).
    fn parse_hex(value: &str) -> Result<Self, String> {
        let hex = value.strip_prefix('#').filter(|hex| {
            hex.is_ascii()
                && matches!(hex.len(), 3 | 6)
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        let Some(hex) = hex else {
            return Err("sidebar token fg must be #RGB or #RRGGBB".to_string());
        };
        let (r, g, b) = if hex.len() == 3 {
            let mut digits = hex
                .bytes()
                .map(|byte| char::from(byte).to_digit(16).expect("validated hex digit") as u8 * 17);
            (
                digits.next().expect("three hex digits"),
                digits.next().expect("three hex digits"),
                digits.next().expect("three hex digits"),
            )
        } else {
            (
                u8::from_str_radix(&hex[0..2], 16).expect("validated hex digits"),
                u8::from_str_radix(&hex[2..4], 16).expect("validated hex digits"),
                u8::from_str_radix(&hex[4..6], 16).expect("validated hex digits"),
            )
        };
        Ok(Self { r, g, b })
    }
}

impl Serialize for SidebarTokenColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b))
    }
}

impl<'de> Deserialize<'de> for SidebarTokenColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse_hex(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidebarTokenStyle {
    pub fg: Option<SidebarTokenColor>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSidebarToken {
    StateIcon,
    StateText,
    Workspace,
    Tab,
    Pane,
    Agent,
    TerminalTitle,
    TerminalTitleStripped,
    Custom(String),
    Styled {
        token: Box<AgentSidebarToken>,
        style: SidebarTokenStyle,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceSidebarToken {
    StateIcon,
    StateText,
    Workspace,
    Branch,
    GitStatus,
    Custom(String),
    Styled {
        token: Box<SpaceSidebarToken>,
        style: SidebarTokenStyle,
    },
}

impl AgentSidebarToken {
    pub(crate) fn parts(&self) -> (&Self, SidebarTokenStyle) {
        match self {
            Self::Styled { token, style } => (token, *style),
            token => (token, SidebarTokenStyle::default()),
        }
    }
}

impl SpaceSidebarToken {
    pub(crate) fn parts(&self) -> (&Self, SidebarTokenStyle) {
        match self {
            Self::Styled { token, style } => (token, *style),
            token => (token, SidebarTokenStyle::default()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStyledSidebarToken {
    token: String,
    #[serde(default)]
    fg: Option<SidebarTokenColor>,
    #[serde(default)]
    bold: Option<bool>,
    #[serde(default)]
    dim: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSidebarToken {
    Plain(String),
    Styled(RawStyledSidebarToken),
}

impl RawSidebarToken {
    fn parts(self) -> (String, Option<SidebarTokenStyle>) {
        match self {
            Self::Plain(token) => (token, None),
            Self::Styled(token) => (
                token.token,
                Some(SidebarTokenStyle {
                    fg: token.fg,
                    bold: token.bold,
                    dim: token.dim,
                }),
            ),
        }
    }
}

fn parse_sidebar_token<T>(value: String, builtins: &[(&str, T)]) -> Result<T, String>
where
    T: Clone + From<String>,
{
    if let Some((_, token)) = builtins.iter().find(|(name, _)| *name == value) {
        return Ok(token.clone());
    }
    let Some(name) = value.strip_prefix('$') else {
        return Err(format!(
            "unknown sidebar token `{value}`; custom tokens must start with `$`"
        ));
    };
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(format!("invalid custom sidebar token `{value}`"));
    }
    Ok(T::from(name.to_string()))
}

fn serialize_styled_token<S>(
    name: String,
    style: SidebarTokenStyle,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(None)?;
    map.serialize_entry("token", &name)?;
    if let Some(fg) = style.fg {
        map.serialize_entry("fg", &fg)?;
    }
    if let Some(bold) = style.bold {
        map.serialize_entry("bold", &bold)?;
    }
    if let Some(dim) = style.dim {
        map.serialize_entry("dim", &dim)?;
    }
    map.end()
}

fn agent_token_name(token: &AgentSidebarToken) -> String {
    match token {
        AgentSidebarToken::StateIcon => "state_icon".into(),
        AgentSidebarToken::StateText => "state_text".into(),
        AgentSidebarToken::Workspace => "workspace".into(),
        AgentSidebarToken::Tab => "tab".into(),
        AgentSidebarToken::Pane => "pane".into(),
        AgentSidebarToken::Agent => "agent".into(),
        AgentSidebarToken::TerminalTitle => "terminal_title".into(),
        AgentSidebarToken::TerminalTitleStripped => "terminal_title_stripped".into(),
        AgentSidebarToken::Custom(name) => format!("${name}"),
        AgentSidebarToken::Styled { token, .. } => agent_token_name(token),
    }
}

fn space_token_name(token: &SpaceSidebarToken) -> String {
    match token {
        SpaceSidebarToken::StateIcon => "state_icon".into(),
        SpaceSidebarToken::StateText => "state_text".into(),
        SpaceSidebarToken::Workspace => "workspace".into(),
        SpaceSidebarToken::Branch => "branch".into(),
        SpaceSidebarToken::GitStatus => "git_status".into(),
        SpaceSidebarToken::Custom(name) => format!("${name}"),
        SpaceSidebarToken::Styled { token, .. } => space_token_name(token),
    }
}

impl Serialize for AgentSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Styled { token, style } => {
                serialize_styled_token(agent_token_name(token), *style, serializer)
            }
            token => serializer.serialize_str(&agent_token_name(token)),
        }
    }
}

impl From<String> for AgentSidebarToken {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

impl<'de> Deserialize<'de> for AgentSidebarToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (value, style) = RawSidebarToken::deserialize(deserializer)?.parts();
        let token = parse_sidebar_token(
            value,
            &[
                ("state_icon", Self::StateIcon),
                ("state_text", Self::StateText),
                ("workspace", Self::Workspace),
                ("tab", Self::Tab),
                ("pane", Self::Pane),
                ("agent", Self::Agent),
                ("terminal_title", Self::TerminalTitle),
                ("terminal_title_stripped", Self::TerminalTitleStripped),
            ],
        )
        .map_err(serde::de::Error::custom)?;
        Ok(style.map_or(token.clone(), |style| Self::Styled {
            token: Box::new(token),
            style,
        }))
    }
}

impl Serialize for SpaceSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Styled { token, style } => {
                serialize_styled_token(space_token_name(token), *style, serializer)
            }
            token => serializer.serialize_str(&space_token_name(token)),
        }
    }
}

impl From<String> for SpaceSidebarToken {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

impl<'de> Deserialize<'de> for SpaceSidebarToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (value, style) = RawSidebarToken::deserialize(deserializer)?.parts();
        let token = parse_sidebar_token(
            value,
            &[
                ("state_icon", Self::StateIcon),
                ("state_text", Self::StateText),
                ("workspace", Self::Workspace),
                ("branch", Self::Branch),
                ("git_status", Self::GitStatus),
            ],
        )
        .map_err(serde::de::Error::custom)?;
        Ok(style.map_or(token.clone(), |style| Self::Styled {
            token: Box::new(token),
            style,
        }))
    }
}

type AgentSidebarRows = Vec<Vec<AgentSidebarToken>>;
type SpaceSidebarRows = Vec<Vec<SpaceSidebarToken>>;

fn deserialize_rows_by_agent<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, AgentSidebarRows>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let rows_by_agent = BTreeMap::<String, AgentSidebarRows>::deserialize(deserializer)?;
    for (id, rows) in &rows_by_agent {
        if crate::detect::parse_canonical_agent_label(id).is_none() {
            return Err(serde::de::Error::custom(format!(
                "unknown canonical agent id `{id}` in sidebar rows_by_agent"
            )));
        }
        validate_sidebar_rows(rows).map_err(serde::de::Error::custom)?;
    }
    Ok(rows_by_agent)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentsSidebarConfig {
    #[serde(deserialize_with = "deserialize_sidebar_rows")]
    pub rows: AgentSidebarRows,
    #[serde(default, deserialize_with = "deserialize_rows_by_agent")]
    pub rows_by_agent: BTreeMap<String, AgentSidebarRows>,
    pub row_gap: u16,
}

impl AgentsSidebarConfig {
    pub(crate) fn rows_for_agent(&self, agent: Option<Agent>) -> &AgentSidebarRows {
        agent
            .and_then(|agent| self.rows_by_agent.get(crate::detect::agent_label(agent)))
            .unwrap_or(&self.rows)
    }
}

impl Default for AgentsSidebarConfig {
    fn default() -> Self {
        Self {
            rows: vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Workspace,
                    AgentSidebarToken::Tab,
                ],
                vec![AgentSidebarToken::Agent],
            ],
            rows_by_agent: BTreeMap::new(),
            row_gap: DEFAULT_SIDEBAR_ROW_GAP,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SpacesSidebarConfig {
    #[serde(deserialize_with = "deserialize_sidebar_rows")]
    pub rows: SpaceSidebarRows,
    pub row_gap: u16,
}

impl Default for SpacesSidebarConfig {
    fn default() -> Self {
        Self {
            rows: vec![
                vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
                vec![SpaceSidebarToken::Branch, SpaceSidebarToken::GitStatus],
            ],
            row_gap: DEFAULT_SIDEBAR_ROW_GAP,
        }
    }
}

/// Expands a single argv/path token's leading `~` at config-load time.
///
/// Argv execution itself never expands `~` (herdr only expands it in coded
/// paths, e.g. `src/worktree.rs`), so any `~`-prefixed token in a `list`
/// section command must be resolved here, before the token ever reaches
/// `std::process::Command`.
fn expand_tilde_token(token: &str) -> String {
    if token == "~" || token.starts_with("~/") {
        crate::worktree::expand_tilde_path(token)
            .to_string_lossy()
            .into_owned()
    } else {
        token.to_string()
    }
}

fn deserialize_argv<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let argv = Vec::<String>::deserialize(deserializer)?;
    Ok(argv.iter().map(|token| expand_tilde_token(token)).collect())
}

fn deserialize_optional_path_token<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.map(|token| expand_tilde_token(&token)))
}

/// True when `token` still contains an unresolved `{...}` substitution
/// placeholder. Used to reject substitution inside `argv[0]`: the executable
/// itself is config-only, never built from provider-controlled row data.
fn contains_substitution_token(token: &str) -> bool {
    token.contains('{')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnWidth {
    #[default]
    Fill,
    Fixed(u16),
}

impl Serialize for ColumnWidth {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Fill => serializer.serialize_str("fill"),
            Self::Fixed(width) => serializer.serialize_u16(*width),
        }
    }
}

impl<'de> Deserialize<'de> for ColumnWidth {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawColumnWidth {
            Fixed(u16),
            Named(String),
        }

        match RawColumnWidth::deserialize(deserializer)? {
            RawColumnWidth::Fixed(width) => Ok(Self::Fixed(width)),
            RawColumnWidth::Named(name) if name == "fill" => Ok(Self::Fill),
            RawColumnWidth::Named(name) => Err(serde::de::Error::custom(format!(
                "unknown sidebar list column width `{name}`; expected \"fill\" or an integer"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnAlign {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnSpec {
    pub width: ColumnWidth,
    pub align: ColumnAlign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionTarget {
    #[default]
    Background,
    Overlay,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListActionConfig {
    pub id: String,
    pub label: String,
    #[serde(deserialize_with = "deserialize_argv")]
    pub command: Vec<String>,
    #[serde(default)]
    pub confirm: Option<String>,
    #[serde(default)]
    pub target: ActionTarget,
    #[serde(default, deserialize_with = "deserialize_optional_path_token")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub validate: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct ListSectionConfig {
    pub enabled: bool,
    pub placement: ListPlacement,
    pub collapsed: bool,
    pub refresh_seconds: u64,
    pub timeout_seconds: u64,
    pub max_visible_rows: u16,
    #[serde(deserialize_with = "deserialize_argv")]
    pub command: Vec<String>,
    pub modes: Vec<String>,
    pub columns: Vec<ColumnSpec>,
    pub styles: HashMap<String, SidebarTokenColor>,
    pub actions: Vec<ListActionConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListPlacement {
    #[default]
    Bottom,
}

impl ListSectionConfig {
    /// Load-time validation. Never panics; every violation becomes a
    /// diagnostic string surfaced through the existing config-diagnostic
    /// mechanism (`Config::collect_diagnostics`).
    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();

        if self.command.is_empty() {
            diagnostics.push(
                "ui.sidebar.list.command must not be empty; the jobs list section is disabled"
                    .to_string(),
            );
        } else if contains_substitution_token(&self.command[0]) {
            diagnostics.push(format!(
                "ui.sidebar.list.command[0] ({:?}) must not contain a substitution token; \
                 the executable is config-only",
                self.command[0]
            ));
        }

        if self.refresh_seconds < 1 {
            diagnostics.push(format!(
                "ui.sidebar.list.refresh_seconds ({}) must be at least 1",
                self.refresh_seconds
            ));
        }

        if self.timeout_seconds < 1 {
            diagnostics.push(format!(
                "ui.sidebar.list.timeout_seconds ({}) must be at least 1",
                self.timeout_seconds
            ));
        } else if self.timeout_seconds >= self.refresh_seconds {
            diagnostics.push(format!(
                "ui.sidebar.list.timeout_seconds ({}) must be less than refresh_seconds ({})",
                self.timeout_seconds, self.refresh_seconds
            ));
        }

        if self.modes.is_empty() {
            diagnostics.push("ui.sidebar.list.modes must not be empty".to_string());
        }

        let mut seen_ids: Vec<&str> = Vec::new();
        for action in &self.actions {
            if seen_ids.contains(&action.id.as_str()) {
                diagnostics.push(format!(
                    "ui.sidebar.list.actions has duplicate id {:?}",
                    action.id
                ));
            } else {
                seen_ids.push(&action.id);
            }

            if action.command.is_empty() {
                diagnostics.push(format!(
                    "ui.sidebar.list.actions[{:?}].command must not be empty",
                    action.id
                ));
            } else if contains_substitution_token(&action.command[0]) {
                diagnostics.push(format!(
                    "ui.sidebar.list.actions[{:?}].command[0] ({:?}) must not contain a \
                     substitution token; the executable is config-only",
                    action.id, action.command[0]
                ));
            }

            for (token, pattern) in &action.validate {
                if let Err(err) = Regex::new(pattern) {
                    diagnostics.push(format!(
                        "ui.sidebar.list.actions[{:?}].validate.{token} ({pattern:?}) is not a \
                         valid regex: {err}",
                        action.id
                    ));
                }
            }
        }

        diagnostics
    }

    /// `self` unchanged if it passes its own [`Self::diagnostics`] checks,
    /// otherwise a disabled fallback (design doc finding: "invalid
    /// cadence/timeout is diagnosed but still applied" -- e.g.
    /// `refresh_seconds = 1` with `timeout_seconds = 60` was reported as a
    /// diagnostic at startup and then run anyway). Every polling code path
    /// (`list_refresh.rs`) already gates on `enabled`, so disabling here is
    /// sufficient to keep a misconfigured refresh/timeout pair -- or any
    /// other `diagnostics()` violation -- from ever actually running,
    /// without silently rewriting values the user wrote.
    pub fn sanitized(&self) -> Self {
        if self.diagnostics().is_empty() {
            self.clone()
        } else {
            Self {
                enabled: false,
                ..self.clone()
            }
        }
    }
}

impl Default for ListSectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            placement: ListPlacement::Bottom,
            collapsed: true,
            refresh_seconds: 10,
            timeout_seconds: 5,
            max_visible_rows: 12,
            command: vec![
                "/usr/bin/python3.11".to_string(),
                // Design doc: "The flags are not optional. Python startup
                // runs sitecustomize and user-site code before main(), and
                // anything that prints there lands on stdout ahead of the
                // JSON... -I (isolated) and -S (no site) close that. Because
                // herdr executes `python3.11 <script>` rather than exec'ing
                // the file, the shebang alone is not enough."
                "-I".to_string(),
                "-S".to_string(),
                expand_tilde_token("~/.config/herdr/scripts/herdr-jobs.py"),
                "--mode".to_string(),
                "{mode}".to_string(),
            ],
            modes: vec!["live".to_string(), "history".to_string()],
            columns: vec![
                ColumnSpec {
                    width: ColumnWidth::Fill,
                    align: ColumnAlign::Left,
                },
                ColumnSpec {
                    width: ColumnWidth::Fixed(3),
                    align: ColumnAlign::Right,
                },
                ColumnSpec {
                    width: ColumnWidth::Fixed(7),
                    align: ColumnAlign::Right,
                },
            ],
            styles: HashMap::from([
                ("ok".to_string(), parse_style_hex("#b8bb26")),
                ("fail".to_string(), parse_style_hex("#fb4934")),
                ("warn".to_string(), parse_style_hex("#fabd2f")),
                ("muted".to_string(), parse_style_hex("#928374")),
                ("normal".to_string(), parse_style_hex("#ebdbb2")),
            ]),
            actions: vec![
                ListActionConfig {
                    id: "cancel".to_string(),
                    label: "Cancel job".to_string(),
                    command: vec![
                        "scancel".to_string(),
                        "--".to_string(),
                        "{id}".to_string(),
                    ],
                    confirm: Some("Cancel job {id} ({cell0})?".to_string()),
                    target: ActionTarget::Background,
                    cwd: None,
                    validate: HashMap::from([(
                        "id".to_string(),
                        r"^[0-9]+(_[0-9]+)?(\+[0-9]+)?$".to_string(),
                    )]),
                },
                ListActionConfig {
                    id: "tail".to_string(),
                    label: "Tail log".to_string(),
                    command: vec![
                        "tail".to_string(),
                        "-f".to_string(),
                        "--".to_string(),
                        "{log}".to_string(),
                    ],
                    confirm: None,
                    target: ActionTarget::Overlay,
                    cwd: Some("{dir}".to_string()),
                    validate: HashMap::new(),
                },
            ],
        }
    }
}

/// Parses a default `#RRGGBB` style color. Only used to build the built-in
/// default style table; the `expect` is unreachable since every literal here
/// is a valid, test-covered hex string.
fn parse_style_hex(hex: &str) -> SidebarTokenColor {
    SidebarTokenColor::parse_hex(hex).expect("default style color must parse")
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SidebarConfig {
    pub agents: AgentsSidebarConfig,
    pub spaces: SpacesSidebarConfig,
    pub list: ListSectionConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_compact_agent_and_existing_space_layouts() {
        let config = SidebarConfig::default();
        assert_eq!(
            config.agents.rows,
            vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Workspace,
                    AgentSidebarToken::Tab,
                ],
                vec![AgentSidebarToken::Agent],
            ]
        );
        assert!(config.agents.rows_by_agent.is_empty());
        assert_eq!(config.agents.row_gap, 0);
        assert_eq!(
            config.spaces.rows,
            vec![
                vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
                vec![SpaceSidebarToken::Branch, SpaceSidebarToken::GitStatus],
            ]
        );
        assert_eq!(config.spaces.row_gap, 0);
    }

    #[test]
    fn parses_builtin_and_arbitrary_custom_tokens() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.agents]
rows = [["state_icon", "workspace"], ["state_text", "agent", "$summary"], ["terminal_title", "terminal_title_stripped", "$terminal_title"]]
row_gap = 1

[ui.sidebar.agents.rows_by_agent]
claude = [["terminal_title_stripped"], ["agent", "$model"]]

[ui.sidebar.spaces]
rows = [["workspace"], ["$jj_status"]]
row_gap = 3
"#,
        )
        .expect("sidebar token config");

        assert_eq!(
            config.ui.sidebar.agents.rows[1],
            vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::Agent,
                AgentSidebarToken::Custom("summary".into()),
            ]
        );
        assert_eq!(
            config.ui.sidebar.agents.rows[2],
            vec![
                AgentSidebarToken::TerminalTitle,
                AgentSidebarToken::TerminalTitleStripped,
                AgentSidebarToken::Custom("terminal_title".into()),
            ]
        );
        assert_eq!(
            config.ui.sidebar.agents.rows_by_agent["claude"],
            vec![
                vec![AgentSidebarToken::TerminalTitleStripped],
                vec![
                    AgentSidebarToken::Agent,
                    AgentSidebarToken::Custom("model".into()),
                ],
            ]
        );
        assert_eq!(config.ui.sidebar.agents.row_gap, 1);
        assert_eq!(
            config.ui.sidebar.spaces.rows[1],
            vec![SpaceSidebarToken::Custom("jj_status".into())]
        );
        assert_eq!(config.ui.sidebar.spaces.row_gap, 3);
    }

    #[test]
    fn parses_occurrence_styles_without_changing_plain_tokens() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.agents]
rows = [[{ token = "workspace", fg = "#abc", bold = false }, "workspace"], [{ token = "$summary", dim = false }]]

[ui.sidebar.agents.rows_by_agent]
claude = [[{ token = "agent", fg = "#112233", bold = true, dim = false }]]

[ui.sidebar.spaces]
rows = [[{ token = "git_status", fg = "#ff00aa" }], [{ token = "$jj", bold = true }]]
"##,
        )
        .unwrap();

        let (token, style) = config.ui.sidebar.agents.rows[0][0].parts();
        assert_eq!(token, &AgentSidebarToken::Workspace);
        assert_eq!(style.bold, Some(false));
        assert_eq!(
            style.fg.unwrap().ratatui(),
            ratatui::style::Color::Rgb(0xaa, 0xbb, 0xcc)
        );
        assert_eq!(
            config.ui.sidebar.agents.rows[0][1],
            AgentSidebarToken::Workspace
        );

        let (token, style) = config.ui.sidebar.agents.rows_by_agent["claude"][0][0].parts();
        assert_eq!(token, &AgentSidebarToken::Agent);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.dim, Some(false));

        let (token, style) = config.ui.sidebar.spaces.rows[0][0].parts();
        assert_eq!(token, &SpaceSidebarToken::GitStatus);
        assert_eq!(
            style.fg.unwrap().ratatui(),
            ratatui::style::Color::Rgb(0xff, 0x00, 0xaa)
        );
        let (token, style) = config.ui.sidebar.spaces.rows[1][0].parts();
        assert_eq!(token, &SpaceSidebarToken::Custom("jj".into()));
        assert_eq!(style.bold, Some(true));
    }

    #[test]
    fn rejects_invalid_occurrence_styles() {
        for entry in [
            r##"{ token = "workspace", fg = "red" }"##,
            r##"{ token = "workspace", fg = "#abcd" }"##,
            r##"{ token = "workspace", underline = true }"##,
        ] {
            let input = format!("[ui.sidebar.agents]\nrows = [[{entry}]]\n");
            assert!(
                toml::from_str::<crate::config::Config>(&input).is_err(),
                "accepted {entry}"
            );
        }
    }

    #[test]
    fn rejects_unknown_bare_and_malformed_custom_tokens() {
        for token in ["summary", "$", "$bad.name"] {
            let input = format!("[ui.sidebar.agents]\\nrows = [[\"{token}\"]]\\n");
            assert!(toml::from_str::<crate::config::Config>(&input).is_err());
        }
    }

    #[test]
    fn rejects_oversized_sidebar_layouts() {
        let too_many_rows = std::iter::repeat_n("[\"agent\"]", MAX_SIDEBAR_ROWS + 1)
            .collect::<Vec<_>>()
            .join(",");
        let input = format!("[ui.sidebar.agents]\nrows = [{too_many_rows}]\n");
        assert!(toml::from_str::<crate::config::Config>(&input).is_err());

        let too_many_tokens = std::iter::repeat_n("\"workspace\"", MAX_SIDEBAR_TOKENS_PER_ROW + 1)
            .collect::<Vec<_>>()
            .join(",");
        let input = format!("[ui.sidebar.spaces]\nrows = [[{too_many_tokens}]]\n");
        assert!(toml::from_str::<crate::config::Config>(&input).is_err());

        let input = format!("[ui.sidebar.agents.rows_by_agent]\nclaude = [{too_many_rows}]\n");
        assert!(toml::from_str::<crate::config::Config>(&input).is_err());
    }

    #[test]
    fn accepts_every_canonical_agent_override_key() {
        let agents = [
            Agent::Pi,
            Agent::Claude,
            Agent::Codex,
            Agent::Gemini,
            Agent::Cursor,
            Agent::Devin,
            Agent::Antigravity,
            Agent::Cline,
            Agent::Omp,
            Agent::Mastracode,
            Agent::OpenCode,
            Agent::GithubCopilot,
            Agent::Kimi,
            Agent::Kiro,
            Agent::Droid,
            Agent::Amp,
            Agent::Grok,
            Agent::Hermes,
            Agent::Kilo,
            Agent::Qodercli,
            Agent::Qwen,
            Agent::Maki,
        ];
        let entries = agents
            .iter()
            .map(|agent| format!("{} = [[\"agent\"]]", crate::detect::agent_label(*agent)))
            .collect::<Vec<_>>()
            .join("\n");
        let input = format!("[ui.sidebar.agents.rows_by_agent]\n{entries}\n");
        let config: crate::config::Config = toml::from_str(&input).expect("canonical keys");

        assert_eq!(config.ui.sidebar.agents.rows_by_agent.len(), agents.len());
    }

    #[test]
    fn rejects_alias_case_whitespace_and_unknown_override_keys() {
        for key in ["claude-code", "Claude", "' claude '", "unknown"] {
            let input = format!("[ui.sidebar.agents.rows_by_agent]\n{key} = [[\"agent\"]]\n");
            assert!(
                toml::from_str::<crate::config::Config>(&input).is_err(),
                "accepted key {key:?}"
            );
        }
    }

    // --- ui.sidebar.list ---

    #[test]
    fn list_section_config_defaults_match_the_spec_example() {
        // Defaults expand `~` against $HOME; hold the shared env lock so a
        // concurrent test mutating HOME can't race this read.
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let list = ListSectionConfig::default();

        assert!(list.enabled);
        assert_eq!(list.placement, ListPlacement::Bottom);
        assert!(list.collapsed);
        assert_eq!(list.refresh_seconds, 10);
        assert_eq!(list.timeout_seconds, 5);
        assert_eq!(list.max_visible_rows, 12);
        assert_eq!(
            list.command,
            vec![
                "/usr/bin/python3.11".to_string(),
                "-I".to_string(),
                "-S".to_string(),
                expand_tilde_token("~/.config/herdr/scripts/herdr-jobs.py"),
                "--mode".to_string(),
                "{mode}".to_string(),
            ]
        );
        assert_eq!(list.modes, vec!["live".to_string(), "history".to_string()]);
        assert_eq!(
            list.columns,
            vec![
                ColumnSpec {
                    width: ColumnWidth::Fill,
                    align: ColumnAlign::Left,
                },
                ColumnSpec {
                    width: ColumnWidth::Fixed(3),
                    align: ColumnAlign::Right,
                },
                ColumnSpec {
                    width: ColumnWidth::Fixed(7),
                    align: ColumnAlign::Right,
                },
            ]
        );
        assert_eq!(list.styles.len(), 5);
        assert_eq!(
            list.styles["ok"].ratatui(),
            ratatui::style::Color::Rgb(0xb8, 0xbb, 0x26)
        );
        assert_eq!(list.actions.len(), 2);
        assert_eq!(list.actions[0].id, "cancel");
        assert_eq!(list.actions[0].target, ActionTarget::Background);
        assert_eq!(list.actions[1].id, "tail");
        assert_eq!(list.actions[1].target, ActionTarget::Overlay);
        assert_eq!(list.actions[1].cwd.as_deref(), Some("{dir}"));

        // The default config is itself load-time valid.
        assert!(list.diagnostics().is_empty());

        // SidebarConfig::default() wires the list section in.
        assert_eq!(SidebarConfig::default().list, list);
    }

    #[test]
    fn list_section_parses_from_toml_matching_the_spec_example() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.list]
enabled = true
placement = "bottom"
collapsed = true
refresh_seconds = 10
timeout_seconds = 5
max_visible_rows = 12
command = ["/usr/bin/python3.11", "-I", "-S", "/opt/herdr-jobs.py", "--mode", "{mode}"]
modes = ["live", "history"]

columns = [
  { width = "fill", align = "left"  },
  { width = 3,      align = "right" },
  { width = 7,      align = "right" },
]

[ui.sidebar.list.styles]
ok = "#b8bb26"
fail = "#fb4934"

[[ui.sidebar.list.actions]]
id = "cancel"
label = "Cancel job"
command = ["scancel", "--", "{id}"]
confirm = "Cancel job {id} ({cell0})?"
validate = { id = "^[0-9]+(_[0-9]+)?(\\+[0-9]+)?$" }

[[ui.sidebar.list.actions]]
id = "tail"
label = "Tail log"
command = ["tail", "-f", "--", "{log}"]
target = "overlay"
cwd = "{dir}"
"##,
        )
        .expect("list section config");

        let list = &config.ui.sidebar.list;
        assert_eq!(
            list.columns,
            vec![
                ColumnSpec {
                    width: ColumnWidth::Fill,
                    align: ColumnAlign::Left,
                },
                ColumnSpec {
                    width: ColumnWidth::Fixed(3),
                    align: ColumnAlign::Right,
                },
                ColumnSpec {
                    width: ColumnWidth::Fixed(7),
                    align: ColumnAlign::Right,
                },
            ]
        );
        assert_eq!(list.actions[0].target, ActionTarget::Background);
        assert_eq!(list.actions[1].target, ActionTarget::Overlay);
        assert_eq!(list.actions[1].cwd.as_deref(), Some("{dir}"));
        assert!(list.diagnostics().is_empty());
    }

    #[test]
    fn list_section_rejects_unknown_column_width() {
        let input = r#"
[ui.sidebar.list]
columns = [{ width = "banana", align = "left" }]
"#;
        assert!(toml::from_str::<crate::config::Config>(input).is_err());
    }

    #[test]
    fn list_section_rejects_unknown_fields_on_columns_and_actions() {
        let input = r#"
[ui.sidebar.list]
columns = [{ width = "fill", align = "left", bogus = true }]
"#;
        assert!(toml::from_str::<crate::config::Config>(input).is_err());

        let input = r#"
[[ui.sidebar.list.actions]]
id = "x"
label = "X"
command = ["true"]
bogus = true
"#;
        assert!(toml::from_str::<crate::config::Config>(input).is_err());
    }

    #[test]
    fn list_section_command_and_action_cwd_tilde_expand_at_load_time() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/test-user");

        let result: Result<crate::config::Config, _> = toml::from_str(
            r#"
[ui.sidebar.list]
command = ["/usr/bin/python3.11", "~/.config/herdr/scripts/herdr-jobs.py"]

[[ui.sidebar.list.actions]]
id = "tail"
label = "Tail log"
command = ["tail", "-f", "--", "{log}"]
cwd = "~/logs"
"#,
        );

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        let config = result.expect("list section config");
        assert_eq!(
            config.ui.sidebar.list.command,
            vec![
                "/usr/bin/python3.11".to_string(),
                "/home/test-user/.config/herdr/scripts/herdr-jobs.py".to_string(),
            ]
        );
        assert_eq!(
            config.ui.sidebar.list.actions[0].cwd.as_deref(),
            Some("/home/test-user/logs")
        );
        // Substitution tokens are not paths and must survive untouched.
        assert_eq!(config.ui.sidebar.list.actions[0].command[3], "{log}");
    }

    #[test]
    fn diagnostics_flags_empty_command() {
        let list = ListSectionConfig {
            command: Vec::new(),
            ..ListSectionConfig::default()
        };
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("command must not be empty")));
    }

    #[test]
    fn diagnostics_flags_substitution_token_in_argv0() {
        let mut list = ListSectionConfig::default();
        list.command[0] = "{mode}".to_string();
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("command[0]") && diag.contains("substitution token")));
    }

    #[test]
    fn diagnostics_flags_refresh_seconds_below_one() {
        let list = ListSectionConfig {
            refresh_seconds: 0,
            ..ListSectionConfig::default()
        };
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("refresh_seconds") && diag.contains("at least 1")));
    }

    #[test]
    fn diagnostics_flags_timeout_seconds_below_one() {
        let list = ListSectionConfig {
            timeout_seconds: 0,
            ..ListSectionConfig::default()
        };
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("timeout_seconds") && diag.contains("at least 1")));
    }

    #[test]
    fn diagnostics_flags_timeout_seconds_not_less_than_refresh_seconds() {
        let mut list = ListSectionConfig {
            refresh_seconds: 5,
            timeout_seconds: 5,
            ..ListSectionConfig::default()
        };
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("must be less than refresh_seconds")));

        list.timeout_seconds = 6;
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("must be less than refresh_seconds")));
    }

    /// Finding 8: an invalid subsection must not run just because it was
    /// diagnosed. `sanitized()` is the startup-time counterpart of live
    /// reload's "keep the previous config" behavior -- there's no previous
    /// config to fall back to on first load, so it disables the section
    /// instead of applying a broken cadence/timeout pair.
    #[test]
    fn sanitized_disables_a_list_section_that_fails_its_own_diagnostics() {
        let list = ListSectionConfig {
            refresh_seconds: 1,
            timeout_seconds: 60,
            ..ListSectionConfig::default()
        };
        assert!(!list.diagnostics().is_empty());

        let sanitized = list.sanitized();
        assert!(!sanitized.enabled);
        // Everything else survives untouched -- only `enabled` changes.
        assert_eq!(sanitized.refresh_seconds, 1);
        assert_eq!(sanitized.timeout_seconds, 60);
    }

    #[test]
    fn sanitized_is_a_no_op_for_a_valid_list_section() {
        let list = ListSectionConfig::default();
        assert!(list.diagnostics().is_empty());
        assert_eq!(list.sanitized(), list);
    }

    #[test]
    fn diagnostics_flags_empty_modes() {
        let list = ListSectionConfig {
            modes: Vec::new(),
            ..ListSectionConfig::default()
        };
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("modes must not be empty")));
    }

    #[test]
    fn diagnostics_flags_duplicate_action_ids() {
        let mut list = ListSectionConfig::default();
        let mut duplicate = list.actions[0].clone();
        duplicate.id = list.actions[1].id.clone();
        list.actions.push(duplicate);
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("duplicate id")));
    }

    #[test]
    fn diagnostics_flags_action_command_empty_and_argv0_substitution() {
        let mut list = ListSectionConfig::default();
        list.actions[0].command = Vec::new();
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("actions[\"cancel\"].command must not be empty")));

        let mut list = ListSectionConfig::default();
        list.actions[0].command[0] = "{id}".to_string();
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("actions[\"cancel\"].command[0]")
                && diag.contains("substitution token")));
    }

    #[test]
    fn diagnostics_flags_invalid_action_validate_regex() {
        let mut list = ListSectionConfig::default();
        list.actions[0]
            .validate
            .insert("id".to_string(), "(unclosed".to_string());
        assert!(list
            .diagnostics()
            .iter()
            .any(|diag| diag.contains("validate.id") && diag.contains("not a valid regex")));
    }
}
