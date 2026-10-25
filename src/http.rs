//! Sends HTTP requests over the network. Knows nothing about the UI.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Redirects followed when [`Request::follow_redirects`] is set; curl's `--max-redirs` default.
const MAX_REDIRECTS: u32 = 50;

/// Largest response body read into memory; bigger responses fail instead of exhausting RAM.
const MAX_BODY_BYTES: u64 = 20 * 1024 * 1024;

/// Largest request body read into memory, summed over all files of a multipart
/// body; curl also loads `@file` bodies whole before sending.
const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// A request body as it goes on the wire. Files are only read when sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Text(String),
    File(PathBuf),
    /// `multipart/form-data`; the boundary is chosen when sent.
    Multipart(Vec<FormPart>),
}

/// One field of a `multipart/form-data` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormPart {
    /// Like `curl --form-string name=value`: the value is sent literally.
    Text { name: String, value: String },
    /// Like `curl -F name=@path`: the file is uploaded with its name and a Content-Type.
    File { name: String, path: PathBuf },
}

/// Why [`send`] failed.
#[derive(Debug)]
pub enum SendError {
    /// The `@file` body could not be read.
    BodyFile { path: PathBuf, source: io::Error },
    /// The `@file` body is larger than [`MAX_UPLOAD_BYTES`].
    BodyFileTooLarge { path: PathBuf },
    /// Building, sending or receiving the HTTP exchange failed.
    Http(ureq::Error),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyFile { path, source } => {
                write!(f, "could not read body file {}: {source}", path.display())
            }
            Self::BodyFileTooLarge { path } => write!(
                f,
                "body file {} is larger than {} MB",
                path.display(),
                MAX_UPLOAD_BYTES / (1024 * 1024)
            ),
            Self::Http(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BodyFile { source, .. } => Some(source),
            Self::BodyFileTooLarge { .. } => None,
            Self::Http(error) => Some(error),
        }
    }
}

impl From<ureq::Error> for SendError {
    fn from(error: ureq::Error) -> Self {
        Self::Http(error)
    }
}

impl From<ureq::http::Error> for SendError {
    fn from(error: ureq::http::Error) -> Self {
        Self::Http(error.into())
    }
}

/// A fully resolved request, ready to go on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// `None` sends no body at all, which is what servers expect for GET and HEAD.
    pub body: Option<Payload>,
    /// Like `curl -L`; when unset, a 3xx response is returned as is.
    pub follow_redirects: bool,
    /// Like `curl --max-time`: limit for the whole exchange (connect, send, receive).
    pub timeout: Duration,
}

/// The server's answer to a [`Request`].
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub reason: &'static str,
    pub elapsed: Duration,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    /// Returns the value of the `Content-Type` header, if the server sent one.
    pub fn content_type(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.as_str())
    }
}

/// Sends `request` and reads the whole response, blocking until done.
///
/// 4xx and 5xx statuses come back as a normal [`Reply`] because an API client
/// must show them. Nothing is retried: the request may not be idempotent (POST),
/// so retrying is the user's decision.
///
/// # Errors
///
/// Returns [`SendError::BodyFile`] or [`SendError::BodyFileTooLarge`] when an
/// `@file` body can't be loaded, and [`SendError::Http`] if the method, URL or a
/// header is malformed, the connection fails, the request's timeout elapses, more
/// than [`MAX_REDIRECTS`] redirects are followed, or the response exceeds
/// [`MAX_BODY_BYTES`].
pub fn send(request: &Request) -> Result<Reply, SendError> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(request.timeout))
        .max_redirects(if request.follow_redirects {
            MAX_REDIRECTS
        } else {
            0
        })
        .http_status_as_error(false)
        .build()
        .new_agent();

    let (body, content_type) = encode_body(request)?;
    let mut builder = ureq::http::Request::builder()
        .method(request.method.as_str())
        .uri(request.url.as_str());
    for (name, value) in &request.headers {
        if content_type.is_none() || !name.eq_ignore_ascii_case("content-type") {
            builder = builder.header(name.as_str(), value.as_str());
        }
    }
    if let Some(content_type) = content_type {
        builder = builder.header("Content-Type", content_type);
    }

    let started = Instant::now();
    let mut response = match body {
        Some(bytes) => agent.run(builder.body(bytes)?)?,
        None => agent.run(builder.body(())?)?,
    };
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()?;
    let elapsed = started.elapsed();

    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            (name.to_string(), value)
        })
        .collect();

    Ok(Reply {
        status: status.as_u16(),
        // Non-standard codes (e.g. 299) have no reason phrase; the number alone is shown.
        reason: status.canonical_reason().unwrap_or(""),
        elapsed,
        headers,
        body,
    })
}

/// Turns the request's payload into bytes, reading any files. For multipart it
/// also returns the `Content-Type` carrying the boundary, which replaces the
/// request's own.
///
/// # Errors
///
/// Returns [`SendError::BodyFile`] or [`SendError::BodyFileTooLarge`] when a file can't be loaded.
fn encode_body(request: &Request) -> Result<(Option<Vec<u8>>, Option<String>), SendError> {
    match &request.body {
        None => Ok((None, None)),
        Some(Payload::Text(text)) => Ok((Some(text.clone().into_bytes()), None)),
        Some(Payload::File(path)) => Ok((Some(read_body_file(path)?), None)),
        Some(Payload::Multipart(parts)) => {
            let boundary = new_boundary();
            let user_type = request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.as_str());
            let content_type = multipart_content_type(user_type, &boundary);
            Ok((Some(multipart_body(parts, &boundary)?), Some(content_type)))
        }
    }
}

