use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use crate::daemon::session::Session;

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<RwLock<HashMap<Uuid, Session>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn insert(&self, session: Session) {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.id, session);
    }

    /// Read-only access to a session via callback (avoids holding lock across await).
    pub fn with<F, R>(&self, id: &Uuid, f: F) -> Option<R>
    where
        F: FnOnce(&Session) -> R,
    {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(f)
    }

    /// Mutable access to a session via callback.
    pub fn with_mut<F, R>(&self, id: &Uuid, f: F) -> Option<R>
    where
        F: FnOnce(&mut Session) -> R,
    {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(id)
            .map(f)
    }

    /// Returns Some(id) if session exists, None otherwise.
    #[allow(dead_code)]
    pub fn get(&self, id: &Uuid) -> Option<Uuid> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|s| s.id)
    }

    /// Returns ids of all sessions.
    pub fn list(&self) -> Vec<Uuid> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::session::Session;
    use tokio::sync::{broadcast, mpsc};

    fn make_session(id: Uuid) -> Session {
        let (cmd_tx, _) = mpsc::channel(8);
        let (log_tx, _) = broadcast::channel(16);
        Session::new(
            id,
            vec!["echo".into()],
            "/tmp".into(),
            Default::default(),
            vec![],
            cmd_tx,
            log_tx,
        )
    }

    #[test]
    fn insert_and_get() {
        let mgr = SessionManager::new();
        let id = Uuid::new_v4();
        mgr.insert(make_session(id));
        assert!(mgr.get(&id).is_some());
    }

    #[test]
    fn list_returns_all() {
        let mgr = SessionManager::new();
        mgr.insert(make_session(Uuid::new_v4()));
        mgr.insert(make_session(Uuid::new_v4()));
        assert_eq!(mgr.list().len(), 2);
    }

    #[test]
    fn get_missing_returns_none() {
        let mgr = SessionManager::new();
        assert!(mgr.get(&Uuid::new_v4()).is_none());
    }
}
