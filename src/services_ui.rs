//! Connects the [`Workspace`] of services to the window: shows the active
//! request in the form, writes the form back, switches and creates services
//! and requests, and saves everything to disk.

use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::service::{Row, SavedRequest, Service, Workspace, matching_requests};
use crate::store::{self, Loaded};
use crate::{
    AppWindow, KeyValue, RequestDraft, RequestEntry, ResponseState, RowTable, ServiceEntry, Tables,
    curl_text, show_error,
};

/// The services and where they are saved, shared by the UI callbacks.
pub struct Services {
    /// A `RefCell` because Slint callbacks are `Fn` closures on the UI thread.
    /// No borrow is held while Slint could call back into this type.
    workspace: RefCell<Workspace>,
    /// `None` when there is nowhere to save; changes then live for this session only.
    dir: Option<PathBuf>,
    tables: Rc<Tables>,
}

impl Services {
    /// Loads the saved services and shows the active request. Problems while
    /// loading are reported in the response panel.
    pub fn open(ui: &AppWindow, tables: Rc<Tables>) -> Rc<Self> {
        let (workspace, dir, problem) = load_workspace();
        let services = Rc::new(Self {
            workspace: RefCell::new(workspace),
            dir,
            tables,
        });
        services.show(ui);
        if let Some(problem) = problem {
            show_error(ui, problem);
        }
        services
    }

    /// Writes the form into the active request and the base URL into its service.
    fn capture(&self, ui: &AppWindow) {
        let draft = ui.get_draft();
        let mut workspace = self.workspace.borrow_mut();
        let service = workspace.active_mut();
        service.scheme = draft.scheme.to_string();
        service.domain = draft.domain.to_string();
        service.version = draft.version.to_string();
        *service.active_mut() = saved_from_draft(&draft);
    }

    /// Captures the form and saves, e.g. before sending, and refreshes the
    /// sidebar, whose titles may have changed with the endpoint.
    pub fn commit(&self, ui: &AppWindow) {
        self.capture(ui);
        if let Err(message) = self.save() {
            show_error(ui, message);
        }
        self.show_lists(ui);
    }

    /// Captures the form and saves, for when the window has closed and there
    /// is nowhere left to show an error.
    ///
    /// # Errors
    ///
    /// Returns a message when the file can't be written.
    pub fn save_on_exit(&self, ui: &AppWindow) -> Result<(), String> {
        self.capture(ui);
        self.save()
    }

    /// Captures the form, applies `change` to the workspace, then shows and saves the result.
    fn switch(&self, ui: &AppWindow, change: impl FnOnce(&mut Workspace)) {
        self.capture(ui);
        change(&mut self.workspace.borrow_mut());
        self.show(ui);
        if let Err(message) = self.save() {
            show_error(ui, message);
        }
    }

    /// Captures the form into the current service, then creates service `name`
    /// and shows it.
    ///
    /// # Errors
    ///
    /// Returns a message when the name is empty or taken; nothing changes then.
    fn create(&self, ui: &AppWindow, name: &str) -> Result<(), String> {
        self.capture(ui);
        self.workspace.borrow_mut().create(name)?;
        self.show(ui);
        if let Err(message) = self.save() {
            show_error(ui, message);
        }
        Ok(())
    }

