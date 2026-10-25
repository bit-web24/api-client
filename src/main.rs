//! Native desktop API client with a deliberately small, Postman-like interface.
//!
//! The UI lives in `ui/*.slint` and mirrors curl's options. This file only wires
//! it up: [`draft`] reads the form, [`request`] builds what to send, [`curl`]
//! renders the same request as a curl command, [`http`] sends it, and
//! [`response`] formats what came back. Requests are grouped into services
//! ([`service`]), kept on disk by [`store`] and shown by [`services_ui`].

mod curl;
mod draft;
mod http;
mod request;
mod response;
mod service;
mod services_ui;
mod store;

use std::rc::Rc;

use slint::{Model, ModelRc, SharedString, VecModel};

use crate::draft::form_from_draft;
use crate::http::{Reply, SendError};
use crate::response::{BodyFormat, format_elapsed, format_size, render_body};
use crate::services_ui::Services;

slint::include_modules!();

/// URL schemes offered by the scheme chooser, in display order.
const SCHEMES: [&str; 2] = ["http://", "https://"];

/// Splits `url` into one of [`SCHEMES`] (matched case-insensitively) and the rest.
///
/// Returns `None` when `url` does not start with a known scheme.
fn split_scheme(url: &str) -> Option<(&'static str, &str)> {
    SCHEMES.into_iter().find_map(|scheme| {
        let rest = url.get(scheme.len()..)?;
        url.get(..scheme.len())?
            .eq_ignore_ascii_case(scheme)
            .then_some((scheme, rest))
    })
}

/// Splits a full URL typed or pasted into the domain box into its scheme, its
/// domain and everything after the domain (path and query).
///
/// Returns `None` unless `text` starts with a known scheme: without one the user
/// is typing a bare domain, and moving text between boxes mid-typing would fight them.
fn split_pasted_url(text: &str) -> Option<(&'static str, &str, &str)> {
    let (scheme, rest) = split_scheme(text)?;
    let path_start = rest.find(['/', '?']).unwrap_or(rest.len());
    let (domain, path) = rest.split_at(path_start);
    Some((scheme, domain, path))
}

/// Returns the blank row editors start with, so there is always a row to type into.
fn blank_row() -> KeyValue {
    KeyValue {
        enabled: true,
        ..KeyValue::default()
    }
}

/// The editable key/value tables, one model per [`RowTable`].
struct Tables {
    params: Rc<VecModel<KeyValue>>,
    headers: Rc<VecModel<KeyValue>>,
    urlencoded_fields: Rc<VecModel<KeyValue>>,
    multipart_parts: Rc<VecModel<KeyValue>>,
}

impl Tables {
    /// Creates every table with one blank row and shows them in `ui`.
    fn attach(ui: &AppWindow) -> Rc<Self> {
        let table = || Rc::new(VecModel::from(vec![blank_row()]));
        let tables = Rc::new(Self {
            params: table(),
            headers: table(),
            urlencoded_fields: table(),
            multipart_parts: table(),
        });
        ui.set_params(ModelRc::from(tables.params.clone()));
        ui.set_headers(ModelRc::from(tables.headers.clone()));
        ui.set_urlencoded_fields(ModelRc::from(tables.urlencoded_fields.clone()));
        ui.set_multipart_parts(ModelRc::from(tables.multipart_parts.clone()));
        tables
    }

    /// Returns the model behind `table`.
    fn get(&self, table: RowTable) -> &VecModel<KeyValue> {
        match table {
            RowTable::Params => &self.params,
            RowTable::Headers => &self.headers,
            RowTable::UrlencodedFields => &self.urlencoded_fields,
            RowTable::MultipartParts => &self.multipart_parts,
        }
    }
}

/// Connects the add, remove and edit actions of every key/value table.
fn wire_tables(ui: &AppWindow, tables: &Rc<Tables>) {
    ui.on_add_row({
        let tables = tables.clone();
        move |table| tables.get(table).push(blank_row())
    });
    ui.on_remove_row({
        let tables = tables.clone();
        move |table, index| remove_row(tables.get(table), index)
    });
    ui.on_row_edited({
        let tables = tables.clone();
        move |table, index, row| update_row(tables.get(table), index, row)
    });
}

/// Removes the row at `index`, ignoring indices the UI may report for rows already gone.
fn remove_row(rows: &VecModel<KeyValue>, index: i32) {
    if let Ok(index) = usize::try_from(index)
        && index < rows.row_count()
    {
        rows.remove(index);
    }
}

/// Replaces the row at `index` with the edited version from the UI.
fn update_row(rows: &VecModel<KeyValue>, index: i32, row: KeyValue) {
    if let Ok(index) = usize::try_from(index)
        && index < rows.row_count()
    {
        rows.set_row_data(index, row);
    }
}

/// Connects the custom title bar's minimize and maximize buttons to the window.
fn wire_window_controls(ui: &AppWindow) {
    ui.on_minimize({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                ui.window().set_minimized(true);
            }
        }
    });
    ui.on_toggle_maximize({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                let window = ui.window();
                window.set_maximized(!window.is_maximized());
            }
        }
    });
}

