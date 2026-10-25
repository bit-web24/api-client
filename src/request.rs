//! Turns what the user entered into a [`Request`] ready to send. Each option
//! behaves like the `curl` flag it is labelled with in the UI.

use std::path::PathBuf;
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::http::{FormPart, Payload, Request};

/// Bytes percent-encoded in query keys and values: all but RFC 3986 "unreserved".
const QUERY_ENCODE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Body content typed into the editor or named as a file (curl's `@file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Inline(String),
    File(PathBuf),
}

impl From<Source> for Payload {
    fn from(source: Source) -> Self {
        match source {
            Source::Inline(text) => Payload::Text(text),
            Source::File(path) => Payload::File(path),
        }
    }
}

/// The request body, one variant per way curl sends one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    None,
    /// `--json`: JSON with JSON `Content-Type` and `Accept`.
    Json(Source),
    /// `--data-raw` or `--data-binary @file`: `application/x-www-form-urlencoded`,
    /// sent verbatim; like curl, nothing is percent-encoded.
    FormUrlEncoded(Source),
    /// `--data-urlencode`: `name=value` pairs, percent-encoded and joined with `&`.
    UrlEncodedPairs(Vec<(String, String)>),
    /// `-F` and `--form-string`: `multipart/form-data` fields and file uploads.
    Multipart(Vec<FormPart>),
    /// `-T`: a file uploaded as is, with no `Content-Type`.
    Upload(PathBuf),
}

impl Body {
    /// Returns the method curl uses for this body when no `-X` is given.
    pub fn implied_method(&self) -> &'static str {
        match self {
            Self::None => "GET",
            Self::Upload(_) => "PUT",
            Self::Json(_)
            | Self::FormUrlEncoded(_)
            | Self::UrlEncodedPairs(_)
            | Self::Multipart(_) => "POST",
        }
    }

    /// Returns the `Content-Type` curl adds for this body. Multipart has none here
    /// because its type carries a boundary chosen when the request is sent.
    fn content_type(&self) -> Option<&'static str> {
        match self {
            Self::Json(_) => Some("application/json"),
            Self::FormUrlEncoded(_) | Self::UrlEncodedPairs(_) => {
                Some("application/x-www-form-urlencoded")
            }
            Self::None | Self::Multipart(_) | Self::Upload(_) => None,
        }
    }

    /// Converts the body into what goes on the wire, or `None` for no body.
    fn into_payload(self) -> Option<Payload> {
        match self {
            Self::None => None,
            Self::Json(source) | Self::FormUrlEncoded(source) => Some(source.into()),
            Self::UrlEncodedPairs(pairs) => Some(Payload::Text(encode_pairs(&pairs))),
            Self::Multipart(parts) => Some(Payload::Multipart(parts)),
            Self::Upload(path) => Some(Payload::File(path)),
        }
    }
}

/// Everything the user entered for one request, already filtered to enabled rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub method: String,
    pub scheme: String,
    pub domain: String,
    /// API version segment such as `v1`; empty when the API has none.
    pub version: String,
    pub endpoint: String,
    pub params: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    pub bearer_token: Option<String>,
    /// `curl -L`: follow redirects instead of returning the 3xx response.
    pub follow_redirects: bool,
    /// `curl --max-time`: limit for the whole exchange.
    pub timeout: Duration,
}

impl Form {
    /// Returns the URL without the query params, which curl adds via `--url-query`.
    pub fn base_url(&self) -> String {
        join_url(&self.scheme, &self.domain, &self.version, &self.endpoint)
    }

    /// Resolves the form into a request: joins the URL parts and query params, and
    /// adds the headers curl would add (`Content-Type`, `Accept` for `--json`,
    /// `Authorization`) unless the user set them by hand.
    pub fn into_request(self) -> Request {
        let mut url = self.base_url();
        append_query(&mut url, &self.params);

        let content_type = self.body.content_type();
        let mut headers = self.headers;
        if let Some(content_type) = content_type
            && !has_header(&headers, "content-type")
        {
            headers.push(("Content-Type".into(), content_type.into()));
        }
        if matches!(self.body, Body::Json(_)) && !has_header(&headers, "accept") {
            headers.push(("Accept".into(), "application/json".into()));
        }
        if let Some(token) = self.bearer_token
            && !has_header(&headers, "authorization")
        {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }

        Request {
            method: self.method,
            url,
            headers,
            body: self.body.into_payload(),
            follow_redirects: self.follow_redirects,
            timeout: self.timeout,
        }
    }
}

/// Joins `scheme`, `domain`, `version` and `endpoint` with exactly one `/` between
/// non-empty parts, whatever slashes or whitespace the user typed around them.
/// The endpoint keeps a trailing slash because servers may route `/users/` and
/// `/users` differently.
fn join_url(scheme: &str, domain: &str, version: &str, endpoint: &str) -> String {
    let mut url = format!("{scheme}{}", domain.trim().trim_end_matches('/'));
    let version = version.trim().trim_matches('/');
    let endpoint = endpoint.trim().trim_start_matches('/');
    for part in [version, endpoint] {
        if !part.is_empty() {
            url.push('/');
            url.push_str(part);
        }
    }
    url
}

/// Percent-encodes a query key or value, as it appears in the final URL.
pub fn encode_query_component(text: &str) -> String {
    utf8_percent_encode(text, QUERY_ENCODE).to_string()
}

/// Encodes `pairs` as `application/x-www-form-urlencoded`, as repeated
/// `--data-urlencode` arguments do.
fn encode_pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                encode_query_component(key),
                encode_query_component(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Returns whether `headers` already contains `name`, compared case-insensitively as HTTP requires.
fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case(name))
}

