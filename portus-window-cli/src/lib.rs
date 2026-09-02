mod args;
mod request;
mod transport;
mod validation;

pub use args::{AuthSessionCommands, Cli, Commands, MediaCommands};
pub use request::request_for;
pub use transport::execute;

use portus_window_protocol::{Response, PROTOCOL_VERSION};

pub struct CliOutcome {
    pub response: Response,
    pub exit_code: i32,
}

impl CliOutcome {
    fn from_response(response: Response) -> Self {
        let exit_code = response.exit_code();
        Self {
            response,
            exit_code,
        }
    }
}

pub fn render_response(response: &Response) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| {
        format!(
            "{{\"status\":\"error\",\"version\":{PROTOCOL_VERSION},\"error\":{{\"code\":\"internal\",\"message\":\"failed to serialize response\"}}}}"
        )
    })
}

#[cfg(test)]
use clap::Parser;
#[cfg(test)]
use portus_window_protocol::{
    ConfigAction, ErrorCode, InteractionAction, MediaAction, OpenSource, Request,
    WindowStateAction, WorkspaceTarget, DEFAULT_INTERACTION_TIMEOUT_MS, MAX_INTERACTION_TIMEOUT_MS,
    MAX_INTERACTION_VALUE_CHARS,
};
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use transport::{outcome_from_daemon_response, outcome_from_daemon_response_for};
#[cfg(test)]
use validation::*;

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_reports_component_name() {
        assert_eq!(Cli::command().get_name(), "portus-window-cli");
    }

    #[test]
    fn ping_maps_to_versioned_request() {
        assert_eq!(request_for(&Commands::Ping).unwrap(), Request::ping());
    }

    #[test]
    fn cli_open_maps_initial_geometry() {
        let request = request_for(&Commands::Open {
            source: "https://example.com".to_string(),
            description: None,
            profile: None,
            wait_loaded: false,
            width: Some(1200),
            height: Some(800),
            x: Some(100),
            y: Some(50),
            maximize: true,
            minimize: false,
            restore: false,
            fullscreen: false,
            always_on_top: Some(true),
            workspace: Some("Code".to_string()),
        })
        .unwrap();
        match request {
            Request::Open { geometry, .. } => {
                let geometry = geometry.expect("geometry should be present");
                assert_eq!(geometry.width, Some(1200));
                assert_eq!(geometry.height, Some(800));
                assert_eq!(geometry.x, Some(100));
                assert_eq!(geometry.y, Some(50));
                assert_eq!(geometry.state, Some(WindowStateAction::Maximize));
                assert_eq!(geometry.always_on_top, Some(true));
                assert_eq!(
                    geometry.workspace,
                    Some(WorkspaceTarget::Name {
                        name: "Code".to_string()
                    })
                );
            }
            _ => panic!("expected open request"),
        }
    }

    #[test]
    fn open_batch_maps_manifest_to_versioned_request() {
        let manifest = serde_json::json!([
            {
                "source": { "kind": "web", "url": "https://example.com" },
                "geometry": { "width": 500, "height": 400 }
            },
            {
                "source": { "kind": "web", "url": "https://www.youtube.com" }
            }
        ]);
        let path =
            std::env::temp_dir().join(format!("portus-open-batch-{}.json", std::process::id()));
        std::fs::write(&path, manifest.to_string()).unwrap();
        let request = request_for(&Commands::OpenBatch {
            file: Some(path.clone()),
        })
        .unwrap();
        std::fs::remove_file(path).unwrap();
        match request {
            Request::OpenBatch { windows, .. } => {
                assert_eq!(windows.len(), 2);
                assert_eq!(
                    windows[0].geometry.as_ref().and_then(|g| g.width),
                    Some(500)
                );
                assert!(matches!(windows[1].source, OpenSource::Web { .. }));
            }
            _ => panic!("expected open-batch request"),
        }
    }

    #[test]
    fn open_batch_rejects_empty_and_oversized_manifests() {
        let empty = std::env::temp_dir().join(format!(
            "portus-open-batch-empty-{}.json",
            std::process::id()
        ));
        std::fs::write(&empty, "[]").unwrap();
        assert!(request_for(&Commands::OpenBatch {
            file: Some(empty.clone())
        })
        .is_err());
        std::fs::remove_file(empty).unwrap();

        let windows = (0..=portus_window_protocol::MAX_OPEN_BATCH_WINDOWS)
            .map(|_| serde_json::json!({ "source": { "kind": "web", "url": "https://example.com" } }))
            .collect::<Vec<_>>();
        let oversized = std::env::temp_dir().join(format!(
            "portus-open-batch-large-{}.json",
            std::process::id()
        ));
        std::fs::write(&oversized, serde_json::to_string(&windows).unwrap()).unwrap();
        assert!(request_for(&Commands::OpenBatch {
            file: Some(oversized.clone())
        })
        .is_err());
        std::fs::remove_file(oversized).unwrap();
    }

    #[test]
    fn cli_classifies_web_and_local_media_sources() {
        let web = request_for(&Commands::Open {
            source: "https://example.com".to_string(),
            description: None,
            profile: None,
            wait_loaded: false,
            width: None,
            height: None,
            x: None,
            y: None,
            maximize: false,
            minimize: false,
            restore: false,
            fullscreen: false,
            always_on_top: None,
            workspace: None,
        })
        .unwrap();
        assert!(matches!(
            web,
            Request::Open {
                source: OpenSource::Web { .. },
                ..
            }
        ));

        let local = request_for(&Commands::Open {
            source: "demo.mp4".to_string(),
            description: None,
            profile: None,
            wait_loaded: false,
            width: None,
            height: None,
            x: None,
            y: None,
            maximize: false,
            minimize: false,
            restore: false,
            fullscreen: false,
            always_on_top: None,
            workspace: None,
        })
        .unwrap();
        match local {
            Request::Open {
                source: OpenSource::LocalMedia { path },
                ..
            } => assert!(Path::new(&path).is_absolute()),
            _ => panic!("expected local media source"),
        }

        assert!(request_for(&Commands::Open {
            source: "file:///tmp/demo.mp4".to_string(),
            description: None,
            profile: None,
            wait_loaded: false,
            width: None,
            height: None,
            x: None,
            y: None,
            maximize: false,
            minimize: false,
            restore: false,
            fullscreen: false,
            always_on_top: None,
            workspace: None,
        })
        .is_err());
        assert!(request_for(&Commands::Open {
            source: "ftp://example.com/demo.mp4".to_string(),
            description: None,
            profile: None,
            wait_loaded: false,
            width: None,
            height: None,
            x: None,
            y: None,
            maximize: false,
            minimize: false,
            restore: false,
            fullscreen: false,
            always_on_top: None,
            workspace: None,
        })
        .is_err());
    }

    #[test]
    fn cli_maps_console_screenshot_and_media_requests() {
        let console = request_for(&Commands::Console {
            target: Some("wsess_00000000000000000000000000000001".to_string()),
            all: false,
        })
        .unwrap();
        assert!(matches!(console, Request::Console { .. }));

        let screenshot = request_for(&Commands::Screenshot {
            target: Some("wsess_00000000000000000000000000000001".to_string()),
            out: Some(PathBuf::from("capture.png")),
            overwrite: true,
            all: false,
        })
        .unwrap();
        match screenshot {
            Request::Screenshot { out, overwrite, .. } => {
                assert!(Path::new(&out).is_absolute());
                assert!(overwrite);
            }
            _ => panic!("expected screenshot request"),
        }

        let media = request_for(&Commands::Media {
            target: Some("wsess_00000000000000000000000000000001".to_string()),
            action: MediaCommands::Seek { seconds: 30.0 },
            all: false,
        })
        .unwrap();
        assert!(matches!(
            media,
            Request::Media {
                action: MediaAction::Seek { seconds: 30.0 },
                ..
            }
        ));
    }

    #[test]
    fn interaction_actions_preserve_order_and_screenshot_mapping() {
        let request = request_for(&Commands::Interact {
            target: "wsess_00000000000000000000000000000001".to_string(),
            actions: vec![
                r##"{"kind":"fill","selector":"#name","value":"Ada"}"##.to_string(),
                r##"{"kind":"click","selector":"#save"}"##.to_string(),
                r##"{"kind":"check_text","text":"Saved","selector":"#status"}"##.to_string(),
            ],
            interaction_timeout_ms: DEFAULT_INTERACTION_TIMEOUT_MS,
            screenshot_out: Some(PathBuf::from("after.png")),
            screenshot_overwrite: true,
        })
        .unwrap();
        match request {
            Request::Interact {
                actions,
                timeout_ms,
                screenshot: Some(screenshot),
                ..
            } => {
                assert!(matches!(actions[0], InteractionAction::Fill { .. }));
                assert!(matches!(actions[1], InteractionAction::Click { .. }));
                assert!(matches!(actions[2], InteractionAction::CheckText { .. }));
                assert_eq!(timeout_ms, DEFAULT_INTERACTION_TIMEOUT_MS);
                assert!(Path::new(&screenshot.out).is_absolute());
                assert!(screenshot.overwrite);
            }
            _ => panic!("expected interaction request"),
        }
    }

    #[test]
    fn interaction_cli_rejects_malformed_or_unbounded_actions() {
        assert!(request_for(&Commands::Interact {
            target: "wsess_00000000000000000000000000000001".to_string(),
            actions: vec!["not json".to_string()],
            interaction_timeout_ms: DEFAULT_INTERACTION_TIMEOUT_MS,
            screenshot_out: None,
            screenshot_overwrite: false,
        })
        .is_err());
        assert!(request_for(&Commands::Interact {
            target: "wsess_00000000000000000000000000000001".to_string(),
            actions: vec![r##"{"kind":"click","selector":" "}"##.to_string()],
            interaction_timeout_ms: DEFAULT_INTERACTION_TIMEOUT_MS,
            screenshot_out: None,
            screenshot_overwrite: false,
        })
        .is_err());
        assert!(request_for(&Commands::Interact {
            target: "wsess_00000000000000000000000000000001".to_string(),
            actions: vec![r##"{"kind":"click","selector":"#go"}"##.to_string()],
            interaction_timeout_ms: MAX_INTERACTION_TIMEOUT_MS + 1,
            screenshot_out: None,
            screenshot_overwrite: false,
        })
        .is_err());
    }

    #[test]
    fn oversized_interaction_requests_fail_before_ipc() {
        let value = "x".repeat(MAX_INTERACTION_VALUE_CHARS);
        let action = serde_json::to_string(&InteractionAction::Fill {
            selector: "#field".to_string(),
            value,
        })
        .unwrap();
        let result = request_for(&Commands::Interact {
            target: "wsess_00000000000000000000000000000001".to_string(),
            actions: vec![action; 20],
            interaction_timeout_ms: DEFAULT_INTERACTION_TIMEOUT_MS,
            screenshot_out: None,
            screenshot_overwrite: false,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("maximum protocol frame"));
    }

    #[test]
    fn media_values_are_fail_closed() {
        assert!(request_for(&Commands::Media {
            target: Some("wsess_00000000000000000000000000000001".to_string()),
            action: MediaCommands::Seek { seconds: -1.0 },
            all: false,
        })
        .is_err());
        assert!(request_for(&Commands::Media {
            target: Some("wsess_00000000000000000000000000000001".to_string()),
            action: MediaCommands::SetVolume { level: 1.1 },
            all: false,
        })
        .is_err());
    }

    #[test]
    fn cli_parses_open_media_and_resize() {
        let open = Cli::try_parse_from([
            "portus-window-cli",
            "open",
            "https://example.com",
            "--description",
            "demo",
        ])
        .expect("open should parse");
        assert!(matches!(open.command, Commands::Open { .. }));

        let history = Cli::try_parse_from(["portus-window-cli", "history", "--query", "demo"])
            .expect("history should parse");
        assert!(matches!(history.command, Commands::History { .. }));

        let config =
            Cli::try_parse_from(["portus-window-cli", "config", "--set", "retention_days=30"])
                .expect("config should parse");
        assert!(matches!(config.command, Commands::Config { .. }));

        let media = Cli::try_parse_from([
            "portus-window-cli",
            "media",
            "wsess_00000000000000000000000000000001",
            "set-volume",
            "--level",
            "0.5",
        ])
        .expect("media should parse");
        assert!(matches!(media.command, Commands::Media { .. }));

        let interact = Cli::try_parse_from([
            "portus-window-cli",
            "interact",
            "wsess_00000000000000000000000000000001",
            "--action",
            r##"{"kind":"click","selector":"#go"}"##,
            "--action",
            r##"{"kind":"check_text","text":"done"}"##,
        ])
        .expect("interact should parse");
        assert!(matches!(interact.command, Commands::Interact { .. }));

        let resize = Cli::try_parse_from([
            "portus-window-cli",
            "resize",
            "wsess_00000000000000000000000000000001",
            "--width",
            "1200",
            "--workspace",
            "Code",
            "--always-on-top",
            "true",
        ])
        .expect("resize should parse");
        assert!(matches!(resize.command, Commands::Resize { .. }));
    }

    #[test]
    fn persistence_commands_map_to_typed_requests() {
        assert!(matches!(
            request_for(&Commands::History {
                query: Some("demo".to_string()),
                clear: false,
            })
            .unwrap(),
            Request::History {
                query: Some(_),
                clear: false,
                ..
            }
        ));
        assert!(matches!(
            request_for(&Commands::Config {
                show: false,
                set: Some("history_enabled=false".to_string()),
            })
            .unwrap(),
            Request::Config {
                action: ConfigAction::SetHistoryEnabled { enabled: false },
                ..
            }
        ));
        assert!(matches!(
            request_for(&Commands::Config {
                show: false,
                set: Some("retention_days=null".to_string()),
            })
            .unwrap(),
            Request::Config {
                action: ConfigAction::SetRetentionDays { days: None },
                ..
            }
        ));
    }

    #[test]
    fn persistence_cli_validation_is_fail_closed() {
        assert!(request_for(&Commands::Config {
            show: false,
            set: Some("unknown=true".to_string()),
        })
        .is_err());
        assert!(request_for(&Commands::Config {
            show: false,
            set: Some("retention_days=0".to_string()),
        })
        .is_err());
        assert!(request_for(&Commands::History {
            query: Some("   ".to_string()),
            clear: false,
        })
        .is_err());
    }

    #[test]
    fn workspace_commands_map_to_typed_requests() {
        assert!(matches!(
            request_for(&Commands::Workspaces).unwrap(),
            Request::Workspaces { .. }
        ));
        assert_eq!(
            workspace_target_for("3").unwrap(),
            WorkspaceTarget::Index { index: 3 }
        );
        assert_eq!(
            workspace_target_for("Code").unwrap(),
            WorkspaceTarget::Name {
                name: "Code".to_string()
            }
        );
    }

    #[test]
    fn resize_maps_to_typed_request() {
        let request = request_for(&Commands::Resize {
            target: Some("wsess_00000000000000000000000000000001".to_string()),
            width: Some(1200),
            height: None,
            x: None,
            y: None,
            maximize: true,
            minimize: false,
            restore: false,
            fullscreen: false,
            workspace: Some("Code".to_string()),
            always_on_top: Some(true),
            all: false,
        })
        .unwrap();
        assert!(matches!(
            request,
            Request::Resize {
                state: Some(WindowStateAction::Maximize),
                always_on_top: Some(true),
                workspace: Some(WorkspaceTarget::Name { ref name }),
                ..
            } if name == "Code"
        ));
    }

    #[test]
    fn close_and_empty_resize_are_rejected_before_ipc() {
        assert!(request_for(&Commands::Close {
            target: None,
            all: false,
        })
        .is_err());
        assert!(request_for(&Commands::Resize {
            target: Some("wsess_00000000000000000000000000000001".to_string()),
            width: None,
            height: None,
            x: None,
            y: None,
            maximize: false,
            minimize: false,
            restore: false,
            fullscreen: false,
            workspace: None,
            always_on_top: None,
            all: false,
        })
        .is_err());
    }

    #[test]
    fn mismatched_daemon_response_version_is_rejected() {
        let response = Response::Ok {
            version: PROTOCOL_VERSION + 1,
            data: serde_json::Value::Null,
        };
        let outcome = outcome_from_daemon_response(response);
        assert_eq!(outcome.exit_code, 2);
        assert!(matches!(
            outcome.response,
            Response::Error {
                error: portus_window_protocol::ResponseError {
                    code: ErrorCode::VersionMismatch,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn lower_daemon_response_version_is_rejected() {
        let response = Response::Ok {
            version: PROTOCOL_VERSION - 1,
            data: serde_json::Value::Null,
        };
        let outcome = outcome_from_daemon_response(response);
        assert_eq!(outcome.exit_code, 2);
        assert!(matches!(
            outcome.response,
            Response::Error {
                error: portus_window_protocol::ResponseError {
                    code: ErrorCode::VersionMismatch,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn incomplete_interaction_results_use_stable_nonzero_cli_exits() {
        let timed_out = portus_window_protocol::InteractionResult {
            window_session_id: "wsess_00000000000000000000000000000001".to_string(),
            completed: false,
            actions: vec![portus_window_protocol::InteractionStepResult {
                index: 0,
                kind: portus_window_protocol::InteractionActionKind::WaitForSelector,
                selector: Some("#elem".to_string()),
                ok: false,
                code: portus_window_protocol::InteractionStepCode::Timeout,
                elapsed_ms: 4_000,
                message: Some("timeout".to_string()),
            }],
            post_status: None,
            console: None,
            screenshot: None,
            output_truncated: false,
            post_errors: Vec::new(),
        };
        let timeout_response = Response::ok(serde_json::to_value(timed_out).unwrap());
        assert_eq!(
            outcome_from_daemon_response_for(timeout_response, true).exit_code,
            5
        );

        let failed = portus_window_protocol::InteractionResult {
            window_session_id: "wsess_00000000000000000000000000000001".to_string(),
            completed: false,
            actions: vec![portus_window_protocol::InteractionStepResult {
                index: 0,
                kind: portus_window_protocol::InteractionActionKind::CheckSelector,
                selector: Some("#elem".to_string()),
                ok: false,
                code: portus_window_protocol::InteractionStepCode::SelectorNotFound,
                elapsed_ms: 1,
                message: Some("failed".to_string()),
            }],
            post_status: None,
            console: None,
            screenshot: None,
            output_truncated: false,
            post_errors: Vec::new(),
        };
        let failed_response = Response::ok(serde_json::to_value(failed).unwrap());
        assert_eq!(
            outcome_from_daemon_response_for(failed_response, true).exit_code,
            6
        );
    }
    #[test]
    fn protocol_errors_map_to_stable_exit_codes() {
        let response = Response::error(ErrorCode::TargetNotFound, "missing");
        assert_eq!(CliOutcome::from_response(response).exit_code, 4);
        assert_eq!(
            CliOutcome::from_response(Response::error(
                ErrorCode::InvalidWorkspace,
                "bad workspace"
            ))
            .exit_code,
            7
        );
        assert_eq!(
            CliOutcome::from_response(Response::error(ErrorCode::DisplayUnavailable, "no display"))
                .exit_code,
            6
        );
    }
}
