use portus_window_protocol::{
    InteractionAction, InteractionActionKind, InteractionResult, InteractionStepCode,
    InteractionStepResult, MAX_FRAME_BYTES, MAX_INTERACTION_ACTIONS, MAX_INTERACTION_KEY_CHARS,
    MAX_INTERACTION_SELECTOR_CHARS, MAX_INTERACTION_TEXT_CHARS, MAX_INTERACTION_TIMEOUT_MS,
    MAX_INTERACTION_VALUE_CHARS, MAX_LOAD_ERROR_CHARS, MIN_INTERACTION_TIMEOUT_MS,
};
use serde::Deserialize;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tauri::WebviewWindow;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const CALLBACK_TIMEOUT_CAP: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct InteractionExecution {
    pub completed: bool,
    pub actions: Vec<InteractionStepResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptResult {
    ok: bool,
    code: InteractionStepCode,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug)]
enum EvalError {
    Timeout,
    Failed(String),
}

pub fn validate_request(actions: &[InteractionAction], timeout_ms: u64) -> Result<(), String> {
    if actions.is_empty() {
        return Err("interaction requires at least one action".to_string());
    }
    if actions.len() > MAX_INTERACTION_ACTIONS {
        return Err(format!(
            "interaction supports at most {MAX_INTERACTION_ACTIONS} ordered actions"
        ));
    }
    if !(MIN_INTERACTION_TIMEOUT_MS..=MAX_INTERACTION_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(format!(
            "interaction timeout must be between {MIN_INTERACTION_TIMEOUT_MS} and {MAX_INTERACTION_TIMEOUT_MS} ms"
        ));
    }
    for action in actions {
        validate_action(action)?;
    }
    Ok(())
}

fn validate_action(action: &InteractionAction) -> Result<(), String> {
    match action {
        InteractionAction::Click { selector }
        | InteractionAction::WaitForSelector { selector }
        | InteractionAction::CheckSelector { selector } => validate_selector(selector),
        InteractionAction::Fill { selector, value } => {
            validate_selector(selector)?;
            if value.chars().count() > MAX_INTERACTION_VALUE_CHARS {
                return Err(format!(
                    "interaction fill value must be at most {MAX_INTERACTION_VALUE_CHARS} characters"
                ));
            }
            Ok(())
        }
        InteractionAction::PressKey { key, selector } => {
            if key.trim().is_empty() {
                return Err("interaction key must not be blank".to_string());
            }
            if key.chars().count() > MAX_INTERACTION_KEY_CHARS {
                return Err(format!(
                    "interaction key must be at most {MAX_INTERACTION_KEY_CHARS} characters"
                ));
            }
            if let Some(selector) = selector {
                validate_selector(selector)?;
            }
            Ok(())
        }
        InteractionAction::WaitForText { text, selector }
        | InteractionAction::CheckText { text, selector } => {
            if text.is_empty() {
                return Err("interaction text must not be empty".to_string());
            }
            if text.chars().count() > MAX_INTERACTION_TEXT_CHARS {
                return Err(format!(
                    "interaction text must be at most {MAX_INTERACTION_TEXT_CHARS} characters"
                ));
            }
            if let Some(selector) = selector {
                validate_selector(selector)?;
            }
            Ok(())
        }
    }
}

fn validate_selector(selector: &str) -> Result<(), String> {
    if selector.trim().is_empty() {
        return Err("interaction selector must not be blank".to_string());
    }
    if selector.chars().count() > MAX_INTERACTION_SELECTOR_CHARS {
        return Err(format!(
            "interaction selector must be at most {MAX_INTERACTION_SELECTOR_CHARS} characters"
        ));
    }
    Ok(())
}

pub fn execute(
    window: &WebviewWindow,
    actions: &[InteractionAction],
    timeout_ms: u64,
) -> InteractionExecution {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut results = Vec::with_capacity(actions.len());

    for (index, action) in actions.iter().enumerate() {
        let started = Instant::now();
        let result = execute_action(window, action, deadline);
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let step = InteractionStepResult {
            index,
            kind: action_kind(action),
            selector: action_selector(action),
            ok: result.ok,
            code: result.code,
            elapsed_ms,
            message: result.message,
        };
        let ok = step.ok;
        results.push(step);
        if !ok {
            break;
        }
    }

    InteractionExecution {
        completed: results.len() == actions.len() && results.iter().all(|result| result.ok),
        actions: results,
    }
}

