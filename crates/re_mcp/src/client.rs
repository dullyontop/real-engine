//! A small blocking MCP client over streamable HTTP, for tests and the
//! evaluation harness (ADR-0046). It writes the protocol as any client does:
//! one POST per JSON-RPC message, `Mcp-Session-Id` kept from `initialize`,
//! `MCP-Protocol-Version` once negotiated, and, for revision 2026-07-28, the
//! `Mcp-Method` and `Mcp-Name` headers. Responses are read as JSON or as SSE
//! `data:` lines.
//!
//! It speaks plain HTTP/1.1 over `std::net`, so it reaches only `http://`
//! URLs; that is all `real mcp --http` serves.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{Value as Json, json};

/// Methods whose `Mcp-Name` comes from `params.name`, and from `params.uri`.
const NAME_FROM_NAME: &[&str] = &["tools/call", "prompts/get"];
const NAME_FROM_URI: &[&str] = &[
    "resources/read",
    "resources/subscribe",
    "resources/unsubscribe",
];

/// The revision whose requests carry the method and name headers.
const HEADER_REVISION: &str = "2026-07-28";

/// One HTTP response from the server.
#[derive(Clone, Debug)]
pub struct HttpReply {
    /// The status code.
    pub status: u16,
    /// The `Content-Type` header, if any.
    pub content_type: Option<String>,
    /// The JSON-RPC messages in the body (several when it is an SSE stream).
    pub messages: Vec<Json>,
}

/// A blocking client for one `real mcp --http` endpoint.
#[derive(Clone, Debug)]
pub struct HttpClient {
    authority: String,
    path: String,
    token: Option<String>,
    session: Option<String>,
    protocol: Option<String>,
    timeout: Duration,
}

impl HttpClient {
    /// A client for `url` (`http://HOST:PORT/PATH`) sending `token`.
    ///
    /// # Errors
    ///
    /// When `url` is not an `http://` URL.
    pub fn new(url: &str, token: Option<&str>) -> anyhow::Result<HttpClient> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| anyhow::anyhow!("`{url}` is not an http:// URL"))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        Ok(HttpClient {
            authority: authority.to_string(),
            path: path.to_string(),
            token: token.map(str::to_string),
            session: None,
            protocol: None,
            timeout: Duration::from_secs(600),
        })
    }

    /// The same client, giving up on a response after `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> HttpClient {
        self.timeout = timeout;
        self
    }

    /// Gives up on a response after `timeout` from now on.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The session id the server gave at `initialize`, if any.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Sends a JSON-RPC request and returns its response (the message with
    /// the same id). Messages before it (notifications) are dropped.
    ///
    /// # Errors
    ///
    /// On a connection error, an HTTP status other than 200, or a body with
    /// no response to `id`.
    pub fn request(&mut self, id: u64, method: &str, params: Json) -> anyhow::Result<Json> {
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let reply = self.post(&message, &[])?;
        anyhow::ensure!(
            reply.status == 200,
            "{method} (id {id}) got HTTP {}",
            reply.status
        );
        let response = reply
            .messages
            .into_iter()
            .find(|m| m["id"] == id && m.get("method").is_none())
            .ok_or_else(|| anyhow::anyhow!("no response to {method} (id {id})"))?;
        if method == "initialize"
            && let Some(version) = response["result"]["protocolVersion"].as_str()
        {
            self.protocol = Some(version.to_string());
        }
        Ok(response)
    }

    /// Sends a JSON-RPC notification.
    ///
    /// # Errors
    ///
    /// On a connection error or an HTTP status other than 200 or 202.
    pub fn notify(&mut self, method: &str, params: Json) -> anyhow::Result<()> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let reply = self.post(&message, &[])?;
        anyhow::ensure!(
            matches!(reply.status, 200 | 202),
            "{method} got HTTP {}",
            reply.status
        );
        Ok(())
    }

    /// POSTs one message with the protocol headers plus `extra` ones (tests
    /// use them to send a wrong `Host` or an `Origin`), and returns the reply
    /// whatever its status.
    ///
    /// # Errors
    ///
    /// On a connection error or a malformed response.
    pub fn post(&mut self, message: &Json, extra: &[(&str, &str)]) -> anyhow::Result<HttpReply> {
        let body = serde_json::to_vec(message)?;
        let mut headers: Vec<(String, String)> = vec![
            ("Content-Type".into(), "application/json".into()),
            (
                "Accept".into(),
                "application/json, text/event-stream".into(),
            ),
        ];
        if let Some(token) = &self.token {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }
        if let Some(session) = &self.session {
            headers.push(("Mcp-Session-Id".into(), session.clone()));
        }
        let method = message["method"].as_str().unwrap_or_default();
        let meta_version =
            message["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str();
        let version = meta_version.or(self.protocol.as_deref());
        if let Some(version) = version
            && method != "initialize"
        {
            headers.push(("MCP-Protocol-Version".into(), version.to_string()));
        }
        if version.is_some_and(|v| v >= HEADER_REVISION) {
            headers.push(("Mcp-Method".into(), method.to_string()));
            let key = if NAME_FROM_NAME.contains(&method) {
                Some("name")
            } else if NAME_FROM_URI.contains(&method) {
                Some("uri")
            } else {
                None
            };
            if let Some(name) = key.and_then(|k| message["params"][k].as_str()) {
                headers.push(("Mcp-Name".into(), header_value(name)));
            }
        }
        for (name, value) in extra {
            headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
            headers.push(((*name).to_string(), (*value).to_string()));
        }
        let host = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| self.authority.clone());
        headers.retain(|(n, _)| !n.eq_ignore_ascii_case("host"));

        let mut stream = TcpStream::connect(&self.authority)?;
        stream.set_read_timeout(Some(self.timeout))?;
        let mut head = format!(
            "POST {} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.path,
            body.len()
        );
        for (name, value) in &headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;

        let reply = read_reply(stream)?;
        if let Some(session) = reply.1 {
            self.session = Some(session);
        }
        Ok(reply.0)
    }
}

