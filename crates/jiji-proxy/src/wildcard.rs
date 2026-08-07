//! Wildcard subdomain host matching, shared by `route_manager.rs` (backend
//! routing) and `cert_store.rs` (TLS SNI resolution) -- both key their table
//! by exact host string, so both need the same "strip exactly one DNS label"
//! fallback to support a route configured for `*.example.com`. Matches
//! `foo.example.com` and `bar.example.com`; does not match the nested
//! `deep.foo.example.com` (only a route configured for the more specific
//! `*.foo.example.com` would), and does not match the bare `example.com`
//! itself, since stripping its own single label leaves no further label to
//! form a match against.

/// The wildcard key that would match `host`, if any route registered under
/// it should apply -- `foo.example.com` -> `Some("*.example.com")`,
/// `deep.foo.example.com` -> `Some("*.foo.example.com")` (deliberately not
/// `*.example.com`: only stripping the left-most label, once, is what keeps
/// wildcard matching to a single subdomain level). `None` for a host with no
/// label to strip (`localhost`).
pub fn parent_wildcard_key(host: &str) -> Option<String> {
    let (_, rest) = host.split_once('.')?;
    if rest.is_empty() {
        return None;
    }
    Some(format!("*.{rest}"))
}

/// Whether `host` is itself a wildcard pattern (as configured, e.g.
/// `*.example.com`), not whether it matches one.
pub fn is_wildcard_host(host: &str) -> bool {
    host.starts_with("*.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_label_subdomain_produces_the_direct_parent_wildcard_key() {
        assert_eq!(
            parent_wildcard_key("foo.example.com"),
            Some("*.example.com".to_string())
        );
    }

    #[test]
    fn nested_subdomain_produces_a_more_specific_key_not_the_broader_one() {
        assert_eq!(
            parent_wildcard_key("deep.foo.example.com"),
            Some("*.foo.example.com".to_string())
        );
    }

    #[test]
    fn host_with_no_label_to_strip_has_no_parent_wildcard_key() {
        assert_eq!(parent_wildcard_key("localhost"), None);
    }

    #[test]
    fn bare_domain_only_computes_its_own_narrow_parent_key() {
        // "example.com" strips to "*.com", never "*.example.com" -- a route
        // for "*.example.com" must never match the bare "example.com" host.
        assert_eq!(
            parent_wildcard_key("example.com"),
            Some("*.com".to_string())
        );
    }

    #[test]
    fn is_wildcard_host_checks_the_configured_pattern_itself() {
        assert!(is_wildcard_host("*.example.com"));
        assert!(!is_wildcard_host("foo.example.com"));
        assert!(!is_wildcard_host("example.com"));
    }
}
