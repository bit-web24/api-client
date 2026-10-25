//! Services and their saved requests: what the sidebar lists and what is kept on
//! disk. Pure data and operations; no UI types and no IO.

use serde::{Deserialize, Deserializer, Serialize};

/// `--max-time` a new request starts with, in seconds.
pub const DEFAULT_MAX_TIME_SECS: u64 = 30;

/// Scheme a new service starts with.
pub const DEFAULT_SCHEME: &str = "https://";

/// One row of a key/value table (params, headers, form fields).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    pub enabled: bool,
    pub key: String,
    pub value: String,
}

impl Row {
    /// Returns the empty, enabled row tables start with, so there is a row to type into.
    pub fn blank() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

/// A request as the user composed it, minus the base URL its service provides.
/// Field meanings follow the UI's `RequestDraft`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedRequest {
    pub method: String,
    pub endpoint: String,
    pub params: Vec<Row>,
    pub headers: Vec<Row>,
    pub body_type: i32,
    pub body_source: i32,
    pub body: String,
    pub body_file: String,
    pub urlencoded_fields: Vec<Row>,
    pub multipart_parts: Vec<Row>,
    pub auth_type: i32,
    pub auth_token_source: i32,
    pub auth_token: String,
    pub auth_token_env: String,
    pub follow_redirects: bool,
    pub max_time: String,
}

impl Default for SavedRequest {
    fn default() -> Self {
        Self {
            method: "GET".into(),
            endpoint: String::new(),
            params: vec![Row::blank()],
            headers: vec![Row::blank()],
            body_type: 0,
            body_source: 0,
            body: String::new(),
            body_file: String::new(),
            urlencoded_fields: vec![Row::blank()],
            multipart_parts: vec![Row::blank()],
            auth_type: 0,
            auth_token_source: 0,
            auth_token: String::new(),
            auth_token_env: String::new(),
            follow_redirects: true,
            max_time: DEFAULT_MAX_TIME_SECS.to_string(),
        }
    }
}

impl SavedRequest {
    /// Returns a new request for another endpoint of the same service: it keeps
    /// what usually stays the same across endpoints (method, headers, auth,
    /// options) and clears what belongs to one endpoint (path, params, body).
    pub fn next_endpoint(&self) -> Self {
        Self {
            method: self.method.clone(),
            headers: self.headers.clone(),
            auth_type: self.auth_type,
            auth_token_source: self.auth_token_source,
            auth_token: self.auth_token.clone(),
            auth_token_env: self.auth_token_env.clone(),
            follow_redirects: self.follow_redirects,
            max_time: self.max_time.clone(),
            ..Self::default()
        }
    }

    /// Returns how the sidebar names this request: its endpoint path.
    pub fn title(&self) -> String {
        let endpoint = self.endpoint.trim().trim_start_matches('/');
        format!("/{endpoint}")
    }
}

/// An API: a base URL shared by many requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Service {
    pub name: String,
    pub scheme: String,
    pub domain: String,
    pub version: String,
    /// Never empty, so there is always a request to show; private so only this
    /// module can change it, and data loaded from disk is fixed by `repair`.
    requests: Vec<SavedRequest>,
    /// Always a valid index into `requests`.
    active_request: usize,
}

impl Default for Service {
    fn default() -> Self {
        Self::new("My service")
    }
}

impl Service {
    /// Creates a service with no base URL yet and one blank request.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            scheme: DEFAULT_SCHEME.into(),
            domain: String::new(),
            version: String::new(),
            requests: vec![SavedRequest::default()],
            active_request: 0,
        }
    }

    /// Returns the base URL as shown under the service name, e.g. `https://api.io/v1`.
    pub fn base_url(&self) -> String {
        let domain = self.domain.trim().trim_end_matches('/');
        if domain.is_empty() {
            return "no base URL yet".into();
        }
        let version = self.version.trim().trim_matches('/');
        if version.is_empty() {
            format!("{}{domain}", self.scheme)
        } else {
            format!("{}{domain}/{version}", self.scheme)
        }
    }

    /// Returns all requests, in the order they were created.
    pub fn requests(&self) -> &[SavedRequest] {
        &self.requests
    }

    /// Returns the index of the request being edited.
    pub fn active_index(&self) -> usize {
        self.active_request
    }

    /// Returns the request being edited.
    pub fn active(&self) -> &SavedRequest {
        self.requests
            .get(self.active_request)
            .expect("invariant: active_request indexes the non-empty requests")
    }

    /// Returns the request being edited, for updating.
    pub fn active_mut(&mut self) -> &mut SavedRequest {
        let index = self.active_request;
        self.requests
            .get_mut(index)
            .expect("invariant: active_request indexes the non-empty requests")
    }

    /// Adds a request for another endpoint (see [`SavedRequest::next_endpoint`]) and makes it active.
    pub fn add_endpoint(&mut self) {
        let next = self.active().next_endpoint();
        self.requests.push(next);
        self.active_request = self.requests.len() - 1;
    }

    /// Makes request `index` active; out-of-range indices are ignored.
    pub fn select(&mut self, index: usize) {
        if index < self.requests.len() {
            self.active_request = index;
        }
    }

    /// Restores the invariants after loading data that may have been edited by hand.
    fn repair(&mut self) {
        if self.requests.is_empty() {
            self.requests.push(SavedRequest::default());
        }
        if self.active_request >= self.requests.len() {
            self.active_request = 0;
        }
    }
}

