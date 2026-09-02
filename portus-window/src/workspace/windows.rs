use super::*;
use ::windows::Win32::Foundation::HWND;
use parking_lot::{Mutex, RwLock};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone)]
struct WindowsWatcher {
    handle: usize,
    placement: Arc<Mutex<WorkspacePlacement>>,
    callback: Arc<dyn Fn(WorkspacePlacement) + Send + Sync>,
}

struct WindowsWorkspaceState {
    watchers: RwLock<BTreeMap<String, WindowsWatcher>>,
    stop: AtomicBool,
    poller: Mutex<Option<JoinHandle<()>>>,
}

impl WindowsWorkspaceState {
    fn new() -> Arc<Self> {
        let state = Arc::new(Self {
            watchers: RwLock::new(BTreeMap::new()),
            stop: AtomicBool::new(false),
            poller: Mutex::new(None),
        });
        let poll_state = Arc::clone(&state);
        let poller = thread::spawn(move || {
            while !poll_state.stop.load(Ordering::Acquire) {
                let watchers: Vec<(String, WindowsWatcher)> = poll_state
                    .watchers
                    .read()
                    .iter()
                    .map(|(id, watcher)| (id.clone(), watcher.clone()))
                    .collect();
                for (window_session_id, watcher) in watchers {
                    let Ok(placement) = query_native_window(watcher.handle) else {
                        continue;
                    };
                    let changed = {
                        let mut current = watcher.placement.lock();
                        if *current == placement {
                            false
                        } else {
                            *current = placement.clone();
                            true
                        }
                    };
                    if changed {
                        let still_registered = poll_state
                            .watchers
                            .read()
                            .get(&window_session_id)
                            .map(|current| current.handle == watcher.handle)
                            .unwrap_or(false);
                        if still_registered {
                            (watcher.callback)(placement);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(250));
            }
        });
        *state.poller.lock() = Some(poller);
        state
    }
}

impl Drop for WindowsWorkspaceState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(poller) = self.poller.get_mut().take() {
            let _ = poller.join();
        }
    }
}

pub struct WindowsWorkspaceService {
    state: Arc<WindowsWorkspaceState>,
}

impl WindowsWorkspaceService {
    pub fn connect() -> Result<Self, WorkspaceError> {
        let count = winvd::get_desktop_count().map_err(|error| {
            WorkspaceError::DisplayUnavailable(format!(
                "virtual desktop API unavailable: {error:?}"
            ))
        })?;
        if count == 0 {
            return Err(WorkspaceError::DisplayUnavailable(
                "Windows reports no virtual desktops".to_string(),
            ));
        }
        winvd::get_current_desktop().map_err(|error| {
            WorkspaceError::DisplayUnavailable(format!(
                "current virtual desktop unavailable: {error:?}"
            ))
        })?;
        Ok(Self {
            state: WindowsWorkspaceState::new(),
        })
    }

