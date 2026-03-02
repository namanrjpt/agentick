use color_eyre::eyre::Context;
use color_eyre::Result;
use std::path::PathBuf;

use crate::config::Config;
use super::instance::{Group, Session};

/// Persistent store for sessions and groups.
///
/// Serialised as JSON in `~/.agentick/sessions.json`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct SessionStore {
    pub sessions: Vec<Session>,
    pub groups: Vec<Group>,
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

    /// Add a group if one with the same name does not already exist.
    pub fn add_group(&mut self, group: Group) {
        if !self.groups.iter().any(|g| g.name == group.name) {
            self.groups.push(group);
        }
    }

    /// Toggle a group's expanded/collapsed state. Returns the new state.
    pub fn toggle_group(&mut self, name: &str) -> Option<bool> {
        if let Some(g) = self.groups.iter_mut().find(|g| g.name == name) {
            g.expanded = !g.expanded;
            Some(g.expanded)
        } else {
            None
        }
    }
}
