//! The demo server's settings, from its environment. Anything missing or wrong stops it with the reason (the client
//! sees it as the `initialize` error).
//!
//! - `AGENT_TOOLS_KEY_NAME`: the key's name, used in messages and access requests.
//! - `AGENT_TOOLS_KEY_SCOPES`: the key's scopes, comma-separated (`catalog:read,stock:write`). It must be set; empty
//!   means no scopes.
//! - `AGENT_TOOLS_RELEASE_LINK` (optional): a symlink to the release the binary runs from; when it moves, the server
//!   exits after its current answer so the client starts the new release (see `release.rs`).
//!
//! In the demo the environment says who the key is. A real host would resolve the key itself (for example by asking
//! its own API which name and scopes belong to the key it was given) and build `KeyInfo` from that answer.

use std::path::PathBuf;

use crate::tool::KeyInfo;

pub struct Config {
    pub key: KeyInfo,
    pub release_link: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        Config::from_vars(|name| std::env::var(name).ok())
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let name = var("AGENT_TOOLS_KEY_NAME").map(|name| name.trim().to_string()).filter(|name| !name.is_empty());
        let name = name.ok_or("AGENT_TOOLS_KEY_NAME is not set")?;
        let listed = var("AGENT_TOOLS_KEY_SCOPES").ok_or("AGENT_TOOLS_KEY_SCOPES is not set (set it empty for a key with no scopes)")?;
        let mut scopes = Vec::new();
        for scope in listed.split(',').map(str::trim).filter(|scope| !scope.is_empty()) {
            if scope.contains(char::is_whitespace) {
                return Err(format!("AGENT_TOOLS_KEY_SCOPES has {scope:?}; scopes are separated by commas and have no spaces"));
            }
            if !scopes.iter().any(|held: &String| held == scope) {
                scopes.push(scope.to_string());
            }
        }
        let release_link =
            var("AGENT_TOOLS_RELEASE_LINK").map(|link| link.trim().to_string()).filter(|link| !link.is_empty()).map(PathBuf::from);
        Ok(Config { key: KeyInfo { name, scopes }, release_link })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config, String> {
        let vars: HashMap<&str, &str> = vars.iter().copied().collect();
        Config::from_vars(|name| vars.get(name).map(|value| value.to_string()))
    }

    #[test]
    fn a_good_environment_reads_back() -> Result<(), String> {
        let read =
            config(&[("AGENT_TOOLS_KEY_NAME", " helper-bot "), ("AGENT_TOOLS_KEY_SCOPES", "catalog:read, stock:write,,catalog:read")])?;
        assert_eq!(read.key, KeyInfo::new("helper-bot", &["catalog:read", "stock:write"]));
        assert_eq!(read.release_link, None);
        let read =
            config(&[("AGENT_TOOLS_KEY_NAME", "k"), ("AGENT_TOOLS_KEY_SCOPES", ""), ("AGENT_TOOLS_RELEASE_LINK", "/opt/my-app/current")])?;
        assert!(read.key.scopes.is_empty());
        assert_eq!(read.release_link, Some(PathBuf::from("/opt/my-app/current")));
        Ok(())
    }

    #[test]
    fn missing_or_bad_settings_are_named() {
        assert_eq!(config(&[("AGENT_TOOLS_KEY_SCOPES", "")]).err().as_deref(), Some("AGENT_TOOLS_KEY_NAME is not set"));
        assert!(config(&[("AGENT_TOOLS_KEY_NAME", "k")]).err().unwrap_or_default().starts_with("AGENT_TOOLS_KEY_SCOPES is not set"));
        assert!(config(&[("AGENT_TOOLS_KEY_NAME", "k"), ("AGENT_TOOLS_KEY_SCOPES", "catalog:read stock:write")]).is_err());
    }
}