    /// Saves the workspace, if there is a place to save it.
    ///
    /// # Errors
    ///
    /// Returns a message naming the directory when writing fails.
    fn save(&self) -> Result<(), String> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        store::save(dir, &self.workspace.borrow()).map_err(|error: io::Error| {
            format!("Could not save services to {}: {error}", dir.display())
        })
    }

    /// Shows the active service's base URL and active request in the form.
    fn show(&self, ui: &AppWindow) {
        {
            let workspace = self.workspace.borrow();
            let service = workspace.active();
            ui.set_scheme(service.scheme.as_str().into());
            ui.set_domain(service.domain.as_str().into());
            ui.set_version(service.version.as_str().into());
            show_request(ui, &self.tables, service.active());
        }
        ui.set_response_state(ResponseState::Idle);
        self.show_lists(ui);
    }

    /// Fills the sidebar: the service switcher and the requests matching the search.
    fn show_lists(&self, ui: &AppWindow) {
        let workspace = self.workspace.borrow();
        let service = workspace.active();
        ui.set_service_name(service.name.as_str().into());
        ui.set_service_base_url(service.base_url().into());
        let entries: Vec<ServiceEntry> = workspace
            .services()
            .iter()
            .map(|service| ServiceEntry {
                name: service.name.as_str().into(),
                base_url: service.base_url().into(),
            })
            .collect();
        ui.set_services(ModelRc::new(VecModel::from(entries)));
        ui.set_active_service(ui_index(workspace.active_index()));

        let requests = service.requests();
        let entries: Vec<RequestEntry> = matching_requests(requests, &ui.get_request_search())
            .into_iter()
            .filter_map(|index| {
                let request = requests.get(index)?;
                Some(RequestEntry {
                    method: request.method.as_str().into(),
                    title: request.title().into(),
                    curl: curl_text(&draft_from_saved(service, request)).into(),
                    index: ui_index(index),
                })
            })
            .collect();
        ui.set_requests(ModelRc::new(VecModel::from(entries)));
        ui.set_active_request(ui_index(service.active_index()));
    }
}

/// Connects the sidebar's request list, the "+" buttons, Ctrl+N and the
/// service switcher to `services`.
pub fn wire(ui: &AppWindow, services: &Rc<Services>) {
    for on_add in [AppWindow::on_new_endpoint, AppWindow::on_new_request] {
        let ui_weak = ui.as_weak();
        let services = services.clone();
        on_add(ui, move || {
            if let Some(ui) = ui_weak.upgrade() {
                services.switch(&ui, |workspace| workspace.active_mut().add_endpoint());
            }
        });
    }
    ui.on_select_request({
        let ui_weak = ui.as_weak();
        let services = services.clone();
        move |index| {
            if let (Some(ui), Ok(index)) = (ui_weak.upgrade(), usize::try_from(index)) {
                services.switch(&ui, |workspace| workspace.active_mut().select(index));
            }
        }
    });
    ui.on_select_service({
        let ui_weak = ui.as_weak();
        let services = services.clone();
        move |index| {
            if let (Some(ui), Ok(index)) = (ui_weak.upgrade(), usize::try_from(index)) {
                services.switch(&ui, |workspace| workspace.select(index));
            }
        }
    });
    ui.on_create_service({
        let ui_weak = ui.as_weak();
        let services = services.clone();
        move |name| {
            let Some(ui) = ui_weak.upgrade() else {
                return SharedString::new();
            };
            match services.create(&ui, &name) {
                Ok(()) => SharedString::new(),
                Err(message) => message.into(),
            }
        }
    });
    ui.on_request_search_edited({
        let ui_weak = ui.as_weak();
        let services = services.clone();
        move |_| {
            if let Some(ui) = ui_weak.upgrade() {
                services.show_lists(&ui);
            }
        }
    });
}

/// Loads the workspace from the config directory.
///
/// Returns the workspace, the directory to save to (`None` disables saving, so
/// a file that couldn't be read is never overwritten), and a problem to report.
fn load_workspace() -> (Workspace, Option<PathBuf>, Option<String>) {
    let Some(dir) = store::config_dir() else {
        let problem = "Neither XDG_CONFIG_HOME nor HOME is set, so services won't be saved.";
        return (Workspace::default(), None, Some(problem.into()));
    };
    match store::load(&dir) {
        Ok(Loaded::Workspace(workspace)) => (workspace, Some(dir), None),
        Ok(Loaded::Damaged {
            workspace,
            kept_at,
            reason,
        }) => {
            let problem = format!(
                "The saved services could not be read ({reason}). The file was kept at {} and a new service was started.",
                kept_at.display()
            );
            (workspace, Some(dir), Some(problem))
        }
        Err(error) => {
            let problem = format!(
                "Could not load services from {}: {error}. Changes won't be saved this session.",
                dir.display()
            );
            (Workspace::default(), None, Some(problem))
        }
    }
}

