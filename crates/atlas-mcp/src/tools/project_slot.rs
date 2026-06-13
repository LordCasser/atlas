use crate::tools::active_project::ActiveProject;

/// Wraps the optional active project so that only the outermost dispatch
/// layer (call_tool) needs to handle the None case. Handlers receive a
/// guaranteed `&mut ActiveProject`.
pub(crate) struct ProjectSlot {
    active: Option<ActiveProject>,
}

impl ProjectSlot {
    pub(crate) fn new(active: Option<ActiveProject>) -> Self {
        Self { active }
    }

    pub(crate) fn require_mut(&mut self) -> Result<&mut ActiveProject, String> {
        self.active
            .as_mut()
            .ok_or_else(|| "No active project. Call project(action=\"open\") first.".to_string())
    }

    pub(crate) fn require(&self) -> Result<&ActiveProject, String> {
        self.active
            .as_ref()
            .ok_or_else(|| "No active project. Call project(action=\"open\") first.".to_string())
    }

    #[allow(dead_code)]
    pub(crate) fn is_some(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn replace(&mut self, project: ActiveProject) {
        self.active = Some(project);
    }
}
