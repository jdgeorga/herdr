//! Provider protocol: parsing and validation of the JSON a list-section
//! provider script prints to stdout.
//!
//! This module is pure — no I/O, no subprocess handling, no `AppState`. It
//! turns an untrusted JSON string into a [`ParsedPayload`] the renderer can
//! trust, or a [`ParseFailure`] describing why the whole payload was
//! rejected. Row-level problems (a bad row, a duplicate id, an unknown style)
//! do not fail the payload; they are dropped and recorded as
//! [`ParseDiagnostic`]s instead.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

/// Only version the parser accepts. A mismatch (including a missing or
/// non-numeric `version` field) is a hard error, not a warning.
pub const PROTOCOL_VERSION: u64 = 1;

pub const MAX_GROUPS: usize = 200;
pub const MAX_ROWS: usize = 2000;
pub const MAX_CELLS_PER_ROW: usize = 16;
pub const MAX_VARS_PER_ROW: usize = 32;
pub const MAX_STRING_BYTES: usize = 1024;
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;

// -- Wire types --------------------------------------------------------
//
// These mirror the JSON schema loosely: unknown fields are ignored (no
// `deny_unknown_fields`), and anything the schema doesn't strictly need is
// defaulted so a single malformed row/group/notify entry fails to
// deserialize on its own, rather than taking the rest of the array down
// with it. Array-of-object fields are decoded as `Value` first and
// converted one element at a time in `parse_provider_payload`.

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderPayload {
    // `version` is validated against the raw `Value` (see the
    // `found_version` check in `parse_provider_payload`) before this struct
    // is decoded, so it is intentionally not a field here.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub groups: Vec<Value>,
    #[serde(default)]
    pub notify: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderGroup {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub rows: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderRow {
    pub id: String,
    #[serde(default)]
    pub cells: Vec<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderNotify {
    pub id: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

// -- Validated output ---------------------------------------------------

/// A row style name. The wire protocol's built-in names (`normal`, `ok`,
/// `fail`, `warn`, `muted`) always resolve to the matching variant here.
/// Design doc: "`style` is a name resolved against config" -- config can
/// also define names outside that built-in set (e.g. `critical` in
/// `[ui.sidebar.list.styles]`), and this parser is pure (no config access),
/// so `parse_provider_payload` takes the caller's configured names as
/// `known_styles` and treats any of them as *valid* (no diagnostic) even
/// though this closed enum still can't carry the arbitrary name through to
/// the renderer -- it resolves to `Normal` either way. Only a name that is
/// neither a built-in nor in `known_styles` is genuinely unknown and is
/// downgraded to `Normal` *with* a diagnostic. Giving `RowStyle` an actual
/// custom-name variant is a renderer-side change (`src/ui/sidebar.rs`'s
/// `row_style_name`/`resolve_row_style`, which exhaustively match on this
/// type) outside this module's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowStyle {
    #[default]
    Normal,
    Ok,
    Fail,
    Warn,
    Muted,
}

impl RowStyle {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "normal" => Some(Self::Normal),
            "ok" => Some(Self::Ok),
            "fail" => Some(Self::Fail),
            "warn" => Some(Self::Warn),
            "muted" => Some(Self::Muted),
            _ => None,
        }
    }
}

/// A notification urgency level, mapped onto toast kinds downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyLevel {
    #[default]
    Info,
    Ok,
    Warn,
    Fail,
}

impl NotifyLevel {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "info" => Some(Self::Info),
            "ok" => Some(Self::Ok),
            "warn" => Some(Self::Warn),
            "fail" => Some(Self::Fail),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub message: String,
}

