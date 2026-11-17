//! The file as lines, and editing one key in it without disturbing the rest.

use super::Key;

/// What one line of the file is.
#[derive(Debug, PartialEq)]
enum Line {
    /// `[table]` or `[[array]]`: the table name, or `None` for an array.
    Header(Option<String>),
    /// `name = value`, the name with its table prefixed (`liveness.grace_ms`
    /// for `grace_ms` under `[liveness]`). `eq` is the byte offset of the `=`.
    Key { name: String, eq: usize },
    /// `# name = value` with nothing before the `#`: an example line.
    Example { name: String },
    Other,
}

/// A key's name without spaces or quotes: `liveness . "grace_ms"` and
/// `liveness.grace_ms` are the same key.
fn normal(name: &str) -> Option<String> {
    let name: String = name.chars().filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'').collect();
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    ok.then_some(name)
}

fn classify(line: &str, table: Option<&str>) -> Line {
    let t = line.trim_start();
    if t.starts_with("[[") {
        return Line::Header(None);
    }
    if let Some(rest) = t.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return Line::Header(normal(&rest[..end]));
        }
    }
    if let Some(rest) = t.strip_prefix('#') {
        if let Some((name, _)) = rest.split_once('=') {
            // A commented-out key is one word; prose has spaces (pinned by
            // `tests::prose_that_holds_an_equals_sign_is_not_an_example_line`).
            if !name.trim().contains(' ') {
                if let Some(name) = normal(name) {
                    return Line::Example { name };
                }
            }
        }
        return Line::Other;
    }
    if let Some(eq) = line.find('=') {
        if let Some(name) = normal(&line[..eq]) {
            let name = match table {
                Some(table) => format!("{table}.{name}"),
                None => name,
            };
            return Line::Key { name, eq };
        }
    }
    Line::Other
}

/// The table an array-of-tables line opens: a name none of our keys has.
const ARRAY_OF_TABLES: &str = "\0";

/// Each line with what it is and the table it's in (`None` at the top).
fn lines(text: &str) -> Vec<(&str, Line, Option<String>)> {
    let mut table: Option<String> = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let kind = classify(line, table.as_deref());
        if let Line::Header(name) = &kind {
            table = Some(name.clone().unwrap_or_else(|| ARRAY_OF_TABLES.into()));
        }
        out.push((line, kind, table.clone()));
    }
    out
}

/// The spellings of `key` as a full name: flat first.
fn names(key: Key) -> Vec<String> {
    let mut names: Vec<String> = key.flat.iter().map(|f| f.to_string()).collect();
    if let Some((table, name)) = key.group {
        names.push(format!("{table}.{name}"));
    }
    names
}

fn comment_out(line: &str) -> String {
    format!("# {}", line.trim_start())
}

fn join(lines: &[String], trailing_newline: bool) -> String {
    let mut out = lines.join("\n");
    if trailing_newline && !out.is_empty() {
        out.push('\n');
    }
    out
}

