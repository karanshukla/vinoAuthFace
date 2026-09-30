//! "Is there a newer release?", for `vinoauthface doctor` and the tray.
//!
//! Never in the authentication path. One anonymous GET to GitHub's releases
//! API through `curl` (already needed by deploy.sh, and it keeps an HTTP/TLS
//! stack out of these static binaries). Any failure means "don't know", never
//! an error. `update_check = false` turns it off.

use std::process::{Command, Stdio};

/// The release tag the build was stamped with (`VINOAUTHFACE_VERSION`: CI, or deploy.sh
/// building a release), or "dev".
pub const CURRENT: &str = match option_env!("VINOAUTHFACE_VERSION") {
    Some(v) => v,
    None => "dev",
};

const API_URL: &str = "https://api.github.com/repos/karanshukla/vinoAuthFace/releases?per_page=1";
pub const RELEASES_URL: &str = "https://github.com/karanshukla/vinoAuthFace/releases";

/// The leading number of a release tag: `v2` is 2, and the older `v1.1` is 1.
/// Anything else (a `dev` build) has none, and is never told to update.
pub fn release_number(tag: &str) -> Option<u64> {
    let rest = tag.strip_prefix('v')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

pub fn is_newer(current: &str, latest: &str) -> bool {
    matches!((release_number(current), release_number(latest)), (Some(c), Some(l)) if l > c)
}

/// The first `"tag_name"` value in a releases API response, if it looks like a
/// tag. The value ends up in menus and URLs, so anything but `v`, digits, dots
/// and dashes is refused rather than passed on.
pub fn latest_tag(json: &str) -> Option<String> {
    let after = &json[json.find("\"tag_name\"")? + "\"tag_name\"".len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let tag = &after[..after.find('"')?];
    let valid = tag.len() <= 32
        && tag.starts_with('v')
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    valid.then(|| tag.to_string())
}

/// The newest release tag of any kind, or `None` if it can't be fetched.
pub fn fetch_latest_tag() -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "5",
            "--max-filesize",
            "1048576",
            "-H",
            "Accept: application/vnd.github+json",
            API_URL,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    latest_tag(&String::from_utf8_lossy(&out.stdout))
}

/// The newer release to offer, if the check is possible and one exists.
pub fn newer_release(current: &str) -> Option<String> {
    release_number(current)?;
    fetch_latest_tag().filter(|latest| is_newer(current, latest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_numbers() {
        assert_eq!(release_number("v2"), Some(2));
        assert_eq!(release_number("v10"), Some(10));
        assert_eq!(release_number("v1.1"), Some(1));
        assert_eq!(release_number("v0.0.1"), Some(0));
        assert_eq!(release_number("dev"), None);
        assert_eq!(release_number("2"), None);
        assert_eq!(release_number("v"), None);
    }

    #[test]
    fn newer_means_a_higher_leading_number() {
        assert!(is_newer("v2", "v3"));
        assert!(is_newer("v9", "v10"), "compared as numbers, not strings");
        assert!(is_newer("v1.1", "v2"));
        assert!(!is_newer("v2", "v2"));
        assert!(!is_newer("v3", "v2"));
        assert!(!is_newer("v1.1", "v1.0.0"));
        assert!(!is_newer("dev", "v3"), "a dev build is never told to update");
        assert!(!is_newer("v2", "nightly"));
    }

    #[test]
    fn tag_is_read_from_the_releases_response() {
        let json = r#"[{"url":"x","tag_name": "v3","name":"v3","body":"tag_name"}]"#;
        assert_eq!(latest_tag(json).as_deref(), Some("v3"));
        assert_eq!(latest_tag(r#"[{"tag_name":"v1.1"}]"#).as_deref(), Some("v1.1"));
        assert_eq!(latest_tag("[]"), None);
        assert_eq!(latest_tag(r#"{"message":"rate limited"}"#), None);
    }

    #[test]
    fn hostile_tags_are_refused() {
        for bad in ["v3\\\"; rm", "v3 <b>", "../v3", "v3/../../x", "", "notav3"] {
            let json = format!(r#"[{{"tag_name":"{bad}"}}]"#);
            assert_eq!(latest_tag(&json), None, "{bad}");
        }
        let long = format!(r#"[{{"tag_name":"v{}"}}]"#, "1".repeat(64));
        assert_eq!(latest_tag(&long), None);
    }
}
