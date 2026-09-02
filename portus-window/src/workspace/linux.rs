use super::*;
use std::collections::BTreeMap;
use std::sync::Condvar;
use std::thread::JoinHandle;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, GetPropertyReply,
    PropertyNotifyEvent, Window,
};
use x11rb::rust_connection::RustConnection;

struct Atoms {
    net_number_of_desktops: Atom,
    net_desktop_names: Atom,
    net_current_desktop: Atom,
    net_wm_desktop: Atom,
    utf8_string: Atom,
}

#[derive(Clone)]
struct LinuxWatcher {
    xid: Window,
    placement: Arc<Mutex<WorkspacePlacement>>,
    changed: Arc<Condvar>,
    callback: Arc<dyn Fn(WorkspacePlacement) + Send + Sync>,
}

pub struct LinuxWorkspaceService {
    conn: Arc<RustConnection>,
    root: Window,
    atoms: Atoms,
    watchers: Arc<parking_lot::RwLock<BTreeMap<String, LinuxWatcher>>>,
    _event_thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl LinuxWorkspaceService {
    pub fn connect() -> Result<Self, WorkspaceError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|error| {
            WorkspaceError::DisplayUnavailable(format!("could not connect to X11: {error}"))
        })?;
        let root = conn.setup().roots[screen_num].root;

        conn.change_window_attributes(
            root,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|error| WorkspaceError::Operation(error.to_string()))?
        .check()
        .map_err(|error| WorkspaceError::Operation(error.to_string()))?;

        let net_number_of_desktops = conn
            .intern_atom(false, b"_NET_NUMBER_OF_DESKTOPS")
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .reply()
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .atom;
        let net_desktop_names = conn
            .intern_atom(false, b"_NET_DESKTOP_NAMES")
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .reply()
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .atom;
        let net_current_desktop = conn
            .intern_atom(false, b"_NET_CURRENT_DESKTOP")
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .reply()
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .atom;
        let net_wm_desktop = conn
            .intern_atom(false, b"_NET_WM_DESKTOP")
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .reply()
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .atom;
        let utf8_string = conn
            .intern_atom(false, b"UTF8_STRING")
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .reply()
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .atom;

        let atoms = Atoms {
            net_number_of_desktops,
            net_desktop_names,
            net_current_desktop,
            net_wm_desktop,
            utf8_string,
        };

        let conn = Arc::new(conn);
        let watchers = Arc::new(parking_lot::RwLock::new(BTreeMap::new()));
        let thread_conn = Arc::clone(&conn);
        let thread_watchers = Arc::clone(&watchers);
        let wm_desktop_atom = net_wm_desktop;

        let event_thread = std::thread::Builder::new()
            .name("portus-x11-workspace".to_string())
            .spawn(move || {
                while let Ok(event) = thread_conn.wait_for_event() {
                    if let x11rb::protocol::Event::PropertyNotify(event) = event {
                        if event.atom == wm_desktop_atom {
                            handle_property_notify(
                                &thread_conn,
                                &thread_watchers,
                                &event,
                                wm_desktop_atom,
                            );
                        }
                    }
                }
            })
            .map_err(|error| WorkspaceError::Operation(error.to_string()))?;

