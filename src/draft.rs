//! Converts the UI's [`RequestDraft`] into a [`Form`]: the one place that knows
//! how the form's tabs, indices and table rows map onto curl's options.

use std::path::PathBuf;
use std::time::Duration;

use slint::{Model, ModelExt, ModelRc};

use crate::http::FormPart;
use crate::request::{BearerToken, Body, EnvVarName, Form, Source};
use crate::{KeyValue, RequestDraft};

/// Index of "Bearer Token" in the Auth tab's options.
const AUTH_BEARER: i32 = 1;

/// Index of "Env var $" in the bearer token's source options; the other is "Value".
const TOKEN_FROM_ENV: i32 = 1;

/// Index of "File @" in the Body tab's source options; the other is "Inline".
const BODY_SOURCE_FILE: i32 = 1;

/// Largest `--max-time` accepted, in seconds; an hour covers slow endpoints
/// without letting a typo leave a request hanging for days.
const MAX_TIMEOUT_SECS: u64 = 3600;

/// Converts the form the user composed into a [`Form`].
///
/// # Errors
///
/// Returns a message for the user when the body type is unknown, a needed file
/// path is missing, the token's variable name is invalid, or `--max-time` is invalid.
pub fn form_from_draft(draft: &RequestDraft) -> Result<Form, String> {
    Ok(Form {
        method: draft.method.to_string(),
        scheme: draft.scheme.to_string(),
        domain: draft.domain.to_string(),
        version: draft.version.to_string(),
        endpoint: draft.endpoint.to_string(),
        params: enabled_pairs(&draft.params),
        headers: enabled_pairs(&draft.headers),
        body: body_from_draft(draft)?,
        bearer_token: bearer_token(draft)?,
        follow_redirects: draft.follow_redirects,
        timeout: parse_max_time(&draft.max_time)?,
    })
}

/// Returns the bearer token from the Auth tab: the typed value, or the name of
/// the environment variable to read it from when sent. A leading `$` is
/// accepted, as people often type the variable the way a shell expands it.
///
/// # Errors
///
/// Returns a message when the variable name is missing or not a valid name.
fn bearer_token(draft: &RequestDraft) -> Result<Option<BearerToken>, String> {
    if draft.auth_type != AUTH_BEARER {
        return Ok(None);
    }
    if draft.auth_token_source != TOKEN_FROM_ENV {
        let token = draft.auth_token.trim();
        return Ok((!token.is_empty()).then(|| BearerToken::Value(token.to_string())));
    }
    let name = draft.auth_token_env.trim();
    let name = name.strip_prefix('$').unwrap_or(name);
    if name.is_empty() {
        return Err("Enter the name of the environment variable holding the token.".into());
    }
    EnvVarName::parse(name)
        .map(|name| Some(BearerToken::Env(name)))
        .ok_or_else(|| {
            format!("\"{name}\" is not a valid variable name: use letters, digits and _, not starting with a digit.")
        })
}

/// Builds the body from the type picked in the Body tab, in the tab's order.
///
/// # Errors
///
/// Returns a message for an unknown body type or a missing file path.
fn body_from_draft(draft: &RequestDraft) -> Result<Body, String> {
    match draft.body_type {
        0 => Ok(Body::None),
        1 => source(draft).map(Body::Json),
        2 => source(draft).map(Body::FormUrlEncoded),
        3 => Ok(Body::UrlEncodedPairs(enabled_pairs(
            &draft.urlencoded_fields,
        ))),
        4 => multipart_parts(&draft.multipart_parts).map(Body::Multipart),
        5 => body_file(draft).map(Body::Upload),
        _ => Err("Unknown body type selected.".into()),
    }
}

/// Returns the editor text, or the `@file` path when the source is a file.
///
/// # Errors
///
/// Returns a message when the source is a file but no path is given.
fn source(draft: &RequestDraft) -> Result<Source, String> {
    if draft.body_source == BODY_SOURCE_FILE {
        body_file(draft).map(Source::File)
    } else {
        Ok(Source::Inline(draft.body.to_string()))
    }
}

/// Returns the body file path.
///
/// # Errors
///
/// Returns a message when the path is empty.
fn body_file(draft: &RequestDraft) -> Result<PathBuf, String> {
    let path = draft.body_file.trim();
    if path.is_empty() {
        return Err("Enter the path of the file to send as the body.".into());
    }
    Ok(path.into())
}

/// Turns multipart rows into parts with curl's `-F` convention: a value starting
/// with `@` uploads that file, anything else is sent as literal text.
///
/// # Errors
///
/// Returns a message naming the field when `@` is not followed by a path.
fn multipart_parts(rows: &ModelRc<KeyValue>) -> Result<Vec<FormPart>, String> {
    enabled_pairs(rows)
        .into_iter()
        .map(|(name, value)| match value.strip_prefix('@') {
            Some(path) if path.trim().is_empty() => Err(format!(
                "Multipart field \"{name}\": enter a file path after @."
            )),
            Some(path) => Ok(FormPart::File {
                name,
                path: path.trim().into(),
            }),
            None => Ok(FormPart::Text { name, value }),
        })
        .collect()
}

/// Collects the enabled rows that have a key, as plain `(key, value)` pairs.
///
/// Reads with dependency tracking, so a binding that calls this (the curl
/// preview) re-evaluates when a row is added, removed or edited.
fn enabled_pairs(rows: &ModelRc<KeyValue>) -> Vec<(String, String)> {
    rows.model_tracker().track_row_count_changes();
    (0..rows.row_count())
        .filter_map(|index| rows.row_data_tracked(index))
        .filter(|row| row.enabled && !row.key.trim().is_empty())
        .map(|row| (row.key.trim().to_string(), row.value.to_string()))
        .collect()
}

