use crate::window_core::sanitize_console_result;
use portus_window_protocol::ConsoleEntry;
use serde::Deserialize;
use std::sync::mpsc;
use std::time::Duration;
use tauri::WebviewWindow;

const CONSOLE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

pub const CONSOLE_INIT_SCRIPT: &str = r#"
(() => {
  const snapshotName = "__PORTUS_WINDOW_CONSOLE_SNAPSHOT__";
  if (Object.prototype.hasOwnProperty.call(window, snapshotName)) return;

  const entries = [];
  let truncated = false;
  const maxEntries = 200;
  const maxMessage = 4096;
  const maxSource = 2048;

  const bounded = (value, limit) => {
    const text = String(value ?? "");
    return text.length > limit ? text.slice(0, limit) : text;
  };

  const printable = (value) => {
    if (typeof value === "string") return value;
    if (value instanceof Error) return value.stack || value.message || String(value);
    try {
      const json = JSON.stringify(value);
      if (json !== undefined) return json;
    } catch (_) {}
    try {
      return String(value);
    } catch (_) {
      return "<unprintable>";
    }
  };

  const push = (level, values, source = null, line = null) => {
    try {
      const message = bounded(values.map(printable).join(" "), maxMessage);
      const entry = {
        level,
        message,
        source: source == null ? null : bounded(source, maxSource),
        line: Number.isFinite(line) && line >= 0 ? Math.trunc(line) : null,
      };
      entries.push(entry);
      if (entries.length > maxEntries) {
        entries.splice(0, entries.length - maxEntries);
        truncated = true;
      }
    } catch (_) {}
  };

  for (const level of ["debug", "log", "info", "warn", "error"]) {
    try {
      const original = console[level];
      if (typeof original !== "function") continue;
      console[level] = (...args) => {
        push(level, args);
        return Reflect.apply(original, console, args);
      };
    } catch (_) {}
  }

  window.addEventListener("error", (event) => {
    push(
      "error",
      [event.message || "window error"],
      event.filename || null,
      event.lineno || null,
    );
  }, true);

  window.addEventListener("unhandledrejection", (event) => {
    push("error", ["Unhandled promise rejection:", event.reason]);
  });

  Object.defineProperty(window, snapshotName, {
    value: () => ({
      entries: entries.map((entry) => ({ ...entry })),
      truncated,
    }),
    enumerable: false,
    configurable: false,
    writable: false,
  });
})();
"#;

const CONSOLE_SNAPSHOT_SCRIPT: &str = r#"
(() => {
  try {
    const snapshot = window.__PORTUS_WINDOW_CONSOLE_SNAPSHOT__;
    const result = typeof snapshot === "function"
      ? snapshot()
      : { entries: [], truncated: false };
    return {
      ok: true,
      entries: Array.isArray(result.entries) ? result.entries : [],
      truncated: result.truncated === true,
      error: null,
    };
  } catch (error) {
    return {
      ok: false,
      entries: [],
      error: error && error.message ? String(error.message) : String(error),
    };
  }
})()
"#;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsoleProbeResult {
    ok: bool,
    #[serde(default)]
    entries: Vec<ConsoleEntry>,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    error: Option<String>,
}

pub fn probe_console(window: &WebviewWindow) -> Result<(Vec<ConsoleEntry>, bool), String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .eval_with_callback(CONSOLE_SNAPSHOT_SCRIPT, move |raw| {
            let parsed = serde_json::from_str::<ConsoleProbeResult>(&raw)
                .map_err(|error| format!("invalid console snapshot result: {error}"))
                .and_then(|result| {
                    if result.ok {
                        let (entries, bounded_truncated) = sanitize_console_result(result.entries);
                        Ok((entries, result.truncated || bounded_truncated))
                    } else {
                        Err(result
                            .error
                            .unwrap_or_else(|| "console snapshot failed".to_string()))
                    }
                });
            let _ = sender.send(parsed);
        })
        .map_err(|error| format!("could not schedule console snapshot: {error}"))?;

    receiver
        .recv_timeout(CONSOLE_PROBE_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                "console snapshot timed out after 3 seconds".to_string()
            }
            mpsc::RecvTimeoutError::Disconnected => {
                "console snapshot callback disconnected".to_string()
            }
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_script_is_bounded_and_non_privileged() {
        assert!(CONSOLE_INIT_SCRIPT.contains("maxEntries = 200"));
        assert!(CONSOLE_INIT_SCRIPT.contains("maxMessage = 4096"));
        assert!(CONSOLE_INIT_SCRIPT.contains("Object.defineProperty"));
        assert!(!CONSOLE_INIT_SCRIPT.contains("__TAURI__"));
        assert!(!CONSOLE_INIT_SCRIPT.contains("invoke("));
    }
}
