//! Loopback-only external plugin HTTP client (SPEC §55-§56).

use std::fmt;
use std::io::{self, BufWriter, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

pub const IOPAINT_PLUGIN: &str = "darask-iopaint";
pub const DIFFUSION_PLUGIN: &str = "darask-ai-diffusion";
pub const PLUGIN_API_VERSION: u32 = 1;
pub const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

fn io_timeout() -> Duration {
    #[cfg(test)]
    {
        Duration::from_millis(150)
    }
    #[cfg(not(test))]
    {
        Duration::from_secs(120)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHealth {
    pub plugin: String,
    pub api: u32,
    pub engine: String,
    pub backend: String,
    pub model: String,
}

#[derive(Debug)]
pub enum PluginError {
    Io(io::Error),
    Timeout,
    InvalidResponse(&'static str),
    ResponseTooLarge,
    Redirect(u16),
    HttpStatus(u16),
    UnexpectedPlugin,
    UnsupportedApi,
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Timeout => f.write_str("request timed out"),
            Self::InvalidResponse(reason) => write!(f, "invalid response: {reason}"),
            Self::ResponseTooLarge => f.write_str("response is too large"),
            Self::Redirect(status) => write!(f, "redirect rejected: {status}"),
            Self::HttpStatus(status) => write!(f, "HTTP status {status}"),
            Self::UnexpectedPlugin => f.write_str("unexpected plugin"),
            Self::UnsupportedApi => f.write_str("unsupported plugin API"),
        }
    }
}

impl From<io::Error> for PluginError {
    fn from(error: io::Error) -> Self {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            Self::Timeout
        } else {
            Self::Io(error)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponseHead {
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
}

pub fn health_check(port: u16) -> Result<PluginHealth, PluginError> {
    let mut stream = connect(port)?;
    write_request_head(&mut stream, port, "GET", "/api/v1/health", 0)?;
    let health = parse_health_json(&read_response(&mut stream, MAX_HEADER_BYTES)?)?;
    if health.api != PLUGIN_API_VERSION {
        return Err(PluginError::UnsupportedApi);
    }
    if health.plugin != IOPAINT_PLUGIN && health.plugin != DIFFUSION_PLUGIN {
        return Err(PluginError::UnexpectedPlugin);
    }
    Ok(health)
}

pub fn iopaint_inpaint(
    port: u16,
    image_png: &[u8],
    mask_png: &[u8],
) -> Result<Vec<u8>, PluginError> {
    post_image_mask(port, image_png, mask_png, b"", b"")
}

pub fn diffusion_generate(
    port: u16,
    prompt: &str,
    negative: Option<&str>,
    width: u32,
    height: u32,
    seed: Option<u64>,
) -> Result<Vec<u8>, PluginError> {
    let body = build_generate_json(prompt, negative, width, height, seed);
    post_json(port, "/api/v1/generate", body.as_bytes())
}

pub fn diffusion_inpaint(
    port: u16,
    image_png: &[u8],
    mask_png: &[u8],
    prompt: &str,
    strength: Option<f32>,
) -> Result<Vec<u8>, PluginError> {
    let escaped = escape_json_string(prompt);
    let tail = match strength {
        Some(value) => format!("\",\"prompt\":\"{escaped}\",\"strength\":{value}"),
        None => format!("\",\"prompt\":\"{escaped}\""),
    };
    post_image_mask(port, image_png, mask_png, tail.as_bytes(), b"}")
}

fn post_image_mask(
    port: u16,
    image_png: &[u8],
    mask_png: &[u8],
    tail: &[u8],
    suffix: &[u8],
) -> Result<Vec<u8>, PluginError> {
    let prefix = b"{\"image\":\"";
    let middle = b"\",\"mask\":\"";
    let default_suffix = b"\"}";
    let (tail, suffix) = if tail.is_empty() {
        (b"".as_slice(), default_suffix.as_slice())
    } else {
        (tail, suffix)
    };
    let body_len = prefix
        .len()
        .checked_add(base64_encoded_len(image_png.len())?)
        .and_then(|n| n.checked_add(middle.len()))
        .and_then(|n| n.checked_add(base64_encoded_len(mask_png.len()).ok()?))
        .and_then(|n| n.checked_add(tail.len()))
        .and_then(|n| n.checked_add(suffix.len()))
        .ok_or(PluginError::ResponseTooLarge)?;
    let mut stream = connect(port)?;
    {
        let mut writer = BufWriter::new(&mut stream);
        write_request_head(&mut writer, port, "POST", "/api/v1/inpaint", body_len)?;
        writer.write_all(prefix)?;
        write_base64(&mut writer, image_png)?;
        writer.write_all(middle)?;
        write_base64(&mut writer, mask_png)?;
        writer.write_all(tail)?;
        writer.write_all(suffix)?;
        writer.flush()?;
    }
    read_response(&mut stream, MAX_RESPONSE_BYTES)
}

fn post_json(port: u16, path: &str, body: &[u8]) -> Result<Vec<u8>, PluginError> {
    let mut stream = connect(port)?;
    write_request_head(&mut stream, port, "POST", path, body.len())?;
    stream.write_all(body)?;
    read_response(&mut stream, MAX_RESPONSE_BYTES)
}

fn connect(port: u16) -> Result<TcpStream, PluginError> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(io_timeout()))?;
    stream.set_write_timeout(Some(io_timeout()))?;
    Ok(stream)
}

fn write_request_head(
    writer: &mut impl Write,
    port: u16,
    method: &str,
    path: &str,
    content_length: usize,
) -> Result<(), PluginError> {
    write!(writer, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n")?;
    Ok(())
}

fn read_response(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>, PluginError> {
    let (head_bytes, initial_body) = read_head(reader)?;
    let head = parse_response_head(&head_bytes)?;
    if (300..400).contains(&head.status) {
        return Err(PluginError::Redirect(head.status));
    }
    if !(200..300).contains(&head.status) {
        return Err(PluginError::HttpStatus(head.status));
    }
    if head.chunked {
        return decode_chunked(reader, initial_body, limit);
    }
    if let Some(length) = head.content_length {
        if length > limit {
            return Err(PluginError::ResponseTooLarge);
        }
        let mut body = initial_body;
        if body.len() > length {
            body.truncate(length);
        }
        while body.len() < length {
            let mut buffer = [0u8; 8192];
            let capacity = (length - body.len()).min(buffer.len());
            let count = reader.read(&mut buffer[..capacity])?;
            if count == 0 {
                return Err(PluginError::InvalidResponse("short body"));
            }
            body.extend_from_slice(&buffer[..count]);
        }
        return Ok(body);
    }
    let mut body = initial_body;
    read_to_limit(reader, &mut body, limit)?;
    Ok(body)
}

fn read_head(reader: &mut impl Read) -> Result<(Vec<u8>, Vec<u8>), PluginError> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Err(PluginError::InvalidResponse("missing header terminator"));
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = find_subslice(&bytes, b"\r\n\r\n") {
            let body = bytes.split_off(end + 4);
            bytes.truncate(end);
            return Ok((bytes, body));
        }
        if bytes.len() > MAX_HEADER_BYTES {
            return Err(PluginError::InvalidResponse("headers too large"));
        }
    }
}

fn parse_response_head(bytes: &[u8]) -> Result<ResponseHead, PluginError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| PluginError::InvalidResponse("non-UTF-8 headers"))?;
    let mut lines = text.split("\r\n");
    let mut parts = lines
        .next()
        .ok_or(PluginError::InvalidResponse("missing status"))?
        .split_ascii_whitespace();
    let version = parts
        .next()
        .ok_or(PluginError::InvalidResponse("missing HTTP version"))?;
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(PluginError::InvalidResponse("unsupported HTTP version"));
    }
    let status = parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(PluginError::InvalidResponse("invalid status"))?;
    let mut content_length = None;
    let mut chunked = false;
    let mut transfer_encoding_seen = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(PluginError::InvalidResponse("malformed header"));
        };
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| PluginError::InvalidResponse("invalid content-length"))?;
            if content_length.replace(parsed).is_some() {
                return Err(PluginError::InvalidResponse("duplicate content-length"));
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if transfer_encoding_seen {
                return Err(PluginError::InvalidResponse("duplicate transfer-encoding"));
            }
            transfer_encoding_seen = true;
            let mut encodings = value.split(',').map(str::trim);
            let Some(encoding) = encodings.next() else {
                return Err(PluginError::InvalidResponse("empty transfer-encoding"));
            };
            if encoding.is_empty()
                || !encoding.eq_ignore_ascii_case("chunked")
                || encodings.next().is_some()
            {
                return Err(PluginError::InvalidResponse(
                    "unsupported transfer-encoding",
                ));
            }
            chunked = true;
        }
    }
    if chunked && content_length.is_some() {
        return Err(PluginError::InvalidResponse("ambiguous body framing"));
    }
    Ok(ResponseHead {
        status,
        content_length,
        chunked,
    })
}

