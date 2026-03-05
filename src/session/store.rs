use color_eyre::eyre::Context;
use color_eyre::Result;
use std::path::PathBuf;

use crate::config::Config;
use super::instance::Session;

/// Persistent store for sessions.
///
/// Serialised as JSON in `~/.agentick/sessions.json`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct SessionStore {
    pub sessions: Vec<Session>,
}

impl SessionStore {
    /// Path to the on-disk store file.
    fn store_path() -> PathBuf {
        Config::data_dir().join("sessions.json")
    }

    /// Load the store from disk, or return an empty store if the file does not
    /// exist yet.
    pub fn load() -> Result<Self> {
        let path = Self::store_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let store: SessionStore = serde_json::from_str(&content)
            .wrap_err("failed to parse sessions.json")?;
        Ok(store)
    }

    /// Persist the store to disk (creates the data directory if needed).
    pub fn save(&self) -> Result<()> {
        let path = Self::store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)
            .wrap_err("failed to serialise sessions")?;
        std::fs::write(&path, json)
            .wrap_err_with(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    /// Add a session to the store (does not persist -- call `save()` after).
    pub fn add_session(&mut self, session: Session) {
        self.sessions.push(session);
    }

    /// Remove a session by id. Returns the removed session if found.
    pub fn remove_session(&mut self, id: &str) -> Option<Session> {
        if let Some(pos) = self.sessions.iter().position(|s| s.id == id) {
            Some(self.sessions.remove(pos))
        } else {
            None
        }
    }

    /// Find a session by id.
    pub fn find_session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// Find a session by id (mutable).
    pub fn find_session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::instance::{Session, Tool};
    use std::path::PathBuf;

    fn make_session(title: &str, tool: Tool) -> Session {
        Session::new(title.into(), PathBuf::from("/tmp/test"), tool)
    }

    #[test]
    fn default_store_is_empty() {
        let store = SessionStore::default();
        assert!(store.sessions.is_empty());
    }

    #[test]
    fn add_session_appends() {
        let mut store = SessionStore::default();
        let s = make_session("one", Tool::Claude);
        let id = s.id.clone();
        store.add_session(s);
        assert_eq!(store.sessions.len(), 1);
        assert_eq!(store.sessions[0].id, id);
    }

    #[test]
    fn remove_session_existing() {
        let mut store = SessionStore::default();
        let s = make_session("one", Tool::Claude);
        let id = s.id.clone();
        store.add_session(s);
        let removed = store.remove_session(&id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, id);
        assert!(store.sessions.is_empty());
    }

    #[test]
    fn remove_session_missing_returns_none() {
        let mut store = SessionStore::default();
        store.add_session(make_session("one", Tool::Claude));
        assert!(store.remove_session("nonexistent").is_none());
        assert_eq!(store.sessions.len(), 1);
    }

    #[test]
    fn find_session_by_id() {
        let mut store = SessionStore::default();
        let s = make_session("target", Tool::Gemini);
        let id = s.id.clone();
        store.add_session(make_session("other", Tool::Claude));
        store.add_session(s);

        let found = store.find_session(&id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "target");
    }

    #[test]
    fn find_session_missing_returns_none() {
        let store = SessionStore::default();
        assert!(store.find_session("nope").is_none());
    }

    #[test]
    fn find_session_mut_modifies() {
        let mut store = SessionStore::default();
        let s = make_session("mutable", Tool::Codex);
        let id = s.id.clone();
        store.add_session(s);

        let found = store.find_session_mut(&id).unwrap();
        found.title = "modified".into();
        assert_eq!(store.sessions[0].title, "modified");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("sessions.json");

        let mut store = SessionStore::default();
        store.add_session(make_session("alpha", Tool::Claude));
        store.add_session(make_session("beta", Tool::Gemini));

        // Save manually to the temp path.
        let json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(&store_path, &json).unwrap();

        // Load back.
        let content = std::fs::read_to_string(&store_path).unwrap();
        let loaded: SessionStore = serde_json::from_str(&content).unwrap();

        assert_eq!(loaded.sessions.len(), 2);
        assert_eq!(loaded.sessions[0].title, "alpha");
        assert_eq!(loaded.sessions[1].title, "beta");
        // Status is serde(skip) — should default to Idle.
        assert_eq!(loaded.sessions[0].status, crate::session::instance::Status::Idle);
    }
}