/// Returns the multipart `Content-Type`: like curl, a user-set `multipart/*` type
/// gets the boundary appended, and any other type the user set is kept as is.
fn multipart_content_type(user_type: Option<&str>, boundary: &str) -> String {
    match user_type {
        None => format!("multipart/form-data; boundary={boundary}"),
        Some(own) if own.trim().to_ascii_lowercase().starts_with("multipart/") => {
            format!("{}; boundary={boundary}", own.trim())
        }
        Some(own) => own.to_string(),
    }
}

/// Returns a boundary in curl's style. It only has to be unlikely to occur in the
/// parts, not secret; a clock before 1970 just makes it less varied.
fn new_boundary() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    format!(
        "------------------------{:08x}{nanos:08x}",
        std::process::id()
    )
}

/// Assembles a `multipart/form-data` body (RFC 7578) from `parts`.
///
/// # Errors
///
/// Returns [`SendError::BodyFile`] if a file can't be read, and
/// [`SendError::BodyFileTooLarge`] once the body passes [`MAX_UPLOAD_BYTES`].
fn multipart_body(parts: &[FormPart], boundary: &str) -> Result<Vec<u8>, SendError> {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            FormPart::Text { name, value } => {
                let disposition = format!(
                    "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                    escape_quoted(name)
                );
                body.extend_from_slice(disposition.as_bytes());
                body.extend_from_slice(value.as_bytes());
            }
            FormPart::File { name, path } => {
                let filename = path
                    .file_name()
                    .map_or_else(String::new, |file| file.to_string_lossy().into_owned());
                let headers = format!(
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                    escape_quoted(name),
                    escape_quoted(&filename),
                    content_type_for_file(path)
                );
                body.extend_from_slice(headers.as_bytes());
                body.extend_from_slice(&read_body_file(path)?);
                if u64::try_from(body.len()).map_or(true, |len| len > MAX_UPLOAD_BYTES) {
                    return Err(SendError::BodyFileTooLarge { path: path.clone() });
                }
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok(body)
}

/// Escapes a field or file name for a quoted header parameter, as curl and
/// browsers do (HTML's multipart encoding): `"`, CR and LF are percent-encoded.
fn escape_quoted(text: &str) -> String {
    text.replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Returns the Content-Type curl gives an uploaded file, from the same table
/// curl uses: a few common extensions, else `application/octet-stream`.
fn content_type_for_file(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("gif") => "image/gif",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain",
        Some("htm" | "html") => "text/html",
        Some("pdf") => "application/pdf",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

/// Reads an `@file` body, refusing files over [`MAX_UPLOAD_BYTES`].
///
/// # Errors
///
/// Returns [`SendError::BodyFile`] if the file can't be opened or read, and
/// [`SendError::BodyFileTooLarge`] if it exceeds the limit.
fn read_body_file(path: &Path) -> Result<Vec<u8>, SendError> {
    let file_error = |source| SendError::BodyFile {
        path: path.to_owned(),
        source,
    };
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(file_error)?
        // One byte past the limit is enough to tell that the file is too large.
        .take(MAX_UPLOAD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(file_error)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_UPLOAD_BYTES) {
        return Err(SendError::BodyFileTooLarge {
            path: path.to_owned(),
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_file_is_read_byte_for_byte() {
        let path = std::env::temp_dir().join(format!("api-client-body-{}.bin", std::process::id()));
        std::fs::write(&path, b"line 1\r\nline 2\n\x00").unwrap();
        let bytes = read_body_file(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(bytes.unwrap(), b"line 1\r\nline 2\n\x00");
    }

    #[test]
    fn multipart_body_has_text_and_file_parts() {
        let path = std::env::temp_dir().join(format!("api-client-part-{}.txt", std::process::id()));
        std::fs::write(&path, b"file body").unwrap();
        let parts = vec![
            FormPart::Text {
                name: "na\"me".into(),
                value: "@literal".into(),
            },
            FormPart::File {
                name: "doc".into(),
                path: path.clone(),
            },
        ];
        let body = multipart_body(&parts, "XyZ");
        let filename = path.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::remove_file(&path).unwrap();
        let expected = format!(
            "--XyZ\r\nContent-Disposition: form-data; name=\"na%22me\"\r\n\r\n@literal\r\n\
             --XyZ\r\nContent-Disposition: form-data; name=\"doc\"; filename=\"{filename}\"\r\n\
             Content-Type: text/plain\r\n\r\nfile body\r\n--XyZ--\r\n"
        );
        assert_eq!(String::from_utf8(body.unwrap()).unwrap(), expected);
    }

    #[test]
    fn multipart_content_type_follows_curl() {
        assert_eq!(
            multipart_content_type(None, "B"),
            "multipart/form-data; boundary=B"
        );
        assert_eq!(
            multipart_content_type(Some("multipart/mixed"), "B"),
            "multipart/mixed; boundary=B"
        );
        assert_eq!(
            multipart_content_type(Some("text/plain"), "B"),
            "text/plain"
        );
    }

    #[test]
    fn file_content_types_match_curls_table() {
        assert_eq!(
            content_type_for_file(Path::new("a/photo.JPG")),
            "image/jpeg"
        );
        assert_eq!(
            content_type_for_file(Path::new("report.pdf")),
            "application/pdf"
        );
        assert_eq!(
            content_type_for_file(Path::new("data.json")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for_file(Path::new("noext")),
            "application/octet-stream"
        );
    }

    #[test]
    fn missing_body_file_names_the_path() {
        let error = read_body_file(Path::new("/nonexistent/body.json")).unwrap_err();
        assert!(matches!(error, SendError::BodyFile { .. }));
        assert!(
            error
                .to_string()
                .starts_with("could not read body file /nonexistent/body.json: ")
        );
    }
}