/// Appends `params` to `url` as percent-encoded `key=value` pairs, extending any query already typed.
fn append_query(url: &mut String, params: &[(String, String)]) {
    for (key, value) in params {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&encode_query_component(key));
        url.push('=');
        url.push_str(&encode_query_component(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form {
            method: "GET".into(),
            scheme: "https://".into(),
            domain: "api.test".into(),
            version: String::new(),
            endpoint: "items".into(),
            params: Vec::new(),
            headers: Vec::new(),
            body: Body::None,
            bearer_token: None,
            follow_redirects: true,
            timeout: Duration::from_secs(30),
        }
    }

    fn pair(key: &str, value: &str) -> (String, String) {
        (key.into(), value.into())
    }

    #[test]
    fn url_parts_are_joined_with_single_slashes() {
        let request = Form {
            domain: "  api.test/ ".into(),
            version: "/v1/".into(),
            endpoint: " /users/42\n".into(),
            ..form()
        }
        .into_request();
        assert_eq!(request.url, "https://api.test/v1/users/42");
    }

    #[test]
    fn endpoint_keeps_its_trailing_slash() {
        let request = Form {
            endpoint: "users/".into(),
            ..form()
        }
        .into_request();
        assert_eq!(request.url, "https://api.test/users/");
    }

    #[test]
    fn empty_version_and_endpoint_are_skipped() {
        let request = Form {
            version: " ".into(),
            endpoint: String::new(),
            ..form()
        }
        .into_request();
        assert_eq!(request.url, "https://api.test");
    }

    #[test]
    fn params_are_encoded_and_appended_to_existing_query() {
        let params = vec![pair("q", "a b&c"), pair("ünï", "1")];
        let request = Form {
            endpoint: "s?page=2".into(),
            params,
            ..form()
        }
        .into_request();
        assert_eq!(
            request.url,
            "https://api.test/s?page=2&q=a%20b%26c&%C3%BCn%C3%AF=1"
        );
    }

    #[test]
    fn first_param_starts_the_query() {
        let request = Form {
            params: vec![pair("a", "1"), pair("b", "")],
            ..form()
        }
        .into_request();
        assert_eq!(request.url, "https://api.test/items?a=1&b=");
    }

    #[test]
    fn no_body_sends_no_body_and_no_content_type() {
        let request = form().into_request();
        assert_eq!(request.body, None);
        assert!(request.headers.is_empty());
    }

    #[test]
    fn json_body_adds_content_type_and_accept_like_curl_json() {
        let request = Form {
            body: Body::Json(Source::Inline("{}".into())),
            ..form()
        }
        .into_request();
        assert_eq!(request.body, Some(Payload::Text("{}".into())));
        assert_eq!(
            request.headers,
            vec![
                pair("Content-Type", "application/json"),
                pair("Accept", "application/json")
            ]
        );
    }

    #[test]
    fn form_urlencoded_body_is_sent_verbatim_like_curl_data_raw() {
        let request = Form {
            body: Body::FormUrlEncoded(Source::Inline("a=1&b=x y".into())),
            ..form()
        }
        .into_request();
        assert_eq!(request.body, Some(Payload::Text("a=1&b=x y".into())));
        assert_eq!(
            request.headers,
            vec![pair("Content-Type", "application/x-www-form-urlencoded")]
        );
    }

    #[test]
    fn urlencoded_pairs_are_encoded_like_curl_data_urlencode() {
        let request = Form {
            body: Body::UrlEncodedPairs(vec![pair("name", "Bittu Kumar"), pair("q", "a&b=c")]),
            ..form()
        }
        .into_request();
        assert_eq!(
            request.body,
            Some(Payload::Text("name=Bittu%20Kumar&q=a%26b%3Dc".into()))
        );
        assert_eq!(
            request.headers,
            vec![pair("Content-Type", "application/x-www-form-urlencoded")]
        );
    }

    #[test]
    fn multipart_and_upload_leave_content_type_to_the_sender() {
        let parts = vec![FormPart::Text {
            name: "a".into(),
            value: "1".into(),
        }];
        let multipart = Form {
            body: Body::Multipart(parts.clone()),
            ..form()
        }
        .into_request();
        assert_eq!(multipart.body, Some(Payload::Multipart(parts)));
        assert!(multipart.headers.is_empty());

        let upload = Form {
            body: Body::Upload("disk.img".into()),
            ..form()
        }
        .into_request();
        assert_eq!(upload.body, Some(Payload::File("disk.img".into())));
        assert!(upload.headers.is_empty());
    }

    #[test]
    fn user_headers_override_generated_ones() {
        let headers = vec![
            pair("content-TYPE", "application/vnd.x+json"),
            pair("ACCEPT", "text/csv"),
            pair("Authorization", "Basic x"),
        ];
        let request = Form {
            headers: headers.clone(),
            body: Body::Json(Source::Inline("{}".into())),
            bearer_token: Some("t".into()),
            ..form()
        }
        .into_request();
        assert_eq!(request.headers, headers);
    }

    #[test]
    fn bearer_token_adds_authorization() {
        let request = Form {
            bearer_token: Some("abc".into()),
            ..form()
        }
        .into_request();
        assert_eq!(request.headers, vec![pair("Authorization", "Bearer abc")]);
    }

    #[test]
    fn file_body_is_passed_on_unread() {
        let request = Form {
            body: Body::Json(Source::File("data/user.json".into())),
            ..form()
        }
        .into_request();
        assert_eq!(request.body, Some(Payload::File("data/user.json".into())));
    }

    #[test]
    fn implied_methods_match_curl() {
        assert_eq!(Body::None.implied_method(), "GET");
        assert_eq!(Body::Multipart(Vec::new()).implied_method(), "POST");
        assert_eq!(Body::Upload("f".into()).implied_method(), "PUT");
    }
}
