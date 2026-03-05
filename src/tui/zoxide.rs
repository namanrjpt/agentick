use std::process::Command;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// A directory entry loaded from zoxide with its frecency score.
#[derive(Debug, Clone)]
pub struct ZoxideEntry {
    pub score: f64,
    pub path: String,
}

/// Load directories from zoxide, sorted by frecency score (highest first).
/// Returns Vec of (score, path) pairs.
/// If zoxide is not available, returns an empty vec.
pub fn load_zoxide_dirs() -> Vec<ZoxideEntry> {
    let output = match Command::new("zoxide").args(["query", "-ls"]).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    parse_zoxide_output(&stdout)
}

/// Parse raw zoxide `query -ls` output into entries.
fn parse_zoxide_output(output: &str) -> Vec<ZoxideEntry> {
    let mut entries: Vec<ZoxideEntry> = output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (score_str, path) = trimmed.split_once(' ')?;
            let score = score_str.parse::<f64>().ok()?;
            let path = path.trim().to_string();
            if path.is_empty() {
                return None;
            }
            Some(ZoxideEntry { score, path })
        })
        .collect();

    // zoxide outputs highest score first, but sort explicitly to be safe
    entries.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    entries
}

/// Filter directories by fuzzy matching against `query`.
/// Returns entries sorted by match score (best first), limited to `max_results`.
pub fn fuzzy_filter<'a>(
    entries: &'a [ZoxideEntry],
    query: &str,
    max_results: usize,
) -> Vec<&'a ZoxideEntry> {
    if query.is_empty() {
        return entries.iter().take(max_results).collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::new(query, CaseMatching::Smart, Normalization::Smart, AtomKind::Fuzzy);

    let mut buf = Vec::new();
    let mut scored: Vec<(&ZoxideEntry, u32)> = entries
        .iter()
        .filter_map(|entry| {
            let haystack = Utf32Str::new(&entry.path, &mut buf);
            pattern.score(haystack, &mut matcher).map(|s| (entry, s))
        })
        .collect();

    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().take(max_results).map(|(e, _)| e).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = "\
  42.5 /home/alice/projects/myapp
  38.2 /home/alice/projects/agentick
  12.1 /home/alice/work/foo
   4.0 /home/bob/bar
";

    #[test]
    fn parse_zoxide_output_basic() {
        let entries = parse_zoxide_output(SAMPLE_OUTPUT);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].path, "/home/alice/projects/myapp");
        assert!((entries[0].score - 42.5).abs() < f64::EPSILON);
        assert_eq!(entries[1].path, "/home/alice/projects/agentick");
        assert!((entries[1].score - 38.2).abs() < f64::EPSILON);
        assert_eq!(entries[3].path, "/home/bob/bar");
        assert!((entries[3].score - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_zoxide_output_empty() {
        let entries = parse_zoxide_output("");
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_zoxide_output_malformed_lines() {
        let output = "not_a_number /some/path\n  10.0 /valid/path\n\n";
        let entries = parse_zoxide_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/valid/path");
    }

    #[test]
    fn fuzzy_filter_empty_query_returns_all() {
        let entries = parse_zoxide_output(SAMPLE_OUTPUT);
        let results = fuzzy_filter(&entries, "", 10);
        assert_eq!(results.len(), 4);
        // Should preserve original (frecency) order
        assert_eq!(results[0].path, "/home/alice/projects/myapp");
    }

    #[test]
    fn fuzzy_filter_empty_query_respects_limit() {
        let entries = parse_zoxide_output(SAMPLE_OUTPUT);
        let results = fuzzy_filter(&entries, "", 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn fuzzy_filter_matches_substring() {
        let entries = parse_zoxide_output(SAMPLE_OUTPUT);
        let results = fuzzy_filter(&entries, "agentick", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].path, "/home/alice/projects/agentick");
    }

    #[test]
    fn fuzzy_filter_no_match() {
        let entries = parse_zoxide_output(SAMPLE_OUTPUT);
        let results = fuzzy_filter(&entries, "zzzzzznotexist", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn fuzzy_filter_respects_max_results() {
        let entries = parse_zoxide_output(SAMPLE_OUTPUT);
        let results = fuzzy_filter(&entries, "alice", 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn fuzzy_filter_ranks_best_match_first() {
        let entries = parse_zoxide_output(SAMPLE_OUTPUT);
        // "work" should match the path containing "work" best
        let results = fuzzy_filter(&entries, "work", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].path, "/home/alice/work/foo");
    }
}