        Ok(Self {
            conn,
            root,
            atoms,
            watchers,
            _event_thread: Arc::new(Mutex::new(Some(event_thread))),
        })
    }

    pub fn list(&self) -> Result<Vec<WorkspaceInfo>, WorkspaceError> {
        let catalog = self.catalog()?;
        Ok(catalog.workspaces)
    }

    pub fn catalog(&self) -> Result<WorkspaceCatalog, WorkspaceError> {
        let current = self.current_workspace()?;
        let total = self.desktop_count()?.unwrap_or(1);
        let names = self.desktop_names()?.unwrap_or_default();

        let mut workspaces = Vec::with_capacity(total as usize);
        for index in 0..total {
            let name = names
                .get(index as usize)
                .cloned()
                .unwrap_or_else(|| format!("Workspace {}", index + 1));
            workspaces.push(WorkspaceInfo {
                index,
                name,
                is_current: index == current,
            });
        }
        Ok(WorkspaceCatalog {
            current,
            workspaces,
        })
    }

    pub fn current_workspace(&self) -> Result<u32, WorkspaceError> {
        let reply = self.get_property(
            self.root,
            self.atoms.net_current_desktop,
            AtomEnum::CARDINAL,
        )?;
        let value = read_u32(&reply).unwrap_or(0);
        Ok(value)
    }

    fn desktop_count(&self) -> Result<Option<u32>, WorkspaceError> {
        let reply = self.get_property(
            self.root,
            self.atoms.net_number_of_desktops,
            AtomEnum::CARDINAL,
        )?;
        Ok(read_u32(&reply))
    }

    fn desktop_names(&self) -> Result<Option<Vec<String>>, WorkspaceError> {
        let reply = self.get_property(
            self.root,
            self.atoms.net_desktop_names,
            self.atoms.utf8_string,
        )?;
        if reply.value.is_empty() {
            return Ok(None);
        }
        let names = String::from_utf8_lossy(&reply.value)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        Ok(Some(names))
    }

    fn get_property(
        &self,
        window: Window,
        atom: Atom,
        property_type: impl Into<Atom>,
    ) -> Result<GetPropertyReply, WorkspaceError> {
        self.conn
            .get_property(false, window, atom, property_type, 0, 1024)
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .reply()
            .map_err(|e| WorkspaceError::Operation(e.to_string()))
    }

    pub fn query_window(&self, xid: usize) -> Result<WorkspacePlacement, WorkspaceError> {
        let xid = xid as Window;
        let reply = self.get_property(xid, self.atoms.net_wm_desktop, AtomEnum::CARDINAL)?;
        let value = read_u32(&reply);
        match value {
            Some(0xFFFFFFFF) => Ok(WorkspacePlacement {
                index: None,
                all: true,
            }),
            Some(index) => Ok(WorkspacePlacement {
                index: Some(index),
                all: false,
            }),
            None => Ok(WorkspacePlacement {
                index: Some(0),
                all: false,
            }),
        }
    }

    pub fn watch_window(
        &self,
        window_session_id: String,
        xid: usize,
        callback: Arc<dyn Fn(WorkspacePlacement) + Send + Sync>,
    ) -> Result<WorkspacePlacement, WorkspaceError> {
        let xid = xid as Window;
        let placement = self.query_window(xid as usize)?;
        let watcher = LinuxWatcher {
            xid,
            placement: Arc::new(Mutex::new(placement.clone())),
            changed: Arc::new(Condvar::new()),
            callback: Arc::clone(&callback),
        };
        self.watchers.write().insert(window_session_id, watcher);
        (callback)(placement.clone());
        Ok(placement)
    }

    pub fn unwatch_window(&self, window_session_id: &str) {
        self.watchers.write().remove(window_session_id);
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
            .watchers
            .read()
            .get(window_session_id)
            .cloned()
            .ok_or_else(|| {
                WorkspaceError::Operation(format!(
                    "window '{window_session_id}' is not registered for workspace events"
                ))
            })?;

        let client_message = ClientMessageEvent {
            response_type: 33,
            format: 32,
            sequence: 0,
            window: watcher.xid,
            type_: self.atoms.net_wm_desktop,
            data: x11rb::protocol::xproto::ClientMessageData::from([index, 1, 0, 0, 0]),
        };

        self.conn
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                client_message,
            )
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?
            .check()
            .map_err(|e| WorkspaceError::Operation(e.to_string()))?;

        let placement = WorkspacePlacement {
            index: Some(index),
            all: false,
        };
        *watcher.placement.lock() = placement.clone();
        (watcher.callback)(placement.clone());
        Ok(placement)
    }
}

impl WorkspaceBackend for LinuxWorkspaceService {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>, WorkspaceError> {
        self.list()
    }

    fn current_workspace(&self) -> Result<u32, WorkspaceError> {
        self.current_workspace()
    }

    fn move_to_workspace(
        &self,
        window_id: &str,
        target: &WorkspaceTarget,
    ) -> Result<(), WorkspaceError> {
        self.move_window_and_confirm(window_id, target).map(|_| ())
    }
}

fn handle_property_notify(
    conn: &RustConnection,
    watchers: &parking_lot::RwLock<BTreeMap<String, LinuxWatcher>>,
    event: &PropertyNotifyEvent,
    wm_desktop_atom: Atom,
) {
    let matching_watchers: Vec<LinuxWatcher> = watchers
        .read()
        .values()
        .filter(|w| w.xid == event.window)
        .cloned()
        .collect();

    for watcher in matching_watchers {
        if let Ok(cookie) = conn.get_property(
            false,
            watcher.xid,
            wm_desktop_atom,
            AtomEnum::CARDINAL,
            0,
            1024,
        ) {
            if let Ok(reply) = cookie.reply() {
                let value = read_u32(&reply);
                let placement = match value {
                    Some(0xFFFFFFFF) => WorkspacePlacement {
                        index: None,
                        all: true,
                    },
                    Some(index) => WorkspacePlacement {
                        index: Some(index),
                        all: false,
                    },
                    None => continue,
                };
                *watcher.placement.lock() = placement.clone();
                watcher.changed.notify_all();
                (watcher.callback)(placement);
            }
        }
    }
}

fn read_u32(reply: &GetPropertyReply) -> Option<u32> {
    if reply.value.len() >= 4 {
        Some(u32::from_ne_bytes(reply.value[0..4].try_into().ok()?))
    } else {
        None
    }
}
