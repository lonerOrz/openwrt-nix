use std::borrow::Cow;

#[allow(dead_code)]
pub(crate) fn escape_single_quotes(s: &str) -> Cow<'_, str> {
    if s.contains('\'') {
        Cow::Owned(s.replace('\'', "'\\''"))
    } else {
        Cow::Borrowed(s)
    }
}

/// Write an optionally-escaped string directly into `out`.
/// Zero allocation when `s` contains no single quotes.
pub(crate) fn push_escaped_single_quotes(out: &mut String, s: &str) {
    if !s.contains('\'') {
        out.push_str(s);
        return;
    }
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
}

pub(crate) fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    push_escaped_single_quotes(&mut out, s);
    out.push('\'');
    out
}

pub(crate) fn extract_package_name(file_name: &str) -> &str {
    let without_ext = file_name
        .strip_suffix(".ipk")
        .or_else(|| file_name.strip_suffix(".apk"))
        .unwrap_or(file_name);

    if file_name.ends_with(".ipk") || without_ext.contains('_') {
        without_ext.split('_').next().unwrap_or(without_ext)
    } else {
        // Standard APK format: zlib-1.3.1-r1 or luci-theme-proton2025-1.2.9-r1.
        // The version is the first dash-part (after the name) starting with a digit.
        let parts: Vec<&str> = without_ext.split('-').collect();
        let split_idx = parts
            .iter()
            .skip(1)
            .position(|p| p.as_bytes().first().is_some_and(u8::is_ascii_digit))
            .map_or(parts.len(), |i| i + 1);
        if split_idx == parts.len() {
            // No version part (e.g. foo-bar.apk): the whole stem is the name
            without_ext
        } else {
            let name_len =
                parts[..split_idx].iter().map(|p| p.len()).sum::<usize>() + split_idx - 1;
            &without_ext[..name_len]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_single_quotes_zero_copy() {
        let plain = "hello";
        assert!(matches!(escape_single_quotes(plain), Cow::Borrowed(_)));

        let escaped = "it's";
        assert_eq!(escape_single_quotes(escaped).as_ref(), "it'\\''s");
    }

    #[test]
    fn push_escaped_single_quotes_no_alloc() {
        let mut out = String::new();
        push_escaped_single_quotes(&mut out, "hello");
        assert_eq!(out, "hello");

        out.clear();
        push_escaped_single_quotes(&mut out, "it's");
        assert_eq!(out, "it'\\''s");
    }

    #[test]
    fn extract_package_name_from_file() {
        for (input, expected) in [
            ("luci-app-nlbwmon_0.3-1_all.ipk", "luci-app-nlbwmon"),
            ("luci-app-nlbwmon_0.3-1_all.apk", "luci-app-nlbwmon"),
            ("luci.ipk", "luci"),
            ("luci-app_1.0", "luci-app"),
            ("zlib-1.3.1-r1.apk", "zlib"),
            (
                "luci-theme-proton2025-1.2.9-r1.apk",
                "luci-theme-proton2025",
            ),
            ("3proxy-0.9.3-r1.apk", "3proxy"),
            ("foo-bar.apk", "foo-bar"),
            ("foo.apk", "foo"),
        ] {
            assert_eq!(extract_package_name(input), expected);
        }
    }
}