impl ParseDiagnostic {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRow {
    pub id: String,
    pub cells: Vec<String>,
    pub style: RowStyle,
    pub vars: BTreeMap<String, String>,
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedGroup {
    pub id: String,
    pub label: String,
    pub rows: Vec<ParsedRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNotify {
    pub id: String,
    pub level: NotifyLevel,
    pub text: String,
}

/// The validated, sanitized result of parsing a provider payload. All
/// strings have had control characters and terminal escape sequences
/// stripped. A payload with no groups and no notifications is a valid,
/// empty result — distinct from a [`ParseFailure`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedPayload {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub groups: Vec<ParsedGroup>,
    pub notify: Vec<ParsedNotify>,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Why a payload was rejected outright, rather than degraded row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseFailure {
    /// The input was not valid JSON, or the top-level value was not a
    /// JSON object.
    Malformed(String),
    /// `version` was missing, not a non-negative integer, or did not equal
    /// [`PROTOCOL_VERSION`].
    VersionMismatch { expected: u64, found: Option<u64> },
    /// One of the documented caps was exceeded.
    Oversized(String),
}

/// Parses and validates a provider's JSON output. `known_styles` is the set
/// of row style names the caller's config actually resolves (design doc:
/// "`style` is a name resolved against config") -- see [`RowStyle`]'s docs
/// for why a name in this list still doesn't get its own `RowStyle` variant
/// yet, only a suppressed diagnostic.
///
/// Row, group and notify entries that are individually malformed, or that
/// duplicate an id already seen, are dropped with a [`ParseDiagnostic`] and
/// do not fail the payload. Exceeding a cap, a JSON syntax error, or a
/// `version` mismatch fail the whole payload.
pub fn parse_provider_payload(
    input: &str,
    known_styles: &[&str],
) -> Result<ParsedPayload, ParseFailure> {
    if input.len() > MAX_TOTAL_BYTES {
        return Err(ParseFailure::Oversized(format!(
            "payload is {} bytes, exceeding the {MAX_TOTAL_BYTES} byte cap",
            input.len()
        )));
    }

    // Bounds nesting depth on the *raw text*, before `serde_json` ever gets
    // to build a `Value` tree out of it. `serde_json`'s own recursive-descent
    // parser has no built-in depth limit, so a payload that is otherwise
    // within the total-size cap (256 KiB of just `[`/`]` pairs is well over
    // 100,000 levels) can stack-overflow *during* `serde_json::from_str`,
    // before any depth check on the parsed `Value` (in `check_value_caps`)
    // ever runs. This check must come first.
    check_raw_nesting_depth(input).map_err(ParseFailure::Oversized)?;

    let root: Value = serde_json::from_str(input)
        .map_err(|err| ParseFailure::Malformed(bounded_diagnostic_detail(&err)))?;
    let Some(obj) = root.as_object() else {
        return Err(ParseFailure::Malformed(
            "top-level value must be a JSON object".to_string(),
        ));
    };

    let found_version = obj.get("version").and_then(Value::as_u64);
    if found_version != Some(PROTOCOL_VERSION) {
        return Err(ParseFailure::VersionMismatch {
            expected: PROTOCOL_VERSION,
            found: found_version,
        });
    }

    check_caps(&root).map_err(ParseFailure::Oversized)?;

    // `obj`'s borrow of `root` is done (its only use was the version check
    // above); `root` moves into the top-level typed decode. A malformed
    // top-level shape (e.g. `groups` not an array at all) fails the whole
    // payload here — unlike a bad group/row/notify *element*, which is
    // isolated and dropped below.
    let payload: ProviderPayload = serde_json::from_value(root)
        .map_err(|err| ParseFailure::Malformed(bounded_diagnostic_detail(&err)))?;

    let mut diagnostics = Vec::new();

    let title = payload.title.map(sanitize_provider_string);
    let summary = payload.summary.map(sanitize_provider_string);

    let mut seen_group_ids = HashSet::new();
    let mut seen_row_ids = HashSet::new();
    let mut groups = Vec::with_capacity(payload.groups.len());

    for raw_group in payload.groups {
        let group: ProviderGroup = match serde_json::from_value(raw_group) {
            Ok(group) => group,
            Err(err) => {
                diagnostics.push(ParseDiagnostic::new(format!(
                    "dropped invalid group: {}",
                    bounded_diagnostic_detail(&err)
                )));
                continue;
            }
        };
        let id = sanitize_provider_string(&group.id);
        if id.is_empty() {
            diagnostics.push(ParseDiagnostic::new("dropped group with empty id"));
            continue;
        }
        if !seen_group_ids.insert(id.clone()) {
            diagnostics.push(ParseDiagnostic::new(format!(
                "dropped duplicate group id `{id}`"
            )));
            continue;
        }
        let label = group
            .label
            .map(|label| sanitize_provider_string(&label))
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| id.clone());

        let mut rows = Vec::with_capacity(group.rows.len());
        for raw_row in group.rows {
            let row: ProviderRow = match serde_json::from_value(raw_row) {
                Ok(row) => row,
                Err(err) => {
                    diagnostics.push(ParseDiagnostic::new(format!(
                        "dropped invalid row: {}",
                        bounded_diagnostic_detail(&err)
                    )));
                    continue;
                }
            };
            let row_id = sanitize_provider_string(&row.id);
            if row_id.is_empty() {
                diagnostics.push(ParseDiagnostic::new("dropped row with empty id"));
                continue;
            }
            if !seen_row_ids.insert(row_id.clone()) {
                diagnostics.push(ParseDiagnostic::new(format!(
                    "dropped duplicate row id `{row_id}`"
                )));
                continue;
            }

            let cells = row.cells.iter().map(sanitize_provider_string).collect();

            let style = match row.style.map(|raw| sanitize_provider_string(&raw)) {
                None => RowStyle::default(),
                Some(raw) => RowStyle::parse(&raw).unwrap_or_else(|| {
                    // A name outside the built-in set is only "unknown" --
                    // and worth a diagnostic -- if the caller's config
                    // doesn't actually define it. A configured custom style
                    // name is legitimate (design doc: "`style` is a name
                    // resolved against config") and must not warn on every
                    // single poll (see `RowStyle`'s docs for the remaining
                    // gap: it still can't be *rendered* as anything but
                    // `Normal` from inside this closed enum).
                    if !known_styles.contains(&raw.as_str()) {
                        diagnostics.push(ParseDiagnostic::new(format!(
                            "row `{row_id}` has unknown style `{raw}`; using normal"
                        )));
                    }
                    RowStyle::default()
                }),
            };

            let vars = row
                .vars
                .into_iter()
                .map(|(key, value)| {
                    (
                        sanitize_provider_string(&key),
                        sanitize_provider_string(&value),
                    )
                })
                .collect();

            let actions = row.actions.iter().map(sanitize_provider_string).collect();

            rows.push(ParsedRow {
                id: row_id,
                cells,
                style,
                vars,
                actions,
            });
        }

        groups.push(ParsedGroup { id, label, rows });
    }

    let mut seen_notify_ids = HashSet::new();
    let mut notify = Vec::with_capacity(payload.notify.len());

    for raw_notify in payload.notify {
        let entry: ProviderNotify = match serde_json::from_value(raw_notify) {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(ParseDiagnostic::new(format!(
                    "dropped invalid notify entry: {}",
                    bounded_diagnostic_detail(&err)
                )));
                continue;
            }
        };
        let id = sanitize_provider_string(&entry.id);
        if id.is_empty() {
            diagnostics.push(ParseDiagnostic::new("dropped notify entry with empty id"));
            continue;
        }
        if !seen_notify_ids.insert(id.clone()) {
            diagnostics.push(ParseDiagnostic::new(format!(
                "dropped duplicate notify id `{id}`"
            )));
            continue;
        }

        let level = match entry.level.map(|raw| sanitize_provider_string(&raw)) {
            None => NotifyLevel::default(),
            Some(raw) => NotifyLevel::parse(&raw).unwrap_or_else(|| {
                diagnostics.push(ParseDiagnostic::new(format!(
                    "notify `{id}` has unknown level `{raw}`; using info"
                )));
                NotifyLevel::default()
            }),
        };

        let text = entry
            .text
            .map(|text| sanitize_provider_string(&text))
            .unwrap_or_default();

        notify.push(ParsedNotify { id, level, text });
    }

