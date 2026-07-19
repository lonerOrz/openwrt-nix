//! UCI section-key construction and classification.
//!
//! Every UCI section has a textual key: `config.name` for a named section and
//! `config.@type[idx]` for an anonymous (list) section. The shape of that key
//! is the single source of truth for "is this a named section?" — both the
//! generator (which builds keys) and the deploy/diff code (which parse keys
//! back from `uci show` output) must agree on it. Before this module existed
//! the rule was reimplemented in four places and could silently diverge.

/// Build the root key of a named section: `config.name`.
pub(crate) fn named_section_key(config: &str, name: &str) -> String {
    format!("{config}.{name}")
}

/// Build the root key of an anonymous (list) section: `config.@type[idx]`.
pub(crate) fn anonymous_section_key(config: &str, ty: &str, idx: usize) -> String {
    format!("{config}.@{ty}[{idx}]")
}

/// Build an option key beneath a named section: `config.name.opt`.
pub(crate) fn named_option_key(config: &str, name: &str, opt: &str) -> String {
    format!("{config}.{name}.{opt}")
}

/// Build an option key beneath an anonymous section: `config.@type[idx].opt`.
pub(crate) fn anonymous_option_key(config: &str, ty: &str, idx: usize, opt: &str) -> String {
    format!("{config}.@{ty}[{idx}].{opt}")
}

/// Whether a `uci show` key refers to a named section root (`config.name`).
///
/// The inverse of the anonymous shape: a named section key has no `@` marker,
/// no `[idx]`, and exactly one dot separating config from section name. This
/// is the *only* place the named/anonymous distinction is decided; callers
/// must route through here instead of re-deriving it from string heuristics.
pub(crate) fn is_named_section_key(key: &str) -> bool {
    !key.contains('@') && !key.contains('[') && key.matches('.').count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_named_and_anonymous_keys() {
        assert_eq!(named_section_key("network", "lan"), "network.lan");
        assert_eq!(
            anonymous_section_key("system", "system", 0),
            "system.@system[0]"
        );
        assert_eq!(
            named_option_key("network", "lan", "proto"),
            "network.lan.proto"
        );
        assert_eq!(
            anonymous_option_key("system", "system", 0, "hostname"),
            "system.@system[0].hostname"
        );
    }

    #[test]
    fn classifies_named_vs_anonymous() {
        assert!(is_named_section_key("network.lan"));
        assert!(is_named_section_key("wireless.default_radio0"));
        assert!(!is_named_section_key("system.@system[0]"));
        assert!(!is_named_section_key("config.@type[2]"));
        // two dots => nested option key, not a section root
        assert!(!is_named_section_key("network.lan.proto"));
        // a section name that itself contains a dot must not be called named
        assert!(!is_named_section_key("network.lan.foo"));
    }
}
