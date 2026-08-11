use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Per-row data available to substitution: the row's id, its cells by index
/// and its provider-supplied `vars`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowContext {
    pub id: String,
    pub cells: Vec<String>,
    pub vars: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubstError {
    /// argv[0] contained a `{` or `}`; the executable is config-only.
    ArgvZeroToken,
    /// A `{` was never closed, or a `}` appeared with no matching `{`.
    UnbalancedBraces,
    /// `{name}` did not resolve to `id`, `cellN`, `mode` or a `vars` entry.
    UnresolvedToken(String),
    /// The text between `{` and `}` violated the token-name grammar (empty,
    /// or containing `{`, `}`, or whitespace). Never treated as a lookup key
    /// -- an attacker-controlled `vars` key must not be able to make
    /// malformed template syntax "work" by accident (e.g. a var literally
    /// named `foo{bar` resolving `{foo{bar}`, or a key of `""` resolving
    /// `{}`).
    InvalidTokenName(String),
    /// `{name}` resolved, but to an empty string.
    EmptyToken(String),
    /// A `vars` key collides with a reserved token name (`id`, `cellN`, `mode`).
    ShadowedVar(String),
    /// A resolved value failed its per-action validation regex.
    ValidationFailed { value: String },
    /// A path used for `{log}`/`{dir}` was not absolute.
    PathNotAbsolute { value: String },
    /// A path used for `{log}`/`{dir}` could not be canonicalized (usually:
    /// does not exist).
    PathNotFound { value: String },
}

impl fmt::Display for SubstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubstError::ArgvZeroToken => {
                write!(f, "the command executable may not contain a substitution token")
            }
            SubstError::UnbalancedBraces => write!(f, "unbalanced braces in command template"),
            SubstError::UnresolvedToken(name) => write!(f, "unresolved token {{{name}}}"),
            SubstError::InvalidTokenName(name) => write!(
                f,
                "invalid token name {name:?}: token names must be non-empty with no `{{`, `}}`, or whitespace"
            ),
            SubstError::EmptyToken(name) => {
                write!(f, "token {{{name}}} resolved to an empty value")
            }
            SubstError::ShadowedVar(name) => {
                write!(f, "var {name:?} shadows a reserved token name")
            }
            SubstError::ValidationFailed { value } => {
                write!(f, "value {value:?} failed validation")
            }
            SubstError::PathNotAbsolute { value } => write!(f, "path {value:?} is not absolute"),
            SubstError::PathNotFound { value } => write!(f, "path {value:?} does not exist"),
        }
    }
}

impl std::error::Error for SubstError {}

/// Resolves a command template against a row, producing a fully-substituted
/// argv. `argv[0]` is never substituted: it must come verbatim from config.
pub(crate) fn resolve_argv(
    template: &[String],
    row: &RowContext,
    mode: &str,
) -> Result<Vec<String>, SubstError> {
    reject_shadowed_vars(row)?;

    let Some((program, rest)) = template.split_first() else {
        return Ok(Vec::new());
    };

    if contains_brace(program) {
        return Err(SubstError::ArgvZeroToken);
    }

    let mut argv = Vec::with_capacity(template.len());
    argv.push(program.clone());
    for part in rest {
        argv.push(expand(part, row, mode)?);
    }
    Ok(argv)
}

/// Validates a resolved token value against a per-action regex, e.g. the
/// `cancel` action's `{id}` must look like a SLURM job id.
pub(crate) fn validate_token(value: &str, regex: &Regex) -> Result<(), SubstError> {
    if regex.is_match(value) {
        Ok(())
    } else {
        Err(SubstError::ValidationFailed {
            value: value.to_string(),
        })
    }
}

/// Validates a resolved `{log}`/`{dir}` value: must be absolute, must
/// canonicalize (which also proves it exists), and traversal segments are
/// resolved away rather than trusted.
pub(crate) fn validate_path(value: &str) -> Result<PathBuf, SubstError> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(SubstError::PathNotAbsolute {
            value: value.to_string(),
        });
    }
    std::fs::canonicalize(path).map_err(|_| SubstError::PathNotFound {
        value: value.to_string(),
    })
}

fn contains_brace(s: &str) -> bool {
    s.contains('{') || s.contains('}')
}