/// The colour theme the app uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Linux,
    Yaru,
}

impl<'de> Deserialize<'de> for ThemeChoice {
    /// Reads a theme name; an unknown one (say, from a newer version) falls back
    /// to the default instead of making the whole file unreadable.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "yaru" => Self::Yaru,
            _ => Self::Linux,
        })
    }
}

/// Everything the app keeps on disk: the services and the chosen theme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    /// Never empty; private for the same reason as [`Service`]'s requests.
    services: Vec<Service>,
    /// Always a valid index into `services`.
    active_service: usize,
    pub theme: ThemeChoice,
}

impl Default for Workspace {
    /// Returns a workspace with one blank service, for a first start.
    fn default() -> Self {
        Self {
            services: vec![Service::default()],
            active_service: 0,
            theme: ThemeChoice::default(),
        }
    }
}

impl Workspace {
    /// Parses a saved workspace, fixing anything hand-editing may have broken.
    ///
    /// # Errors
    ///
    /// Returns the parse error when `json` is not a saved workspace.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let mut workspace: Self = serde_json::from_str(json)?;
        workspace.repair();
        Ok(workspace)
    }

    /// Serialises the workspace for saving.
    ///
    /// # Errors
    ///
    /// Returns the serialisation error; with these plain types it does not happen in practice.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Returns all services, in the order they were created.
    pub fn services(&self) -> &[Service] {
        &self.services
    }

    /// Returns the index of the service being worked on.
    pub fn active_index(&self) -> usize {
        self.active_service
    }

    /// Restores the invariants after loading: at least one service, every
    /// service with at least one request, and active indices in range.
    fn repair(&mut self) {
        if self.services.is_empty() {
            self.services.push(Service::default());
        }
        if self.active_service >= self.services.len() {
            self.active_service = 0;
        }
        self.services.iter_mut().for_each(Service::repair);
    }

    /// Returns the service being worked on.
    pub fn active(&self) -> &Service {
        self.services
            .get(self.active_service)
            .expect("invariant: active_service indexes the non-empty services")
    }

    /// Returns the service being worked on, for updating.
    pub fn active_mut(&mut self) -> &mut Service {
        let index = self.active_service;
        self.services
            .get_mut(index)
            .expect("invariant: active_service indexes the non-empty services")
    }

    /// Makes service `index` active; out-of-range indices are ignored.
    pub fn select(&mut self, index: usize) {
        if index < self.services.len() {
            self.active_service = index;
        }
    }

    /// Creates a service named `name` (trimmed) and makes it active.
    ///
    /// # Errors
    ///
    /// Returns a message when the name is empty or another service already has it.
    pub fn create(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Give the service a name.".into());
        }
        if self
            .services
            .iter()
            .any(|service| service.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("A service named \"{name}\" already exists."));
        }
        self.services.push(Service::new(name));
        self.active_service = self.services.len() - 1;
        Ok(())
    }
}

