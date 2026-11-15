//! Values, as the file spells them: the literals `edit` writes.

pub fn float(v: f32) -> String {
    // Shortest form that reads back as v, always with a point: `1` would be
    // an integer, which toml won't read into an f32 field.
    let s = format!("{v}");
    if s.contains(['.', 'e', 'E']) || !v.is_finite() {
        s
    } else {
        format!("{s}.0")
    }
}

pub fn int(v: u64) -> String {
    v.to_string()
}

pub fn boolean(v: bool) -> String {
    v.to_string()
}

pub fn string(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_spelled_as_toml() {
        assert_eq!(float(0.6), "0.6");
        assert_eq!(float(1.0), "1.0");
        assert_eq!(float(0.0), "0.0");
        assert_eq!(string("/dev/video2"), "\"/dev/video2\"");
        assert_eq!(string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        let table: toml::Table = toml::from_str(&format!("x = {}", string("tab\there\u{1}"))).unwrap();
        assert_eq!(table["x"].as_str(), Some("tab\there\u{1}"));
    }
}
