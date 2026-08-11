pub(crate) fn named_section_key(config: &str, name: &str) -> String {
    format!("{config}.{name}")
}

pub(crate) fn anonymous_section_key(config: &str, ty: &str, idx: usize) -> String {
    format!("{config}.@{ty}[{idx}]")
}

pub(crate) fn named_option_key(config: &str, name: &str, opt: &str) -> String {
    format!("{config}.{name}.{opt}")
}

pub(crate) fn anonymous_option_key(config: &str, ty: &str, idx: usize, opt: &str) -> String {
    format!("{config}.@{ty}[{idx}].{opt}")
}

/// Whether a `uci show` key refers to a named section root (`config.name`).
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
        assert!(!is_named_section_key("network.lan.proto"));
        assert!(!is_named_section_key("network.lan.foo"));
    }
}