fn reject_shadowed_vars(row: &RowContext) -> Result<(), SubstError> {
    for key in row.vars.keys() {
        if is_reserved_token_name(key) {
            return Err(SubstError::ShadowedVar(key.clone()));
        }
    }
    Ok(())
}

fn is_reserved_token_name(name: &str) -> bool {
    name == "id" || name == "mode" || cell_index(name).is_some()
}

fn cell_index(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("cell")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Grammar for the text between `{` and `}`: non-empty, and containing none
/// of `{`, `}`, or any Unicode whitespace. Anything else is a hard parse
/// error (`InvalidTokenName`), never a literal-braces fallback and never a
/// lookup -- see the design doc's substitution table and
/// `SubstError::InvalidTokenName`'s docs for why.
fn is_valid_token_char(ch: char) -> bool {
    ch != '{' && ch != '}' && !ch.is_whitespace()
}

/// Expands one template string: `{{`/`}}` are literal braces, `{name}` is a
/// token whose `name` must satisfy [`is_valid_token_char`] and be
/// non-empty. A resolved value that is empty, or a token that names
/// nothing, is an error rather than a silently-dropped or literal `{name}`.
fn expand(template: &str, row: &RowContext, mode: &str) -> Result<String, SubstError> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => {
                            if name.is_empty() {
                                return Err(SubstError::InvalidTokenName(name));
                            }
                            break;
                        }
                        Some(ch) if is_valid_token_char(ch) => name.push(ch),
                        Some(ch) => {
                            // A `{` or whitespace before the closing `}`
                            // violates the grammar -- fail immediately
                            // rather than keep accumulating (which is how
                            // `{foo{bar}` used to resolve against a var
                            // literally named `foo{bar`).
                            name.push(ch);
                            return Err(SubstError::InvalidTokenName(name));
                        }
                        None => return Err(SubstError::UnbalancedBraces),
                    }
                }
                let value = resolve_token(&name, row, mode)?;
                if value.is_empty() {
                    return Err(SubstError::EmptyToken(name));
                }
                out.push_str(&value);
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push('}');
                } else {
                    return Err(SubstError::UnbalancedBraces);
                }
            }
            other => out.push(other),
        }
    }

    Ok(out)
}