/// Returns the indices of `requests` whose method or path contains `query`
/// (case-insensitive), in order; an empty query matches everything.
pub fn matching_requests(requests: &[SavedRequest], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    requests
        .iter()
        .enumerate()
        .filter(|(_, request)| {
            query.is_empty()
                || request.title().to_lowercase().contains(&query)
                || request.method.to_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, endpoint: &str) -> SavedRequest {
        SavedRequest {
            method: method.into(),
            endpoint: endpoint.into(),
            ..SavedRequest::default()
        }
    }

    #[test]
    fn next_endpoint_keeps_shared_settings_and_clears_the_rest() {
        let current = SavedRequest {
            method: "POST".into(),
            endpoint: "users".into(),
            params: vec![Row {
                enabled: true,
                key: "page".into(),
                value: "2".into(),
            }],
            headers: vec![Row {
                enabled: true,
                key: "X-Id".into(),
                value: "7".into(),
            }],
            body_type: 1,
            body: "{}".into(),
            auth_type: 1,
            auth_token_env: "API_TOKEN".into(),
            max_time: "5".into(),
            ..SavedRequest::default()
        };
        let next = current.next_endpoint();
        assert_eq!(next.method, "POST");
        assert_eq!(next.headers, current.headers);
        assert_eq!(
            (next.auth_type, next.auth_token_env.as_str()),
            (1, "API_TOKEN")
        );
        assert_eq!(next.max_time, "5");
        assert_eq!(next.endpoint, "");
        assert_eq!(next.params, vec![Row::blank()]);
        assert_eq!((next.body_type, next.body.as_str()), (0, ""));
    }

    #[test]
    fn add_endpoint_appends_and_activates() {
        let mut service = Service::new("Api");
        service.add_endpoint();
        assert_eq!(service.requests.len(), 2);
        assert_eq!(service.active_request, 1);
        service.select(0);
        assert_eq!(service.active_request, 0);
        service.select(9);
        assert_eq!(service.active_request, 0);
    }

    #[test]
    fn base_url_joins_parts_or_says_it_is_missing() {
        let mut service = Service::new("Api");
        assert_eq!(service.base_url(), "no base URL yet");
        service.domain = "api.io/".into();
        assert_eq!(service.base_url(), "https://api.io");
        service.version = "/v1/".into();
        assert_eq!(service.base_url(), "https://api.io/v1");
    }

    #[test]
    fn titles_are_endpoint_paths() {
        assert_eq!(request("GET", "users/42").title(), "/users/42");
        assert_eq!(request("GET", " /users ").title(), "/users");
        assert_eq!(request("GET", "").title(), "/");
    }

    #[test]
    fn create_rejects_empty_and_duplicate_names() {
        let mut workspace = Workspace::default();
        assert!(workspace.create("  ").is_err());
        assert_eq!(workspace.create(" Billing "), Ok(()));
        assert_eq!(workspace.active().name, "Billing");
        assert_eq!(workspace.active_index(), 1);
        assert_eq!(
            workspace.create("billing"),
            Err("A service named \"billing\" already exists.".into())
        );
    }

    #[test]
    fn loading_repairs_hand_edited_data() {
        let json =
            r#"{"services":[{"name":"Api","requests":[],"active_request":5}],"active_service":3}"#;
        let workspace = Workspace::from_json(json).unwrap();
        assert_eq!(workspace.active_index(), 0);
        assert_eq!(workspace.active().requests().len(), 1);
        assert_eq!(workspace.active().active_index(), 0);

        let empty = Workspace::from_json(r#"{"services":[]}"#).unwrap();
        assert_eq!(empty.services().len(), 1);
    }

    #[test]
    fn workspace_survives_a_save_and_load() {
        let mut workspace = Workspace::default();
        workspace.create("Billing").unwrap();
        workspace.active_mut().domain = "billing.io".into();
        workspace.active_mut().add_endpoint();
        workspace.active_mut().active_mut().endpoint = "invoices".into();
        let loaded = Workspace::from_json(&workspace.to_json().unwrap()).unwrap();
        assert_eq!(loaded, workspace);
    }

    #[test]
    fn search_matches_path_or_method() {
        let requests = [
            request("GET", "users"),
            request("POST", "orders"),
            request("DELETE", "users/1"),
        ];
        assert_eq!(matching_requests(&requests, ""), [0, 1, 2]);
        assert_eq!(matching_requests(&requests, "USERS"), [0, 2]);
        assert_eq!(matching_requests(&requests, "post"), [1]);
        assert_eq!(matching_requests(&requests, "nothing"), Vec::<usize>::new());
    }

    #[test]
    fn theme_defaults_to_linux_and_survives_unknown_names() {
        assert_eq!(Workspace::default().theme, ThemeChoice::Linux);
        let yaru = Workspace::from_json(r#"{"theme":"yaru"}"#).unwrap();
        assert_eq!(yaru.theme, ThemeChoice::Yaru);
        let unknown = Workspace::from_json(r#"{"theme":"solarized"}"#).unwrap();
        assert_eq!(unknown.theme, ThemeChoice::Linux);
        assert!(yaru.to_json().unwrap().contains(r#""theme": "yaru""#));
    }

    #[test]
    fn missing_fields_in_saved_json_fall_back_to_defaults() {
        let request: SavedRequest = serde_json::from_str(r#"{"endpoint":"users"}"#).unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.max_time, "30");
        assert!(request.follow_redirects);
    }
}
