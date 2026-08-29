use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Exact session-id lookup. Never a full-table scan.
const SESSION_BY_ID: &str = "SELECT id FROM session WHERE id = ?1 LIMIT 1";
/// Bounded same-session providerID lookup. Not a spend scan.
const MESSAGE_DATA_FOR_SESSION: &str =
    "SELECT data FROM message WHERE session_id = ?1 ORDER BY time_created DESC LIMIT 8";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Api { has_secret: bool },
    Oauth,
    WellKnown { has_secret: bool },
}

impl CredentialKind {
    fn has_secret(self) -> bool {
        match self {
            Self::Api { has_secret } | Self::WellKnown { has_secret } => has_secret,
            Self::Oauth => true,
        }
    }

    fn is_api_like(self) -> bool {
        matches!(self, Self::Api { .. } | Self::WellKnown { .. })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthMap {
    entries: BTreeMap<String, CredentialKind>,
}

impl AuthMap {
    pub fn get(&self, provider_id: &str) -> Option<CredentialKind> {
        self.entries.get(&provider_id.to_ascii_lowercase()).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthReadError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvidence {
    pub session_id: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLookup {
    Found(SessionEvidence),
    Missing,
    Unreadable,
}

#[derive(Debug, Clone)]
pub struct OpenCodePaths {
    pub auth: PathBuf,
    pub db: PathBuf,
}

impl OpenCodePaths {
    pub fn from_env() -> Option<Self> {
        let dir = opencode_data_dir()?;
        Some(Self {
            auth: dir.join("auth.json"),
            db: dir.join("opencode.db"),
        })
    }

    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            auth: dir.join("auth.json"),
            db: dir.join("opencode.db"),
        }
    }
}

fn opencode_data_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(xdg).join("opencode"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/share/opencode"))
}

pub fn env_go_key_present() -> bool {
    std::env::var_os("OPENCODE_API_KEY").is_some_and(|value| !value.is_empty())
}

pub fn read_auth(paths: &OpenCodePaths) -> Result<AuthMap, AuthReadError> {
    read_auth_file(&paths.auth)
}

pub fn lookup_session(paths: &OpenCodePaths, session_id: &str) -> SessionLookup {
    lookup_session_db(&paths.db, session_id)
}

fn read_auth_file(path: &Path) -> Result<AuthMap, AuthReadError> {
    let bytes = fs::read(path).map_err(|_| AuthReadError)?;
    parse_auth_json(&bytes)
}

pub fn parse_auth_json(bytes: &[u8]) -> Result<AuthMap, AuthReadError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| AuthReadError)?;
    let object = value.as_object().ok_or(AuthReadError)?;
    let mut entries = BTreeMap::new();
    for (provider_id, entry) in object {
        let Some(kind) = credential_kind(entry) else {
            continue;
        };
        entries.insert(provider_id.to_ascii_lowercase(), kind);
    }
    Ok(AuthMap { entries })
}

fn credential_kind(entry: &Value) -> Option<CredentialKind> {
    let object = entry.as_object()?;
    let kind = object.get("type").and_then(Value::as_str)?;
    match kind {
        "api" => Some(CredentialKind::Api {
            has_secret: non_empty_secret(object.get("key")),
        }),
        "wellknown" => Some(CredentialKind::WellKnown {
            has_secret: non_empty_secret(object.get("token")),
        }),
        "oauth" => Some(CredentialKind::Oauth),
        _ => None,
    }
}

fn non_empty_secret(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|secret| !secret.is_empty())
}