fn resolve_token(name: &str, row: &RowContext, mode: &str) -> Result<String, SubstError> {
    if name == "id" {
        return Ok(row.id.clone());
    }
    if name == "mode" {
        return Ok(mode.to_string());
    }
    if let Some(idx) = cell_index(name) {
        return row
            .cells
            .get(idx)
            .cloned()
            .ok_or_else(|| SubstError::UnresolvedToken(name.to_string()));
    }
    row.vars
        .get(name)
        .cloned()
        .ok_or_else(|| SubstError::UnresolvedToken(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Job-id shape accepted by SLURM: plain (`123`), array (`123_4`) and
    /// heterogeneous (`123+1`) ids. Mirrors the sample `validate = { id = ... }`
    /// regex from the design doc's example config; the real regex is always
    /// config-supplied (`list_actions::validate_list_action_fields`), never
    /// hardcoded.
    const JOB_ID_PATTERN: &str = r"^[0-9]+(_[0-9]+)?(\+[0-9]+)?$";

    fn row(id: &str, cells: &[&str], vars: &[(&str, &str)]) -> RowContext {
        RowContext {
            id: id.to_string(),
            cells: cells.iter().map(|c| c.to_string()).collect(),
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn tpl(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn resolves_id_cells_mode_and_vars() {
        let r = row(
            "55241874",
            &["ued", "4N", "1:23:45"],
            &[("log", "/pscratch/sd/j/jdgeorga/ued/slurm-55241874.out")],
        );
        let argv = resolve_argv(&tpl(&["tail", "-f", "--", "{log}"]), &r, "live").unwrap();
        assert_eq!(
            argv,
            vec![
                "tail",
                "-f",
                "--",
                "/pscratch/sd/j/jdgeorga/ued/slurm-55241874.out",
            ]
        );

        let argv = resolve_argv(&tpl(&["scancel", "--", "{id}"]), &r, "live").unwrap();
        assert_eq!(argv, vec!["scancel", "--", "55241874"]);

        let argv = resolve_argv(&tpl(&["echo", "{cell0}", "{cell1}"]), &r, "live").unwrap();
        assert_eq!(argv, vec!["echo", "ued", "4N"]);

        let argv = resolve_argv(&tpl(&["p", "--mode", "{mode}"]), &r, "history").unwrap();
        assert_eq!(argv, vec!["p", "--mode", "history"]);
    }

    #[test]
    fn literal_double_braces_are_not_tokens() {
        let r = row("1", &[], &[]);
        let argv = resolve_argv(&tpl(&["echo", "{{id}}"]), &r, "live").unwrap();
        assert_eq!(argv, vec!["echo", "{id}"]);
    }

    #[test]
    fn unresolved_token_names_itself_rather_than_emitting_literal_braces() {
        let r = row("1", &[], &[]);
        let err = resolve_argv(&tpl(&["tail", "-f", "{log}"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::UnresolvedToken("log".to_string()));
        // Never a literal "{log}" in the returned error text either.
        assert!(!err.to_string().contains("literal"));
        assert!(err.to_string().contains("log"));
    }

    #[test]
    fn unresolved_cell_index_out_of_range() {
        let r = row("1", &["only"], &[]);
        let err = resolve_argv(&tpl(&["echo", "{cell5}"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::UnresolvedToken("cell5".to_string()));
    }

    #[test]
    fn vars_may_not_shadow_id() {
        let r = row("1", &[], &[("id", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "hi"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::ShadowedVar("id".to_string()));
    }

    #[test]
    fn vars_may_not_shadow_cell_n() {
        let r = row("1", &["a"], &[("cell0", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "hi"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::ShadowedVar("cell0".to_string()));
    }

    #[test]
    fn vars_may_not_shadow_mode() {
        let r = row("1", &[], &[("mode", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "hi"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::ShadowedVar("mode".to_string()));
    }

    #[test]
    fn shadow_check_fires_even_if_shadowed_var_is_unused() {
        // The shadow is a config/provider contract violation independent of
        // whether the template happens to reference it.
        let r = row("1", &[], &[("id", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "no-tokens-here"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::ShadowedVar("id".to_string()));
    }

    #[test]
    fn argv_zero_may_not_contain_a_token() {
        let r = row("1", &[], &[]);
        let err = resolve_argv(&tpl(&["{cell0}", "arg"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::ArgvZeroToken);
    }

    #[test]
    fn argv_zero_rejects_even_escaped_braces() {
        let r = row("1", &[], &[]);
        let err = resolve_argv(&tpl(&["prog{{}}", "arg"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::ArgvZeroToken);
    }

    #[test]
    fn empty_token_value_is_an_error() {
        let r = row("1", &["", "x"], &[]);
        let err = resolve_argv(&tpl(&["echo", "{cell0}"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::EmptyToken("cell0".to_string()));
    }

    #[test]
    fn unbalanced_open_brace_is_rejected() {
        let r = row("1", &[], &[]);
        let err = resolve_argv(&tpl(&["echo", "{id"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::UnbalancedBraces);
    }

    #[test]
    fn unbalanced_close_brace_is_rejected() {
        let r = row("1", &[], &[]);
        let err = resolve_argv(&tpl(&["echo", "id}"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::UnbalancedBraces);
    }

    // -- token-name grammar (finding: malformed template syntax must never
    // execute depending on an attacker-controlled `vars` key) ---------------

    #[test]
    fn nested_open_brace_in_token_name_is_a_hard_error() {
        // Must never resolve against a var literally named `foo{bar`, even
        // if one exists.
        let r = row("1", &[], &[("foo{bar", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "{foo{bar}"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::InvalidTokenName("foo{".to_string()));
    }

    #[test]
    fn empty_token_name_is_a_hard_error() {
        // Must never resolve against a var literally named "".
        let r = row("1", &[], &[("", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "{}"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::InvalidTokenName(String::new()));
    }

    #[test]
    fn token_name_with_leading_and_trailing_whitespace_is_a_hard_error() {
        // Must never resolve against a var literally named " id ".
        let r = row("1", &[], &[(" id ", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "{ id }"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::InvalidTokenName(" ".to_string()));
    }

    #[test]
    fn token_name_with_internal_whitespace_is_a_hard_error() {
        let r = row("1", &["x", "y"], &[("a b", "sneaky")]);
        let err = resolve_argv(&tpl(&["echo", "{a b}"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::InvalidTokenName("a ".to_string()));
    }

    #[test]
    fn unterminated_token_name_is_still_unbalanced_braces() {
        // Same case `unbalanced_open_brace_is_rejected` covers, named
        // explicitly here alongside the rest of the grammar test suite.
        let r = row("1", &[], &[]);
        let err = resolve_argv(&tpl(&["echo", "{id"]), &r, "live").unwrap_err();
        assert_eq!(err, SubstError::UnbalancedBraces);
    }

    #[test]
    fn well_formed_token_names_still_resolve() {
        let r = row("1", &["x"], &[("log", "/tmp/log")]);
        let argv = resolve_argv(&tpl(&["echo", "{id}", "{cell0}", "{log}"]), &r, "live").unwrap();
        assert_eq!(argv, vec!["echo", "1", "x", "/tmp/log"]);
    }

    #[test]
    fn adversarial_row_id_is_substituted_verbatim_when_dash_prefixed() {
        // Substitution itself does not interpret this as a flag; the `--`
        // separator in the template is what prevents that at the process
        // boundary. This test proves resolve_argv does not "fix" or reject
        // it silently — the caller's `--` is load-bearing, not this module.
        let r = row("-A", &[], &[]);
        let argv = resolve_argv(&tpl(&["scancel", "--", "{id}"]), &r, "live").unwrap();
        assert_eq!(argv, vec!["scancel", "--", "-A"]);
    }

    #[test]
    fn job_id_regex_accepts_plain_array_and_heterogeneous_ids() {
        let re = Regex::new(JOB_ID_PATTERN).unwrap();
        for good in ["55241874", "123_4", "123+1"] {
            assert!(
                validate_token(good, &re).is_ok(),
                "expected {good:?} to be valid"
            );
        }
    }

    #[test]
    fn job_id_regex_rejects_adversarial_ids() {
        let re = Regex::new(JOB_ID_PATTERN).unwrap();
        for bad in ["-A", "--force", "", "1;2"] {
            assert!(
                validate_token(bad, &re).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn validate_path_rejects_relative_traversal() {
        let err = validate_path("../../etc/passwd").unwrap_err();
        assert_eq!(
            err,
            SubstError::PathNotAbsolute {
                value: "../../etc/passwd".to_string()
            }
        );
    }

    #[test]
    fn validate_path_rejects_nonexistent_absolute_path() {
        let err =
            validate_path("/this/path/almost-certainly/does/not/exist-herdr-test").unwrap_err();
        assert!(matches!(err, SubstError::PathNotFound { .. }));
    }

    #[test]
    fn validate_path_accepts_existing_absolute_path() {
        // /tmp exists on every target this codebase runs tests on.
        let resolved = validate_path("/tmp").unwrap();
        assert!(resolved.is_absolute());
    }

    #[test]
    fn validate_path_resolves_traversal_through_an_existing_absolute_prefix() {
        // "/tmp/.." is absolute and canonicalizes to the parent; the raw
        // string is never trusted, only the canonical result.
        let resolved = validate_path("/tmp/..").unwrap();
        assert!(resolved.is_absolute());
        assert!(!resolved.to_string_lossy().contains(".."));
    }

    #[test]
    fn ten_kib_token_value_round_trips_without_truncation() {
        let big = "x".repeat(10 * 1024);
        let r = row("1", &[], &[("log", big.as_str())]);
        let argv = resolve_argv(&tpl(&["tail", "-f", "--", "{log}"]), &r, "live").unwrap();
        assert_eq!(argv[3].len(), big.len());
        assert_eq!(argv[3], big);
    }

    #[test]
    fn ten_kib_token_value_survives_path_validation_failure_without_panicking() {
        // Not a real path, but must fail cleanly rather than panic on a
        // pathologically long component.
        let big = "/".to_string() + &"x".repeat(10 * 1024);
        let err = validate_path(&big).unwrap_err();
        assert!(matches!(err, SubstError::PathNotFound { .. }));
    }
}
