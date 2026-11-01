//! Who may call what. Port of `app/agents.py` and the cookie rules in `app/h7020_panel.py`.
//!
//! The house LAN accepts only the browser cookie. The agent socket accepts only a
//! bearer token. Sending the other credential is a refusal. Every comparison of a
//! secret runs over the bytes of both values, with no exit at the first difference
//! and none at a length mismatch.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The cookie the browser holds, as the Python sets it.
pub const COOKIE: &str = "tallow_panel";
/// The label the cookie HMAC is taken over. A browser logged in to Python stays logged in.
const SESSION_LABEL: &[u8] = b"tallow-h7020-panel";
pub const SCOPES: [&str; 4] = ["read", "control", "library", "schedule"];

/// Which listener a request came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Lan,
    Agent,
}

/// A token record from the agents file. The token itself is never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub token_sha256: String,
    pub scopes: Vec<String>,
}

/// Who a request is, and what it may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub actor: String,
    pub scopes: Vec<String>,
}

/// An agents file that cannot be used. `class` is the Python exception name the log shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFileError {
    pub class: &'static str,
    pub message: String,
}

impl AgentFileError {
    fn value(path: &Path, message: &str) -> Self {
        Self {
            class: "ValueError",
            message: format!("{} {message}", path.display()),
        }
    }
}

impl std::fmt::Display for AgentFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.class, self.message)
    }
}

/// The cookie value that proves a browser is logged in.
pub fn session_token(password: &str) -> String {
    // HMAC takes a key of any length, so this cannot fail.
    let mut mac = Hmac::<Sha256>::new_from_slice(password.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(SESSION_LABEL);
    hex(&mac.finalize().into_bytes())
}

pub fn sha256_hex(token: &[u8]) -> String {
    hex(&Sha256::digest(token))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Equal bytes, compared without an early exit. A length mismatch is folded into the
/// result rather than returned at once.
pub fn secret_eq(a: &[u8], b: &[u8]) -> bool {
    let mut difference = u64::try_from(a.len() ^ b.len()).unwrap_or(u64::MAX);
    let longest = a.len().max(b.len());
    for index in 0..longest {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
        difference |= u64::from(x ^ y);
    }
    difference == 0
}

/// The value of one named cookie, read the way Starlette reads it. The last pair with
/// the name wins, and a pair of surrounding double quotes is removed.
pub fn cookie_value<'a>(header: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let mut found = None;
    for pair in header.split(|byte| *byte == b';') {
        let (key, value) = match pair.iter().position(|byte| *byte == b'=') {
            Some(at) => (&pair[..at], &pair[at + 1..]),
            None => (&b""[..], pair),
        };
        let key = key.trim_ascii();
        let value = value.trim_ascii();
        if key != name.as_bytes() {
            continue;
        }
        let unquoted = match value {
            [b'"', inner @ .., b'"'] => inner,
            other => other,
        };
        found = Some(unquoted);
    }
    found
}

/// Decides who a request is. `cookie` is the browser cookie's value, if the request
/// carried one, and `authorization` is the raw Authorization header.
pub fn authenticate(
    channel: Channel,
    authorization: Option<&[u8]>,
    cookie: Option<&[u8]>,
    browser: &str,
    agents: &[Agent],
) -> Option<Principal> {
    match channel {
        Channel::Lan => {
            if authorization.is_some_and(|value| !value.is_empty()) {
                return None;
            }
            let cookie = cookie.filter(|value| !value.is_empty())?;
            if !secret_eq(cookie, browser.as_bytes()) {
                return None;
            }
            Some(Principal {
                actor: "browser".to_owned(),
                scopes: SCOPES.iter().map(|scope| (*scope).to_owned()).collect(),
            })
        }
        Channel::Agent => {
            if cookie.is_some_and(|value| !value.is_empty()) {
                return None;
            }
            let header = authorization?;
            let (scheme, secret) = match header.iter().position(|byte| *byte == b' ') {
                Some(at) => (&header[..at], &header[at + 1..]),
                None => (header, &b""[..]),
            };
            if !scheme.eq_ignore_ascii_case(b"bearer")
                || secret.is_empty()
                || secret != secret.trim_ascii()
                || secret.contains(&b' ')
            {
                return None;
            }
            let agent = match_agent(agents, secret)?;
            Some(Principal {
                actor: agent.name.clone(),
                scopes: agent.scopes.clone(),
            })
        }
    }
}

/// The agent whose token hashes to this one. Every record is compared.
pub fn match_agent<'a>(agents: &'a [Agent], token: &[u8]) -> Option<&'a Agent> {
    let digest = sha256_hex(token);
    let mut found = None;
    for agent in agents {
        if secret_eq(agent.token_sha256.as_bytes(), digest.as_bytes()) && found.is_none() {
            found = Some(agent);
        }
    }
    found
}