struct BufferedInput<'a, R> {
    reader: &'a mut R,
    buffer: Vec<u8>,
    offset: usize,
}
impl<'a, R: Read> BufferedInput<'a, R> {
    fn new(reader: &'a mut R, buffer: Vec<u8>) -> Self {
        Self {
            reader,
            buffer,
            offset: 0,
        }
    }
    fn fill(&mut self) -> Result<bool, PluginError> {
        if self.offset > 0 {
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        let mut chunk = [0u8; 4096];
        let count = self.reader.read(&mut chunk)?;
        self.buffer.extend_from_slice(&chunk[..count]);
        Ok(count != 0)
    }
    fn read_line(&mut self, limit: usize) -> Result<Vec<u8>, PluginError> {
        loop {
            if let Some(relative) = find_subslice(&self.buffer[self.offset..], b"\r\n") {
                let end = self.offset + relative;
                let line = self.buffer[self.offset..end].to_vec();
                self.offset = end + 2;
                return Ok(line);
            }
            if self.buffer.len().saturating_sub(self.offset) > limit {
                return Err(PluginError::InvalidResponse("chunk line too large"));
            }
            if !self.fill()? {
                return Err(PluginError::InvalidResponse("short chunked body"));
            }
        }
    }
    fn read_exact_into(&mut self, output: &mut Vec<u8>, count: usize) -> Result<(), PluginError> {
        let target = output
            .len()
            .checked_add(count)
            .ok_or(PluginError::ResponseTooLarge)?;
        while output.len() < target {
            if self.offset == self.buffer.len() && !self.fill()? {
                return Err(PluginError::InvalidResponse("short chunk"));
            }
            let available = (target - output.len()).min(self.buffer.len() - self.offset);
            output.extend_from_slice(&self.buffer[self.offset..self.offset + available]);
            self.offset += available;
        }
        Ok(())
    }
}

fn decode_chunked(
    reader: &mut impl Read,
    initial: Vec<u8>,
    limit: usize,
) -> Result<Vec<u8>, PluginError> {
    let mut input = BufferedInput::new(reader, initial);
    let mut output = Vec::new();
    loop {
        let line = input.read_line(MAX_HEADER_BYTES)?;
        let size_bytes = line
            .split(|byte| *byte == b';')
            .next()
            .ok_or(PluginError::InvalidResponse("invalid chunk size"))?;
        let size_text = std::str::from_utf8(size_bytes)
            .map_err(|_| PluginError::InvalidResponse("invalid chunk size"))?;
        let size = usize::from_str_radix(size_text.trim(), 16)
            .map_err(|_| PluginError::InvalidResponse("invalid chunk size"))?;
        if size == 0 {
            loop {
                if input.read_line(MAX_HEADER_BYTES)?.is_empty() {
                    return Ok(output);
                }
            }
        }
        if output
            .len()
            .checked_add(size)
            .filter(|n| *n <= limit)
            .is_none()
        {
            return Err(PluginError::ResponseTooLarge);
        }
        input.read_exact_into(&mut output, size)?;
        let mut terminator = Vec::new();
        input.read_exact_into(&mut terminator, 2)?;
        if terminator != b"\r\n" {
            return Err(PluginError::InvalidResponse("invalid chunk terminator"));
        }
    }
}

fn read_to_limit(
    reader: &mut impl Read,
    output: &mut Vec<u8>,
    limit: usize,
) -> Result<(), PluginError> {
    if output.len() > limit {
        return Err(PluginError::ResponseTooLarge);
    }
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        if output
            .len()
            .checked_add(count)
            .filter(|n| *n <= limit)
            .is_none()
        {
            return Err(PluginError::ResponseTooLarge);
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

pub fn build_generate_json(
    prompt: &str,
    negative: Option<&str>,
    width: u32,
    height: u32,
    seed: Option<u64>,
) -> String {
    let mut json = format!(
        "{{\"prompt\":\"{}\",\"width\":{},\"height\":{}",
        escape_json_string(prompt),
        width,
        height
    );
    if let Some(negative) = negative {
        json.push_str(",\"negative\":\"");
        json.push_str(&escape_json_string(negative));
        json.push('"');
    }
    if let Some(seed) = seed {
        json.push_str(&format!(",\"seed\":{seed}"));
    }
    json.push('}');
    json
}

fn escape_json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c <= '\u{1f}' => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped
}

pub fn parse_health_json(bytes: &[u8]) -> Result<PluginHealth, PluginError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| PluginError::InvalidResponse("health is not UTF-8"))?;
    Ok(PluginHealth {
        plugin: json_string(text, "plugin")?,
        api: json_u32(text, "api")?,
        engine: json_string(text, "engine")?,
        backend: json_string(text, "backend")?,
        model: json_string(text, "model")?,
    })
}

