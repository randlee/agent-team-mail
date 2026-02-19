//! Developer-instructions injection for Codex tool calls.
//!
//! At the start of every `codex` or `codex-reply` turn the proxy injects a
//! session-context block into the `developer-instructions` field of the tool
//! arguments. This gives the Codex agent the current identity, team, and git
//! context without the caller needing to pass it manually.
//!
//! # Injection rules
//!
//! - If `developer-instructions` already exists in the params object, the
//!   context block is **appended** with a newline separator.
//! - If `developer-instructions` is absent, it is **set** to the context
//!   block.
//! - `base-instructions` is **never touched** (FR-2.3).

use serde_json::Value;

/// Build the session-context block for injection into `developer-instructions`.
///
/// # Arguments
///
/// * `identity` — ATM identity bound to this session.
/// * `team` — ATM team name.
/// * `repo_name` — Git repository name, or `None` when not in a git repo.
/// * `repo_root` — Absolute git repository root path, or `None`.
/// * `branch` — Current git branch, or `None`.
/// * `cwd` — Effective working directory.
///
/// # Returns
///
/// A formatted string wrapped in `<session-context>…</session-context>` tags.
///
/// # Examples
///
/// ```
/// use atm_agent_mcp::inject::build_session_context;
///
/// let ctx = build_session_context(
///     "arch-ctm",
///     "atm-dev",
///     Some("agent-team-mail"),
///     Some("/home/user/agent-team-mail"),
///     Some("develop"),
///     "/home/user/agent-team-mail",
/// );
/// assert!(ctx.contains("Identity:  arch-ctm"));
/// assert!(ctx.contains("Team:      atm-dev"));
/// ```
pub fn build_session_context(
    identity: &str,
    team: &str,
    repo_name: Option<&str>,
    repo_root: Option<&str>,
    branch: Option<&str>,
    cwd: &str,
) -> String {
    let repo_name_str = repo_name.unwrap_or("null");
    let repo_root_str = repo_root.unwrap_or("null");
    let branch_str = branch.unwrap_or("null");

    format!(
        "<session-context>\nIdentity:  {identity}\nTeam:      {team}\nRepo:      {repo_name_str} ({repo_root_str})\nBranch:    {branch_str}\nCWD:       {cwd}\n</session-context>"
    )
}

/// Inject a session-context string into `developer-instructions` in `params`.
///
/// `params` must be a JSON object (the `arguments` field of a `tools/call`
/// request). If `params` is not an object this function does nothing.
///
/// # Behaviour
///
/// - **Appends** to `developer-instructions` when already present (separated
///   by `"\n"`).
/// - **Sets** `developer-instructions` when absent.
/// - **Never** modifies `base-instructions` (FR-2.3).
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use atm_agent_mcp::inject::inject_developer_instructions;
///
/// let mut params = json!({"prompt": "hello"});
/// inject_developer_instructions(&mut params, "<session-context>…</session-context>");
/// assert!(params["developer-instructions"].as_str().is_some());
/// ```
pub fn inject_developer_instructions(params: &mut Value, context: &str) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };

    if let Some(existing) = obj.get_mut("developer-instructions") {
        if let Some(s) = existing.as_str() {
            let appended = format!("{s}\n{context}");
            *existing = Value::String(appended);
        } else {
            // Existing value is not a string — replace it
            *existing = Value::String(context.to_string());
        }
    } else {
        obj.insert(
            "developer-instructions".to_string(),
            Value::String(context.to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─── build_session_context ────────────────────────────────────────────────

    #[test]
    fn build_session_context_formats_correctly() {
        let ctx = build_session_context(
            "arch-ctm",
            "atm-dev",
            Some("agent-team-mail"),
            Some("/home/user/agent-team-mail"),
            Some("develop"),
            "/home/user/agent-team-mail",
        );
        assert!(ctx.contains("<session-context>"), "missing opening tag");
        assert!(ctx.contains("</session-context>"), "missing closing tag");
        assert!(ctx.contains("Identity:  arch-ctm"));
        assert!(ctx.contains("Team:      atm-dev"));
        assert!(ctx.contains("Repo:      agent-team-mail (/home/user/agent-team-mail)"));
        assert!(ctx.contains("Branch:    develop"));
        assert!(ctx.contains("CWD:       /home/user/agent-team-mail"));
    }

    #[test]
    fn build_session_context_null_repo_fields_when_not_in_git() {
        let ctx = build_session_context(
            "dev-agent",
            "atm-dev",
            None,
            None,
            None,
            "/tmp/workspace",
        );
        assert!(ctx.contains("Repo:      null (null)"));
        assert!(ctx.contains("Branch:    null"));
    }

    #[test]
    fn inject_null_repo_fields_when_not_in_git() {
        let ctx = build_session_context("dev", "team", None, None, None, "/tmp");
        let mut params = json!({});
        inject_developer_instructions(&mut params, &ctx);
        let di = params["developer-instructions"].as_str().unwrap();
        assert!(di.contains("null (null)"));
    }

    // ─── inject_developer_instructions ───────────────────────────────────────

    #[test]
    fn inject_sets_when_no_existing_developer_instructions() {
        let mut params = json!({"prompt": "hello"});
        inject_developer_instructions(&mut params, "ctx-block");
        assert_eq!(params["developer-instructions"], "ctx-block");
    }

    #[test]
    fn inject_appends_to_existing_developer_instructions() {
        let mut params = json!({"developer-instructions": "original"});
        inject_developer_instructions(&mut params, "appended");
        let di = params["developer-instructions"].as_str().unwrap();
        assert_eq!(di, "original\nappended");
    }

    #[test]
    fn inject_does_not_modify_base_instructions() {
        let mut params = json!({
            "base-instructions": "do not touch me",
            "prompt": "hello"
        });
        inject_developer_instructions(&mut params, "ctx");
        // base-instructions must be unchanged
        assert_eq!(params["base-instructions"], "do not touch me");
        // developer-instructions must be set
        assert_eq!(params["developer-instructions"], "ctx");
    }

    #[test]
    fn inject_on_non_object_does_nothing() {
        let mut params = json!("not an object");
        inject_developer_instructions(&mut params, "ctx");
        // Should remain unchanged
        assert_eq!(params, json!("not an object"));
    }

    #[test]
    fn inject_on_null_params_does_nothing() {
        let mut params = json!(null);
        inject_developer_instructions(&mut params, "ctx");
        assert_eq!(params, json!(null));
    }

    #[test]
    fn inject_replaces_non_string_developer_instructions() {
        let mut params = json!({"developer-instructions": 42});
        inject_developer_instructions(&mut params, "ctx");
        // Non-string value should be replaced
        assert_eq!(params["developer-instructions"], "ctx");
    }

    #[test]
    fn inject_multiple_times_appends_each_time() {
        let mut params = json!({});
        inject_developer_instructions(&mut params, "first");
        inject_developer_instructions(&mut params, "second");
        inject_developer_instructions(&mut params, "third");
        let di = params["developer-instructions"].as_str().unwrap();
        assert_eq!(di, "first\nsecond\nthird");
    }
}
