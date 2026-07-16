use std::{fmt, ops::Range};

use sha2::{Digest, Sha256};

use crate::{AppId, LocalConfigDocument, SteamError};

/// A conservative, whitespace-token operation on one app's LaunchOptions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchOptionsEdit<'token> {
    EnsureToken(&'token str),
    RemoveToken(&'token str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    InsertLaunchOptions,
    ReplaceLaunchOptions,
}

/// A content fingerprint used to reject plans made from stale source bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileFingerprint {
    length: u64,
    sha256: [u8; 32],
}

impl FileFingerprint {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            length: bytes.len() as u64,
            sha256: Sha256::digest(bytes).into(),
        }
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

/// Metadata about a targeted edit. It intentionally excludes the old and new
/// LaunchOptions values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    pub app_id: AppId,
    pub kind: ChangeKind,
    pub byte_range: Range<usize>,
}

/// A side-effect-free replacement plan bound to the exact source bytes.
#[derive(Clone, Eq, PartialEq)]
pub struct EditPlan {
    changed: bool,
    source_fingerprint: FileFingerprint,
    replacement: Vec<u8>,
    changes: Vec<Change>,
}

impl EditPlan {
    pub const fn changed(&self) -> bool {
        self.changed
    }

    pub const fn source_fingerprint(&self) -> &FileFingerprint {
        &self.source_fingerprint
    }

    pub fn replacement(&self) -> &[u8] {
        &self.replacement
    }

    pub fn into_replacement(self) -> Vec<u8> {
        self.replacement
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }
}

impl fmt::Debug for EditPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EditPlan")
            .field("changed", &self.changed)
            .field("source_fingerprint", &self.source_fingerprint)
            .field("replacement_len", &self.replacement.len())
            .field("changes", &self.changes)
            .finish()
    }
}

impl<'source> LocalConfigDocument<'source> {
    pub fn plan_launch_options_edit(
        &self,
        app_id: AppId,
        edit: LaunchOptionsEdit<'_>,
    ) -> Result<EditPlan, SteamError> {
        let token = match edit {
            LaunchOptionsEdit::EnsureToken(token) | LaunchOptionsEdit::RemoveToken(token) => token,
        };
        validate_token(token)?;

        let current = self.launch_options(app_id)?;
        let source_fingerprint = FileFingerprint::from_bytes(self.source.as_bytes());
        let present = current.is_some_and(|value| has_token(value, token));
        let should_change = match edit {
            LaunchOptionsEdit::EnsureToken(_) => !present,
            LaunchOptionsEdit::RemoveToken(_) => present,
        };
        if !should_change {
            return Ok(EditPlan {
                changed: false,
                source_fingerprint,
                replacement: self.source.as_bytes().to_vec(),
                changes: Vec::new(),
            });
        }

        let located = locate_launch_options(self.source, app_id)?;
        let (replacement, change) = match edit {
            LaunchOptionsEdit::EnsureToken(token) => match located.value {
                Some(span) => {
                    let current = span
                        .value
                        .as_deref()
                        .ok_or_else(|| SteamError::invalid_edit("launch_options_value_missing"))?;
                    let next = if current.trim().is_empty() {
                        token.to_owned()
                    } else {
                        format!("{} {token}", current.trim_end())
                    };
                    replace_quoted_value(self.source, app_id, span, &next)
                }
                None => insert_launch_options(self.source, app_id, located, token)?,
            },
            LaunchOptionsEdit::RemoveToken(token) => {
                let span = located
                    .value
                    .ok_or_else(|| SteamError::invalid_edit("launch_options_location_missing"))?;
                let current = span
                    .value
                    .as_deref()
                    .ok_or_else(|| SteamError::invalid_edit("launch_options_value_missing"))?;
                let next = current
                    .split_whitespace()
                    .filter(|candidate| *candidate != token)
                    .collect::<Vec<_>>()
                    .join(" ");
                replace_quoted_value(self.source, app_id, span, &next)
            }
        };

        Ok(EditPlan {
            changed: true,
            source_fingerprint,
            replacement: replacement.into_bytes(),
            changes: vec![change],
        })
    }
}

fn validate_token(token: &str) -> Result<(), SteamError> {
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        Err(SteamError::invalid_edit(
            "token_must_be_one_nonempty_whitespace_token",
        ))
    } else {
        Ok(())
    }
}

fn has_token(value: &str, token: &str) -> bool {
    value.split_whitespace().any(|candidate| candidate == token)
}

#[derive(Clone, Debug)]
struct SpanToken {
    value: Option<String>,
    start: usize,
    end: usize,
    brace: i8,
}

