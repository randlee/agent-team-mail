//! Identity alias resolution
//!
//! Aliases allow role-based names to map to actual inbox identities.
//! For example, `arch-atm` can resolve to `team-lead`.
//!
//! Aliases are configured in `.atm.toml` under the `[aliases]` section:
//!
//! ```toml
//! [aliases]
//! arch-atm = "team-lead"
//! dev = "worker-1"
//! ```

use std::collections::HashMap;

/// Resolve an identity through the alias table.
///
/// If `name` matches a key in `aliases`, returns the corresponding value.
/// Otherwise returns the original name unchanged (pass-through).
///
/// Alias resolution is case-sensitive and non-recursive: if the resolved
/// value is itself an alias key it is NOT resolved further.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use agent_team_mail_core::config::aliases::resolve_alias;
///
/// let mut aliases = HashMap::new();
/// aliases.insert("arch-atm".to_string(), "team-lead".to_string());
///
/// assert_eq!(resolve_alias("arch-atm", &aliases), "team-lead");
/// assert_eq!(resolve_alias("unknown", &aliases), "unknown");
/// ```
pub fn resolve_alias(name: &str, aliases: &HashMap<String, String>) -> String {
    aliases.get(name).cloned().unwrap_or_else(|| name.to_string())
}

/// Resolve an identity through the roles table first, then aliases.
///
/// Resolution order:
/// 1. Check `roles` map — if found, return resolved value (no further lookup)
/// 2. Check `aliases` map — if found, return resolved value
/// 3. Return original name unchanged (literal fallback)
///
/// Resolution is non-recursive and case-sensitive.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use agent_team_mail_core::config::resolve_identity;
///
/// let mut roles = HashMap::new();
/// roles.insert("team-lead".to_string(), "arch-atm".to_string());
///
/// let mut aliases = HashMap::new();
/// aliases.insert("arch-atm".to_string(), "team-lead".to_string());
///
/// // Role takes precedence: "team-lead" resolves via roles to "arch-atm"
/// assert_eq!(resolve_identity("team-lead", &roles, &aliases), "arch-atm");
/// // Alias used when no role matches
/// assert_eq!(resolve_identity("arch-atm", &roles, &aliases), "team-lead");
/// // Literal fallback when neither matches
/// assert_eq!(resolve_identity("unknown", &roles, &aliases), "unknown");
/// ```
pub fn resolve_identity(
    name: &str,
    roles: &HashMap<String, String>,
    aliases: &HashMap<String, String>,
) -> String {
    if let Some(resolved) = roles.get(name) {
        return resolved.clone();
    }
    aliases.get(name).cloned().unwrap_or_else(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_aliases() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("arch-atm".to_string(), "team-lead".to_string());
        m.insert("dev".to_string(), "worker-1".to_string());
        m
    }

    #[test]
    fn test_resolve_alias_known_name() {
        let aliases = make_aliases();
        assert_eq!(resolve_alias("arch-atm", &aliases), "team-lead");
    }

    #[test]
    fn test_resolve_alias_second_entry() {
        let aliases = make_aliases();
        assert_eq!(resolve_alias("dev", &aliases), "worker-1");
    }

    #[test]
    fn test_resolve_alias_passthrough_unknown() {
        let aliases = make_aliases();
        assert_eq!(resolve_alias("team-lead", &aliases), "team-lead");
    }

    #[test]
    fn test_resolve_alias_empty_map() {
        let aliases = HashMap::new();
        assert_eq!(resolve_alias("any-name", &aliases), "any-name");
    }

    #[test]
    fn test_resolve_alias_case_sensitive() {
        let aliases = make_aliases();
        // "Arch-Atm" is NOT the same as "arch-atm"
        assert_eq!(resolve_alias("Arch-Atm", &aliases), "Arch-Atm");
        assert_eq!(resolve_alias("ARCH-ATM", &aliases), "ARCH-ATM");
    }

    #[test]
    fn test_resolve_alias_non_recursive() {
        // Alias chains are NOT followed: if the resolved value is itself an alias
        // key it is returned as-is without further resolution.
        let mut aliases = HashMap::new();
        aliases.insert("a".to_string(), "b".to_string());
        aliases.insert("b".to_string(), "c".to_string());

        // "a" resolves to "b", but "b" is NOT resolved further to "c"
        assert_eq!(resolve_alias("a", &aliases), "b");
    }

    fn make_roles() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("team-lead".to_string(), "arch-atm".to_string());
        m
    }

    #[test]
    fn test_resolve_identity_role_takes_precedence_over_alias() {
        // When the same key appears in both roles and aliases, roles win.
        let mut roles = HashMap::new();
        roles.insert("team-lead".to_string(), "arch-atm".to_string());
        let mut aliases = HashMap::new();
        aliases.insert("team-lead".to_string(), "other-identity".to_string());

        assert_eq!(
            resolve_identity("team-lead", &roles, &aliases),
            "arch-atm",
            "role should take precedence over alias for same key"
        );
    }

    #[test]
    fn test_resolve_identity_alias_used_when_no_role_matches() {
        let roles = make_roles();
        let aliases = make_aliases();

        // "arch-atm" is in aliases but not roles → alias applies
        assert_eq!(resolve_identity("arch-atm", &roles, &aliases), "team-lead");
    }

    #[test]
    fn test_resolve_identity_literal_fallback() {
        let roles = make_roles();
        let aliases = make_aliases();

        // "unknown" not in either map → returned unchanged
        assert_eq!(resolve_identity("unknown", &roles, &aliases), "unknown");
    }

    #[test]
    fn test_resolve_identity_empty_maps_return_original() {
        let roles: HashMap<String, String> = HashMap::new();
        let aliases: HashMap<String, String> = HashMap::new();

        assert_eq!(resolve_identity("any-name", &roles, &aliases), "any-name");
    }

    #[test]
    fn test_resolve_identity_non_recursive() {
        // Resolution is not recursive: if roles value is itself a key, no further lookup.
        let mut roles = HashMap::new();
        roles.insert("a".to_string(), "b".to_string());
        let mut aliases = HashMap::new();
        aliases.insert("b".to_string(), "c".to_string());

        // "a" → roles → "b", but "b" is NOT resolved further through aliases
        assert_eq!(resolve_identity("a", &roles, &aliases), "b");
    }

    #[test]
    fn test_resolve_alias_empty_string_key() {
        let mut aliases = HashMap::new();
        aliases.insert(String::new(), "nobody".to_string());

        // Empty string lookup should resolve if the empty string is a key
        assert_eq!(resolve_alias("", &aliases), "nobody");
    }
}
