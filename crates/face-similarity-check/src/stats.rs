//! Accept/reject rates over genuine and impostor score sets.

/// Fraction of `scores` at or above `threshold`. Empty sets give 0.
pub fn accept_rate(scores: &[f32], threshold: f32) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    scores.iter().filter(|&&s| s >= threshold).count() as f64 / scores.len() as f64
}

/// Parse `start:end:step` into the thresholds it covers, end inclusive.
pub fn parse_sweep(spec: &str) -> Result<Vec<f32>, String> {
    let parts: Vec<f32> = spec
        .split(':')
        .map(|p| {
            p.parse::<f32>()
                .map_err(|_| format!("bad number {p:?} in sweep {spec:?}"))
        })
        .collect::<Result<_, _>>()?;
    let [start, end, step] = parts[..] else {
        return Err(format!("sweep must be start:end:step, got {spec:?}"));
    };
    if !(start.is_finite() && end.is_finite() && step.is_finite()) || step <= 0.0 || end < start {
        return Err(format!("sweep {spec:?} needs step > 0 and end >= start"));
    }
    let n = ((end - start) / step + 1e-4).floor() as usize;
    Ok((0..=n).map(|i| start + i as f32 * step).collect())
}

/// Equal error rate: the score threshold where FAR and FRR are closest, and
/// their mean there. Candidates are the observed scores, so it does not depend
/// on a sweep grid. `None` unless both sets are non-empty.
pub fn equal_error_rate(genuine: &[f32], impostor: &[f32]) -> Option<(f32, f64)> {
    if genuine.is_empty() || impostor.is_empty() {
        return None;
    }
    genuine
        .iter()
        .chain(impostor)
        .map(|&t| {
            let far = accept_rate(impostor, t);
            let frr = 1.0 - accept_rate(genuine, t);
            (t, far, frr)
        })
        .min_by(|a, b| (a.1 - a.2).abs().total_cmp(&(b.1 - b.2).abs()))
        .map(|(t, far, frr)| (t, (far + frr) / 2.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_rate_counts_at_or_above_threshold() {
        let s = [0.2, 0.5, 0.5, 0.9];
        assert_eq!(accept_rate(&s, 0.5), 0.75);
        assert_eq!(accept_rate(&s, 0.95), 0.0);
        assert_eq!(accept_rate(&[], 0.5), 0.0);
    }

    #[test]
    fn sweep_includes_both_ends() {
        let t = parse_sweep("0.30:0.40:0.05").unwrap();
        assert_eq!(t.len(), 3);
        assert!((t[0] - 0.30).abs() < 1e-6 && (t[2] - 0.40).abs() < 1e-6);
    }

    #[test]
    fn sweep_rejects_bad_specs() {
        for bad in [
            "0.3:0.7",
            "0.3:0.7:0",
            "0.7:0.3:0.1",
            "a:b:c",
            "0.3:0.7:-0.1",
        ] {
            assert!(parse_sweep(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn eer_is_zero_for_separable_sets() {
        let (t, eer) = equal_error_rate(&[0.8, 0.9], &[0.1, 0.2]).unwrap();
        assert_eq!(eer, 0.0);
        assert!(t > 0.2 && t <= 0.8);
    }

    #[test]
    fn eer_reflects_overlap() {
        // One genuine below one impostor: 50% FRR meets 50% FAR at 0.5.
        let (_, eer) = equal_error_rate(&[0.4, 0.9], &[0.5, 0.1]).unwrap();
        assert!((eer - 0.5).abs() < 1e-9, "{eer}");
    }

    #[test]
    fn eer_needs_both_sets() {
        assert!(equal_error_rate(&[0.9], &[]).is_none());
    }
}