fn action_kind(action: &InteractionAction) -> InteractionActionKind {
    match action {
        InteractionAction::Click { .. } => InteractionActionKind::Click,
        InteractionAction::Fill { .. } => InteractionActionKind::Fill,
        InteractionAction::PressKey { .. } => InteractionActionKind::PressKey,
        InteractionAction::WaitForSelector { .. } => InteractionActionKind::WaitForSelector,
        InteractionAction::WaitForText { .. } => InteractionActionKind::WaitForText,
        InteractionAction::CheckSelector { .. } => InteractionActionKind::CheckSelector,
        InteractionAction::CheckText { .. } => InteractionActionKind::CheckText,
    }
}

fn action_selector(action: &InteractionAction) -> Option<String> {
    match action {
        InteractionAction::Click { selector }
        | InteractionAction::Fill { selector, .. }
        | InteractionAction::WaitForSelector { selector }
        | InteractionAction::CheckSelector { selector } => Some(selector.clone()),
        InteractionAction::PressKey { selector, .. }
        | InteractionAction::WaitForText { selector, .. }
        | InteractionAction::CheckText { selector, .. } => selector.clone(),
    }
}

fn execute_action(
    window: &WebviewWindow,
    action: &InteractionAction,
    deadline: Instant,
) -> ScriptResult {
    if Instant::now() >= deadline {
        return timeout_result("interaction batch deadline expired before action started");
    }

    match action {
        InteractionAction::WaitForSelector { .. } | InteractionAction::WaitForText { .. } => {
            execute_wait(window, action, deadline)
        }
        _ => evaluate_for_action(window, script_for(action), deadline),
    }
}

fn execute_wait(
    window: &WebviewWindow,
    action: &InteractionAction,
    deadline: Instant,
) -> ScriptResult {
    loop {
        if Instant::now() >= deadline {
            return timeout_result("interaction wait condition was not satisfied before timeout");
        }
        let result = evaluate_for_action(window, script_for(action), deadline);
        if result.ok {
            return result;
        }
        if !matches!(
            result.code,
            InteractionStepCode::SelectorNotFound | InteractionStepCode::TextNotFound
        ) {
            return result;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return timeout_result("interaction wait condition was not satisfied before timeout");
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

fn evaluate_for_action(window: &WebviewWindow, script: String, deadline: Instant) -> ScriptResult {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return timeout_result("interaction batch deadline expired");
    }
    match evaluate(window, script, CALLBACK_TIMEOUT_CAP.min(remaining)) {
        Ok(result) => result,
        Err(EvalError::Timeout) => timeout_result("interaction evaluation callback timed out"),
        Err(EvalError::Failed(message)) => {
            failure_result(InteractionStepCode::ScriptError, bounded_message(message))
        }
    }
}

fn evaluate(
    window: &WebviewWindow,
    script: String,
    timeout: Duration,
) -> Result<ScriptResult, EvalError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .eval_with_callback(script, move |raw| {
            let parsed = serde_json::from_str::<ScriptResult>(&raw)
                .map_err(|error| format!("invalid interaction result: {error}"))
                .and_then(sanitize_script_result);
            let _ = sender.send(parsed);
        })
        .map_err(|error| EvalError::Failed(format!("could not schedule interaction: {error}")))?;

    receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => EvalError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => {
                EvalError::Failed("interaction callback disconnected".to_string())
            }
        })?
        .map_err(EvalError::Failed)
}

fn sanitize_script_result(mut result: ScriptResult) -> Result<ScriptResult, String> {
    if result.ok != (result.code == InteractionStepCode::Ok) {
        return Err("interaction result returned inconsistent success/code state".to_string());
    }
    result.message = if result.ok {
        None
    } else {
        result.message.map(bounded_message)
    };
    Ok(result)
}