/// Offers [`SCHEMES`] in the chooser and spreads a full URL typed or pasted into
/// the domain box over the scheme, domain and endpoint boxes, so the request
/// never becomes `https://https://...`.
fn wire_url(ui: &AppWindow) {
    let schemes: Vec<SharedString> = SCHEMES.into_iter().map(SharedString::from).collect();
    ui.set_schemes(ModelRc::new(VecModel::from(schemes)));
    ui.on_domain_edited({
        let ui = ui.as_weak();
        move |text| {
            if let Some((scheme, domain, path)) = split_pasted_url(&text)
                && let Some(ui) = ui.upgrade()
            {
                ui.set_scheme(scheme.into());
                ui.set_domain(domain.into());
                // The pasted path already holds any version, so it moves whole into the endpoint.
                if !path.is_empty() {
                    ui.set_version("".into());
                    ui.set_endpoint(path.into());
                }
            }
        }
    });
}

/// Fills the curl preview, or explains in a shell comment why the form can't be sent.
fn wire_curl_preview(ui: &AppWindow) {
    ui.on_curl_command(|draft| match form_from_draft(&draft) {
        Ok(form) => curl::command(&form).into(),
        Err(message) => format!("# {message}").into(),
    });
}

/// Converts a reply into what the response panel displays.
fn to_ui_response(reply: &Reply) -> Response {
    let format = BodyFormat::detect(reply.content_type(), &reply.body);
    let headers: Vec<KeyValue> = reply
        .headers
        .iter()
        .map(|(key, value)| KeyValue {
            enabled: true,
            key: key.into(),
            value: value.into(),
        })
        .collect();
    Response {
        status_code: i32::from(reply.status),
        status_text: reply.reason.into(),
        elapsed: format_elapsed(reply.elapsed).into(),
        size: format_size(reply.body.len()).into(),
        format: format.label().into(),
        body: render_body(format, &reply.body).into(),
        headers: ModelRc::new(VecModel::from(headers)),
    }
}

/// Shows `message` in the response panel as a failed request.
fn show_error(ui: &AppWindow, message: String) {
    ui.set_response_error(message.into());
    ui.set_response_state(ResponseState::Failed);
}

/// Shows the outcome of a finished request in the response panel.
fn show_result(ui: &AppWindow, result: Result<Reply, SendError>) {
    match result {
        Ok(reply) => {
            ui.set_response(to_ui_response(&reply));
            ui.set_response_state(ResponseState::Received);
        }
        Err(error) => show_error(ui, error.to_string()),
    }
}

/// Sends the composed request on Send or Enter, saving it to its service first.
///
/// The network call runs on a worker thread so the window stays responsive;
/// its result is applied back on the UI thread.
fn wire_send(ui: &AppWindow, services: &Rc<Services>) {
    let ui_weak = ui.as_weak();
    let services = services.clone();
    ui.on_send(move || {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        // One request at a time: a second one would race the first for the response panel.
        if ui.get_response_state() == ResponseState::Sending {
            return;
        }
        services.commit(&ui);
        let draft = ui.get_draft();
        if draft.domain.trim().is_empty() {
            return show_error(&ui, "Enter a domain to send the request to.".into());
        }
        let request =
            form_from_draft(&draft).and_then(|form| form.into_request(|name| std::env::var(name)));
        let request = match request {
            Ok(request) => request,
            Err(message) => return show_error(&ui, message),
        };
        ui.set_response_state(ResponseState::Sending);

        let ui_weak = ui.as_weak();
        // Detached on purpose: `http::send` is bounded by its timeout and owns no shared state.
        std::thread::spawn(move || {
            let result = http::send(&request);
            // This only fails once the event loop has quit, when there is no window left to update.
            let _ = ui_weak.upgrade_in_event_loop(move |ui| show_result(&ui, result));
        });
    });
}

/// Opens the main window and runs the event loop until it is closed.
///
/// # Errors
///
/// Returns [`slint::PlatformError`] if no windowing backend can be initialized.
fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    wire_window_controls(&ui);
    wire_url(&ui);

    let tables = Tables::attach(&ui);
    wire_tables(&ui, &tables);
    let services = Services::open(&ui, tables);
    services_ui::wire(&ui, &services);
    wire_send(&ui, &services);
    wire_curl_preview(&ui);

    ui.run()?;
    if let Err(message) = services.save_on_exit(&ui) {
        // The window is gone, so stderr is the only place left to report this.
        eprintln!("{message}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_scheme_separates_known_schemes() {
        assert_eq!(split_scheme("https://a.io/x"), Some(("https://", "a.io/x")));
        assert_eq!(split_scheme("http://a.io"), Some(("http://", "a.io")));
    }

    #[test]
    fn split_scheme_ignores_case() {
        assert_eq!(split_scheme("HTTPS://a.io"), Some(("https://", "a.io")));
    }

    #[test]
    fn split_scheme_rejects_urls_without_known_scheme() {
        assert_eq!(split_scheme("a.io/https://"), None);
        assert_eq!(split_scheme("ftp://a.io"), None);
        assert_eq!(split_scheme("http:/"), None);
        assert_eq!(split_scheme(""), None);
    }

    #[test]
    fn split_pasted_url_spreads_full_urls() {
        assert_eq!(
            split_pasted_url("https://api.io/v1/users?id=2"),
            Some(("https://", "api.io", "/v1/users?id=2"))
        );
        assert_eq!(
            split_pasted_url("http://api.io?q=1"),
            Some(("http://", "api.io", "?q=1"))
        );
        assert_eq!(
            split_pasted_url("HTTPS://api.io"),
            Some(("https://", "api.io", ""))
        );
    }

    #[test]
    fn split_pasted_url_leaves_bare_domains_alone() {
        assert_eq!(split_pasted_url("api.io/v1"), None);
        assert_eq!(split_pasted_url("api.io"), None);
    }
}
