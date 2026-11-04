//! Lenient parsing of a pasted ICE servers configuration (spec section 5, Tab B).
//!
//! Accepts a JSON array, an object containing `iceServers`, or the JavaScript snippet as
//! Metered shows it (`var iceServers = [ ... ];`, `new RTCPeerConnection({ iceServers: [...] })`,
//! unquoted keys, single quotes, trailing commas, comments). The text is NEVER executed: the
//! first balanced `[...]`/`{...}` region is cut out (skipping strings and comments) and parsed
//! as JSON5 data.

use crate::model::*;
use serde_json::Value;

/// Hard cap on pasted text.
pub const MAX_PASTE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("the pasted text is empty")]
    Empty,
    #[error("the pasted text is too large")]
    TooLarge,
    #[error("no ICE servers array was found in the pasted text")]
    NoArray,
    #[error("the ICE servers array could not be read")]
    Syntax,
    #[error(transparent)]
    Invalid(#[from] ValidationError),
}

/// Finds the first balanced JSON-ish region starting at `[` or `{`, ignoring brackets inside
/// strings and comments.
fn balanced_region(text: &str) -> Option<&str> {
    let b = text.as_bytes();
    let mut start = None;
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'[' | b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b']' | b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return text.get(start?..=i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn as_server_list(v: Value) -> Option<Vec<Value>> {
    match v {
        Value::Array(a) => Some(a),
        Value::Object(mut o) => {
            if let Some(Value::Array(a)) = o.remove("iceServers") {
                Some(a)
            } else if o.contains_key("urls") || o.contains_key("url") {
                Some(vec![Value::Object(o)])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parses and validates pasted text into ICE servers.
pub fn parse_ice_servers(text: &str) -> Result<Vec<IceServer>, ParseError> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(ParseError::TooLarge);
    }
    let t = text.trim();
    if t.is_empty() {
        return Err(ParseError::Empty);
    }
    let region = balanced_region(t).ok_or(ParseError::NoArray)?;
    let value: Value = json5::from_str(region).map_err(|_| ParseError::Syntax)?;
    let list = as_server_list(value).ok_or(ParseError::NoArray)?;
    validate_list(list)
}

/// Validates a list of raw `RTCIceServer`-shaped values.
pub fn validate_list(list: Vec<Value>) -> Result<Vec<IceServer>, ParseError> {
    if list.is_empty() {
        return Err(ValidationError::Empty.into());
    }
    if list.len() > MAX_SERVERS {
        return Err(ValidationError::TooMany.into());
    }
    let mut out = Vec::new();
    for (idx, v) in list.into_iter().enumerate() {
        let Value::Object(o) = v else {
            return Err(ValidationError::BadUrl(idx).into());
        };
        let urls_v = o
            .get("urls")
            .or_else(|| o.get("url"))
            .cloned()
            .unwrap_or(Value::Null);
        let raw_urls: Vec<String> = match urls_v {
            Value::String(s) => vec![s],
            Value::Array(a) => a
                .into_iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        if raw_urls.is_empty() {
            return Err(ValidationError::NoUrls(idx).into());
        }
        if raw_urls.len() > MAX_URLS_PER_SERVER {
            return Err(ValidationError::TooMany.into());
        }
        let urls = raw_urls
            .iter()
            .map(|u| parse_ice_url(u, idx))
            .collect::<Result<Vec<_>, _>>()?;
        let text_field = |k: &str, max: usize| -> Result<Option<String>, ValidationError> {
            match o.get(k) {
                Some(Value::String(s)) if s.len() > max => Err(ValidationError::TooLong(idx)),
                Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
                _ => Ok(None),
            }
        };
        let username = text_field("username", MAX_USERNAME_LEN)?;
        let credential = text_field("credential", MAX_CREDENTIAL_LEN)?;
        if urls.iter().any(|u| u.scheme.is_turn()) && (username.is_none() || credential.is_none()) {
            return Err(ValidationError::MissingCredential(idx).into());
        }
        out.push(IceServer {
            urls,
            username,
            credential,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const METERED_SNIPPET: &str = r#"
        // Metered shows something like this (placeholders, not real values):
        var myPeerConnection = new RTCPeerConnection({
          iceServers: [
              {
                urls: "stun:stun.relay.metered.ca:80",
              },
              {
                urls: "turn:global.relay.metered.ca:80",
                username: "<USERNAME>",
                credential: "<CREDENTIAL>",
              },
              {
                urls: "turn:global.relay.metered.ca:80?transport=tcp",
                username: '<USERNAME>',
                credential: '<CREDENTIAL>',
              },
              /* TLS */
              {
                urls: "turns:global.relay.metered.ca:443?transport=tcp",
                username: "<USERNAME>",
                credential: "<CREDENTIAL>",
              },
          ],
        });"#;

    #[test]
    fn javascript_snippet_with_unquoted_keys_comments_and_trailing_commas() {
        let s = parse_ice_servers(METERED_SNIPPET).unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(s[3].urls[0].transport(), Transport::Tls);
        assert_eq!(s[1].username.as_deref(), Some("<USERNAME>"));
        assert_eq!(
            hostnames(&s),
            vec!["global.relay.metered.ca", "stun.relay.metered.ca"]
        );
    }

    #[test]
    fn plain_json_array_and_object_forms() {
        let arr = r#"[{"urls":["stun:a.example.invalid:3478","turn:a.example.invalid:3478"],"username":"u","credential":"c"}]"#;
        assert_eq!(parse_ice_servers(arr).unwrap()[0].urls.len(), 2);
        let obj = r#"{"iceServers":[{"urls":"stun:a.example.invalid"}]}"#;
        assert_eq!(parse_ice_servers(obj).unwrap().len(), 1);
        let single = r#"{"urls":"stun:a.example.invalid"}"#;
        assert_eq!(parse_ice_servers(single).unwrap().len(), 1);
        let var = "const iceServers = [{urls: 'stun:a.example.invalid'}];";
        assert_eq!(parse_ice_servers(var).unwrap().len(), 1);
    }

    #[test]
    fn code_is_never_executed_and_junk_is_rejected() {
        // Anything that is not data fails to parse; nothing runs.
        assert_eq!(
            parse_ice_servers("fetch('https://evil.invalid'); [alert(1)]"),
            Err(ParseError::Syntax)
        );
        assert_eq!(
            parse_ice_servers("just some words"),
            Err(ParseError::NoArray)
        );
        assert_eq!(parse_ice_servers("   "), Err(ParseError::Empty));
        assert_eq!(
            parse_ice_servers("[]"),
            Err(ParseError::Invalid(ValidationError::Empty))
        );
        assert_eq!(
            parse_ice_servers(&"x".repeat(MAX_PASTE_BYTES + 1)),
            Err(ParseError::TooLarge)
        );
        // Brackets inside strings and comments do not confuse the region finder.
        let tricky = r#"/* [ */ [{ urls: "stun:a.example.invalid", note: "]" }]"#;
        assert_eq!(parse_ice_servers(tricky).unwrap().len(), 1);
    }

    #[test]
    fn validation_rules() {
        let turn_no_creds = r#"[{"urls":"turn:a.example.invalid"}]"#;
        assert_eq!(
            parse_ice_servers(turn_no_creds),
            Err(ParseError::Invalid(ValidationError::MissingCredential(0)))
        );
        let bad_scheme = r#"[{"urls":"http://a.example.invalid"}]"#;
        assert_eq!(
            parse_ice_servers(bad_scheme),
            Err(ParseError::Invalid(ValidationError::BadScheme(0)))
        );
        let many: Vec<String> = (0..17)
            .map(|_| r#"{"urls":"stun:a.example.invalid"}"#.to_owned())
            .collect();
        assert_eq!(
            parse_ice_servers(&format!("[{}]", many.join(","))),
            Err(ParseError::Invalid(ValidationError::TooMany))
        );
        let long = format!(
            r#"[{{"urls":"turn:a.example.invalid","username":"u","credential":"{}"}}]"#,
            "c".repeat(600)
        );
        assert_eq!(
            parse_ice_servers(&long),
            Err(ParseError::Invalid(ValidationError::TooLong(0)))
        );
        let no_urls = r#"[{"username":"u"}]"#;
        assert_eq!(
            parse_ice_servers(no_urls),
            Err(ParseError::Invalid(ValidationError::NoUrls(0)))
        );
    }
}