pub fn bound_result_for_frame(mut result: InteractionResult) -> InteractionResult {
    const FRAME_HEADROOM: usize = 1_024;
    const RESULT_BUDGET: usize = MAX_FRAME_BYTES - FRAME_HEADROOM;

    result.output_truncated = false;
    while serialized_len(&result) > RESULT_BUDGET {
        let mut changed = false;
        if let Some(console) = result.console.as_mut() {
            if !console.entries.is_empty() {
                console.entries.remove(0);
                console.truncated = true;
                changed = true;
            }
        }
        if !changed {
            if let Some(status) = result.post_status.as_mut() {
                if !status.console_errors.is_empty() {
                    status.console_errors.remove(0);
                    status.console_errors_truncated = true;
                    changed = true;
                } else if let Some(history) = status.url_history.as_mut() {
                    if history.len() > 1 {
                        history.remove(0);
                        status.url_history_truncated = Some(true);
                        changed = true;
                    }
                }
            }
        }
        if !changed && result.console.is_some() {
            result.console = None;
            changed = true;
        }
        if !changed && result.post_status.is_some() {
            result.post_status = None;
            changed = true;
        }
        if !changed {
            break;
        }
        result.output_truncated = true;
    }
    result
}

fn serialized_len(result: &InteractionResult) -> usize {
    serde_json::to_vec(result)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_FRAME_BYTES)
}

fn bounded_message(message: String) -> String {
    if message.chars().count() <= MAX_LOAD_ERROR_CHARS {
        message
    } else {
        message.chars().take(MAX_LOAD_ERROR_CHARS).collect()
    }
}

fn timeout_result(message: &str) -> ScriptResult {
    failure_result(InteractionStepCode::Timeout, message.to_string())
}

fn failure_result(code: InteractionStepCode, message: String) -> ScriptResult {
    ScriptResult {
        ok: false,
        code,
        message: Some(bounded_message(message)),
    }
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value)
        .expect("serializing a Rust string to a JSON/JavaScript string literal cannot fail")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn script_for(action: &InteractionAction) -> String {
    match action {
        InteractionAction::Click { selector } => {
            let selector = js_string(selector);
            format!(
                r#"(() => {{
  try {{
    const selector = {selector};
    const element = document.querySelector(selector);
    if (!element) return {{ ok:false, code:"selector_not_found", message:"selector did not match an element" }};
    if (typeof element.click !== "function" || element.disabled === true) return {{ ok:false, code:"selector_not_interactable", message:"matched element is not clickable" }};
    if (typeof element.scrollIntoView === "function") element.scrollIntoView({{ block:"center", inline:"center" }});
    if (typeof element.focus === "function") element.focus();
    element.click();
    return {{ ok:true, code:"ok", message:null }};
  }} catch (error) {{
    return {{ ok:false, code:"script_error", message:error && error.message ? String(error.message) : String(error) }};
  }}
}})()"#
            )
        }
        InteractionAction::Fill { selector, value } => {
            let selector = js_string(selector);
            let value = js_string(value);
            format!(
                r#"(() => {{
  try {{
    const selector = {selector};
    const value = {value};
    const element = document.querySelector(selector);
    if (!element) return {{ ok:false, code:"selector_not_found", message:"selector did not match an element" }};
    if (element.disabled === true || element.readOnly === true) return {{ ok:false, code:"selector_not_interactable", message:"matched element is disabled or read-only" }};
    const type = String(element.type || "").toLowerCase();
    if (["checkbox","radio","file","button","submit","reset"].includes(type)) return {{ ok:false, code:"selector_not_interactable", message:"matched input type is not fillable" }};
    if (typeof element.focus === "function") element.focus();
    if (element.isContentEditable === true) {{
      element.textContent = value;
    }} else if ("value" in element) {{
      const descriptor = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value");
      if (descriptor && typeof descriptor.set === "function") descriptor.set.call(element, value);
      else element.value = value;
    }} else {{
      return {{ ok:false, code:"selector_not_interactable", message:"matched element has no fillable value" }};
    }}
    element.dispatchEvent(new Event("input", {{ bubbles:true, composed:true }}));
    element.dispatchEvent(new Event("change", {{ bubbles:true, composed:true }}));
    return {{ ok:true, code:"ok", message:null }};
  }} catch (error) {{
    return {{ ok:false, code:"script_error", message:error && error.message ? String(error.message) : String(error) }};
  }}
}})()"#
            )
        }
        InteractionAction::PressKey { key, selector } => {
            let key = js_string(key);
            let selector = selector
                .as_deref()
                .map(js_string)
                .unwrap_or_else(|| "null".to_string());
            format!(
                r#"(() => {{
  try {{
    const selector = {selector};
    const key = {key};
    const target = selector === null ? document.activeElement : document.querySelector(selector);
    if (!target) return {{ ok:false, code:"selector_not_found", message:"key target is unavailable" }};
    if (typeof target.focus === "function") target.focus();
    const options = {{ key, bubbles:true, cancelable:true, composed:true }};
    target.dispatchEvent(new KeyboardEvent("keydown", options));
    if (key.length === 1) target.dispatchEvent(new KeyboardEvent("keypress", options));
    target.dispatchEvent(new KeyboardEvent("keyup", options));
    return {{ ok:true, code:"ok", message:null }};
  }} catch (error) {{
    return {{ ok:false, code:"script_error", message:error && error.message ? String(error.message) : String(error) }};
  }}
}})()"#
            )
        }
        InteractionAction::WaitForSelector { selector } => selector_probe(selector, false),
        InteractionAction::CheckSelector { selector } => selector_probe(selector, true),
        InteractionAction::WaitForText { text, selector } => text_probe(text, selector.as_deref()),
        InteractionAction::CheckText { text, selector } => text_probe(text, selector.as_deref()),
    }
}