/// Parses the `--max-time` box into a timeout.
///
/// # Errors
///
/// Returns a message unless `text` is a whole number from 1 to [`MAX_TIMEOUT_SECS`].
fn parse_max_time(text: &str) -> Result<Duration, String> {
    text.trim()
        .parse::<u64>()
        .ok()
        .filter(|secs| (1..=MAX_TIMEOUT_SECS).contains(secs))
        .map(Duration::from_secs)
        .ok_or_else(|| {
            format!("--max-time must be a whole number of seconds from 1 to {MAX_TIMEOUT_SECS}.")
        })
}

#[cfg(test)]
mod tests {
    use slint::VecModel;

    use super::*;

    fn rows(pairs: &[(bool, &str, &str)]) -> ModelRc<KeyValue> {
        let rows: Vec<KeyValue> = pairs
            .iter()
            .map(|&(enabled, key, value)| KeyValue {
                enabled,
                key: key.into(),
                value: value.into(),
            })
            .collect();
        ModelRc::new(VecModel::from(rows))
    }

    fn draft(body_type: i32) -> RequestDraft {
        RequestDraft {
            body_type,
            max_time: "30".into(),
            ..RequestDraft::default()
        }
    }

    #[test]
    fn multipart_rows_follow_curls_at_convention() {
        let parts = multipart_parts(&rows(&[
            (true, "name", "Bittu"),
            (true, "avatar", "@ img/me.png "),
            (false, "skipped", "x"),
            (true, " ", "no key"),
        ]));
        assert_eq!(
            parts,
            Ok(vec![
                FormPart::Text {
                    name: "name".into(),
                    value: "Bittu".into()
                },
                FormPart::File {
                    name: "avatar".into(),
                    path: "img/me.png".into()
                },
            ])
        );
    }

    #[test]
    fn multipart_at_without_path_is_rejected() {
        let error = multipart_parts(&rows(&[(true, "doc", "@ ")])).unwrap_err();
        assert_eq!(error, "Multipart field \"doc\": enter a file path after @.");
    }

    #[test]
    fn body_types_map_in_tab_order() {
        assert_eq!(body_from_draft(&draft(0)), Ok(Body::None));
        let json = RequestDraft {
            body: "{}".into(),
            ..draft(1)
        };
        assert_eq!(
            body_from_draft(&json),
            Ok(Body::Json(Source::Inline("{}".into())))
        );
        let form_file = RequestDraft {
            body_source: BODY_SOURCE_FILE,
            body_file: " form.txt ".into(),
            ..draft(2)
        };
        assert_eq!(
            body_from_draft(&form_file),
            Ok(Body::FormUrlEncoded(Source::File("form.txt".into())))
        );
        let pairs = RequestDraft {
            urlencoded_fields: rows(&[(true, "a", "1")]),
            ..draft(3)
        };
        assert_eq!(
            body_from_draft(&pairs),
            Ok(Body::UrlEncodedPairs(vec![("a".into(), "1".into())]))
        );
        let upload = RequestDraft {
            body_file: "disk.img".into(),
            ..draft(5)
        };
        assert_eq!(
            body_from_draft(&upload),
            Ok(Body::Upload("disk.img".into()))
        );
        assert!(body_from_draft(&draft(6)).is_err());
    }

    #[test]
    fn upload_without_path_is_rejected() {
        assert!(body_from_draft(&draft(5)).is_err());
    }

    #[test]
    fn bearer_token_from_value_or_env_var() {
        let typed = RequestDraft {
            auth_type: AUTH_BEARER,
            auth_token: " abc ".into(),
            ..draft(0)
        };
        assert_eq!(
            bearer_token(&typed),
            Ok(Some(BearerToken::Value("abc".into())))
        );
        let from_env = RequestDraft {
            auth_type: AUTH_BEARER,
            auth_token_source: TOKEN_FROM_ENV,
            auth_token_env: " $API_TOKEN ".into(),
            ..draft(0)
        };
        assert_eq!(
            bearer_token(&from_env),
            Ok(Some(BearerToken::Env(
                EnvVarName::parse("API_TOKEN").unwrap()
            )))
        );
        let no_auth = RequestDraft {
            auth_token: "abc".into(),
            ..draft(0)
        };
        assert_eq!(bearer_token(&no_auth), Ok(None));
    }

    #[test]
    fn env_var_name_must_be_given_and_valid() {
        let missing = RequestDraft {
            auth_type: AUTH_BEARER,
            auth_token_source: TOKEN_FROM_ENV,
            ..draft(0)
        };
        assert!(bearer_token(&missing).is_err());
        let invalid = RequestDraft {
            auth_token_env: "API-TOKEN".into(),
            ..missing
        };
        assert!(
            bearer_token(&invalid)
                .unwrap_err()
                .starts_with("\"API-TOKEN\" is not a valid")
        );
    }

    #[test]
    fn max_time_accepts_whole_seconds_within_bounds() {
        assert_eq!(parse_max_time(" 30 "), Ok(Duration::from_secs(30)));
        assert_eq!(parse_max_time("1"), Ok(Duration::from_secs(1)));
        assert_eq!(parse_max_time("3600"), Ok(Duration::from_secs(3600)));
        for invalid in ["0", "3601", "-1", "1.5", "", "ten"] {
            assert!(parse_max_time(invalid).is_err(), "{invalid:?} was accepted");
        }
    }
}
