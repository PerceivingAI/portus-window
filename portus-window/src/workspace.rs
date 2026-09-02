#[cfg(target_os = "linux")]
use parking_lot::Mutex;
use portus_window_protocol::{WorkspaceInfo, WorkspaceTarget};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub const MAX_WORKSPACES: u32 = 64;
pub const MAX_WORKSPACE_NAME_CHARS: usize = 128;
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace target '{0}' is invalid")]
    InvalidWorkspace(String),
    #[error("display is unavailable: {0}")]
    DisplayUnavailable(String),
    #[error("workspace operation failed: {0}")]
    Operation(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePlacement {
    pub index: Option<u32>,
    pub all: bool,
}

#[derive(Clone, Debug)]
pub struct WorkspaceCatalog {
    pub current: u32,
    pub workspaces: Vec<WorkspaceInfo>,
}

impl WorkspaceCatalog {
    pub fn resolve(&self, target: &WorkspaceTarget) -> Result<u32, WorkspaceError> {
        match target {
            WorkspaceTarget::Index { index } => {
                if *index >= MAX_WORKSPACES {
                    return Err(WorkspaceError::InvalidWorkspace(format!(
                        "workspace index {index} exceeds maximum {MAX_WORKSPACES}"
                    )));
                }
                if self.workspaces.iter().any(|ws| ws.index == *index) {
                    Ok(*index)
                } else {
                    Err(WorkspaceError::InvalidWorkspace(format!(
                        "workspace index {index} not found in catalog"
                    )))
                }
            }
            WorkspaceTarget::Name { name } => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > MAX_WORKSPACE_NAME_CHARS {
                    return Err(WorkspaceError::InvalidWorkspace(
                        "workspace name must be non-empty and bounded".to_string(),
                    ));
                }
                let matches: Vec<&WorkspaceInfo> = self
                    .workspaces
                    .iter()
                    .filter(|ws| ws.name.eq_ignore_ascii_case(name))
                    .collect();
                match matches.len() {
                    0 => Err(WorkspaceError::InvalidWorkspace(format!(
                        "workspace name '{name}' not found"
                    ))),
                    1 => Ok(matches[0].index),
                    _ => Err(WorkspaceError::InvalidWorkspace(format!(
                        "workspace name '{name}' matches multiple workspaces"
                    ))),
                }
            }
        }
    }
}

pub trait WorkspaceBackend: Send + Sync {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>, WorkspaceError>;
    fn current_workspace(&self) -> Result<u32, WorkspaceError>;
    fn move_to_workspace(
        &self,
        window_id: &str,
        target: &WorkspaceTarget,
    ) -> Result<(), WorkspaceError>;
}

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(not(target_os = "linux"))]
pub mod windows;

#[derive(Clone)]
pub struct WorkspaceService {
    #[cfg(target_os = "linux")]
    inner: Arc<linux::LinuxWorkspaceService>,
    #[cfg(not(target_os = "linux"))]
    inner: Arc<windows::WindowsWorkspaceService>,
}

impl WorkspaceService {
    pub fn connect() -> Result<Self, WorkspaceError> {
        #[cfg(target_os = "linux")]
        {
            Ok(Self {
                inner: Arc::new(linux::LinuxWorkspaceService::connect()?),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Self {
                inner: Arc::new(windows::WindowsWorkspaceService::connect()?),
            })
        }
    }

    pub fn list(&self) -> Result<Vec<WorkspaceInfo>, WorkspaceError> {
        self.inner.list()
    }

    pub fn catalog(&self) -> Result<WorkspaceCatalog, WorkspaceError> {
        self.inner.catalog()
    }

    pub fn query_window(
        &self,
        window_handle_id: usize,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        self.inner.query_window(window_handle_id)
    }

    pub fn watch_window(
        &self,
        window_session_id: String,
        window_handle_id: usize,
        callback: Arc<dyn Fn(WorkspacePlacement) + Send + Sync>,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        self.inner
            .watch_window(window_session_id, window_handle_id, callback)
    }

    pub fn unwatch_window(&self, window_session_id: &str) {
        self.inner.unwatch_window(window_session_id);
    }
    pub fn move_window_and_confirm(
        &self,
        window_session_id: &str,
        target: &WorkspaceTarget,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        self.inner
            .move_window_and_confirm(window_session_id, target)
    }

    pub fn move_window_to_index_and_confirm(
        &self,
        window_session_id: &str,
        index: u32,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        self.inner
            .move_window_to_index_and_confirm(window_session_id, index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_resolves_valid_index_and_name() {
        let catalog = WorkspaceCatalog {
            current: 0,
            workspaces: vec![
                WorkspaceInfo {
                    index: 0,
                    name: "Main".to_string(),
                    is_current: true,
                },
                WorkspaceInfo {
                    index: 1,
                    name: "Code".to_string(),
                    is_current: false,
                },
            ],
        };

        assert_eq!(
            catalog
                .resolve(&WorkspaceTarget::Index { index: 1 })
                .unwrap(),
            1
        );
        assert_eq!(
            catalog
                .resolve(&WorkspaceTarget::Name {
                    name: "code".to_string()
                })
                .unwrap(),
            1
        );
        assert!(catalog
            .resolve(&WorkspaceTarget::Index { index: 99 })
            .is_err());
        assert!(catalog
            .resolve(&WorkspaceTarget::Name {
                name: "Nonexistent".to_string()
            })
            .is_err());
    }
}