fn selector_probe(selector: &str, _check: bool) -> String {
    let selector = js_string(selector);
    let missing_code = "selector_not_found";
    format!(
        r#"(() => {{
  try {{
    const selector = {selector};
    const matched = document.querySelector(selector) !== null;
    return matched
      ? {{ ok:true, code:"ok", message:null }}
      : {{ ok:false, code:"{missing_code}", message:"selector condition is not satisfied" }};
  }} catch (error) {{
    return {{ ok:false, code:"script_error", message:error && error.message ? String(error.message) : String(error) }};
  }}
}})()"#
    )
}

fn text_probe(text: &str, selector: Option<&str>) -> String {
    let text = js_string(text);
    let selector = selector
        .map(js_string)
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"(() => {{
  try {{
    const selector = {selector};
    const expected = {text};
    const root = selector === null ? document.body : document.querySelector(selector);
    if (!root) return {{ ok:false, code:"selector_not_found", message:"text root selector did not match an element" }};
    const matched = String(root.textContent || "").includes(expected);
    return matched
      ? {{ ok:true, code:"ok", message:null }}
      : {{ ok:false, code:"text_not_found", message:"text condition is not satisfied" }};
  }} catch (error) {{
    return {{ ok:false, code:"script_error", message:error && error.message ? String(error.message) : String(error) }};
  }}
}})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_validation_is_bounded_and_allows_empty_fill_value() {
        assert!(validate_request(
            &[InteractionAction::Fill {
                selector: "#name".to_string(),
                value: String::new(),
            }],
            4_000,
        )
        .is_ok());
        assert!(validate_request(&[], 4_000).is_err());
        assert!(validate_request(
            &[InteractionAction::Click {
                selector: " ".to_string(),
            }],
            4_000,
        )
        .is_err());
        assert!(validate_request(
            &[InteractionAction::Click {
                selector: "#ok".to_string(),
            }],
            MAX_INTERACTION_TIMEOUT_MS + 1,
        )
        .is_err());
    }

    #[test]
    fn selectors_values_text_and_keys_are_json_literals_not_raw_source() {
        let hostile = r#"\");globalThis.__PORTUS_PWNED__=true;//"#;
        for action in [
            InteractionAction::Click {
                selector: hostile.to_string(),
            },
            InteractionAction::Fill {
                selector: "#field".to_string(),
                value: hostile.to_string(),
            },
            InteractionAction::PressKey {
                key: hostile.to_string(),
                selector: Some("#field".to_string()),
            },
            InteractionAction::CheckText {
                text: hostile.to_string(),
                selector: None,
            },
        ] {
            let script = script_for(&action);
            let literal = js_string(hostile);
            assert!(script.contains(&literal));
            assert!(!script.contains("__TAURI__"));
            assert!(!script.contains("invoke("));
        }
    }

    #[test]
    fn wait_scripts_are_synchronous_predicates_without_page_timers() {
        let selector = script_for(&InteractionAction::WaitForSelector {
            selector: "#ready".to_string(),
        });
        let text = script_for(&InteractionAction::WaitForText {
            text: "done".to_string(),
            selector: None,
        });
        for script in [selector, text] {
            assert!(!script.contains("setTimeout"));
            assert!(!script.contains("setInterval"));
            assert!(!script.contains("Promise"));
        }
    }

    #[test]
    fn controlled_fixture_contains_every_interaction_primitive() {
        let fixture = include_str!("../tests/fixtures/interaction.html");
        for marker in [
            "id=\"name\"",
            "id=\"increment\"",
            "id=\"delay\"",
            "id=\"count\"",
            "id=\"key-result\"",
            "id=\"ready\"",
            "console.log('ready')",
        ] {
            assert!(fixture.contains(marker), "fixture is missing {marker}");
        }
    }

    #[test]
    fn interaction_results_are_bounded_before_transport() {
        use portus_window_protocol::{
            ActiveWindow, ConsoleEntry, ConsoleLevel, ConsoleResult, ContentKind, LoadState,
            SourceKind,
        };

        let entries = (0..200)
            .map(|_| ConsoleEntry {
                level: ConsoleLevel::Error,
                message: "x".repeat(4_096),
                source: Some("https://example.com/script.js".to_string()),
                line: Some(1),
            })
            .collect::<Vec<_>>();
        let status = ActiveWindow {
            window_session_id: "wsess_00000000000000000000000000000001".to_string(),
            source_kind: SourceKind::Web,
            content_kind: ContentKind::Web,
            requested_source: "https://example.com/".to_string(),
            current_url: Some("https://example.com/".to_string()),
            rendered_url: None,
            url_history: Some(
                (0..128)
                    .map(|i| format!("https://example.com/{i}/{}", "u".repeat(128)))
                    .collect(),
            ),
            url_history_truncated: Some(false),
            title: "Example".to_string(),
            load_state: LoadState::Loaded,
            load_error: None,
            console_errors: entries.clone(),
            console_errors_truncated: false,
            media_state: None,
            description: None,
            width: 1024,
            height: 768,
            x: 0,
            y: 0,
            is_maximized: false,
            is_minimized: false,
            is_focused: true,
            is_always_on_top: false,
            workspace: None,
            is_on_all_workspaces: false,
            workspace_history: Vec::new(),
            workspace_history_truncated: false,
            authenticated: Some(false),
            profile: None,
        };
        let result = InteractionResult {
            window_session_id: "wsess_00000000000000000000000000000001".to_string(),
            completed: true,
            actions: Vec::new(),
            post_status: Some(status),
            console: Some(ConsoleResult {
                window_session_id: "wsess_00000000000000000000000000000001".to_string(),
                entries,
                truncated: false,
            }),
            screenshot: None,
            post_errors: Vec::new(),
            output_truncated: false,
        };
        let bounded = bound_result_for_frame(result);
        assert!(bounded.output_truncated);
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= MAX_FRAME_BYTES - 1_024);
    }

    #[test]
    fn action_scripts_cover_expected_minimal_dom_surface() {
        let click = script_for(&InteractionAction::Click {
            selector: "#go".to_string(),
        });
        assert!(click.contains("element.click()"));

        let fill = script_for(&InteractionAction::Fill {
            selector: "#name".to_string(),
            value: "Ada".to_string(),
        });
        assert!(fill.contains("new Event(\"input\""));
        assert!(fill.contains("new Event(\"change\""));

        let key = script_for(&InteractionAction::PressKey {
            key: "Enter".to_string(),
            selector: Some("#name".to_string()),
        });
        assert!(key.contains("new KeyboardEvent(\"keydown\""));
        assert!(key.contains("new KeyboardEvent(\"keyup\""));
    }
}
