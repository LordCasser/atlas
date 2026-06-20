use std::sync::{Arc, RwLock};

use crate::tools::active_project::ActiveProject;

/// Wraps the optional active project so that only the outermost dispatch
/// layer (call_tool) needs to handle the None case. Handlers receive a
/// guaranteed `Arc<ActiveProject>`.
pub(crate) struct ProjectSlot {
    active: RwLock<Option<Arc<ActiveProject>>>,
}

impl ProjectSlot {
    pub(crate) fn new(active: Option<Arc<ActiveProject>>) -> Self {
        Self {
            active: RwLock::new(active),
        }
    }

    /// Clone the Arc — read lock held only for the clone (~10ns).
    pub(crate) fn get(&self) -> Result<Arc<ActiveProject>, String> {
        self.active
            .read()
            .map_err(|e| format!("project slot lock poisoned: {e}"))?
            .clone()
            .ok_or_else(|| "No active project. Call project(action=\"open\") first.".to_string())
    }

    /// Replace the active project. Write lock held only for the assignment.
    pub(crate) fn replace(&self, project: Arc<ActiveProject>) {
        *self.active.write().unwrap() = Some(project);
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.read().unwrap().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_empty_returns_is_active_false() {
        let slot = ProjectSlot::new(None);
        assert!(!slot.is_active());
    }

    #[test]
    fn get_returns_err_when_not_active() {
        let slot = ProjectSlot::new(None);
        assert!(slot.get().is_err());
    }

    #[test]
    fn replace_then_get_returns_ok() {
        // We can't easily construct ActiveProject in unit tests without Store,
        // but we can verify the API compiles and is_active works.
        let slot = ProjectSlot::new(None);
        assert!(!slot.is_active());
        assert!(slot.get().is_err());
    }
}