    pub fn list(&self) -> Result<Vec<WorkspaceInfo>, WorkspaceError> {
        let desktops = winvd::get_desktops().map_err(|error| {
            WorkspaceError::Operation(format!(
                "enumerating Windows virtual desktops failed: {error:?}"
            ))
        })?;
        let current_index = winvd::get_current_desktop()
            .map_err(|error| {
                WorkspaceError::Operation(format!(
                    "reading current Windows virtual desktop failed: {error:?}"
                ))
            })?
            .get_index()
            .map_err(|error| {
                WorkspaceError::Operation(format!(
                    "resolving current Windows virtual desktop failed: {error:?}"
                ))
            })?
            + 1;
        let mut workspaces = Vec::with_capacity(desktops.len());
        for (position, desktop) in desktops.into_iter().enumerate() {
            let native_index = desktop.get_index().map_err(|error| {
                WorkspaceError::Operation(format!(
                    "resolving Windows virtual desktop index failed: {error:?}"
                ))
            })?;
            let index = native_index + 1;
            let name = desktop
                .get_name()
                .ok()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("Desktop {}", position + 1));
            workspaces.push(WorkspaceInfo {
                index,
                name,
                is_current: index == current_index,
            });
        }
        workspaces.sort_by_key(|workspace| workspace.index);
        Ok(workspaces)
    }

    pub fn catalog(&self) -> Result<WorkspaceCatalog, WorkspaceError> {
        let workspaces = self.list()?;
        let current = workspaces
            .iter()
            .find(|workspace| workspace.is_current)
            .map(|workspace| workspace.index)
            .ok_or_else(|| {
                WorkspaceError::Operation(
                    "current Windows virtual desktop is not present in catalog".to_string(),
                )
            })?;
        Ok(WorkspaceCatalog {
            current,
            workspaces,
        })
    }

    pub fn query_window(&self, handle: usize) -> Result<WorkspacePlacement, WorkspaceError> {
        let desktop = winvd::get_desktop_by_window(hwnd(handle)).map_err(|error| {
            WorkspaceError::Operation(format!("querying window virtual desktop failed: {error:?}"))
        })?;
        let native_index = desktop.get_index().map_err(|error| {
            WorkspaceError::Operation(format!(
                "resolving window virtual desktop index failed: {error:?}"
            ))
        })?;
        let index = native_index + 1;
        Ok(WorkspacePlacement {
            index: Some(index),
            all: false,
        })
    }

    pub fn watch_window(
        &self,
        window_session_id: String,
        handle: usize,
        callback: Arc<dyn Fn(WorkspacePlacement) + Send + Sync>,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        // Tauri can report a valid HWND immediately after show/focus while the
        // Windows virtual-desktop service has not registered that top-level window yet.
        // Treat that short registration interval as transient instead of failing the
        // entire open operation. The final error is still returned if the handle never
        // becomes visible to winvd.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let placement = loop {
            match self.query_window(handle) {
                Ok(placement) => break placement,
                Err(error) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25));
                    let _ = error;
                }
                Err(error) => return Err(error),
            }
        };
        let watcher = WindowsWatcher {
            handle,
            placement: Arc::new(Mutex::new(placement.clone())),
            callback: Arc::clone(&callback),
        };
        self.state
            .watchers
            .write()
            .insert(window_session_id, watcher);
        (callback)(placement.clone());
        Ok(placement)
    }

    pub fn unwatch_window(&self, window_session_id: &str) {
        self.state.watchers.write().remove(window_session_id);
    }

    pub fn move_window_and_confirm(
        &self,
        window_session_id: &str,
        target: &WorkspaceTarget,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        let catalog = self.catalog()?;
        let index = catalog.resolve(target)?;
        self.move_window_to_index_and_confirm(window_session_id, index)
    }

    pub fn move_window_to_index_and_confirm(
        &self,
        window_session_id: &str,
        index: u32,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        let watcher = self
            .state
            .watchers
            .read()
            .get(window_session_id)
            .cloned()
            .ok_or_else(|| {
                WorkspaceError::Operation(format!(
                    "window '{window_session_id}' is not registered for workspace events"
                ))
            })?;
        let catalog = self.catalog()?;
        if !catalog
            .workspaces
            .iter()
            .any(|workspace| workspace.index == index)
        {
            return Err(WorkspaceError::InvalidWorkspace(format!(
                "workspace index {index} not found in catalog"
            )));
        }
        let native_index = index.checked_sub(1).ok_or_else(|| {
            WorkspaceError::InvalidWorkspace(
                "Windows workspace indexes are 1-based; index 0 is invalid".to_string(),
            )
        })?;
        winvd::move_window_to_desktop(winvd::get_desktop(native_index), &hwnd(watcher.handle))
            .map_err(|error| {
                WorkspaceError::Operation(format!(
                    "moving window to Windows virtual desktop {index} failed: {error:?}"
                ))
            })?;

        let deadline = std::time::Instant::now() + CONFIRMATION_TIMEOUT;
        loop {
            let placement = self.query_window(watcher.handle)?;
            if placement.index == Some(index) {
                *watcher.placement.lock() = placement.clone();
                (watcher.callback)(placement.clone());
                return Ok(placement);
            }
            if std::time::Instant::now() >= deadline {
                return Err(WorkspaceError::Operation(format!(
                    "Windows virtual desktop move to index {index} was not confirmed"
                )));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

fn hwnd(handle: usize) -> HWND {
    HWND(handle as *mut std::ffi::c_void)
}

fn query_native_window(handle: usize) -> Result<WorkspacePlacement, WorkspaceError> {
    let desktop = winvd::get_desktop_by_window(hwnd(handle)).map_err(|error| {
        WorkspaceError::Operation(format!("querying window virtual desktop failed: {error:?}"))
    })?;
    let native_index = desktop.get_index().map_err(|error| {
        WorkspaceError::Operation(format!(
            "resolving window virtual desktop index failed: {error:?}"
        ))
    })?;
    let index = native_index + 1;
    Ok(WorkspacePlacement {
        index: Some(index),
        all: false,
    })
}

impl WorkspaceBackend for WindowsWorkspaceService {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>, WorkspaceError> {
        self.list()
    }

    fn current_workspace(&self) -> Result<u32, WorkspaceError> {
        self.catalog().map(|catalog| catalog.current)
    }

    fn move_to_workspace(
        &self,
        window_id: &str,
        target: &WorkspaceTarget,
    ) -> Result<(), WorkspaceError> {
        self.move_window_and_confirm(window_id, target).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_workspace_catalog_reflects_real_desktops() {
        let service = WindowsWorkspaceService::connect().unwrap();
        let catalog = service.catalog().unwrap();
        assert!(!catalog.workspaces.is_empty());
        assert!(catalog
            .workspaces
            .iter()
            .any(|workspace| workspace.index == catalog.current));
        assert!(catalog
            .workspaces
            .windows(2)
            .all(|pair| pair[0].index < pair[1].index));
        assert_eq!(catalog.workspaces.first().unwrap().index, 1);
    }
}