fn lookup_session_db(path: &Path, session_id: &str) -> SessionLookup {
    if session_id.is_empty() {
        return SessionLookup::Missing;
    }
    let Ok(connection) = open_readonly(path) else {
        return SessionLookup::Unreadable;
    };
    match session_exists(&connection, session_id) {
        Ok(false) => SessionLookup::Missing,
        Err(_) => SessionLookup::Unreadable,
        Ok(true) => match session_provider(&connection, session_id) {
            Ok((provider_id, model_id)) => SessionLookup::Found(SessionEvidence {
                session_id: session_id.to_string(),
                provider_id,
                model_id,
            }),
            Err(_) => SessionLookup::Unreadable,
        },
    }
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn session_exists(connection: &Connection, session_id: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare(SESSION_BY_ID)?;
    let mut rows = statement.query([session_id])?;
    Ok(rows.next()?.is_some())
}

fn session_provider(
    connection: &Connection,
    session_id: &str,
) -> rusqlite::Result<(Option<String>, Option<String>)> {
    let mut statement = connection.prepare(MESSAGE_DATA_FOR_SESSION)?;
    let mut rows = statement.query([session_id])?;
    while let Some(row) = rows.next()? {
        let data: String = row.get(0)?;
        if let Some((provider_id, model_id)) = provider_from_message_data(&data) {
            return Ok((Some(provider_id), model_id));
        }
    }
    Ok((None, None))
}

fn provider_from_message_data(data: &str) -> Option<(String, Option<String>)> {
    let value: Value = serde_json::from_str(data).ok()?;
    let provider_id = string_field(&value, "providerID")
        .or_else(|| {
            value
                .get("model")
                .and_then(|model| string_field(model, "providerID"))
        })?
        .trim()
        .to_string();
    if provider_id.is_empty() {
        return None;
    }
    let model_id = string_field(&value, "modelID").or_else(|| {
        value
            .get("model")
            .and_then(|model| string_field(model, "modelID"))
    });
    Some((provider_id, model_id))
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The only OpenCode subscription route. Upstream writes exactly this id for
/// the Go plan; OpenCode Zen (`opencode`) is pay-per-token and is not one.
fn is_approved_go_provider(provider_id: &str) -> bool {
    provider_id.trim().eq_ignore_ascii_case("opencode-go")
}

fn go_credential_approved(kind: CredentialKind, env_go_key_present: bool) -> bool {
    match kind {
        CredentialKind::Api { has_secret } | CredentialKind::WellKnown { has_secret } => {
            has_secret || env_go_key_present
        }
        CredentialKind::Oauth => false,
    }
}

pub fn classify_opencode(
    lookup: SessionLookup,
    auth: Result<&AuthMap, AuthReadError>,
    env_go_key_present: bool,
) -> crate::model::Resolution {
    use crate::model::{BillingTarget, Resolution};

    let Ok(auth) = auth else {
        return Resolution::Indeterminate;
    };
    let SessionLookup::Found(session) = lookup else {
        return Resolution::Indeterminate;
    };
    let Some(provider_id) = session.provider_id.as_deref() else {
        return Resolution::Indeterminate;
    };
    let credential = auth.get(provider_id);

    if is_approved_go_provider(provider_id) {
        let approved = match credential {
            Some(kind) => go_credential_approved(kind, env_go_key_present),
            None => env_go_key_present,
        };
        return if approved {
            Resolution::Subscription(BillingTarget::opencode_go())
        } else {
            Resolution::Indeterminate
        };
    }

    // For any other backend, an API-style key filed in OpenCode's own auth.json
    // under that exact provider id is proof the session pays per token, or
    // through a plan this plugin cannot read. Either way it owns no quota here,
    // so stale quota is cleared once. OAuth logins, missing credentials, and
    // unrecognised entry shapes stay Indeterminate and keep prior metadata.
    match credential {
        Some(kind) if kind.is_api_like() && kind.has_secret() => Resolution::NoSubscription,
        _ => Resolution::Indeterminate,
    }
}

#[cfg(test)]
pub(crate) fn write_fixture_db(path: &Path, rows: &[(&str, &str)]) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL DEFAULT 'proj',
            slug TEXT NOT NULL DEFAULT 's',
            directory TEXT NOT NULL DEFAULT '',
            title TEXT NOT NULL DEFAULT 't',
            version TEXT NOT NULL DEFAULT '1',
            time_created INTEGER NOT NULL DEFAULT 1,
            time_updated INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL
        );",
    )?;
    for (index, (session_id, data)) in rows.iter().enumerate() {
        connection.execute(
            "INSERT INTO session (id) VALUES (?1)
             ON CONFLICT(id) DO NOTHING",
            [*session_id],
        )?;
        connection.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![format!("msg_{index}"), *session_id, index as i64 + 1, *data],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn exact_go_session_reads_provider_from_bounded_message_lookup() {
        let directory = tempdir().unwrap();
        let db = directory.path().join("opencode.db");
        write_fixture_db(
            &db,
            &[(
                "ses_go",
                r#"{"role":"assistant","providerID":"opencode-go","modelID":"kimi-k2.5"}"#,
            )],
        )
        .unwrap();
        let paths = OpenCodePaths {
            auth: directory.path().join("auth.json"),
            db,
        };
        match lookup_session(&paths, "ses_go") {
            SessionLookup::Found(session) => {
                assert_eq!(session.provider_id.as_deref(), Some("opencode-go"));
                assert_eq!(session.model_id.as_deref(), Some("kimi-k2.5"));
            }
            other => panic!("expected found session, got {other:?}"),
        }
    }

    #[test]
    fn missing_session_is_missing_even_when_auth_has_one_key() {
        let directory = tempdir().unwrap();
        let db = directory.path().join("opencode.db");
        write_fixture_db(
            &db,
            &[(
                "ses_go",
                r#"{"role":"assistant","providerID":"opencode-go","modelID":"kimi-k2.5"}"#,
            )],
        )
        .unwrap();
        let paths = OpenCodePaths::from_dir(directory.path());
        assert_eq!(lookup_session(&paths, "ses_absent"), SessionLookup::Missing);
    }

    #[test]
    fn malformed_database_is_unreadable() {
        let directory = tempdir().unwrap();
        let db = directory.path().join("opencode.db");
        fs::write(&db, b"this is not a sqlite database").unwrap();
        let paths = OpenCodePaths::from_dir(directory.path());
        assert_eq!(lookup_session(&paths, "ses_go"), SessionLookup::Unreadable);
    }

    #[test]
    fn malformed_auth_is_an_error() {
        assert!(parse_auth_json(b"{not json").is_err());
        assert!(parse_auth_json(b"[1]").is_err());
    }

    #[test]
    fn auth_parser_records_kind_without_keeping_secrets() {
        let auth = parse_auth_json(
            br#"{
                "opencode-go": {"type":"api","key":"placeholder"},
                "anthropic": {"type":"api","key":"placeholder"}
            }"#,
        )
        .unwrap();
        assert_eq!(
            auth.get("opencode-go"),
            Some(CredentialKind::Api { has_secret: true })
        );
        assert_eq!(
            format!("{:?}", auth.get("opencode-go")),
            "Some(Api { has_secret: true })"
        );
        assert!(!format!("{auth:?}").contains("placeholder"));
    }

    #[test]
    fn user_message_model_object_is_accepted() {
        let directory = tempdir().unwrap();
        let db = directory.path().join("opencode.db");
        write_fixture_db(
            &db,
            &[(
                "ses_go",
                r#"{"role":"user","model":{"providerID":"opencode-go","modelID":"glm-5.2"}}"#,
            )],
        )
        .unwrap();
        match lookup_session_db(&db, "ses_go") {
            SessionLookup::Found(session) => {
                assert_eq!(session.provider_id.as_deref(), Some("opencode-go"));
            }
            other => panic!("expected found, got {other:?}"),
        }
    }

    #[test]
    fn database_opens_under_a_path_containing_uri_punctuation() {
        let directory = tempdir().unwrap();
        let store = directory.path().join("we?ird#dir");
        fs::create_dir_all(&store).unwrap();
        write_fixture_db(
            &store.join("opencode.db"),
            &[(
                "ses_go",
                r#"{"role":"assistant","providerID":"opencode-go","modelID":"kimi-k3"}"#,
            )],
        )
        .unwrap();
        let paths = OpenCodePaths::from_dir(&store);
        assert!(matches!(
            lookup_session(&paths, "ses_go"),
            SessionLookup::Found(_)
        ));
    }

    #[test]
    fn queries_are_exact_session_lookups() {
        assert!(SESSION_BY_ID.contains("WHERE id = ?1"));
        assert!(!SESSION_BY_ID.to_ascii_lowercase().contains("scan"));
        assert!(MESSAGE_DATA_FOR_SESSION.contains("WHERE session_id = ?1"));
        assert!(MESSAGE_DATA_FOR_SESSION.contains("LIMIT 8"));
        assert!(!MESSAGE_DATA_FOR_SESSION.contains("SUM("));
        assert!(!MESSAGE_DATA_FOR_SESSION.contains("cost"));
    }
}
