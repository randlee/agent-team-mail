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

    #[test]
    fn test_resolve_alias_empty_string_key() {
        let mut aliases = HashMap::new();
        aliases.insert(String::new(), "nobody".to_string());

        // Empty string lookup should resolve if the empty string is a key
        assert_eq!(resolve_alias("", &aliases), "nobody");
    }
}