    Ok(ParsedPayload {
        title,
        summary,
        groups,
        notify,
        diagnostics,
    })
}

/// Nesting depth cap shared by [`check_raw_nesting_depth`] (on the raw text,
/// before `serde_json` parses it) and [`check_value_caps`] (on the parsed
/// `Value`, as defense in depth). Comfortably above anything the documented
/// protocol shape needs (payload -> groups -> group -> rows -> row ->
/// cells/vars is a handful of levels), far below anything that risks a
/// stack overflow.
const MAX_JSON_DEPTH: usize = 64;

/// Scans raw JSON text for `{`/`[` nesting depth, without decoding it,
/// bailing as soon as depth exceeds [`MAX_JSON_DEPTH`]. Structural bracket
/// characters inside JSON string literals are ignored (a naive byte scan
/// would miscount a string that happens to contain `[`/`{`), tracked with a
/// minimal string/escape state machine -- this doesn't need to validate the
/// JSON, only find where strings start and end, which does not require
/// interpreting `\uXXXX` escapes: skipping exactly one character after an
/// unescaped `\\` is enough to never mistake an escaped `\"` for the end of
/// the string.
///
/// This must run *before* `serde_json::from_str`: `serde_json`'s recursive
/// descent parser has no depth limit of its own, so without this,
/// pathologically nested input well within [`MAX_TOTAL_BYTES`] (a few
/// hundred KiB of nothing but `[`/`]`) can overflow the stack while
/// `serde_json` is still building the `Value` tree, before this module ever
/// gets a `Value` to inspect.
fn check_raw_nesting_depth(input: &str) -> Result<(), String> {
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    for byte in input.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(format!(
                        "payload nesting exceeds the {MAX_JSON_DEPTH} level depth cap"
                    ));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Cheap, structural pre-scan for the documented caps. Runs before any
/// sanitization, per-row validation, or serde decoding, so an oversized or
/// pathologically nested payload is rejected before any of that more
/// expensive (and, for serde decoding, diagnostic-message-producing) work.
fn check_caps(root: &Value) -> Result<(), String> {
    // Walks every string in the payload -- object values *and* object
    // keys (e.g. `vars` keys), at every depth -- against the per-string
    // byte cap, instead of the previous field-by-field allowlist (title,
    // summary, group id/label, row fields), which missed keys entirely and
    // missed any string sitting somewhere that allowlist didn't visit (for
    // example a raw string as a `groups[]` element instead of an object).
    // Depth-bounded so recursion can't blow the stack on nested input.
    check_value_caps(root, 0)?;

    let groups: &[Value] = root
        .get("groups")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if groups.len() > MAX_GROUPS {
        return Err(format!(
            "{} groups, exceeding the {MAX_GROUPS} group cap",
            groups.len()
        ));
    }

    let mut total_rows = 0usize;
    for group in groups {
        let rows: &[Value] = group
            .get("rows")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        total_rows += rows.len();
        if total_rows > MAX_ROWS {
            return Err(format!("more than {MAX_ROWS} rows across all groups"));
        }

        for row in rows {
            if let Some(cells) = row.get("cells").and_then(Value::as_array) {
                if cells.len() > MAX_CELLS_PER_ROW {
                    return Err(format!(
                        "row has {} cells, exceeding the {MAX_CELLS_PER_ROW} cell cap",
                        cells.len()
                    ));
                }
            }
            if let Some(vars) = row.get("vars").and_then(Value::as_object) {
                if vars.len() > MAX_VARS_PER_ROW {
                    return Err(format!(
                        "row has {} vars, exceeding the {MAX_VARS_PER_ROW} var cap",
                        vars.len()
                    ));
                }
            }
        }
    }

    Ok(())
}

/// Recursively enforces the per-string byte cap ([`MAX_STRING_BYTES`]) on
/// every JSON string in `value` -- both object *values* and object *keys*
/// -- and rejects input nested deeper than [`MAX_JSON_DEPTH`]. Byte length
/// (`str::len`), not `chars().count()`, per the design doc: the cap is
/// measured in UTF-8 bytes.
///
/// Never includes the offending string's own contents in the returned
/// message: the string that triggered this can be up to [`MAX_TOTAL_BYTES`]
/// long, and this message is surfaced to the user via [`ParseFailure`].
fn check_value_caps(value: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_JSON_DEPTH {
        return Err(format!(
            "payload nesting exceeds the {MAX_JSON_DEPTH} level depth cap"
        ));
    }
    match value {
        Value::String(s) => {
            if s.len() > MAX_STRING_BYTES {
                return Err(format!(
                    "a string is {} bytes, exceeding the {MAX_STRING_BYTES} byte string cap",
                    s.len()
                ));
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                check_value_caps(item, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (key, value) in map {
                if key.len() > MAX_STRING_BYTES {
                    return Err(format!(
                        "an object key is {} bytes, exceeding the {MAX_STRING_BYTES} byte string cap",
                        key.len()
                    ));
                }
                check_value_caps(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Longest a diagnostic message may retain of text derived from
/// provider-controlled input (a serde error's `Display`, which for a type
/// mismatch can embed the offending value verbatim -- e.g. `invalid type:
/// string "...the whole string...", expected u64`). A `ParseDiagnostic` or
/// `ParseFailure::Malformed` is rendered directly in the terminal, so it may
/// never carry an unbounded or unsanitized copy of provider text, only a
/// truncated, control-character-free one (or a fixed generic message).
const MAX_DIAGNOSTIC_DETAIL_BYTES: usize = 200;

/// Sanitizes and truncates `err`'s `Display` output for inclusion in a
/// diagnostic. See [`MAX_DIAGNOSTIC_DETAIL_BYTES`].
fn bounded_diagnostic_detail(err: &impl std::fmt::Display) -> String {
    truncate_at_char_boundary(
        &sanitize_provider_string(err.to_string()),
        MAX_DIAGNOSTIC_DETAIL_BYTES,
    )
}

/// Truncates `s` to at most `max_bytes` bytes, backing off to the nearest
/// earlier `char` boundary rather than splitting one, and marks truncated
/// output with a trailing `…` so it's visibly partial.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}

/// Cap on consecutive Unicode combining marks stacked on a single base
/// character. Legitimate text essentially never needs more than a couple;
/// this bounds "zalgo"-style combining-mark floods (e.g. 500 combining
/// marks piled onto one base character in a cell) without rejecting
/// ordinary accented text. Marks beyond the cap are dropped, not the whole
/// string.
const MAX_COMBINING_MARKS_PER_BASE: usize = 5;

/// Strips control characters (including the C0/C1 ranges and DEL),
/// terminal escape sequences (CSI, OSC, DCS and other `ESC`-prefixed
/// sequences), bidi and other invisible/format Unicode characters, and
/// bounds runs of combining marks, from untrusted provider strings. This is
/// a security control: job names, log paths and every other
/// provider-supplied string reach the renderer -- and confirmation-prompt
/// text -- only after passing through this function.
///
/// `char::is_control()` alone does not catch Unicode *format* characters
/// (general category `Cf`): bidi override/isolate controls and zero-width
/// joiners are not "control characters" by that definition, but a job named
/// with U+202E (RIGHT-TO-LEFT OVERRIDE) can visually reverse a confirmation
/// prompt's rendered text without changing what it actually says -- the
/// user reads one thing and confirms another. [`is_unicode_format_control`]
/// closes that gap.
fn sanitize_provider_string(input: impl AsRef<str>) -> String {
    let chars: Vec<char> = input.as_ref().chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    let mut combining_run = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\u{1b}' {
            i = skip_escape_sequence(&chars, i);
            combining_run = 0;
            continue;
        }
        if ch.is_control() || is_unicode_format_control(ch) {
            i += 1;
            continue;
        }
        if is_combining_mark(ch) {
            combining_run += 1;
            if combining_run > MAX_COMBINING_MARKS_PER_BASE {
                i += 1;
                continue;
            }
        } else {
            combining_run = 0;
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// True for Unicode *format* characters (general category `Cf`) that are
/// invisible or reorder visible text without tripping `char::is_control()`:
/// bidi embedding/override/isolate controls, directional marks, joiners,
/// and other zero-width format characters.
///
/// This crate has no Unicode character-database dependency, so this is a
/// hand-maintained set rather than a `Cf`-category lookup: every codepoint
/// the threat model calls out by name (bidi controls U+202A..U+202E and
/// U+2066..U+2069, U+200E/U+200F, the zero-width set U+200B..U+200D and
/// U+FEFF) plus the rest of `Cf`'s well-known ranges (Unicode 15/16). It
/// intentionally errs toward stripping more, not less: a false positive
/// here only removes an invisible character from untrusted provider text.
fn is_unicode_format_control(ch: char) -> bool {
    matches!(ch,
        '\u{00AD}'
        | '\u{0600}'..='\u{0605}'
        | '\u{061C}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{200B}'..='\u{200F}' // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}' // LRI, RLI, FSI, PDI + reserved
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{13430}'..='\u{13438}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}'
        | '\u{E0020}'..='\u{E007F}'
    )
}

/// True for the common Unicode combining-mark blocks used by "zalgo" style
/// text (stacking dozens to hundreds of marks on one base character): the
/// combining diacritical marks block and its extended/supplement/symbol
/// variants, plus combining half marks. Not a complete general-category
/// `Mn`/`Mc`/`Me` classifier (this crate has no Unicode database
/// dependency), but it covers the ranges this class of attack actually
/// uses.
fn is_combining_mark(ch: char) -> bool {
    matches!(ch,
        '\u{0300}'..='\u{036F}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{FE20}'..='\u{FE2F}'
    )
}

/// `chars[start]` is the `ESC` that begins the sequence. Returns the index
/// just past the end of the whole sequence (best-effort: an unterminated
/// sequence consumes the rest of the string).
fn skip_escape_sequence(chars: &[char], start: usize) -> usize {
    let mut i = start + 1;
    let Some(&next) = chars.get(i) else {
        // Lone ESC at end of input.
        return i;
    };
    match next {
        '[' => {
            // CSI: ESC '[' parameter-bytes intermediate-bytes final-byte
            i += 1;
            while matches!(chars.get(i), Some('0'..='?')) {
                i += 1;
            }
            while matches!(chars.get(i), Some(' '..='/')) {
                i += 1;
            }
            if chars.get(i).is_some() {
                i += 1; // final byte
            }
            i
        }
        ']' => {
            // OSC: ESC ']' ... terminated by BEL or ST (ESC '\\')
            i += 1;
            while let Some(&c) = chars.get(i) {
                if c == '\u{7}' {
                    i += 1;
                    break;
                }
                if c == '\u{1b}' && chars.get(i + 1) == Some(&'\\') {
                    i += 2;
                    break;
                }
                i += 1;
            }
            i
        }
        'P' | 'X' | '^' | '_' => {
            // DCS / SOS / PM / APC: terminated by ST
            i += 1;
            while let Some(&c) = chars.get(i) {
                if c == '\u{1b}' && chars.get(i + 1) == Some(&'\\') {
                    i += 2;
                    break;
                }
                i += 1;
            }
            i
        }
        _ => i + 1, // simple two-char (Fe) escape
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Most tests don't exercise config-driven style names, so this passes
    /// an empty `known_styles` list. Tests for finding-5 behavior call
    /// `parse_provider_payload` directly with a real list.
    fn parse(json: &str) -> Result<ParsedPayload, ParseFailure> {
        parse_provider_payload(json, &[])
    }

    // -- sanitize_provider_string --------------------------------------

    #[test]
    fn sanitize_leaves_plain_text_alone() {
        assert_eq!(sanitize_provider_string("ued 4N 1:23:45"), "ued 4N 1:23:45");
    }

    #[test]
    fn sanitize_preserves_non_control_unicode() {
        assert_eq!(sanitize_provider_string("café · 日本語"), "café · 日本語");
    }

    #[test]
    fn sanitize_strips_csi_color_codes() {
        assert_eq!(
            sanitize_provider_string("\u{1b}[31;1mHACKED\u{1b}[0m"),
            "HACKED"
        );
    }

    #[test]
    fn sanitize_strips_osc_title_injection_up_to_bel() {
        assert_eq!(
            sanitize_provider_string("\u{1b}]0;evil title\u{7}rest"),
            "rest"
        );
    }

    #[test]
    fn sanitize_strips_osc_terminated_by_st() {
        assert_eq!(
            sanitize_provider_string("before\u{1b}]8;;http://evil\u{1b}\\after"),
            "beforeafter"
        );
    }

    #[test]
    fn sanitize_strips_dcs_sequence() {
        assert_eq!(
            sanitize_provider_string("a\u{1b}Pq#0;2;0;0;0#1;2;100;100;100\u{1b}\\b"),
            "ab"
        );
    }

    #[test]
    fn sanitize_strips_bare_control_characters() {
        assert_eq!(sanitize_provider_string("a\u{0}\u{7}\u{7f}b"), "ab");
    }

    #[test]
    fn sanitize_strips_c1_control_range() {
        // U+009C (ST) and friends are control chars even without a
        // preceding ESC.
        assert_eq!(sanitize_provider_string("a\u{9c}b"), "ab");
    }

    #[test]
    fn sanitize_strips_newline_tab_carriage_return() {
        assert_eq!(sanitize_provider_string("a\nb\tc\rd"), "abcd");
    }

    #[test]
    fn sanitize_handles_lone_trailing_escape() {
        assert_eq!(sanitize_provider_string("value\u{1b}"), "value");
    }

    #[test]
    fn sanitize_handles_unterminated_csi() {
        assert_eq!(sanitize_provider_string("value\u{1b}[31"), "value");
    }

    #[test]
    fn sanitize_handles_simple_fe_escape() {
        // ESC 'c' is a full reset (RIS); not CSI/OSC/DCS, just a two-char
        // sequence to drop.
        assert_eq!(sanitize_provider_string("a\u{1b}cb"), "ab");
    }

    // -- sanitize: bidi / invisible Unicode (finding: these survive
    // `char::is_control()`, letting a job name visually reverse or hide
    // parts of a confirmation prompt) ---------------------------------------

    #[test]
    fn sanitize_strips_bidi_override_and_embedding_controls() {
        for ch in ['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}'] {
            assert_eq!(
                sanitize_provider_string(format!("a{ch}b")),
                "ab",
                "U+{:04X} should be stripped",
                ch as u32
            );
        }
    }

    #[test]
    fn sanitize_strips_bidi_isolate_controls() {
        for ch in ['\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}'] {
            assert_eq!(
                sanitize_provider_string(format!("a{ch}b")),
                "ab",
                "U+{:04X} should be stripped",
                ch as u32
            );
        }
    }

    #[test]
    fn sanitize_strips_directional_marks() {
        assert_eq!(sanitize_provider_string("a\u{200E}b\u{200F}c"), "abc");
    }

    #[test]
    fn sanitize_strips_zero_width_characters() {
        assert_eq!(
            sanitize_provider_string("a\u{200B}b\u{200C}c\u{200D}d\u{FEFF}e"),
            "abcde"
        );
    }

    #[test]
    fn sanitize_reversal_attack_is_neutralized() {
        // A job named with an RLO followed by reversed-looking text would
        // otherwise render backwards in a confirmation prompt while the
        // underlying bytes (and thus what actually gets matched/executed)
        // stay whatever they are -- the RLO must not survive.
        let evil = "\u{202E}gnihtemos";
        assert_eq!(sanitize_provider_string(evil), "gnihtemos");
    }

    #[test]
    fn sanitize_bounds_pathological_combining_mark_runs() {
        let base = 'e';
        let combining = "\u{0301}".repeat(500);
        let input = format!("{base}{combining}");
        let out = sanitize_provider_string(&input);
        // The base character plus at most the cap's worth of combining
        // marks -- not all 500.
        assert!(out.chars().count() <= 1 + MAX_COMBINING_MARKS_PER_BASE);
        assert!(out.starts_with(base));
    }

    #[test]
    fn sanitize_combining_run_resets_per_base_character() {
        let combining = "\u{0301}".repeat(20);
        let input = format!("a{combining}b{combining}");
        let out = sanitize_provider_string(&input);
        let a_run = out.chars().take_while(|&c| c != 'b').count();
        assert_eq!(a_run, 1 + MAX_COMBINING_MARKS_PER_BASE);
        assert!(out.contains('b'));
    }

    // -- version handling ------------------------------------------------

    #[test]
    fn valid_minimal_payload_parses() {
        let parsed = parse(r#"{"version":1}"#).expect("should parse");
        assert_eq!(parsed, ParsedPayload::default());
    }

    #[test]
    fn missing_version_is_hard_error() {
        let err = parse(r#"{"groups":[]}"#).unwrap_err();
        assert_eq!(
            err,
            ParseFailure::VersionMismatch {
                expected: PROTOCOL_VERSION,
                found: None
            }
        );
    }

    #[test]
    fn wrong_version_is_hard_error() {
        let err = parse(r#"{"version":2}"#).unwrap_err();
        assert_eq!(
            err,
            ParseFailure::VersionMismatch {
                expected: PROTOCOL_VERSION,
                found: Some(2)
            }
        );
    }

    #[test]
    fn non_numeric_version_is_hard_error() {
        let err = parse(r#"{"version":"1"}"#).unwrap_err();
        assert_eq!(
            err,
            ParseFailure::VersionMismatch {
                expected: PROTOCOL_VERSION,
                found: None
            }
        );
    }

    #[test]
    fn invalid_json_is_malformed() {
        let err = parse("not json").unwrap_err();
        assert!(matches!(err, ParseFailure::Malformed(_)));
    }

    #[test]
    fn non_object_top_level_is_malformed() {
        let err = parse("[1,2,3]").unwrap_err();
        assert!(matches!(err, ParseFailure::Malformed(_)));
    }

    // -- valid empty vs failure -------------------------------------------

    #[test]
    fn valid_empty_payload_is_distinct_from_failure() {
        let parsed = parse(r#"{"version":1,"groups":[],"notify":[]}"#).expect("valid");
        assert!(parsed.groups.is_empty());
        assert!(parsed.notify.is_empty());
        assert!(parsed.diagnostics.is_empty());
    }

    // -- unknown fields / enum fallback ------------------------------------

    #[test]
    fn unknown_top_level_fields_are_ignored() {
        let parsed = parse(r#"{"version":1,"totally_unknown":{"x":1}}"#).expect("valid");
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn unknown_row_fields_are_ignored() {
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","cells":["a"],"bogus_field":"ignored"}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].id, "1");
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn unknown_style_falls_back_to_normal_with_diagnostic() {
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","style":"rainbow"}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].style, RowStyle::Normal);
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn unknown_level_falls_back_to_info_with_diagnostic() {
        let json = r#"{"version":1,"notify":[{"id":"n1","level":"catastrophic"}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.notify[0].level, NotifyLevel::Info);
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn known_styles_and_levels_round_trip() {
        let json = r#"{"version":1,
            "groups":[{"id":"g","rows":[
                {"id":"1","style":"ok"},
                {"id":"2","style":"fail"},
                {"id":"3","style":"warn"},
                {"id":"4","style":"muted"},
                {"id":"5","style":"normal"}
            ]}],
            "notify":[
                {"id":"n1","level":"ok"},
                {"id":"n2","level":"warn"},
                {"id":"n3","level":"fail"},
                {"id":"n4","level":"info"}
            ]}"#;
        let parsed = parse(json).expect("valid");
        let styles: Vec<RowStyle> = parsed.groups[0].rows.iter().map(|r| r.style).collect();
        assert_eq!(
            styles,
            vec![
                RowStyle::Ok,
                RowStyle::Fail,
                RowStyle::Warn,
                RowStyle::Muted,
                RowStyle::Normal
            ]
        );
        let levels: Vec<NotifyLevel> = parsed.notify.iter().map(|n| n.level).collect();
        assert_eq!(
            levels,
            vec![
                NotifyLevel::Ok,
                NotifyLevel::Warn,
                NotifyLevel::Fail,
                NotifyLevel::Info
            ]
        );
        assert!(parsed.diagnostics.is_empty());
    }

    // -- duplicate ids -----------------------------------------------------

    #[test]
    fn duplicate_row_id_across_groups_first_wins() {
        let json = r#"{"version":1,"groups":[
            {"id":"g1","rows":[{"id":"42","cells":["first"]}]},
            {"id":"g2","rows":[{"id":"42","cells":["second"]}]}
        ]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups[0].rows.len(), 1);
        assert_eq!(parsed.groups[0].rows[0].cells, vec!["first".to_string()]);
        assert!(parsed.groups[1].rows.is_empty());
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn duplicate_group_id_first_wins() {
        let json = r#"{"version":1,"groups":[
            {"id":"g","label":"First","rows":[]},
            {"id":"g","label":"Second","rows":[]}
        ]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.groups[0].label, "First");
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn duplicate_notify_id_first_wins() {
        let json = r#"{"version":1,"notify":[
            {"id":"n","text":"first"},
            {"id":"n","text":"second"}
        ]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.notify.len(), 1);
        assert_eq!(parsed.notify[0].text, "first");
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    // -- one bad row among good ---------------------------------------------

    #[test]
    fn one_invalid_row_is_dropped_rest_applies() {
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","cells":["ok1"]},
            {"cells":["missing id"]},
            {"id":"3","cells":["ok3"]}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        let ids: Vec<&str> = parsed.groups[0]
            .rows
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, vec!["1", "3"]);
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn row_with_non_string_cell_is_dropped() {
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","cells":["a", 5, "c"]},
            {"id":"2","cells":["fine"]}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        let ids: Vec<&str> = parsed.groups[0]
            .rows
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, vec!["2"]);
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn row_with_empty_id_is_dropped() {
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"","cells":["nope"]},
            {"id":"ok","cells":["yes"]}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups[0].rows.len(), 1);
        assert_eq!(parsed.groups[0].rows[0].id, "ok");
    }

    #[test]
    fn notify_missing_id_is_dropped() {
        let json = r#"{"version":1,"notify":[
            {"level":"ok","text":"no id"},
            {"id":"n2","level":"ok","text":"has id"}
        ]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.notify.len(), 1);
        assert_eq!(parsed.notify[0].id, "n2");
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn invalid_group_is_dropped_others_applied() {
        let json = r#"{"version":1,"groups":[
            {"label":"missing id"},
            {"id":"good","rows":[{"id":"1","cells":["x"]}]}
        ]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.groups[0].id, "good");
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    // -- label fallback ------------------------------------------------------

    #[test]
    fn group_label_defaults_to_id_when_missing() {
        let json = r#"{"version":1,"groups":[{"id":"running"}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups[0].label, "running");
    }

    // -- full example from the spec ------------------------------------------

    #[test]
    fn spec_example_payload_parses_end_to_end() {
        let json = r#"{
          "version": 1,
          "title": "JOBS",
          "summary": "2R  1Q  1✓",
          "groups": [
            { "id": "running", "label": "Running", "rows": [
              { "id": "55241874",
                "cells": ["ued", "4N", "1:23:45"],
                "style": "normal",
                "vars": { "log": "/pscratch/sd/j/jdgeorga/ued/slurm-55241874.out",
                          "dir": "/pscratch/sd/j/jdgeorga/ued" },
                "actions": ["cancel", "tail"] } ] } ],
          "notify": [ { "id": "done-55241874", "level": "ok", "text": "55241874 (ued) finished" } ]
        }"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.title.as_deref(), Some("JOBS"));
        assert_eq!(parsed.groups.len(), 1);
        let group = &parsed.groups[0];
        assert_eq!(group.id, "running");
        assert_eq!(group.label, "Running");
        assert_eq!(group.rows.len(), 1);
        let row = &group.rows[0];
        assert_eq!(row.id, "55241874");
        assert_eq!(row.cells, vec!["ued", "4N", "1:23:45"]);
        assert_eq!(row.style, RowStyle::Normal);
        assert_eq!(
            row.vars.get("log").map(String::as_str),
            Some("/pscratch/sd/j/jdgeorga/ued/slurm-55241874.out")
        );
        assert_eq!(row.actions, vec!["cancel", "tail"]);
        assert_eq!(parsed.notify.len(), 1);
        assert_eq!(parsed.notify[0].id, "done-55241874");
        assert_eq!(parsed.notify[0].level, NotifyLevel::Ok);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn injected_control_characters_in_row_data_are_neutralized() {
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1",
             "cells":["\u001b[31mHACKED\u001b[0m", "clean"],
             "vars":{"log":"/tmp/\u0007evil"},
             "actions":["cancel\u001b]0;pwn\u0007"]}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        let row = &parsed.groups[0].rows[0];
        assert_eq!(row.cells[0], "HACKED");
        assert_eq!(row.cells[1], "clean");
        assert_eq!(row.vars.get("log").map(String::as_str), Some("/tmp/evil"));
        assert_eq!(row.actions[0], "cancel");
    }

    // -- caps: groups --------------------------------------------------------

    fn groups_payload(count: usize) -> String {
        let groups: Vec<String> = (0..count)
            .map(|i| format!(r#"{{"id":"g{i}","rows":[]}}"#))
            .collect();
        format!(r#"{{"version":1,"groups":[{}]}}"#, groups.join(","))
    }

    #[test]
    fn groups_at_cap_succeeds() {
        let payload = groups_payload(MAX_GROUPS);
        let parsed = parse(&payload).expect("valid");
        assert_eq!(parsed.groups.len(), MAX_GROUPS);
    }

    #[test]
    fn groups_over_cap_fails_oversized() {
        let payload = groups_payload(MAX_GROUPS + 1);
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    // -- caps: rows ------------------------------------------------------------

    fn rows_payload(count: usize) -> String {
        let rows: Vec<String> = (0..count).map(|i| format!(r#"{{"id":"r{i}"}}"#)).collect();
        format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{}]}}]}}"#,
            rows.join(",")
        )
    }

    #[test]
    fn rows_at_cap_succeeds() {
        let payload = rows_payload(MAX_ROWS);
        let parsed = parse(&payload).expect("valid");
        assert_eq!(parsed.groups[0].rows.len(), MAX_ROWS);
    }

    #[test]
    fn rows_over_cap_fails_oversized() {
        let payload = rows_payload(MAX_ROWS + 1);
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    // -- caps: cells per row -----------------------------------------------------

    fn cells_payload(count: usize) -> String {
        let cells: Vec<String> = (0..count).map(|i| format!(r#""c{i}""#)).collect();
        format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{{"id":"1","cells":[{}]}}]}}]}}"#,
            cells.join(",")
        )
    }

    #[test]
    fn cells_at_cap_succeeds() {
        let payload = cells_payload(MAX_CELLS_PER_ROW);
        let parsed = parse(&payload).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].cells.len(), MAX_CELLS_PER_ROW);
    }

    #[test]
    fn cells_over_cap_fails_oversized() {
        let payload = cells_payload(MAX_CELLS_PER_ROW + 1);
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    // -- caps: vars per row -----------------------------------------------------

    fn vars_payload(count: usize) -> String {
        let vars: Vec<String> = (0..count).map(|i| format!(r#""k{i}":"v{i}""#)).collect();
        format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{{"id":"1","vars":{{{}}}}}]}}]}}"#,
            vars.join(",")
        )
    }

