use crate::config::{Config, Url};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const MAX_RESPONSE: usize = 1024 * 1024;

fn io_error(error: io::Error) -> String {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        format!("HTTP timeout: {error}")
    } else {
        format!("HTTP I/O error: {error}")
    }
}

pub fn get(config: &Config) -> Result<String, String> {
    let deadline = Instant::now()
        .checked_add(config.timeout)
        .ok_or("HTTP timeout: configured duration is out of range")?;
    let addresses = (config.url.host.as_str(), config.url.port)
        .to_socket_addrs()
        .map_err(|error| format!("HTTP resolve error: {error}"))?;
    let mut last_error = "HTTP resolution returned no addresses".to_string();
    let mut connected = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, remaining(deadline)?) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(error) => last_error = io_error(error),
        }
    }
    let mut stream = connected.ok_or(last_error)?;
    let request = request(&config.url);
    let mut pending = request.as_bytes();
    while !pending.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(io_error)?;
        match stream.write(pending) {
            Ok(0) => return Err("HTTP I/O error: request write returned zero bytes".into()),
            Ok(count) => pending = &pending[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(error)),
        }
    }
    let response = read_response(&mut &stream, || {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(io_error)
    })?;
    remaining(deadline)?;
    body(&response)
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| "HTTP timeout: overall deadline exceeded".into())
}

fn request(url: &Url) -> String {
    let host = if url.port == 80 {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    format!("GET {} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: modqos-stats/{}\r\nConnection: close\r\n\r\n", url.path, env!("CARGO_PKG_VERSION"))
}

fn read_response(
    reader: &mut impl Read,
    mut before_read: impl FnMut() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    let mut response = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        before_read()?;
        let capacity = buffer.len().min(MAX_RESPONSE + 1 - response.len());
        match reader.read(&mut buffer[..capacity]) {
            Ok(0) => return Ok(response),
            Ok(count) => response.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(error)),
        }
        if response.len() > MAX_RESPONSE {
            return Err("HTTP response exceeds 1 MiB".into());
        }
    }
}

fn body(response: &[u8]) -> Result<String, String> {
    let end = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .ok_or("HTTP response missing header separator")?;
    let status_end = response
        .windows(2)
        .position(|bytes| bytes == b"\r\n")
        .ok_or("HTTP response missing status line")?;
    let status = String::from_utf8_lossy(&response[..status_end]);
    let bytes = status.as_bytes();
    if bytes.len() < 13
        || &bytes[..7] != b"HTTP/1."
        || !bytes[7].is_ascii_digit()
        || &bytes[8..13] != b" 200 "
    {
        return Err(format!("HTTP status: {}", crate::diagnostic_text(&status)));
    }
    let payload = &response[end + 4..];
    for header in response[..end].split(|byte| *byte == b'\n').skip(1) {
        if let Some(colon) = header.iter().position(|byte| *byte == b':') {
            if header[..colon].eq_ignore_ascii_case(b"Content-Length") {
                let value = std::str::from_utf8(&header[colon + 1..])
                    .map_err(|_| "malformed HTTP Content-Length")?
                    .trim();
                let length =
                    crate::config::unsigned(value).map_err(|_| "malformed HTTP Content-Length")?;
                if payload.len() as u64 != length {
                    return Err(format!(
                        "HTTP body length mismatch: got {} of {length} bytes",
                        payload.len()
                    ));
                }
            }
        }
    }
    String::from_utf8(payload.to_vec()).map_err(|error| format!("HTTP body is not UTF-8: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_contract() {
        let url = Url::parse("http://www.example/server-qos?auto").unwrap();
        assert_eq!(request(&url), format!("GET /server-qos?auto HTTP/1.0\r\nHost: www.example\r\nUser-Agent: modqos-stats/{}\r\nConnection: close\r\n\r\n", env!("CARGO_PKG_VERSION")));
        let url = Url::parse("http://www.example:8080/").unwrap();
        assert!(request(&url).contains("Host: www.example:8080\r\n"));
    }

    #[test]
    fn response_boundaries() {
        assert_eq!(body(b"HTTP/1.9 200 OK\r\n\r\nhello").unwrap(), "hello");
        assert_eq!(body(b"HTTP/1.0 200 \r\n\r\n").unwrap(), "");
        for response in [
            b"HTTP/1.0 500 Nope\r\n\r\n".as_slice(),
            b"HTTP/2.0 200 OK\r\n\r\n",
            b"HTTP/1.x 200 OK\r\n\r\n",
            b"HTTP/1.0 2000 OK\r\n\r\n",
            b"HTTP/1.0 200 OK\n\n",
            b"HTTP/1.0 200 OK\r\n\r\n\xff",
        ] {
            assert!(body(response).is_err());
        }
        assert!(body(b"HTTP/1.0 503 Busy\r\n\r\n")
            .unwrap_err()
            .contains("HTTP/1.0 503 Busy"));
    }

    #[test]
    fn content_length_integrity() {
        assert_eq!(
            body(b"HTTP/1.0 200 OK\r\ncOnTeNt-LeNgTh: 5\r\n\r\nhello").unwrap(),
            "hello"
        );
        for (length, payload) in [
            ("6", "hello"),
            ("4", "hello"),
            ("nope", "hello"),
            ("-1", ""),
            ("", ""),
        ] {
            assert!(body(
                format!("HTTP/1.0 200 OK\r\nContent-Length: {length}\r\n\r\n{payload}").as_bytes()
            )
            .is_err());
        }
    }

    #[test]
    fn response_cap() {
        let mut input = std::io::Cursor::new(vec![b'x'; MAX_RESPONSE]);
        assert_eq!(
            read_response(&mut input, || Ok(())).unwrap().len(),
            MAX_RESPONSE
        );
        let mut input = std::io::Cursor::new(vec![b'x'; MAX_RESPONSE + 1]);
        assert!(read_response(&mut input, || Ok(()))
            .unwrap_err()
            .contains("1 MiB"));
    }
}