/// Loads `request` into the form and its tables.
fn show_request(ui: &AppWindow, tables: &Tables, request: &SavedRequest) {
    ui.set_method(request.method.as_str().into());
    ui.set_endpoint(request.endpoint.as_str().into());
    ui.set_body_type(request.body_type);
    ui.set_body_source(request.body_source);
    ui.set_body(request.body.as_str().into());
    ui.set_body_file(request.body_file.as_str().into());
    ui.set_auth_type(request.auth_type);
    ui.set_auth_token_source(request.auth_token_source);
    ui.set_auth_token(request.auth_token.as_str().into());
    ui.set_auth_token_env(request.auth_token_env.as_str().into());
    ui.set_follow_redirects(request.follow_redirects);
    ui.set_max_time(request.max_time.as_str().into());
    for (table, rows) in [
        (RowTable::Params, &request.params),
        (RowTable::Headers, &request.headers),
        (RowTable::UrlencodedFields, &request.urlencoded_fields),
        (RowTable::MultipartParts, &request.multipart_parts),
    ] {
        tables.get(table).set_vec(table_rows(rows));
    }
}

/// Converts saved rows for a table, keeping one blank row to type into.
fn table_rows(rows: &[Row]) -> Vec<KeyValue> {
    if rows.is_empty() {
        return vec![KeyValue {
            enabled: true,
            ..KeyValue::default()
        }];
    }
    rows.iter()
        .map(|row| KeyValue {
            enabled: row.enabled,
            key: row.key.as_str().into(),
            value: row.value.as_str().into(),
        })
        .collect()
}

/// Rebuilds the form a saved request would show, so it can be rendered as curl
/// without loading it.
fn draft_from_saved(service: &Service, request: &SavedRequest) -> RequestDraft {
    let rows = |rows: &[Row]| ModelRc::new(VecModel::from(table_rows(rows)));
    RequestDraft {
        method: request.method.as_str().into(),
        scheme: service.scheme.as_str().into(),
        domain: service.domain.as_str().into(),
        version: service.version.as_str().into(),
        endpoint: request.endpoint.as_str().into(),
        params: rows(&request.params),
        headers: rows(&request.headers),
        body_type: request.body_type,
        body_source: request.body_source,
        body: request.body.as_str().into(),
        body_file: request.body_file.as_str().into(),
        urlencoded_fields: rows(&request.urlencoded_fields),
        multipart_parts: rows(&request.multipart_parts),
        auth_type: request.auth_type,
        auth_token_source: request.auth_token_source,
        auth_token: request.auth_token.as_str().into(),
        auth_token_env: request.auth_token_env.as_str().into(),
        follow_redirects: request.follow_redirects,
        max_time: request.max_time.as_str().into(),
    }
}

/// Copies the form into a [`SavedRequest`], keeping every row, even disabled
/// or empty ones, so the request comes back exactly as left.
fn saved_from_draft(draft: &RequestDraft) -> SavedRequest {
    let rows = |model: &ModelRc<KeyValue>| -> Vec<Row> {
        model
            .iter()
            .map(|row| Row {
                enabled: row.enabled,
                key: row.key.to_string(),
                value: row.value.to_string(),
            })
            .collect()
    };
    SavedRequest {
        method: draft.method.to_string(),
        endpoint: draft.endpoint.to_string(),
        params: rows(&draft.params),
        headers: rows(&draft.headers),
        body_type: draft.body_type,
        body_source: draft.body_source,
        body: draft.body.to_string(),
        body_file: draft.body_file.to_string(),
        urlencoded_fields: rows(&draft.urlencoded_fields),
        multipart_parts: rows(&draft.multipart_parts),
        auth_type: draft.auth_type,
        auth_token_source: draft.auth_token_source,
        auth_token: draft.auth_token.to_string(),
        auth_token_env: draft.auth_token_env.to_string(),
        follow_redirects: draft.follow_redirects,
        max_time: draft.max_time.to_string(),
    }
}

/// Converts a list position for Slint's `int`. Lists never get near `i32::MAX`;
/// if one did, `-1` would just leave nothing highlighted.
fn ui_index(index: usize) -> i32 {
    i32::try_from(index).unwrap_or(-1)
}
