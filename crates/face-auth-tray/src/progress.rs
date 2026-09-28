//! `face-enroll`'s stdout, read back into progress. Its human-readable lines
//! are the only channel from the root process to the tray: no new IPC, and
//! nothing root writes anywhere the user controls.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// About to capture frame `frame` of `wanted`.
    Capturing { frame: usize, wanted: usize },
    Captured { captured: usize, wanted: usize },
    /// Something the user can fix by moving.
    Hint(&'static str),
    /// The closing "Saved …" or "Added …" summary.
    Finished(String),
}

/// One line of `face-enroll` output, or `None` for the banner and anything
/// else not worth showing.
pub fn parse(line: &str) -> Option<Event> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("Capturing frame ") {
        let (frame, wanted) = fraction(rest.split_whitespace().next()?)?;
        return Some(Event::Capturing { frame, wanted });
    }
    if let Some(rest) = line.strip_prefix("captured ") {
        let (captured, wanted) = fraction(rest)?;
        return Some(Event::Captured { captured, wanted });
    }
    if line.starts_with("Saved ") || line.starts_with("Added ") {
        return Some(Event::Finished(line.to_owned()));
    }
    let hint = match line {
        "nothing in frame, retrying..." => "Nothing in frame: look at the camera",
        "no face detected, retrying..." => "No face detected: look at the camera",
        "face too small, move closer..." => "Face too small: move closer",
        _ => return None,
    };
    Some(Event::Hint(hint))
}

fn fraction(s: &str) -> Option<(usize, usize)> {
    let (a, b) = s.split_once('/')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Tooltip and notification text for an event.
pub fn describe(event: &Event) -> String {
    match event {
        Event::Capturing { frame, wanted } => format!("Capturing frame {frame}/{wanted}"),
        Event::Captured { captured, wanted } => format!("Captured {captured}/{wanted}"),
        Event::Hint(hint) => (*hint).to_owned(),
        Event::Finished(summary) => summary.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact lines crates/face-enroll/src/main.rs prints.
    #[test]
    fn reads_every_progress_line_face_enroll_prints() {
        assert_eq!(
            parse("Capturing frame 3/30 (attempt 4)..."),
            Some(Event::Capturing { frame: 3, wanted: 30 })
        );
        assert_eq!(parse("  captured 3/30"), Some(Event::Captured { captured: 3, wanted: 30 }));
        assert_eq!(parse("  face too small, move closer..."), Some(Event::Hint("Face too small: move closer")));
        assert!(matches!(parse("  no face detected, retrying..."), Some(Event::Hint(_))));
        assert!(matches!(parse("  nothing in frame, retrying..."), Some(Event::Hint(_))));
        assert_eq!(
            parse("Added 30 embeddings for 'alice' (60 total)"),
            Some(Event::Finished("Added 30 embeddings for 'alice' (60 total)".into()))
        );
        assert!(matches!(parse("Saved 30 embeddings for 'alice'"), Some(Event::Finished(_))));
    }

    #[test]
    fn ignores_the_banner_and_anything_malformed() {
        for line in [
            "Enrolling 'alice'",
            "  camera:     /dev/video2",
            "",
            "Capturing frame x/30 (attempt 1)...",
            "Capturing frame 3 (attempt 1)...",
            "  captured 3/",
            "  sudo ./pin-camera.sh /dev/video2",
        ] {
            assert_eq!(parse(line), None, "{line:?}");
        }
    }

    #[test]
    fn describes_progress_as_a_count() {
        assert_eq!(describe(&Event::Capturing { frame: 3, wanted: 30 }), "Capturing frame 3/30");
    }
}