fn json_value_start<'a>(text: &'a str, key: &str) -> Result<&'a str, PluginError> {
    let needle = format!("\"{key}\"");
    let start = text
        .find(&needle)
        .ok_or(PluginError::InvalidResponse("missing JSON field"))?;
    let rest = &text[start + needle.len()..];
    let colon = rest
        .find(':')
        .ok_or(PluginError::InvalidResponse("missing JSON colon"))?;
    Ok(rest[colon + 1..].trim_start())
}

fn json_u32(text: &str, key: &str) -> Result<u32, PluginError> {
    let rest = json_value_start(text, key)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return Err(PluginError::InvalidResponse("invalid JSON integer"));
    }
    rest[..end]
        .parse()
        .map_err(|_| PluginError::InvalidResponse("invalid JSON integer"))
}

fn json_string(text: &str, key: &str) -> Result<String, PluginError> {
    let rest = json_value_start(text, key)?;
    let Some(mut chars) = rest.strip_prefix('"').map(str::chars) else {
        return Err(PluginError::InvalidResponse("invalid JSON string"));
    };
    let mut output = String::new();
    while let Some(character) = chars.next() {
        match character {
            '"' => return Ok(output),
            '\\' => match chars.next() {
                Some('"') => output.push('"'),
                Some('\\') => output.push('\\'),
                Some('/') => output.push('/'),
                Some('b') => output.push('\u{8}'),
                Some('f') => output.push('\u{c}'),
                Some('n') => output.push('\n'),
                Some('r') => output.push('\r'),
                Some('t') => output.push('\t'),
                _ => return Err(PluginError::InvalidResponse("unsupported JSON escape")),
            },
            c if c <= '\u{1f}' => {
                return Err(PluginError::InvalidResponse("control in JSON string"))
            }
            c => output.push(c),
        }
    }
    Err(PluginError::InvalidResponse("unterminated JSON string"))
}

