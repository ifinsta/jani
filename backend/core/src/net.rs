//! Shared URL inspection and bounded retry helpers for the HTTP clients.
//! Keeping these in the kernel prevents each transport from re-implementing
//! its own host parsing or backoff policy, which previously disagreed on
//! what counted as loopback.

use std::time::Duration;

/// Authority portion (host[:port]) of a URL, or of a bare authority string.
/// Returns an empty slice when the input is empty.
pub fn url_authority(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    rest.split('/').next().unwrap_or("")
}

/// Host portion of an authority, without the port or IPv6 brackets.
/// Returns an empty slice for malformed bracketed authorities.
pub fn authority_host(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) if tail.is_empty() || tail.starts_with(':') => host,
            _ => "",
        };
    }
    authority
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(authority)
}

/// True when the authority ends with an explicit `:port`.
pub fn authority_has_port(authority: &str) -> bool {
    match authority.rsplit_once(']') {
        Some((_, tail)) => tail.starts_with(':'),
        None => authority.contains(':'),
    }
}

/// True when the authority identifies the local host: `localhost`, `127.0.0.1`
/// or `::1`, with or without brackets and an optional port.
pub fn is_loopback_authority(authority: &str) -> bool {
    let host = authority_host(authority);
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// Shared backoff ceiling. Each service tunes its own base delay (model 250ms,
/// updater 400ms, env-overridable) but the cap on a single sleep is one policy.
pub const RETRY_CAP: Duration = Duration::from_secs(8);

/// Retry base delay from an env override holding milliseconds, falling back
/// to `default` when unset, unparsable or zero.
pub fn retry_base(env_name: &str, default: Duration) -> Duration {
    std::env::var(env_name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// Exponential backoff: `base * 2^(attempt-1)` bounded at 64x, then capped.
/// `attempt` is 1-based.
pub fn retry_delay(base: Duration, attempt: u64, cap: Duration) -> Duration {
    let factor = 1u64 << attempt.saturating_sub(1).min(6);
    base.saturating_mul(factor as u32).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_authority_extracts_host_and_port() {
        assert_eq!(url_authority("https://a.b:443/x"), "a.b:443");
        assert_eq!(url_authority("http://[::1]:9000/y"), "[::1]:9000");
        assert_eq!(url_authority("plain.host/path"), "plain.host");
        assert_eq!(url_authority(""), "");
    }

    #[test]
    fn authority_host_strips_port_and_brackets() {
        assert_eq!(authority_host("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(authority_host("localhost"), "localhost");
        assert_eq!(authority_host("[::1]:8080"), "::1");
        assert_eq!(authority_host("[::1]"), "::1");
        assert_eq!(authority_host("[::1]junk"), "");
    }

    #[test]
    fn loopback_detection() {
        for authority in [
            "localhost",
            "localhost:9",
            "127.0.0.1:9",
            "[::1]",
            "[::1]:9",
        ] {
            assert!(is_loopback_authority(authority), "{authority}");
        }
        for authority in ["::1", "127.0.0.1.evil.com:80", "0.0.0.0", "[::1]x:9", ""] {
            assert!(!is_loopback_authority(authority), "{authority}");
        }
    }

    #[test]
    fn port_detection() {
        assert!(authority_has_port("localhost:8080"));
        assert!(authority_has_port("[::1]:8080"));
        assert!(!authority_has_port("localhost"));
        assert!(!authority_has_port("[::1]"));
    }

    #[test]
    fn retry_delay_backoff() {
        let base = Duration::from_millis(250);
        let cap = Duration::from_secs(8);
        assert_eq!(retry_delay(base, 1, cap), base);
        assert_eq!(retry_delay(base, 2, cap), base * 2);
        assert_eq!(retry_delay(base, 6, cap), base * 32);
        assert_eq!(retry_delay(base, 7, cap), cap);
        assert_eq!(retry_delay(base, 100, cap), cap);
    }
}
