//! Renders a [`Form`] as the `curl` command that sends the same request, so the
//! UI can show what it is about to do in curl's own terms.

use std::path::Path;

use crate::http::FormPart;
use crate::request::{Body, Form, Source, encode_query_component};

/// Separator between arguments: one option per line, shell-continuation style.
const ARG_SEPARATOR: &str = " \\\n  ";

/// Returns the `curl` command equivalent to sending `form`.
pub fn command(form: &Form) -> String {
    let mut args = vec!["curl".to_string()];
    args.extend(method_flag(form));
    args.push(quote(&form.base_url()));
    if form.follow_redirects {
        args.push("-L".into());
    }
    args.push(format!("--max-time {}", form.timeout.as_secs()));
    for (key, value) in &form.params {
        // curl URL-encodes the value itself but expects the key already encoded.
        let pair = format!("{}={value}", encode_query_component(key));
        args.push(format!("--url-query {}", quote(&pair)));
    }
    for (name, value) in &form.headers {
        args.push(format!("-H {}", quote(&format!("{name}: {value}"))));
    }
    if let Some(token) = &form.bearer_token {
        args.push(format!("--oauth2-bearer {}", quote(token)));
    }
    args.extend(body_args(&form.body));
    args.join(ARG_SEPARATOR)
}

/// Returns the curl arguments that send `body`.
fn body_args(body: &Body) -> Vec<String> {
    match body {
        Body::None => Vec::new(),
        Body::Json(Source::Inline(text)) => vec![format!("--json {}", quote(text))],
        Body::Json(Source::File(path)) => vec![format!("--json {}", at_file(path))],
        Body::FormUrlEncoded(Source::Inline(text)) => vec![format!("--data-raw {}", quote(text))],
        // `-d @file` would strip newlines; --data-binary sends the file as is, like the app.
        Body::FormUrlEncoded(Source::File(path)) => {
            vec![format!("--data-binary {}", at_file(path))]
        }
        Body::UrlEncodedPairs(pairs) => pairs
            .iter()
            .map(|(key, value)| {
                // As with --url-query, curl encodes the value but not the name.
                let pair = format!("{}={value}", encode_query_component(key));
                format!("--data-urlencode {}", quote(&pair))
            })
            .collect(),
        Body::Multipart(parts) => parts.iter().map(form_part_arg).collect(),
        Body::Upload(path) => vec![format!("-T {}", quote(&path.display().to_string()))],
    }
}

/// Returns the curl argument for one multipart field: `--form-string` keeps text
/// literal (plain `-F` would treat a leading `@` or `<` as a file), `-F` uploads files.
fn form_part_arg(part: &FormPart) -> String {
    match part {
        FormPart::Text { name, value } => {
            format!("--form-string {}", quote(&format!("{name}={value}")))
        }
        FormPart::File { name, path } => {
            format!("-F {}", quote(&format!("{name}=@{}", path.display())))
        }
    }
}

/// Returns `@path` quoted for the shell, curl's syntax for "read this file".
fn at_file(path: &Path) -> String {
    quote(&format!("@{}", path.display()))
}

/// Returns the flag selecting the method, or `None` where curl already picks it
/// for this body (see [`Body::implied_method`]). HEAD uses `-I` because
/// `-X HEAD` makes curl wait for a body that never comes.
fn method_flag(form: &Form) -> Option<String> {
    match form.method.as_str() {
        method if method == form.body.implied_method() => None,
        "HEAD" if form.body == Body::None => Some("-I".into()),
        method => Some(format!("-X {method}")),
    }
}

