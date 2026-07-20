//! Credential scrubbing for any text that may embed a device request URL.
//!
//! A vendor client (`tasmota-core`, `shelly-core`) builds device request
//! URLs with credentials in the query string or as HTTP basic auth
//! (`http://host/cm?cmnd=...&user=admin&password=SECRET`), and the
//! underlying HTTP client attaches the full request URL to transport-level
//! errors (timeout, connection refused, DNS failure). A raw
//! `switchkit::Error` rendered to text can therefore carry a device's
//! plaintext password, so every sink that turns one into a string (an HTTP
//! response body, a log line, or stored fleet state) must scrub it first
//! with [`scrub_credentials`].

/// Redacts the values of `user=` and `password=` query-string parameters
/// (case-insensitive key match) wherever they appear in `s`, replacing each
/// value with `***`.
///
/// The redacted span is everything from immediately after `=` up to the next
/// `&`, ASCII whitespace, or `:` (whichever comes first), or the end of the
/// string if none of those appear. This bounds the redaction to the
/// credential value itself, so the rest of a diagnostic message (host, error
/// kind) stays intact and useful for debugging.
///
/// Deliberately a manual scan rather than a regex: no regex crate is a
/// dependency of this crate, and pulling one in just for a fixed two-key
/// scrub would be a disproportionate addition.
pub fn scrub_credentials(s: &str) -> String {
    scrub_key(&scrub_key(s, "user="), "password=")
}

/// Redacts every case-insensitive occurrence of `key` (which must end in
/// `=`) in `s`.
fn scrub_key(s: &str, key: &str) -> String {
    let key_len = key.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let is_match = s
            .get(i..i + key_len)
            .map(|slice| slice.eq_ignore_ascii_case(key))
            .unwrap_or(false);
        if is_match {
            out.push_str(&s[i..i + key_len]);
            i += key_len;
            let value_start = i;
            while i < s.len() {
                let c = s[i..].chars().next().expect("i < s.len()");
                if c == '&' || c == ':' || c.is_whitespace() {
                    break;
                }
                i += c.len_utf8();
            }
            if i > value_start {
                out.push_str("***");
            }
        } else {
            let c = s[i..].chars().next().expect("i < s.len()");
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::scrub_credentials;

    /// The exact failure string a real unreachable-device-with-password error
    /// produces: `tasmota-core` builds the request URL with credentials in
    /// the query string, and `ureq` attaches the full URL to its transport
    /// error's Display. Neither the username value nor the password may
    /// survive scrubbing; the host and the rest of the diagnostic text must,
    /// so the message stays useful for debugging.
    #[test]
    fn scrubs_the_real_connection_failure_string() {
        let input = "192.0.2.123: http://192.0.2.123/cm?cmnd=Status%200&user=admin&password=SUPER_SECRET_PW: Connection Failed: Connect error: connection timed out";
        let out = scrub_credentials(input);
        assert!(
            !out.contains("SUPER_SECRET_PW"),
            "password leaked into scrubbed output: {out}"
        );
        assert!(
            !out.contains("user=admin"),
            "username value leaked into scrubbed output: {out}"
        );
        assert!(
            out.contains("192.0.2.123"),
            "host should survive scrubbing for debugging: {out}"
        );
        assert!(
            out.contains("Connection Failed"),
            "error kind should survive scrubbing for debugging: {out}"
        );
    }

    /// A `password=` value at the very end of the string (no trailing `&`,
    /// whitespace, or `:`) must still be fully redacted.
    #[test]
    fn scrubs_a_password_with_no_trailing_delimiter() {
        let out = scrub_credentials("password=SECRET");
        assert!(!out.contains("SECRET"));
        assert_eq!(out, "password=***");
    }

    /// The key match is case-insensitive: a capitalized `Password=` (as a
    /// hand-written log line or device response might use) is redacted too.
    #[test]
    fn scrubs_a_capitalized_password_key() {
        let out = scrub_credentials("Password=Secret123");
        assert!(!out.contains("Secret123"));
        assert_eq!(out, "Password=***");
    }

    #[test]
    fn scrubs_a_capitalized_user_key() {
        let out = scrub_credentials("User=admin");
        assert!(!out.contains("admin"));
        assert_eq!(out, "User=***");
    }

    /// A message with no credentials at all must pass through unchanged.
    #[test]
    fn leaves_text_without_credentials_untouched() {
        let out = scrub_credentials("192.0.2.10 returned HTTP 500");
        assert_eq!(out, "192.0.2.10 returned HTTP 500");
    }
}