/// A name the Python accepts: a lowercase letter, then lowercase letters, digits, or
/// hyphens, 63 characters at most.
fn is_agent_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// Reads the agents file. A missing file means no agents yet. Any other problem is an
/// error, and the caller then treats the list as empty, as the Python does.
pub fn load_agents(path: &Path) -> Result<Vec<Agent>, AgentFileError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    if !path.is_file() {
        return Err(AgentFileError::value(path, "is not a file"));
    }
    let metadata = std::fs::metadata(path).map_err(|error| AgentFileError {
        class: "OSError",
        message: error.to_string(),
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(AgentFileError {
            class: "PermissionError",
            message: format!(
                "{} is mode {mode:04o}. chmod 0600 {}",
                path.display(),
                path.display()
            ),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|error| AgentFileError {
        class: "OSError",
        message: error.to_string(),
    })?;
    let data: Value = serde_json::from_str(&text).map_err(|error| AgentFileError {
        class: "JSONDecodeError",
        message: error.to_string(),
    })?;
    let Some(items) = data.get("agents").and_then(Value::as_array) else {
        return Err(AgentFileError::value(path, "is missing agents"));
    };
    let mut cleaned = Vec::with_capacity(items.len());
    for item in items {
        let Some(record) = item.as_object() else {
            return Err(AgentFileError::value(path, "has a bad agent record"));
        };
        let name = record.get("name").and_then(Value::as_str);
        let digest = record.get("token_sha256").and_then(Value::as_str);
        let scopes = record.get("scopes").and_then(Value::as_array);
        let Some(name) = name.filter(|name| is_agent_name(name)) else {
            return Err(AgentFileError::value(path, "has a bad agent name"));
        };
        let Some(digest) = digest.filter(|digest| digest.chars().count() == 64) else {
            return Err(AgentFileError::value(path, "has a bad token hash"));
        };
        let requested: Vec<&str> = match scopes {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(|scope| scope.as_str().filter(|text| SCOPES.contains(text)))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| AgentFileError::value(path, "has bad scopes"))?,
            _ => return Err(AgentFileError::value(path, "has bad scopes")),
        };
        cleaned.push(Agent {
            name: name.to_owned(),
            token_sha256: digest.to_owned(),
            scopes: SCOPES
                .iter()
                .filter(|scope| requested.contains(scope))
                .map(|scope| (*scope).to_owned())
                .collect(),
        });
    }
    Ok(cleaned)
}

#[cfg(test)]
pub mod testing {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use serde_json::{json, Value};

    use super::sha256_hex;

    /// One agent for `write_agents`: a name, its token, and its scopes.
    pub type Record<'a> = (&'a str, &'a str, &'a [&'a str]);

    /// Writes an agents file the way `create_agent` does: the hash only, mode 0600.
    /// The tokens are passed in, because the tests need known ones.
    pub fn write_agents(path: &Path, records: &[Record<'_>]) {
        let agents: Vec<Value> = records
            .iter()
            .map(|(name, token, scopes)| {
                json!({
                    "name": name,
                    "token_sha256": sha256_hex(token.as_bytes()),
                    "scopes": scopes,
                })
            })
            .collect();
        let doc = json!({ "agents": agents });
        let temp = path.with_extension("json.tmp");
        std::fs::write(
            &temp,
            format!("{}\n", serde_json::to_string_pretty(&doc).unwrap()),
        )
        .expect("write the agents file");
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .expect("chmod the agents file");
        std::fs::rename(&temp, path).expect("move the agents file into place");
    }
}
