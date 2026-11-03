//! Every key in `face-auth.toml`, and editing one in a file's text without
//! disturbing the rest of it.
//!
//! The file is hand-edited and heavily commented (the shipped example is
//! mostly comments), and deploy.sh and pin-camera.sh append plain keys to its
//! end. So edits are line by line, as `setting-mode.sh` makes them: a value
//! is replaced where it is, or written over its commented-out example line
//! (`# liveness.motion_threshold = 0.01`), or else added at the end of the
//! top-level keys. A key set back to its default is commented out, the way the
//! example shows a default. Comments and other keys are left as they were.

/// One setting, under each name it can be spelled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    /// The top-level spelling (`liveness_motion_threshold`), which wins
    /// over the grouped one. `None` for `liveness.preset`, which has none.
    pub flat: Option<&'static str>,
    /// The table and the key in it (`liveness`, `motion_threshold`): the
    /// spelling the example uses, and so the one written.
    pub group: Option<(&'static str, &'static str)>,
}

const fn flat(name: &'static str) -> Key {
    Key { flat: Some(name), group: None }
}

const fn grouped(flat: &'static str, table: &'static str, name: &'static str) -> Key {
    Key { flat: Some(flat), group: Some((table, name)) }
}

pub const DEVICE: Key = flat("device");
pub const THRESHOLD: Key = flat("threshold");
pub const MODEL_PATH: Key = flat("model_path");
pub const EMBEDDINGS_DIR: Key = flat("embeddings_dir");
pub const CAPTURE_TIMEOUT_MS: Key = flat("capture_timeout_ms");
pub const DETECTOR_MODEL_PATH: Key = flat("detector_model_path");
pub const DETECTOR_THRESHOLD: Key = flat("detector_threshold");
pub const SCAN_DURATION_MS: Key = flat("scan_duration_ms");
pub const SCAN_INTERVAL_MS: Key = flat("scan_interval_ms");
pub const BACKEND: Key = flat("backend");
pub const NPU_DEVICE: Key = flat("npu_device");
pub const MIN_FACE_SIZE_RATIO: Key = flat("min_face_size_ratio");
pub const PINNED_CAMERA_PATH: Key = flat("pinned_camera_path");
pub const PINNED_CAMERA_INDEX: Key = flat("pinned_camera_index");
pub const SEAL_EMBEDDINGS: Key = flat("seal_embeddings");
pub const BIND_CAMERA: Key = flat("bind_camera");
pub const UPDATE_CHECK: Key = flat("update_check");
pub const TRAY_IDLE_MINUTES: Key = flat("tray_idle_minutes");

pub const LIVENESS_PRESET: Key = Key { flat: None, group: Some(("liveness", "preset")) };
pub const LIVENESS_MOTION: Key = grouped("liveness_motion_threshold", "liveness", "motion_threshold");
pub const LIVENESS_RESIDUAL: Key =
    grouped("liveness_residual_motion_threshold", "liveness", "residual_motion_threshold");
pub const LIVENESS_WINDOW_MS: Key = grouped("liveness_window_ms", "liveness", "window_ms");
pub const LIVENESS_GRACE_MS: Key = grouped("liveness_grace_ms", "liveness", "grace_ms");

pub const LOCKOUT_THRESHOLD: Key = grouped("lockout_threshold", "lockout", "threshold");
pub const LOCKOUT_BASE_DELAY_MS: Key = grouped("lockout_base_delay_ms", "lockout", "base_delay_ms");
pub const LOCKOUT_MAX_DELAY_MS: Key = grouped("lockout_max_delay_ms", "lockout", "max_delay_ms");

pub const SEAT_CHECK: Key = grouped("seat_check", "guards", "seat_check");
pub const ABORT_IF_SSH: Key = grouped("abort_if_ssh", "guards", "abort_if_ssh");
pub const ABORT_IF_LID_CLOSED: Key = grouped("abort_if_lid_closed", "guards", "abort_if_lid_closed");
pub const START_DELAY_MS: Key = grouped("start_delay_ms", "guards", "start_delay_ms");
pub const START_DELAY_SCOPE: Key = grouped("start_delay_scope", "guards", "start_delay_scope");
pub const REQUIRE_CONFIRMATION: Key =
    grouped("require_confirmation_elevation", "guards", "require_confirmation_elevation");