/// A header value as SEP-2243 writes it: plain when it is visible ASCII,
/// otherwise `=?base64?...?=`.
fn header_value(value: &str) -> String {
    let plain = !value.is_empty()
        && value.bytes().all(|b| (0x20..0x7f).contains(&b))
        && !value.starts_with([' ', '\t'])
        && !value.ends_with([' ', '\t'])
        && !value.starts_with("=?base64?");
    if plain {
        value.to_string()
    } else {
        format!("=?base64?{}?=", crate::server::base64(value.as_bytes()))
    }
}

/// Reads a whole `Connection: close` response: the reply and the session id
/// header, if any.
fn read_reply(stream: TcpStream) -> anyhow::Result<(HttpReply, Option<String>)> {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("bad HTTP status line: {status_line:?}"))?;
    let (mut content_type, mut session, mut chunked, mut length) = (None, None, false, None);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-type" => content_type = Some(value),
            "mcp-session-id" => session = Some(value),
            "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
            "content-length" => length = value.parse::<usize>().ok(),
            _ => {}
        }
    }
    let body = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(n) = length {
        let mut body = vec![0; n];
        reader.read_exact(&mut body)?;
        body
    } else {
        let mut body = Vec::new();
        reader.read_to_end(&mut body)?;
        body
    };
    let text = String::from_utf8_lossy(&body);
    let is_sse = content_type
        .as_deref()
        .is_some_and(|t| t.starts_with("text/event-stream"));
    let messages = if is_sse {
        sse_messages(&text)?
    } else if content_type
        .as_deref()
        .is_some_and(|t| t.starts_with("application/json"))
        && !text.trim().is_empty()
    {
        vec![serde_json::from_str(&text)?]
    } else {
        Vec::new()
    };
    Ok((
        HttpReply {
            status,
            content_type,
            messages,
        },
        session,
    ))
}

fn read_chunked(reader: &mut impl BufRead) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        reader.read_line(&mut size)?;
        let size = size.trim().split(';').next().unwrap_or_default();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| anyhow::anyhow!("bad chunk size {size:?}"))?;
        if size == 0 {
            return Ok(body);
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        let mut crlf = [0; 2];
        reader.read_exact(&mut crlf)?;
    }
}

/// The JSON messages in an SSE body: each event's `data:` lines joined
/// (events with empty data, such as rmcp's priming event, are skipped).
fn sse_messages(text: &str) -> anyhow::Result<Vec<Json>> {
    let mut messages = Vec::new();
    for event in text.split("\n\n") {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect();
        let data = data.join("\n");
        if !data.trim().is_empty() {
            messages.push(serde_json::from_str(&data)?);
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_bodies_yield_their_messages() {
        let body = "data: \nid: 0\nretry: 3000\n\ndata: {\"id\":1}\nid: 0/1\n\n";
        assert_eq!(sse_messages(body).unwrap(), vec![json!({ "id": 1 })]);
    }

    #[test]
    fn chunked_bodies_are_joined() {
        let mut input = "4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n".as_bytes();
        assert_eq!(read_chunked(&mut input).unwrap(), b"Wikipedia");
    }

    #[test]
    fn urls_split_into_authority_and_path() {
        let c = HttpClient::new("http://127.0.0.1:9/mcp", Some("t")).unwrap();
        assert_eq!(
            (c.authority.as_str(), c.path.as_str()),
            ("127.0.0.1:9", "/mcp")
        );
        assert!(HttpClient::new("https://x/mcp", None).is_err());
    }

    #[test]
    fn odd_header_values_are_base64() {
        assert_eq!(header_value("query"), "query");
        assert_eq!(header_value("é"), "=?base64?w6k=?=");
    }
}