/// `text` with `key` set to `value` (a literal from [`float`], [`int`],
/// [`boolean`] or [`string`]), or commented out when `value` is `None`.
pub fn edit(text: &str, key: Key, value: Option<&str>) -> String {
    let parsed = lines(text);
    let names = names(key);
    let mut out: Vec<String> = parsed.iter().map(|(line, _, _)| line.to_string()).collect();
    let trailing = text.is_empty() || text.ends_with('\n');

    // Lines that set the key now, by spelling, flat first.
    let active: Vec<usize> = names
        .iter()
        .flat_map(|want| {
            parsed.iter().enumerate().filter_map(move |(i, (_, kind, _))| match kind {
                Line::Key { name, .. } if name == want => Some(i),
                _ => None,
            })
        })
        .collect();

    let Some(value) = value else {
        for i in active {
            out[i] = comment_out(&out[i]);
        }
        return join(&out, trailing);
    };

    if let Some((&keep, rest)) = active.split_first() {
        // The flat spelling wins; the other is commented out so the file
        // doesn't say two things (pinned by
        // `tests::the_flat_spelling_wins_and_the_other_is_retired`).
        if let Line::Key { eq, .. } = parsed[keep].1 {
            out[keep] = tidy(&parsed[keep].0[..eq], value);
        }
        for &i in rest {
            out[i] = comment_out(&out[i]);
        }
        return join(&out, trailing);
    }

    let written = key.written();
    let header_of = |table: &str| {
        parsed.iter().position(|(_, kind, _)| matches!(kind, Line::Header(Some(name)) if name == table))
    };
    // A table opened with a header can't also take dotted keys from the top
    // (pinned by `tests::a_grouped_key_joins_its_table_if_it_has_a_header`).
    if let Some((table, name)) = key.group {
        if let Some(header) = header_of(table) {
            let end = parsed[header + 1..]
                .iter()
                .position(|(_, kind, _)| matches!(kind, Line::Header(_)))
                .map_or(parsed.len(), |n| header + 1 + n);
            let at = last_content(&parsed, header + 1, end);
            out.insert(at, format!("{name} = {value}"));
            return join(&out, trailing);
        }
    }

    let top_end = parsed.iter().position(|(_, kind, _)| matches!(kind, Line::Header(_))).unwrap_or(parsed.len());
    // Over the example line, where the file explains it (pinned by
    // `tests::a_new_value_takes_its_example_line`).
    if let Some(i) = (0..top_end).find(|&i| {
        matches!(&parsed[i].1, Line::Example { name } if names.contains(name) || *name == written)
    }) {
        out[i] = format!("{written} = {value}");
        return join(&out, trailing);
    }
    let at = if top_end == parsed.len() { parsed.len() } else { last_content(&parsed, 0, top_end) };
    out.insert(at, format!("{written} = {value}"));
    join(&out, trailing)
}

/// `name = value`, keeping the line's indentation and spelling of the name.
fn tidy(before_eq: &str, value: &str) -> String {
    format!("{} = {value}", before_eq.trim_end())
}