fn tokens(source: &str) -> Result<Vec<SpanToken>, SteamError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                cursor += 2;
                while cursor < bytes.len() && bytes[cursor] != b'\n' {
                    cursor += 1;
                }
            }
            b'"' => {
                let start = cursor;
                cursor += 1;
                let mut value = Vec::new();
                let mut closed = false;
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'\\' if cursor + 1 < bytes.len() => {
                            cursor += 1;
                            value.push(match bytes[cursor] {
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'\\' => b'\\',
                                b'"' => b'"',
                                _ => {
                                    return Err(SteamError::invalid_edit("unsupported_vdf_escape"));
                                }
                            });
                            cursor += 1;
                        }
                        b'"' => {
                            cursor += 1;
                            closed = true;
                            break;
                        }
                        byte => {
                            value.push(byte);
                            cursor += 1;
                        }
                    }
                }
                if !closed {
                    return Err(SteamError::invalid_edit("unterminated_vdf_string"));
                }
                let value = String::from_utf8(value)
                    .map_err(|_| SteamError::invalid_edit("invalid_vdf_string_utf8"))?;
                tokens.push(SpanToken {
                    value: Some(value),
                    start,
                    end: cursor,
                    brace: 0,
                });
            }
            b'{' => {
                tokens.push(SpanToken {
                    value: None,
                    start: cursor,
                    end: cursor + 1,
                    brace: 1,
                });
                cursor += 1;
            }
            b'}' => {
                tokens.push(SpanToken {
                    value: None,
                    start: cursor,
                    end: cursor + 1,
                    brace: -1,
                });
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    Ok(tokens)
}

#[derive(Clone, Debug)]
struct LocatedLaunchOptions {
    app_open: usize,
    app_close: usize,
    value: Option<SpanToken>,
}

fn locate_launch_options(source: &str, app_id: AppId) -> Result<LocatedLaunchOptions, SteamError> {
    let tokens = tokens(source)?;
    let app_id = app_id.get().to_string();
    let path = [
        "UserLocalConfigStore",
        "Software",
        "Valve",
        "Steam",
        "apps",
        app_id.as_str(),
    ];
    let mut stack = Vec::<String>::new();
    let mut pending = None::<String>;
    let mut app_open = None;
    let mut app_close = None;
    let mut launch_value = None;

    for token in tokens {
        if let Some(value) = &token.value {
            if pending.is_none() {
                pending = Some(value.clone());
            } else {
                if stack.iter().map(String::as_str).eq(path)
                    && pending.as_deref() == Some("LaunchOptions")
                    && launch_value.replace(token.clone()).is_some()
                {
                    return Err(SteamError::invalid_edit(
                        "ambiguous_launch_options_location",
                    ));
                }
                pending = None;
            }
        } else if token.brace == 1 {
            let key = pending
                .take()
                .ok_or_else(|| SteamError::invalid_edit("vdf_object_key_missing"))?;
            stack.push(key);
            if stack.iter().map(String::as_str).eq(path) && app_open.replace(token.start).is_some()
            {
                return Err(SteamError::invalid_edit("ambiguous_app_location"));
            }
        } else {
            if stack.iter().map(String::as_str).eq(path) && app_close.replace(token.start).is_some()
            {
                return Err(SteamError::invalid_edit("ambiguous_app_location"));
            }
            stack.pop();
            pending = None;
        }
    }

    Ok(LocatedLaunchOptions {
        app_open: app_open.ok_or_else(|| SteamError::invalid_edit("app_location_missing"))?,
        app_close: app_close.ok_or_else(|| SteamError::invalid_edit("app_location_missing"))?,
        value: launch_value,
    })
}

fn replace_quoted_value(
    source: &str,
    app_id: AppId,
    span: SpanToken,
    next: &str,
) -> (String, Change) {
    let replacement = format!(
        "{}\"{}\"{}",
        &source[..span.start],
        escape(next),
        &source[span.end..]
    );
    let change = Change {
        app_id,
        kind: ChangeKind::ReplaceLaunchOptions,
        byte_range: span.start..span.end,
    };
    (replacement, change)
}

fn insert_launch_options(
    source: &str,
    app_id: AppId,
    located: LocatedLaunchOptions,
    token: &str,
) -> Result<(String, Change), SteamError> {
    if located.app_open >= located.app_close {
        return Err(SteamError::invalid_edit("invalid_app_object_range"));
    }
    let line_start = source[..located.app_close]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let indent = &source[line_start..located.app_close];
    if !indent.chars().all(char::is_whitespace) {
        return Err(SteamError::invalid_edit("unsafe_app_closing_indentation"));
    }
    let insertion = format!("{indent}\t\"LaunchOptions\"\t\t\"{}\"\n", escape(token));
    let replacement = format!(
        "{}{}{}",
        &source[..located.app_close],
        insertion,
        &source[located.app_close..]
    );
    let change = Change {
        app_id,
        kind: ChangeKind::InsertLaunchOptions,
        byte_range: located.app_close..located.app_close,
    };
    Ok((replacement, change))
}

fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#""UserLocalConfigStore"
{
	"Software"
	{
		"Valve"
		{
			"Steam"
			{
				"apps"
				{
					"570"
					{
						"LastPlayed" "1"
					}
					"730"
					{
						"LaunchOptions" "-console"
					}
				}
			}
		}
	}
}
"#;

    #[test]
    fn ensure_inserts_only_into_the_requested_app() {
        let document = LocalConfigDocument::parse(SAMPLE).unwrap();

        let plan = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("-gamestateintegration"),
            )
            .unwrap();
        let replacement = String::from_utf8(plan.replacement().to_vec()).unwrap();

        assert!(plan.changed());
        assert_eq!(plan.changes().len(), 1);
        assert_eq!(plan.changes()[0].kind, ChangeKind::InsertLaunchOptions);
        assert!(replacement.contains("\"LaunchOptions\"\t\t\"-gamestateintegration\""));
        assert!(replacement
            .contains("\"730\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LaunchOptions\" \"-console\""));
    }

    #[test]
    fn ensure_is_idempotent_for_an_exact_token() {
        let source = SAMPLE.replace(
            "\"LastPlayed\" \"1\"",
            "\"LaunchOptions\" \"-novid -gamestateintegration\"",
        );
        let document = LocalConfigDocument::parse(&source).unwrap();

        let plan = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("-gamestateintegration"),
            )
            .unwrap();

        assert!(!plan.changed());
        assert!(plan.changes().is_empty());
        assert_eq!(plan.replacement(), source.as_bytes());
    }

    #[test]
    fn ensure_does_not_treat_a_substring_as_the_token() {
        let source = SAMPLE.replace(
            "\"LastPlayed\" \"1\"",
            "\"LaunchOptions\" \"foo-gamestateintegration\"",
        );
        let document = LocalConfigDocument::parse(&source).unwrap();

        let plan = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("-gamestateintegration"),
            )
            .unwrap();

        assert!(String::from_utf8(plan.into_replacement())
            .unwrap()
            .contains("foo-gamestateintegration -gamestateintegration"));
    }

    #[test]
    fn ensure_preserves_quoted_unicode_and_backslash_arguments() {
        let source = SAMPLE.replace(
            "\"LastPlayed\" \"1\"",
            "\"LaunchOptions\" \"-language \\\"简体 中文\\\" -path D:\\\\Games\"",
        );
        let document = LocalConfigDocument::parse(&source).unwrap();

        let plan = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("-gamestateintegration"),
            )
            .unwrap();
        let replacement = String::from_utf8(plan.into_replacement()).unwrap();

        assert!(replacement.contains(
            "\"LaunchOptions\" \"-language \\\"简体 中文\\\" -path D:\\\\Games -gamestateintegration\""
        ));
    }

    #[test]
    fn ensure_preserves_supported_vdf_control_escapes() {
        let source = SAMPLE.replace(
            "\"LastPlayed\" \"1\"",
            "\"LaunchOptions\" \"-novid\\t-console\\n-next\\r-end\"",
        );
        let document = LocalConfigDocument::parse(&source).unwrap();

        let plan = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("-gamestateintegration"),
            )
            .unwrap();
        let replacement = String::from_utf8(plan.replacement().to_vec()).unwrap();

        assert!(replacement.contains(
            "\"LaunchOptions\" \"-novid\\t-console\\n-next\\r-end -gamestateintegration\""
        ));
        let reparsed = LocalConfigDocument::parse(&replacement).unwrap();
        assert_eq!(
            reparsed.launch_options(AppId::new(570)).unwrap(),
            Some("-novid\t-console\n-next\r-end -gamestateintegration")
        );
    }

    #[test]
    fn remove_deletes_only_exact_whitespace_tokens() {
        let source = SAMPLE.replace(
            "\"LastPlayed\" \"1\"",
            "\"LaunchOptions\" \"foo-gamestateintegration -gamestateintegration -console\"",
        );
        let document = LocalConfigDocument::parse(&source).unwrap();

        let plan = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::RemoveToken("-gamestateintegration"),
            )
            .unwrap();
        let replacement = String::from_utf8(plan.into_replacement()).unwrap();

        assert!(replacement.contains("\"LaunchOptions\" \"foo-gamestateintegration -console\""));
    }

    #[test]
    fn token_with_whitespace_is_rejected_without_echoing_it() {
        let document = LocalConfigDocument::parse(SAMPLE).unwrap();

        let error = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("secret token"),
            )
            .unwrap_err();

        assert!(matches!(error, SteamError::InvalidEdit { .. }));
        assert!(!error.to_string().contains("secret token"));
    }

    #[test]
    fn fingerprint_changes_with_any_source_byte() {
        let original = FileFingerprint::from_bytes(b"abc");
        let changed = FileFingerprint::from_bytes(b"abd");

        assert_ne!(original, changed);
        assert_eq!(original.length(), 3);
    }

    #[test]
    fn edit_plan_debug_redacts_replacement_bytes() {
        let source = SAMPLE.replace("\"LastPlayed\" \"1\"", "\"Secret\" \"do-not-log\"");
        let document = LocalConfigDocument::parse(&source).unwrap();
        let plan = document
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("-gamestateintegration"),
            )
            .unwrap();

        let debug = format!("{plan:?}");

        assert!(!debug.contains("do-not-log"));
        assert!(!debug.contains("replacement:"));
        assert!(debug.contains("replacement_len"));
    }
}
