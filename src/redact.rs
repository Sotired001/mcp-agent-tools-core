//! Keeps keys and tokens out of what the agent sees: every failure's text goes through `Redactor::redact` before it
//! leaves the server. Case-insensitive, it replaces
//!
//! - `Bearer <anything up to a space>`,
//! - `api_key=<value>`,
//! - tokens that start with one of the host's own prefixes (for example `sk_live_`), up to the first character that
//!   isn't a letter, digit, `_` or `-`,
//! - and each configured secret, as it is.
//!
//! The result is at most `MAX_ERROR_CHARS` characters.

const REDACTED: &str = "[REDACTED]";
pub const MAX_ERROR_CHARS: usize = 2000;

/// What to take out of failures: the host's token prefixes and its secrets (the API key the server runs with).
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    token_prefixes: Vec<String>,
    secrets: Vec<String>,
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

impl Redactor {
    pub fn new() -> Redactor {
        Redactor::default()
    }

    /// Tokens that start with `prefix` are redacted wherever they appear.
    pub fn token_prefix(mut self, prefix: &str) -> Redactor {
        if !prefix.trim().is_empty() {
            self.token_prefixes.push(prefix.trim().to_string());
        }
        self
    }

    /// `secret` is redacted wherever it appears. Blank secrets are ignored.
    pub fn secret(mut self, secret: &str) -> Redactor {
        if !secret.trim().is_empty() {
            self.secrets.push(secret.trim().to_string());
        }
        self
    }

    /// How many bytes of `rest` a sensitive pattern covers, if one starts here.
    fn sensitive_len(&self, rest: &str) -> Option<usize> {
        if starts_with_ignore_case(rest, "bearer") {
            let after = &rest["bearer".len()..];
            let spaces = after.len() - after.trim_start().len();
            let value = after[spaces..].find(char::is_whitespace).unwrap_or(after.len() - spaces);
            if spaces > 0 && value > 0 {
                return Some("bearer".len() + spaces + value);
            }
        }
        for prefix in &self.token_prefixes {
            if starts_with_ignore_case(rest, prefix) {
                let after = &rest[prefix.len()..];
                let value = after.find(|c: char| !token_char(c)).unwrap_or(after.len());
                if value > 0 {
                    return Some(prefix.len() + value);
                }
            }
        }
        if starts_with_ignore_case(rest, "api_key=") {
            let after = &rest["api_key=".len()..];
            let value = after.find(|c: char| c.is_whitespace() || "&,;\"'".contains(c)).unwrap_or(after.len());
            if value > 0 {
                return Some("api_key=".len() + value);
            }
        }
        None
    }

    /// `message` with every key and token replaced by `[REDACTED]`, cut to `MAX_ERROR_CHARS` characters.
    pub fn redact(&self, message: &str) -> String {
        let message = message.trim();
        let message = if message.is_empty() { "the tool failed without saying why" } else { message };
        let mut safe = String::with_capacity(message.len());
        let mut at = 0;
        while at < message.len() {
            let rest = &message[at..];
            if let Some(length) = self.sensitive_len(rest) {
                safe.push_str(REDACTED);
                at += length;
            } else if let Some(c) = rest.chars().next() {
                safe.push(c);
                at += c.len_utf8();
            } else {
                break;
            }
        }
        let safe = self.secrets.iter().fold(safe, |text, secret| text.replace(secret.as_str(), REDACTED));
        safe.chars().take(MAX_ERROR_CHARS).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_keys_are_redacted() {
        let redactor = Redactor::new().token_prefix("sk_live_").token_prefix("demo_");
        assert_eq!(redactor.redact("Authorization: Bearer abc.def ok"), "Authorization: [REDACTED] ok");
        assert_eq!(redactor.redact("key DEMO_abc-123_x, more"), "key [REDACTED], more");
        assert_eq!(redactor.redact("sk_live_ABC and demo_9"), "[REDACTED] and [REDACTED]");
        assert_eq!(redactor.redact("url?api_key=s3cret&x=1"), "url?[REDACTED]&x=1");
        assert_eq!(Redactor::new().secret("k-123").redact("the key is k-123 here"), "the key is [REDACTED] here");
    }

    #[test]
    fn plain_text_is_left_alone() {
        let redactor = Redactor::new().token_prefix("demo_").secret("  ");
        assert_eq!(redactor.redact("  HTTP 404: no such bike  "), "HTTP 404: no such bike");
        assert_eq!(redactor.redact("bearer"), "bearer");
        assert_eq!(redactor.redact("demo_"), "demo_");
        assert_eq!(redactor.redact(""), "the tool failed without saying why");
        assert_eq!(redactor.redact("café ünïcode"), "café ünïcode");
    }

    #[test]
    fn long_messages_are_cut() {
        assert_eq!(Redactor::new().redact(&"x".repeat(5000)).chars().count(), MAX_ERROR_CHARS);
    }
}