/// Quotes `text` for a POSIX shell: nothing is special inside single quotes,
/// and a single quote itself is written as `'\''`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn form() -> Form {
        Form {
            method: "GET".into(),
            scheme: "https://".into(),
            domain: "api.test".into(),
            version: "v1".into(),
            endpoint: "items".into(),
            params: Vec::new(),
            headers: Vec::new(),
            body: Body::None,
            bearer_token: None,
            follow_redirects: true,
            timeout: Duration::from_secs(30),
        }
    }

    fn post(body: Body) -> Form {
        Form {
            method: "POST".into(),
            body,
            ..form()
        }
    }

    fn lines(command: &str) -> Vec<&str> {
        command.split(ARG_SEPARATOR).collect()
    }

    fn body_lines(form: &Form) -> Vec<String> {
        lines(&command(form))[4..]
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn plain_get_needs_no_method_flag() {
        let command = command(&form());
        assert_eq!(
            lines(&command),
            ["curl", "'https://api.test/v1/items'", "-L", "--max-time 30"]
        );
    }

    #[test]
    fn post_with_json_uses_curls_implied_method() {
        let command = command(&Form {
            follow_redirects: false,
            ..post(Body::Json(Source::Inline(r#"{"a":1}"#.into())))
        });
        assert_eq!(
            lines(&command),
            [
                "curl",
                "'https://api.test/v1/items'",
                "--max-time 30",
                r#"--json '{"a":1}'"#
            ]
        );
    }

    #[test]
    fn other_methods_are_explicit() {
        let get_with_body = Form {
            body: Body::FormUrlEncoded(Source::Inline("a=1".into())),
            ..form()
        };
        assert_eq!(lines(&command(&get_with_body))[1], "-X GET");
        let delete = Form {
            method: "DELETE".into(),
            ..form()
        };
        assert_eq!(lines(&command(&delete))[1], "-X DELETE");
        let head = Form {
            method: "HEAD".into(),
            ..form()
        };
        assert_eq!(lines(&command(&head))[1], "-I");
    }

    #[test]
    fn upload_implies_put() {
        let put = Form {
            method: "PUT".into(),
            body: Body::Upload("disk image.img".into()),
            ..form()
        };
        assert_eq!(lines(&command(&put))[1], "'https://api.test/v1/items'");
        assert_eq!(body_lines(&put), ["-T 'disk image.img'"]);
        let post_upload = post(Body::Upload("a.bin".into()));
        assert_eq!(lines(&command(&post_upload))[1], "-X POST");
    }

    #[test]
    fn params_headers_and_auth_map_to_their_flags() {
        let command = command(&Form {
            params: vec![("ünï".into(), "a b".into())],
            headers: vec![("X-Id".into(), "7".into())],
            bearer_token: Some("tok".into()),
            ..form()
        });
        assert_eq!(
            lines(&command)[4..],
            [
                "--url-query '%C3%BCn%C3%AF=a b'",
                "-H 'X-Id: 7'",
                "--oauth2-bearer 'tok'",
            ]
        );
    }

    #[test]
    fn form_urlencoded_bodies_use_data_flags() {
        let inline = post(Body::FormUrlEncoded(Source::Inline("x=1".into())));
        assert_eq!(body_lines(&inline), ["--data-raw 'x=1'"]);
        let file = post(Body::FormUrlEncoded(Source::File("form.txt".into())));
        assert_eq!(body_lines(&file), ["--data-binary '@form.txt'"]);
        let pairs = post(Body::UrlEncodedPairs(vec![
            ("full name".into(), "Bittu Kumar".into()),
            ("lang".into(), "rust".into()),
        ]));
        assert_eq!(
            body_lines(&pairs),
            [
                "--data-urlencode 'full%20name=Bittu Kumar'",
                "--data-urlencode 'lang=rust'"
            ]
        );
    }

    #[test]
    fn json_file_uses_at_syntax() {
        let json = post(Body::Json(Source::File("my data/user.json".into())));
        assert_eq!(body_lines(&json), ["--json '@my data/user.json'"]);
    }

    #[test]
    fn multipart_uses_form_string_for_text_and_f_for_files() {
        let multipart = post(Body::Multipart(vec![
            FormPart::Text {
                name: "note".into(),
                value: "@not a file".into(),
            },
            FormPart::File {
                name: "avatar".into(),
                path: "img/me.png".into(),
            },
        ]));
        assert_eq!(
            body_lines(&multipart),
            [
                "--form-string 'note=@not a file'",
                "-F 'avatar=@img/me.png'"
            ]
        );
    }

    #[test]
    fn single_quotes_are_escaped_for_the_shell() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
