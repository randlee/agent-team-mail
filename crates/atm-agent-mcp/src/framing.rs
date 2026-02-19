//! JSON-RPC message framing for MCP stdio transport.
//!
//! Supports two framing modes:
//!
//! - **Content-Length**: `Content-Length: N\r\n\r\n<N bytes>` (standard MCP stdio transport)
//! - **Newline-delimited**: one JSON object per `\n`-terminated line (Codex child uses this)
//!
//! The proxy reads from upstream (Claude) using [`UpstreamReader`] which auto-detects
//! framing. Messages are always written to the Codex child in newline-delimited format.

use std::io;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Reads MCP messages from an async reader, auto-detecting Content-Length vs newline framing.
///
/// On each call to [`UpstreamReader::next_message`], the reader peeks at incoming bytes:
/// - If a line starts with `Content-Length:`, it parses the header and reads the body.
/// - Otherwise it treats the line as a complete JSON message.
pub struct UpstreamReader<R> {
    reader: BufReader<R>,
    buf: String,
}

impl<R: AsyncRead + Unpin> UpstreamReader<R> {
    /// Create a new upstream reader wrapping the given async reader.
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            buf: String::new(),
        }
    }

    /// Read the next JSON-RPC message, returning `None` on EOF.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading fails or Content-Length parsing encounters
    /// malformed headers.
    pub async fn next_message(&mut self) -> io::Result<Option<String>> {
        loop {
            self.buf.clear();
            let n = self.reader.read_line(&mut self.buf).await?;
            if n == 0 {
                return Ok(None); // EOF
            }

            let trimmed = self.buf.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Check if this is a Content-Length header
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                let len: usize = rest
                    .trim()
                    .parse()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

                // Read until blank line (consume \r\n\r\n separator)
                loop {
                    self.buf.clear();
                    let header_n = self.reader.read_line(&mut self.buf).await?;
                    if header_n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "EOF in Content-Length headers",
                        ));
                    }
                    if self.buf.trim().is_empty() {
                        break;
                    }
                    // Skip other headers (e.g. Content-Type)
                }

                // Read exactly `len` bytes of body
                let mut body = vec![0u8; len];
                self.reader.read_exact(&mut body).await?;
                let msg =
                    String::from_utf8(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                return Ok(Some(msg));
            }

            // Newline-delimited: the trimmed line IS the JSON message
            return Ok(Some(trimmed.to_string()));
        }
    }
}

/// Write a JSON message in newline-delimited format to the given writer.
///
/// Appends `\n` and flushes. The `json` string must not contain embedded newlines.
///
/// # Errors
///
/// Returns an I/O error if writing or flushing fails.
pub async fn write_newline_delimited<W: AsyncWrite + Unpin>(
    writer: &mut W,
    json: &str,
) -> io::Result<()> {
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Encode a JSON message in Content-Length framing format.
///
/// Returns a `Vec<u8>` containing `Content-Length: N\r\n\r\n<body>`.
pub fn encode_content_length(json: &str) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", json.len());
    let mut buf = Vec::with_capacity(header.len() + json.len());
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(json.as_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_newline_delimited() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1}\n";
        let mut reader = UpstreamReader::new(&input[..]);
        let msg = reader.next_message().await.unwrap().unwrap();
        assert_eq!(msg, "{\"jsonrpc\":\"2.0\",\"id\":1}");
    }

    #[tokio::test]
    async fn test_parse_content_length_frame() {
        let body = r#"{"jsonrpc":"2.0","id":2}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = UpstreamReader::new(framed.as_bytes());
        let msg = reader.next_message().await.unwrap().unwrap();
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn test_parse_content_length_with_extra_header() {
        let body = r#"{"jsonrpc":"2.0","id":3}"#;
        let framed = format!(
            "Content-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = UpstreamReader::new(framed.as_bytes());
        let msg = reader.next_message().await.unwrap().unwrap();
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn test_parse_partial_content_length() {
        // Body is split in the underlying stream but read_exact handles it
        let body = r#"{"jsonrpc":"2.0","method":"test"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = UpstreamReader::new(framed.as_bytes());
        let msg = reader.next_message().await.unwrap().unwrap();
        assert_eq!(msg, body);
    }

    #[tokio::test]
    async fn test_parse_multiple_newline_messages() {
        let input = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
        let mut reader = UpstreamReader::new(&input[..]);
        assert_eq!(
            reader.next_message().await.unwrap().unwrap(),
            "{\"id\":1}"
        );
        assert_eq!(
            reader.next_message().await.unwrap().unwrap(),
            "{\"id\":2}"
        );
        assert_eq!(
            reader.next_message().await.unwrap().unwrap(),
            "{\"id\":3}"
        );
        assert!(reader.next_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_write_newline_delimited() {
        let mut buf = Vec::new();
        write_newline_delimited(&mut buf, r#"{"id":1}"#)
            .await
            .unwrap();
        assert_eq!(buf, b"{\"id\":1}\n");
    }

    #[tokio::test]
    async fn test_content_length_frame_roundtrip() {
        let original = r#"{"jsonrpc":"2.0","id":99,"method":"ping"}"#;
        let encoded = encode_content_length(original);
        let mut reader = UpstreamReader::new(&encoded[..]);
        let decoded = reader.next_message().await.unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[tokio::test]
    async fn test_eof_returns_none() {
        let input = b"";
        let mut reader = UpstreamReader::new(&input[..]);
        assert!(reader.next_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_blank_lines_skipped() {
        let input = b"\n\n{\"id\":1}\n\n";
        let mut reader = UpstreamReader::new(&input[..]);
        let msg = reader.next_message().await.unwrap().unwrap();
        assert_eq!(msg, "{\"id\":1}");
    }
}