#[cfg(test)]
pub fn base64_encode(input: &[u8]) -> String {
    let mut output = Vec::with_capacity(base64_encoded_len(input.len()).unwrap_or(0));
    if write_base64(&mut output, input).is_err() {
        return String::new();
    }
    String::from_utf8(output).unwrap_or_default()
}

#[cfg(test)]
pub fn base64_decode(input: &str) -> Result<Vec<u8>, PluginError> {
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !bytes.len().is_multiple_of(4) {
        return Err(PluginError::InvalidResponse("invalid base64 length"));
    }
    let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
    let groups = bytes.len() / 4;
    for (index, quartet) in bytes.chunks_exact(4).enumerate() {
        let a = base64_value(quartet[0])?;
        let b = base64_value(quartet[1])?;
        let c = if quartet[2] == b'=' {
            64
        } else {
            base64_value(quartet[2])?
        };
        let d = if quartet[3] == b'=' {
            64
        } else {
            base64_value(quartet[3])?
        };
        if a == 64
            || b == 64
            || (c == 64 && d != 64)
            || ((c == 64 || d == 64) && index + 1 != groups)
        {
            return Err(PluginError::InvalidResponse("invalid base64 padding"));
        }
        output.push((a << 2) | (b >> 4));
        if c != 64 {
            output.push((b << 4) | (c >> 2));
        }
        if d != 64 {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_encoded_len(length: usize) -> Result<usize, PluginError> {
    length
        .checked_add(2)
        .and_then(|n| n.checked_div(3))
        .and_then(|n| n.checked_mul(4))
        .ok_or(PluginError::ResponseTooLarge)
}

fn write_base64(writer: &mut impl Write, input: &[u8]) -> Result<(), PluginError> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        let encoded = [
            TABLE[(a >> 2) as usize],
            TABLE[(((a & 3) << 4) | (b >> 4)) as usize],
            if chunk.len() > 1 {
                TABLE[(((b & 15) << 2) | (c >> 6)) as usize]
            } else {
                b'='
            },
            if chunk.len() > 2 {
                TABLE[(c & 63) as usize]
            } else {
                b'='
            },
        ];
        writer.write_all(&encoded)?;
    }
    Ok(())
}

#[cfg(test)]
fn base64_value(byte: u8) -> Result<u8, PluginError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(PluginError::InvalidResponse("invalid base64 character")),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::thread;

    fn serve(response: Vec<Vec<u8>>, delay: Option<Duration>) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                if let Some(delay) = delay {
                    thread::sleep(delay);
                }
                for part in response {
                    let _ = stream.write_all(&part);
                }
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(10));
            }
        });
        port
    }

    #[test]
    fn base64_round_trips_and_matches_vectors() {
        for input in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"hello",
            &[0, 1, 254, 255],
        ] {
            assert_eq!(
                base64_decode(&base64_encode(input)).ok().as_deref(),
                Some(input)
            );
        }
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }

    #[test]
    fn base64_rejects_invalid_input() {
        assert!(base64_decode("abc").is_err());
        assert!(base64_decode("ab?=").is_err());
        assert!(base64_decode("=aaa").is_err());
    }

    #[test]
    fn generate_json_escapes_optional_fields() {
        assert_eq!(
            build_generate_json("a\n\"b", None, 3, 4, None),
            "{\"prompt\":\"a\\n\\\"b\",\"width\":3,\"height\":4}"
        );
        assert!(build_generate_json("x", Some("bad"), 5, 6, Some(7)).contains("\"seed\":7"));
    }

    #[test]
    fn health_json_parses_fixed_schema() {
        let health = parse_health_json(
            br#"{"plugin":"darask-iopaint","api":1,"engine":"2","backend":"ready","model":"lama"}"#,
        )
        .expect("health");
        assert_eq!(health.plugin, IOPAINT_PLUGIN);
        assert_eq!(health.model, "lama");
    }

    #[test]
    fn headers_parse_and_reject_ambiguity() {
        assert_eq!(
            parse_response_head(b"HTTP/1.1 200 OK\r\nContent-Length: 3")
                .expect("head")
                .content_length,
            Some(3)
        );
        assert!(
            parse_response_head(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked")
                .expect("head")
                .chunked
        );
        assert!(parse_response_head(b"HTTP/1.1 200 OK\r\nContent-Length: x").is_err());
        assert!(parse_response_head(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked"
        )
        .is_err());
        assert!(parse_response_head(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked"
        )
        .is_err());
        assert!(parse_response_head(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip").is_err());
        assert!(
            parse_response_head(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked").is_err()
        );
    }

    #[test]
    fn base64_streaming_batches_large_inputs_before_the_lower_writer() {
        struct CountingWriter {
            calls: usize,
            bytes: usize,
        }

        impl Write for CountingWriter {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                self.bytes += buffer.len();
                Ok(buffer.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let input = vec![0x5a; 6 * 1024 * 1024 + 1];
        let expected_len = base64_encoded_len(input.len()).expect("encoded length");
        let mut lower = CountingWriter { calls: 0, bytes: 0 };
        {
            let mut writer = BufWriter::with_capacity(8192, &mut lower);
            write_base64(&mut writer, &input).expect("base64");
            writer.flush().expect("flush");
        }
        assert_eq!(lower.bytes, expected_len);
        assert!(
            lower.calls < 2_000,
            "large streaming input used {} lower-level writes",
            lower.calls
        );
    }

    #[test]
    fn chunked_decodes_fragmented_and_rejects_oversize() {
        let mut reader = Cursor::new(b"ki\r\n0\r\nX: y\r\n\r\n".to_vec());
        assert_eq!(
            decode_chunked(&mut reader, b"4\r\nWi".to_vec(), 16).expect("chunked"),
            b"Wiki"
        );
        let mut reader = Cursor::new(b"5\r\nhello\r\n0\r\n\r\n".to_vec());
        assert!(matches!(
            decode_chunked(&mut reader, Vec::new(), 4),
            Err(PluginError::ResponseTooLarge)
        ));
    }

    #[test]
    fn health_handles_fragmentation_redirect_status_and_mismatch() {
        let json =
            br#"{"plugin":"darask-iopaint","api":1,"engine":"2","backend":"ready","model":"lama"}"#;
        let port = serve(
            vec![
                b"HTTP/1.1 200 OK\r\nContent-Len".to_vec(),
                format!("gth: {}\r\n\r\n", json.len()).into_bytes(),
                json.to_vec(),
            ],
            None,
        );
        assert_eq!(health_check(port).expect("health").plugin, IOPAINT_PLUGIN);
        let port = serve(
            vec![b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n".to_vec()],
            None,
        );
        assert!(matches!(
            health_check(port),
            Err(PluginError::Redirect(302))
        ));
        let port = serve(
            vec![b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\n\r\n".to_vec()],
            None,
        );
        assert!(matches!(
            health_check(port),
            Err(PluginError::HttpStatus(503))
        ));
    }

    #[test]
    fn health_rejects_bad_plugin_api_and_timeout() {
        for json in [
            br#"{"plugin":"other","api":1,"engine":"x","backend":"ready","model":"x"}"#.as_slice(),
            br#"{"plugin":"darask-iopaint","api":2,"engine":"x","backend":"ready","model":"x"}"#
                .as_slice(),
        ] {
            let port = serve(
                vec![
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", json.len())
                        .into_bytes(),
                    json.to_vec(),
                ],
                None,
            );
            assert!(health_check(port).is_err());
        }
        let port = serve(Vec::new(), Some(Duration::from_millis(300)));
        assert!(matches!(health_check(port), Err(PluginError::Timeout)));
    }

    fn read_full_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let count = stream.read(&mut buffer).expect("read request");
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = find_subslice(&request, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        request
    }

    #[test]
    fn inpaint_and_generate_round_trip_through_local_stub() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept inpaint");
            let request = read_full_request(&mut stream);
            let text = String::from_utf8_lossy(&request);
            assert!(text.starts_with("POST /api/v1/inpaint HTTP/1.1\r\n"));
            assert!(text.contains(&format!("Host: 127.0.0.1:{port}")));
            assert!(text.contains("Content-Type: application/json"));
            assert!(!text.contains("Origin:"));
            assert!(text.contains("{\"image\":\"cG5n\",\"mask\":\"bWFzaw==\"}"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nPNG")
                .expect("reply");
        });
        assert_eq!(
            iopaint_inpaint(port, b"png", b"mask").expect("inpaint"),
            b"PNG"
        );
        server.join().expect("server");

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept generate");
            let request = read_full_request(&mut stream);
            let text = String::from_utf8_lossy(&request);
            assert!(text.starts_with("POST /api/v1/generate HTTP/1.1\r\n"));
            assert!(text.contains("\"prompt\":\"cat\""));
            assert!(text.contains("\"width\":17"));
            assert!(text.contains("\"height\":19"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nPNG\r\n0\r\n\r\n",
                )
                .expect("reply");
        });
        assert_eq!(
            diffusion_generate(port, "cat", None, 17, 19, Some(4)).expect("generate"),
            b"PNG"
        );
        server.join().expect("server");
    }

    #[test]
    fn response_rejects_huge_and_invalid_length() {
        let mut huge = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n".to_vec());
        assert!(matches!(
            read_response(&mut huge, 8),
            Err(PluginError::ResponseTooLarge)
        ));
        let mut invalid = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n".to_vec());
        assert!(read_response(&mut invalid, 8).is_err());
    }
}