    #[test]
    fn vars_at_cap_succeeds() {
        let payload = vars_payload(MAX_VARS_PER_ROW);
        let parsed = parse(&payload).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].vars.len(), MAX_VARS_PER_ROW);
    }

    #[test]
    fn vars_over_cap_fails_oversized() {
        let payload = vars_payload(MAX_VARS_PER_ROW + 1);
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    // -- caps: per-string byte length ---------------------------------------------

    fn string_payload(len: usize) -> String {
        let value = "a".repeat(len);
        format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{{"id":"1","cells":["{value}"]}}]}}]}}"#
        )
    }

    #[test]
    fn string_at_cap_succeeds() {
        let payload = string_payload(MAX_STRING_BYTES);
        let parsed = parse(&payload).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].cells[0].len(), MAX_STRING_BYTES);
    }

    #[test]
    fn string_over_cap_fails_oversized() {
        let payload = string_payload(MAX_STRING_BYTES + 1);
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    // -- caps: total payload size --------------------------------------------------

    #[test]
    fn large_but_compliant_payload_succeeds() {
        // Exercise a payload that is large (well beyond a single string's
        // cap) but stays under the total-size cap and every per-item cap.
        let rows: Vec<String> = (0..MAX_ROWS)
            .map(|i| format!(r#"{{"id":"r{i}","cells":["{}"]}}"#, "a".repeat(80)))
            .collect();
        let payload = format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{}]}}]}}"#,
            rows.join(",")
        );
        assert!(payload.len() < MAX_TOTAL_BYTES);
        let parsed = parse(&payload).expect("valid");
        assert_eq!(parsed.groups[0].rows.len(), MAX_ROWS);
    }

    #[test]
    fn total_size_over_cap_fails_oversized_even_with_compliant_rows() {
        // Every individual row and string stays within its own cap, but
        // the aggregate exceeds the 256 KiB total cap.
        let rows: Vec<String> = (0..MAX_ROWS)
            .map(|i| format!(r#"{{"id":"r{i}","cells":["{}"]}}"#, "a".repeat(200)))
            .collect();
        let payload = format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{}]}}]}}"#,
            rows.join(",")
        );
        assert!(payload.len() > MAX_TOTAL_BYTES);
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    // -- caps: positions the old field-by-field allowlist missed ---------------

    #[test]
    fn oversized_string_as_a_bare_array_element_is_rejected_without_echoing_it() {
        // Regression: `{"groups":["AAAA....200000 A's...."]}` -- the huge
        // string sits directly at `groups[0]`, not nested under a field the
        // old cap check specifically visited (`id`/`label`/row fields). It
        // used to slip past the cap check entirely, reach serde's decode
        // attempt, and get echoed whole into a `ParseFailure::Malformed`
        // that is rendered directly in the terminal.
        let huge = "A".repeat(200_000);
        let payload = format!(r#"{{"version":1,"groups":["{huge}"]}}"#);
        let err = parse(&payload).unwrap_err();
        match err {
            ParseFailure::Oversized(message) => {
                assert!(
                    message.len() < 300,
                    "diagnostic must not embed the raw string: {} bytes",
                    message.len()
                );
                assert!(!message.contains(&huge));
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    #[test]
    fn oversized_vars_key_is_rejected() {
        // The old check recursed into `vars` *values* but never `vars`
        // *keys* at all -- an oversized key passed unconditionally.
        let huge_key = "k".repeat(MAX_STRING_BYTES + 1);
        let payload = format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{{"id":"1","vars":{{"{huge_key}":"v"}}}}]}}]}}"#
        );
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    #[test]
    fn vars_key_at_cap_succeeds() {
        let key = "k".repeat(MAX_STRING_BYTES);
        let payload = format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{{"id":"1","vars":{{"{key}":"v"}}}}]}}]}}"#
        );
        let parsed = parse(&payload).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].vars.len(), 1);
    }

    #[test]
    fn oversized_group_object_key_is_rejected() {
        // An oversized key anywhere in the tree, not just in `vars`, must be
        // caught -- e.g. an unknown top-level field's own key.
        let huge_key = "k".repeat(MAX_STRING_BYTES + 1);
        let payload = format!(r#"{{"version":1,"{huge_key}":"v"}}"#);
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    // -- caps: nesting depth --------------------------------------------------

    #[test]
    fn pathologically_nested_payload_is_rejected_not_parsed() {
        // Far beyond MAX_JSON_DEPTH, but still small; this must be rejected
        // by the raw pre-scan before `serde_json` ever attempts to build a
        // `Value` tree out of it (see `check_raw_nesting_depth`'s docs for
        // why that ordering matters).
        let mut nested = "0".to_string();
        for _ in 0..5000 {
            nested = format!("[{nested}]");
        }
        let payload = format!(
            r#"{{"version":1,"groups":[{{"id":"g","rows":[{{"id":"1","vars":{{"log":{nested}}}}}]}}]}}"#
        );
        let err = parse(&payload).unwrap_err();
        assert!(matches!(err, ParseFailure::Oversized(_)));
    }

    #[test]
    fn moderately_nested_but_normal_payload_is_unaffected() {
        // The protocol's own shape (payload -> groups -> group -> rows ->
        // row -> cells/vars/actions) nests only a handful of levels; the
        // depth cap must not false-positive on it.
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","cells":["a","b"],"vars":{"log":"/tmp/x"},"actions":["cancel"]}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].cells, vec!["a", "b"]);
    }

    #[test]
    fn depth_cap_does_not_flag_a_string_that_merely_contains_brackets() {
        // The raw pre-scan must not mistake `[`/`{` inside a JSON string
        // literal for real structural nesting.
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","cells":["[[[[[not actually nested]]]]]"]}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(
            parsed.groups[0].rows[0].cells[0],
            "[[[[[not actually nested]]]]]"
        );
    }

    #[test]
    fn depth_cap_does_not_miscount_an_escaped_quote_inside_a_string() {
        // `\"` inside a string must not be treated as the string's end --
        // otherwise the bracket that follows in the *real* JSON would be
        // (mis)counted as unterminated-string content or vice versa.
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","cells":["say \"hi\" [ok]"]}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].cells[0], "say \"hi\" [ok]");
    }

    // -- diagnostics must not carry unbounded/unsanitized provider text ------

    #[test]
    fn bounded_diagnostic_detail_truncates_long_text() {
        let huge = format!("boom: {}", "x".repeat(10_000));
        let bounded = bounded_diagnostic_detail(&huge);
        assert!(bounded.len() <= MAX_DIAGNOSTIC_DETAIL_BYTES + '\u{2026}'.len_utf8());
        assert!(bounded.starts_with("boom:"));
    }

    #[test]
    fn bounded_diagnostic_detail_strips_control_characters_and_escapes() {
        let evil = "before\u{1b}[31mHACKED\u{1b}[0mafter".to_string();
        assert_eq!(bounded_diagnostic_detail(&evil), "beforeHACKEDafter");
    }

    #[test]
    fn malformed_decode_error_diagnostic_is_bounded() {
        // A field with the wrong JSON type (a string where a number is
        // expected) makes serde build an `invalid type: string "...",
        // expected ...` message that can embed the offending value
        // verbatim. Keep the value short here (caps already reject
        // anything over MAX_STRING_BYTES before this point) but confirm the
        // resulting diagnostic is bounded and control-character-free
        // regardless.
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","cells":"not an array"}
        ]}]}"#;
        let parsed = parse(json).expect("valid");
        assert!(parsed.groups[0].rows.is_empty());
        assert_eq!(parsed.diagnostics.len(), 1);
        assert!(parsed.diagnostics[0].message.len() < MAX_DIAGNOSTIC_DETAIL_BYTES + 64);
    }

    // -- finding 5: style names resolved against caller-supplied config -------

    #[test]
    fn configured_custom_style_name_resolves_without_a_diagnostic() {
        // `critical` isn't one of the protocol's built-in names, but the
        // caller's config defines it (e.g. `[ui.sidebar.list.styles]` has a
        // `critical` entry) -- this must not warn on every single poll.
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","style":"critical"}
        ]}]}"#;
        let parsed = parse_provider_payload(json, &["critical"]).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].style, RowStyle::Normal);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn unconfigured_style_name_still_warns() {
        // Same unrecognized-by-the-enum name, but this time it is *not* in
        // the caller's configured styles either -- genuinely unknown, so the
        // existing warn-and-fall-back-to-normal behavior still applies.
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","style":"critical"}
        ]}]}"#;
        let parsed = parse_provider_payload(json, &["ok", "fail"]).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].style, RowStyle::Normal);
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn builtin_style_names_never_warn_regardless_of_known_styles() {
        // The five wire-protocol built-ins are always structurally valid,
        // independent of whatever the config's styles table currently
        // contains (a user may have removed one from
        // `[ui.sidebar.list.styles]`; that's a rendering-time fallback to
        // the default text color, not a protocol-level "unknown style").
        let json = r#"{"version":1,"groups":[{"id":"g","rows":[
            {"id":"1","style":"warn"}
        ]}]}"#;
        let parsed = parse_provider_payload(json, &[]).expect("valid");
        assert_eq!(parsed.groups[0].rows[0].style, RowStyle::Warn);
        assert!(parsed.diagnostics.is_empty());
    }
}
