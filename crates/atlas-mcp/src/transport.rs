//! Stdio JSON-RPC transport — reads from stdin, writes to stdout.
//!
//! Uses MCP Content-Length header framing:
//! ```text
//! Content-Length: <N>\r\n
//! \r\n
//! <JSON body>
//! ```

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use super::protocol::{Request, Response};

/// Reads a single JSON-RPC request from stdin.
///
/// Parses Content-Length header then reads exactly that many bytes
/// as the JSON body.
pub async fn read_request<R>(reader: &mut R) -> Result<Option<Request>>
where
    R: AsyncBufRead + Unpin,
{
    let content_len = match read_content_length(reader).await? {
        Some(len) => len,
        None => return Ok(None),
    };

    // Read exactly content_len bytes
    let mut body = vec![0u8; content_len];
    reader.read_exact(&mut body).await?;

    let req: Request = serde_json::from_slice(&body).with_context(|| {
        format!(
            "Failed to parse JSON-RPC request: {}",
            String::from_utf8_lossy(&body)
        )
    })?;

    Ok(Some(req))
}

async fn read_content_length<R>(reader: &mut R) -> Result<Option<usize>>
where
    R: AsyncBufRead + Unpin,
{
    let mut content_len = None;
    let mut saw_header = false;

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            if saw_header {
                bail!("Unexpected EOF while reading MCP headers");
            }
            return Ok(None);
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }

        saw_header = true;
        if is_content_length_header(trimmed) {
            content_len =
                Some(parse_content_length(trimmed).context("Invalid Content-Length header")?);
        }
    }

    content_len
        .context("Missing Content-Length header")
        .map(Some)
}

/// Writes a JSON-RPC response to stdout with Content-Length framing.
pub async fn write_response(writer: &mut tokio::io::Stdout, resp: &Response) -> Result<()> {
    let json = resp.serialize_line();
    let header = format!("Content-Length: {}\r\n\r\n", json.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Write a raw JSON value as a response (convenience for notifications or ad-hoc).
pub async fn write_json(writer: &mut tokio::io::Stdout, value: &Value) -> Result<()> {
    let json = serde_json::to_string(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", json.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

fn parse_content_length(line: &str) -> Result<usize> {
    let trimmed = line.trim();
    let prefix = "Content-Length:";
    if !is_content_length_header(trimmed) {
        bail!("Missing Content-Length header, got: {}", trimmed);
    }
    let value = trimmed[prefix.len()..].trim();
    value
        .parse::<usize>()
        .with_context(|| format!("Invalid Content-Length value: {}", value))
}

fn is_content_length_header(line: &str) -> bool {
    let prefix = "Content-Length:";
    line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[test]
    fn test_parse_content_length() {
        assert_eq!(
            parse_content_length("Content-Length: 123\r\n").unwrap(),
            123
        );
        assert_eq!(parse_content_length("content-length: 0\n").unwrap(), 0);
        assert_eq!(
            parse_content_length("Content-Length:  456  \n").unwrap(),
            456
        );
        assert!(parse_content_length("Bad: 100").is_err());
    }

    #[tokio::test]
    async fn read_request_accepts_extra_headers() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let frame = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = BufReader::new(frame.as_bytes());

        let request = read_request(&mut reader).await.unwrap().unwrap();

        assert_eq!(request.method, "initialize");
    }

    #[tokio::test]
    async fn read_request_ignores_header_order() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let frame = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = BufReader::new(frame.as_bytes());

        let request = read_request(&mut reader).await.unwrap().unwrap();

        assert_eq!(request.method, "tools/list");
    }
}
