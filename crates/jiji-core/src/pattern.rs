/// Matches `value` against a `*`-wildcard pattern (e.g. `"web*"`, `"10.0.0.*"`, `"*-1-*"`).
/// Every other character, including other regex metacharacters, is matched literally.
pub fn matches_pattern(value: &str, pattern: &str) -> bool {
    let value: Vec<char> = value.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    matches(&value, &pattern)
}

fn matches(value: &[char], pattern: &[char]) -> bool {
    match pattern.split_first() {
        None => value.is_empty(),
        Some((&'*', rest)) => {
            matches(value, rest) || (!value.is_empty() && matches(&value[1..], pattern))
        }
        Some((p, rest)) => value.first() == Some(p) && matches(&value[1..], rest),
    }
}

#[cfg(test)]
mod tests {
    use super::matches_pattern;

    #[test]
    fn exact_match_without_wildcard() {
        assert!(matches_pattern("web1", "web1"));
        assert!(!matches_pattern("web1", "web2"));
    }

    #[test]
    fn prefix_wildcard() {
        assert!(matches_pattern("web1", "web*"));
        assert!(matches_pattern("web-frontend", "web*"));
        assert!(!matches_pattern("db1", "web*"));
    }

    #[test]
    fn suffix_wildcard() {
        assert!(matches_pattern("10.0.0.5", "10.0.0.*"));
        assert!(!matches_pattern("10.0.1.5", "10.0.0.*"));
    }

    #[test]
    fn wildcard_in_middle() {
        assert!(matches_pattern("web-frontend-1", "web-*-1"));
        assert!(!matches_pattern("web-frontend-2", "web-*-1"));
    }

    #[test]
    fn multiple_wildcards() {
        assert!(matches_pattern("a-b-c", "*-*-*"));
        assert!(!matches_pattern("abc", "*-*-*"));
    }

    #[test]
    fn literal_dots_do_not_act_as_regex_wildcards() {
        assert!(matches_pattern("10.0.0.5", "10.0.0.5"));
        assert!(!matches_pattern("10x0x0x5", "10.0.0.5"));
    }

    #[test]
    fn bare_wildcard_matches_everything() {
        assert!(matches_pattern("anything", "*"));
        assert!(matches_pattern("", "*"));
    }

    #[test]
    fn empty_pattern_only_matches_empty_value() {
        assert!(matches_pattern("", ""));
        assert!(!matches_pattern("x", ""));
    }
}
