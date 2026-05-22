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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::protocol::{Request, Response};

/// Reads a single JSON-RPC request from stdin.
///
/// Parses Content-Length header then reads exactly that many bytes
/// as the JSON body.
pub async fn read_request(reader: &mut BufReader<tokio::io::Stdin>) -> Result<Option<Request>> {
    let mut header_line = String::new();
    let n = reader.read_line(&mut header_line).await?;
    if n == 0 {
        return Ok(None); // EOF
    }

    let content_len =
        parse_content_length(&header_line).context("Invalid Content-Length header")?;

    // Read empty separator line (\r\n or \n)
    let mut blank = String::new();
    reader.read_line(&mut blank).await?;

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
    if !trimmed.to_lowercase().starts_with(&prefix.to_lowercase()) {
        bail!("Missing Content-Length header, got: {}", trimmed);
    }
    let value = trimmed[prefix.len()..].trim();
    value
        .parse::<usize>()
        .with_context(|| format!("Invalid Content-Length value: {}", value))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
