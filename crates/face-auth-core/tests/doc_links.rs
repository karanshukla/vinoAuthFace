//! Every test a comment cites must be a `#[test]` fn, so a rule that names its
//! test fails CI when the test is renamed or deleted. rustdoc can't check
//! these: `#[test]` fns are compiled out of the docs. Scans every crate in the
//! workspace. See docs/comment-style.md.

use std::fs;
use std::path::{Path, PathBuf};

/// What introduces a citation. Changing the convention is changing this.
const PHRASE: &str = "(pinned by";

#[test]
fn every_cited_test_exists() {
    let mut broken = Vec::new();
    for src in crate_sources() {
        for file in rust_files(&src) {
            let text = fs::read_to_string(&file).unwrap();
            for (line_no, cited) in citations(&text) {
                if !resolves(&src, &text, &cited) {
                    broken.push(format!("{}:{line_no}: `{cited}`", file.display()));
                }
            }
        }
    }
    assert!(broken.is_empty(), "no such test:\n{}", broken.join("\n"));
}

#[test]
fn every_test_a_comment_names_is_cited_with_the_phrase() {
    let mut loose = Vec::new();
    for src in crate_sources() {
        for file in rust_files(&src) {
            let text = fs::read_to_string(&file).unwrap();
            let cited: Vec<usize> = citations(&text).into_iter().map(|(line_no, _)| line_no).collect();
            for (i, line) in text.lines().enumerate() {
                let named = line.trim_start().starts_with("//") && !test_spans(line).is_empty();
                if named && !cited.contains(&(i + 1)) {
                    loose.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(loose.is_empty(), "cite tests as {PHRASE} `tests::name`):\n{}", loose.join("\n"));
}

#[test]
fn the_check_finds_a_cited_test_and_rejects_a_missing_one() {
    let text = "/// Rule (pinned by `tests::real`,\n/// `tests::gone`).\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn real() {}\n    fn helper() {}\n}\n";
    let found: Vec<_> = citations(text).into_iter().map(|(_, c)| c).collect();
    assert_eq!(found, ["tests::real", "tests::gone"]);
    let src = Path::new("src");
    assert!(resolves(src, text, "tests::real"));
    assert!(!resolves(src, text, "tests::helper"), "not a #[test]");
    assert!(!resolves(src, text, "tests::gone"));
}

#[test]
fn a_test_named_without_the_phrase_is_not_a_citation() {
    assert!(citations("// see `tests::real`\n").is_empty());
    assert_eq!(test_spans("// see `tests::real`"), ["tests::real"]);
}

/// Each workspace crate's `src/`.
fn crate_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut sources: Vec<PathBuf> = fs::read_dir(crates)
        .unwrap()
        .flatten()
        .map(|entry| entry.path().join("src"))
        .filter(|src| src.is_dir())
        .collect();
    sources.sort();
    assert!(sources.len() > 1, "found no crates beside {}", crates.display());
    sources
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
    files
}

/// `(line, test)` for every test cited after [`PHRASE`], up to its closing
/// parenthesis, which may be some comment lines further down.
fn citations(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut open = false;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            open = false;
            continue;
        }
        let mut rest = trimmed;
        loop {
            if !open {
                let Some(at) = rest.find(PHRASE) else { break };
                rest = &rest[at + PHRASE.len()..];
                open = true;
            }
            let (inside, closed) = match rest.find(')') {
                Some(end) => (&rest[..end], true),
                None => (rest, false),
            };
            found.extend(test_spans(inside).into_iter().map(|t| (i + 1, t.to_string())));
            if !closed {
                break;
            }
            rest = &rest[inside.len() + 1..];
            open = false;
        }
    }
    found
}

/// Code spans like `` `tests::name` `` or `` `module::tests::name` ``.
fn test_spans(line: &str) -> Vec<&str> {
    line.split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| {
            span.contains("tests::") && span.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        })
        .collect()
}

/// `tests::name` is in `text`; `module::tests::name` in `src/module.rs` (or
/// `src/module/mod.rs`) of the same crate.
fn resolves(src: &Path, text: &str, cited: &str) -> bool {
    let (module, name) = cited.rsplit_once("tests::").unwrap();
    let module = module.trim_end_matches("::");
    if module.is_empty() {
        return is_test_fn(text, name);
    }
    let base = src.join(module.replace("::", "/"));
    [base.with_extension("rs"), base.join("mod.rs")]
        .iter()
        .any(|file| fs::read_to_string(file).is_ok_and(|text| is_test_fn(&text, name)))
}

/// A `fn name(` whose attributes, above it, include `#[test]`.
fn is_test_fn(text: &str, name: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    let signature = format!("fn {name}(");
    lines.iter().enumerate().any(|(i, line)| {
        line.trim_start().starts_with(&signature)
            && lines[..i]
                .iter()
                .rev()
                .map(|l| l.trim())
                .take_while(|l| l.starts_with("#[") || l.starts_with("///"))
                .any(|l| l == "#[test]")
    })
}
