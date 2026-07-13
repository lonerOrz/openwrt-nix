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

    if file_name.ends_with(".ipk") || without_ext.contains('_') {
        without_ext.split('_').next().unwrap_or(without_ext)
    } else {
        // Standard APK format: zlib-1.3.1-r1 or luci-theme-proton2025-1.2.9-r1
        let parts: Vec<&str> = without_ext.split('-').collect();
        if parts.len() <= 1 {
            return without_ext;
        }

        let mut split_idx = parts.len();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 && !part.is_empty() && part.chars().next().unwrap().is_ascii_digit() {
                split_idx = i;
                break;
            }
        }

        if split_idx == parts.len() {
            if parts.len() > 2 {
                split_idx = parts.len() - 2;
            } else {
                split_idx = parts.len() - 1;
            }
        }

        let mut end_pos = 0;
        for (i, part) in parts.iter().enumerate().take(split_idx) {
            if i > 0 {
                end_pos += 1;
            }
            end_pos += part.len();
        }
        &without_ext[..end_pos]
    }
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

    #[test]
    fn extract_pkg_apk_hyphen_format() {
        assert_eq!(extract_package_name("zlib-1.3.1-r1.apk"), "zlib");
        assert_eq!(
            extract_package_name("luci-theme-proton2025-1.2.9-r1.apk"),
            "luci-theme-proton2025"
        );
        assert_eq!(extract_package_name("3proxy-0.9.3-r1.apk"), "3proxy");
    }
}