/// Where a new last key of lines `from..to` goes: after its last key or
/// header, so the comments and blank lines that lead into the next table stay
/// with it.
fn last_content(parsed: &[(&str, Line, Option<String>)], from: usize, to: usize) -> usize {
    (from..to)
        .rev()
        .find(|&i| matches!(parsed[i].1, Line::Key { .. }))
        .map_or(from, |i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::*;
    use face_auth_core::FaceAuthConfig;

    fn parse(text: &str) -> FaceAuthConfig {
        FaceAuthConfig::from_toml(text).unwrap_or_else(|e| panic!("{e:#}\n---\n{text}"))
    }

    const HAND_EDITED: &str = "\
# My notes: raised after the false accept in March.
threshold = 0.6
my_own_key = \"kept\"   # not one of ours

[experimental]
# nothing we know about
frames = 3
";

    #[test]
    fn comments_and_unknown_keys_survive_an_edit() {
        let out = edit(HAND_EDITED, SCAN_DURATION_MS, Some("3000"));
        let without_the_new_line: Vec<&str> = out.lines().filter(|l| *l != "scan_duration_ms = 3000").collect();
        assert_eq!(without_the_new_line, HAND_EDITED.lines().collect::<Vec<_>>());
        assert_eq!(out.lines().count(), HAND_EDITED.lines().count() + 1);

        let out = edit(HAND_EDITED, THRESHOLD, Some("0.7"));
        let changed: Vec<(&str, &str)> = HAND_EDITED.lines().zip(out.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(changed, [("threshold = 0.6", "threshold = 0.7")]);

        let out = edit(HAND_EDITED, THRESHOLD, None);
        let changed: Vec<(&str, &str)> = HAND_EDITED.lines().zip(out.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(changed, [("threshold = 0.6", "# threshold = 0.6")]);
    }

    /// The other side of the rule above: the one line an edit rewrites is
    /// rewritten whole, so a comment after its value goes with the old value.
    #[test]
    fn a_comment_after_the_edited_value_is_not_kept() {
        let out = edit("threshold = 0.6 # mine\nscan_duration_ms = 5000 # also mine\n", THRESHOLD, Some("0.7"));
        assert_eq!(out, "threshold = 0.7\nscan_duration_ms = 5000 # also mine\n");
    }

    #[test]
    fn an_unknown_key_with_one_of_our_names_in_another_table_is_not_ours() {
        let text = "[experimental]\nthreshold = 9\n";
        let out = edit(text, THRESHOLD, Some("0.7"));
        assert_eq!(out, "threshold = 0.7\n[experimental]\nthreshold = 9\n");
    }

    #[test]
    fn a_value_is_replaced_where_it_is() {
        let text = "# Cosine similarity\nthreshold = 0.6\nscan_duration_ms = 5000\n";
        assert_eq!(
            edit(text, THRESHOLD, Some("0.7")),
            "# Cosine similarity\nthreshold = 0.7\nscan_duration_ms = 5000\n"
        );
        assert_eq!(edit("  threshold=0.6 # mine\n", THRESHOLD, Some("0.7")), "  threshold = 0.7\n");
    }

    #[test]
    fn a_new_value_takes_its_example_line() {
        let text = "# Fraction changed.\n# liveness.motion_threshold = 0.01\n\nthreshold = 0.6\n";
        let out = edit(text, LIVENESS_MOTION, Some("0.02"));
        assert_eq!(out, "# Fraction changed.\nliveness.motion_threshold = 0.02\n\nthreshold = 0.6\n");
        assert_eq!(parse(&out).liveness_motion_threshold(), 0.02);
    }

    #[test]
    fn prose_that_holds_an_equals_sign_is_not_an_example_line() {
        let prose = "# the user sets threshold = higher\n";
        assert_eq!(edit(prose, THRESHOLD, Some("0.7")), "# the user sets threshold = higher\nthreshold = 0.7\n");
    }

    #[test]
    fn a_new_value_goes_above_any_table() {
        let text = "threshold = 0.6\n\n# Lockout\n[lockout]\nthreshold = 5\n";
        let out = edit(text, SCAN_DURATION_MS, Some("3000"));
        assert_eq!(out, "threshold = 0.6\nscan_duration_ms = 3000\n\n# Lockout\n[lockout]\nthreshold = 5\n");
        assert_eq!(parse(&out).scan_duration_ms(), 3000);
        assert_eq!(parse(&out).lockout_policy().threshold, 5);
    }

    #[test]
    fn a_grouped_key_joins_its_table_if_it_has_a_header() {
        let text = "threshold = 0.6\n[lockout]\nthreshold = 5\n\n# next\n[guards]\nseat_check = true\n";
        let out = edit(text, LOCKOUT_MAX_DELAY_MS, Some("60000"));
        assert_eq!(
            out,
            "threshold = 0.6\n[lockout]\nthreshold = 5\nmax_delay_ms = 60000\n\n# next\n[guards]\nseat_check = true\n"
        );
        let c = parse(&out);
        assert_eq!(c.lockout_policy().max_delay_ms, 60000);
        assert!(c.seat_check());
    }

    #[test]
    fn a_grouped_key_is_a_dotted_key_at_the_top_without_a_header() {
        let out = edit("threshold = 0.6\n", LOCKOUT_MAX_DELAY_MS, Some("60000"));
        assert_eq!(out, "threshold = 0.6\nlockout.max_delay_ms = 60000\n");
    }

    #[test]
    fn a_value_set_inside_a_table_is_replaced_there() {
        let text = "[guards]\nabort_if_ssh = true\n";
        let out = edit(text, ABORT_IF_SSH, Some("false"));
        assert_eq!(out, "[guards]\nabort_if_ssh = false\n");
        assert!(!parse(&out).abort_if_ssh());
    }

    #[test]
    fn the_flat_spelling_wins_and_the_other_is_retired() {
        let text = "liveness.grace_ms = 2000\nliveness_grace_ms = 3000\n";
        let out = edit(text, LIVENESS_GRACE_MS, Some("1000"));
        assert_eq!(out, "# liveness.grace_ms = 2000\nliveness_grace_ms = 1000\n");
        assert_eq!(parse(&out).liveness_grace_ms(), 1000);
    }

    #[test]
    fn a_default_is_commented_out_in_every_spelling() {
        let text = "threshold = 0.7\n[liveness]\ngrace_ms = 2000\n";
        assert_eq!(edit(text, THRESHOLD, None), "# threshold = 0.7\n[liveness]\ngrace_ms = 2000\n");
        let out = edit(text, LIVENESS_GRACE_MS, None);
        assert_eq!(out, "threshold = 0.7\n[liveness]\n# grace_ms = 2000\n");
        assert_eq!(parse(&out).liveness_grace_ms(), 4000);
    }

    #[test]
    fn a_commented_out_default_set_again_comes_back_where_it_was() {
        let again = edit("# threshold = 0.7\n", THRESHOLD, Some("0.8"));
        assert_eq!(again, "threshold = 0.8\n");
    }

    #[test]
    fn an_empty_file_gets_one_line() {
        assert_eq!(edit("", DEVICE, Some(&string("/dev/video2"))), "device = \"/dev/video2\"\n");
        assert_eq!(edit("", DEVICE, None), "");
        assert_eq!(edit("threshold = 0.6", SCAN_DURATION_MS, Some("3000")), "threshold = 0.6\nscan_duration_ms = 3000");
    }

    /// Every key, set on the shipped example, reads back through the real
    /// loader as the value written, only that key changes, and unsetting it
    /// again leaves it unset.
    #[test]
    fn every_key_edits_the_shipped_example() {
        let example = include_str!("../../../../config/face-auth.toml.example");
        let samples = |key: Key| -> &'static str {
            match key {
                LIVENESS_PRESET => "\"strict\"",
                BACKEND => "\"openvino\"",
                NPU_DEVICE => "\"CPU\"",
                START_DELAY_SCOPE => "\"all\"",
                DEVICE | MODEL_PATH | DETECTOR_MODEL_PATH | EMBEDDINGS_DIR | PINNED_CAMERA_PATH => "\"/x/y\"",
                THRESHOLD | DETECTOR_THRESHOLD => "0.77",
                MIN_FACE_SIZE_RATIO | LIVENESS_MOTION | LIVENESS_RESIDUAL => "0.25",
                SEAL_EMBEDDINGS | BIND_CAMERA | UPDATE_CHECK | SEAT_CHECK | ABORT_IF_SSH | ABORT_IF_LID_CLOSED
                | REQUIRE_CONFIRMATION => "false",
                _ => "1234",
            }
        };
        let before: toml::Table = toml::from_str(example).unwrap();
        for key in ALL {
            let out = edit(example, key, Some(samples(key)));
            let after: toml::Table = toml::from_str(&out).unwrap_or_else(|e| panic!("{key:?}: {e}"));
            let want: toml::Value = toml::from_str::<toml::Table>(&format!("v = {}", samples(key))).unwrap()["v"].clone();
            assert_eq!(key.value(&after), Some(&want), "{key:?}");
            for other in ALL.iter().filter(|o| **o != key) {
                assert_eq!(other.value(&after), other.value(&before), "{key:?} changed {other:?}");
            }
            FaceAuthConfig::from_toml(&out).unwrap_or_else(|e| panic!("{key:?}: {e:#}"));
            let back: toml::Table = toml::from_str(&edit(&out, key, None)).unwrap();
            assert_eq!(key.value(&back), None, "{key:?} stayed set");
        }
    }
}
