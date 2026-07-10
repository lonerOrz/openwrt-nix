use serde_json::{Map, Value};

pub(crate) fn escape_single_quotes(s: &str) -> String {
    s.replace('\'', "'\\''")
}

pub(crate) fn iter_options(map: &Map<String, Value>) -> impl Iterator<Item = (&str, &Value)> {
    map.iter()
        .filter(|(k, _)| k.as_str() != "_type")
        .map(|(k, v)| (k.as_str(), v))
}

pub(crate) fn iter_options_mut(
    map: &mut Map<String, Value>,
) -> impl Iterator<Item = (&str, &mut Value)> {
    map.iter_mut()
        .filter(|(k, _)| k.as_str() != "_type")
        .map(|(k, v)| (k.as_str(), v))
}

pub(crate) fn extract_package_name(file_name: &str) -> &str {
    let without_ext = file_name
        .strip_suffix(".ipk")
        .or_else(|| file_name.strip_suffix(".apk"))
        .unwrap_or(file_name);
    without_ext.split('_').next().unwrap_or(without_ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_no_quotes() {
        assert_eq!(escape_single_quotes("hello"), "hello");
    }

    #[test]
    fn escape_with_quotes() {
        assert_eq!(escape_single_quotes("it's"), "it'\\''s");
    }

    #[test]
    fn extract_pkg_standard() {
        assert_eq!(
            extract_package_name("luci-app-nlbwmon_0.3-1_all.ipk"),
            "luci-app-nlbwmon"
        );
    }

    #[test]
    fn extract_pkg_apk_extension() {
        assert_eq!(
            extract_package_name("luci-app-nlbwmon_0.3-1_all.apk"),
            "luci-app-nlbwmon"
        );
    }

    #[test]
    fn extract_pkg_no_version() {
        assert_eq!(extract_package_name("luci.ipk"), "luci");
    }

    #[test]
    fn extract_pkg_no_extension() {
        assert_eq!(extract_package_name("luci-app_1.0"), "luci-app");
    }
}