/// Every key `FaceAuthConfig` reads. A test holds this to the struct.
pub const ALL: [Key; 32] = [
    DEVICE,
    THRESHOLD,
    MODEL_PATH,
    EMBEDDINGS_DIR,
    CAPTURE_TIMEOUT_MS,
    DETECTOR_MODEL_PATH,
    DETECTOR_THRESHOLD,
    SCAN_DURATION_MS,
    SCAN_INTERVAL_MS,
    BACKEND,
    NPU_DEVICE,
    MIN_FACE_SIZE_RATIO,
    PINNED_CAMERA_PATH,
    PINNED_CAMERA_INDEX,
    SEAL_EMBEDDINGS,
    BIND_CAMERA,
    UPDATE_CHECK,
    TRAY_IDLE_MINUTES,
    LIVENESS_PRESET,
    LIVENESS_MOTION,
    LIVENESS_RESIDUAL,
    LIVENESS_WINDOW_MS,
    LIVENESS_GRACE_MS,
    LOCKOUT_THRESHOLD,
    LOCKOUT_BASE_DELAY_MS,
    LOCKOUT_MAX_DELAY_MS,
    SEAT_CHECK,
    ABORT_IF_SSH,
    ABORT_IF_LID_CLOSED,
    START_DELAY_MS,
    START_DELAY_SCOPE,
    REQUIRE_CONFIRMATION,
];

impl Key {
    /// The name a new line is written with.
    pub fn written(self) -> String {
        match (self.group, self.flat) {
            (Some((table, name)), _) => format!("{table}.{name}"),
            (None, Some(flat)) => flat.to_string(),
            (None, None) => unreachable!("every key has a spelling"),
        }
    }

    /// The value set in a parsed file, flat spelling first, as
    /// `FaceAuthConfig::fold_sections` reads it.
    pub fn value(self, table: &toml::Table) -> Option<&toml::Value> {
        self.flat.and_then(|flat| table.get(flat)).or_else(|| {
            let (group, name) = self.group?;
            table.get(group)?.as_table()?.get(name)
        })
    }
}

// Values, as the file spells them

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

// Lines

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
            // Prose has spaces in it; a commented-out key is one word.
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

/// Each line with what it is and the table it's in (`None` at the top).
fn lines(text: &str) -> Vec<(&str, Line, Option<String>)> {
    let mut table: Option<String> = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let kind = classify(line, table.as_deref());
        if let Line::Header(name) = &kind {
            // An array of tables isn't a place for any of our keys.
            table = Some(name.clone().unwrap_or_else(|| "\0".into()));
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
            let line = out[i].trim_start().to_string();
            out[i] = format!("# {line}");
        }
        return join(&out, trailing);
    };

    if let Some((&keep, rest)) = active.split_first() {
        // The flat spelling, if any, wins: set that one and comment out the
        // grouped one so the file doesn't say two things.
        if let Line::Key { eq, .. } = parsed[keep].1 {
            out[keep] = tidy(&parsed[keep].0[..eq], value);
        }
        for &i in rest {
            let line = out[i].trim_start().to_string();
            out[i] = format!("# {line}");
        }
        return join(&out, trailing);
    }

    let written = key.written();
    let header_of = |table: &str| {
        parsed.iter().position(|(_, kind, _)| matches!(kind, Line::Header(Some(name)) if name == table))
    };
    // A table already opened with a header can't also take dotted keys from
    // the top: write the key inside it.
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
    // Over the example line, where the file explains it.
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
    use face_auth_core::FaceAuthConfig;

    fn parse(text: &str) -> FaceAuthConfig {
        FaceAuthConfig::from_toml(text).unwrap_or_else(|e| panic!("{e:#}\n---\n{text}"))
    }

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
        // Prose that happens to hold an "=" is left alone.
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
        // And a dotted key at the top otherwise.
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
        // Then set again, it comes back where it was.
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
    /// loader as the value written, and only that key changes.
    #[test]
    fn every_key_edits_the_shipped_example() {
        let example = include_str!("../../../config/face-auth.toml.example");
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
            // The real loader agrees, folding included.
            FaceAuthConfig::from_toml(&out).unwrap_or_else(|e| panic!("{key:?}: {e:#}"));
            // And unset again, it reads as the example did.
            let back: toml::Table = toml::from_str(&edit(&out, key, None)).unwrap();
            assert_eq!(key.value(&back), None, "{key:?} stayed set");
        }
    }

    /// `ALL` names every field `FaceAuthConfig` reads: a key added to the
    /// struct and not here would be a setting the window can't show.
    #[test]
    fn every_config_field_has_a_key() {
        let fields = format!("{:?}", FaceAuthConfig::from_toml("").unwrap());
        let inner = fields.trim_start_matches("FaceAuthConfig {").trim_end_matches('}');
        let mut missing = Vec::new();
        let fields: Vec<&str> = inner.split(", ").filter_map(|f| f.split_once(':').map(|(n, _)| n.trim())).collect();
        assert!(fields.len() > 30, "couldn't read the fields from {inner}");
        for field in fields {
            if matches!(field, "liveness" | "lockout" | "guards") {
                continue;
            }
            if !ALL.iter().any(|k| k.flat == Some(field)) {
                missing.push(field);
            }
        }
        assert!(missing.is_empty(), "no Key for {missing:?}");
    }
}
