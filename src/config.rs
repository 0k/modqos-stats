use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(input: &str) -> Result<Self, String> {
        if input.starts_with("https://") {
            return Err("TLS is not supported (plain HTTP only)".into());
        }
        let rest = input
            .strip_prefix("http://")
            .ok_or("URL must start with http://")?;
        if rest
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            || rest.contains(['#', '\\'])
        {
            return Err("invalid HTTP URL".into());
        }
        let boundary = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..boundary];
        let suffix = &rest[boundary..];
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, parse_port(port)?),
            None => (authority, 80),
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err("invalid or empty HTTP host".into());
        }
        let path = if suffix.is_empty() {
            "/".into()
        } else if suffix.starts_with('?') {
            format!("/{suffix}")
        } else {
            suffix.into()
        };
        Ok(Self {
            host: host.into(),
            port,
            path,
        })
    }
}

pub fn unsigned(input: &str) -> Result<u64, String> {
    if input.is_empty() || !input.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected a non-negative integer".into());
    }
    input.parse().map_err(|_| "integer out of range".into())
}

pub fn parse_port(input: &str) -> Result<u16, String> {
    let number = unsigned(input).map_err(|_| "invalid port (expected 1-65535)")?;
    u16::try_from(number)
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| "invalid port (expected 1-65535)".into())
}

#[derive(Debug)]
pub struct Config {
    pub url: Url,
    pub timeout: Duration,
}

pub fn load() -> Result<Config, String> {
    let path = config_path(std::env::var_os("MODQOS_STATS_CONFIG"));
    let path = Path::new(&path);
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read config {}: {error}", path.display()))?;
    parse(&content).map_err(|error| format!("{}: {error}", path.display()))
}

fn config_path(override_path: Option<std::ffi::OsString>) -> std::ffi::OsString {
    match override_path {
        Some(value) if !value.is_empty() => value,
        _ => "/etc/modqos-stats/config.yml".into(),
    }
}

pub fn parse(content: &str) -> Result<Config, String> {
    let mut url = None;
    let mut timeout = None;
    for (index, line) in content.lines().enumerate() {
        let fail = |message: String| format!("config line {}: {message}", index + 1);
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            return Err(fail("indented/nested values are unsupported".into()));
        }
        let (key, raw) = line
            .split_once(": ")
            .ok_or_else(|| fail("expected key: value".into()))?;
        let value = scalar(raw).map_err(&fail)?;
        match key {
            "url" => {
                if url.is_some() {
                    return Err(fail("duplicate key url".into()));
                }
                url = Some(Url::parse(value).map_err(&fail)?);
            }
            "timeout" => {
                if timeout.is_some() {
                    return Err(fail("duplicate key timeout".into()));
                }
                let seconds = unsigned(value).map_err(&fail)?;
                if seconds == 0 {
                    return Err(fail("timeout must be at least 1 second".into()));
                }
                timeout = Some(Duration::from_secs(seconds));
            }
            _ => return Err(fail(format!("unknown key {key}"))),
        }
    }
    Ok(Config {
        url: url.ok_or("missing url in config")?,
        timeout: timeout.unwrap_or(Duration::from_secs(5)),
    })
}

fn scalar(raw: &str) -> Result<&str, String> {
    let raw = raw.trim();
    let value = if raw.starts_with(['\'', '"']) {
        let quote = &raw[..1];
        let end = raw[1..].find(quote).ok_or("unclosed quoted value")? + 1;
        let tail = raw[end + 1..].trim_start();
        if !tail.is_empty() && !tail.starts_with("# ") {
            return Err("unexpected content after quoted value".into());
        }
        &raw[1..end]
    } else {
        raw.split_once(" # ")
            .map_or(raw, |(value, _)| value)
            .trim_end()
    };
    if value.is_empty() {
        return Err("empty value".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn config_path_selection() {
        for input in [None, Some("".into())] {
            assert_eq!(config_path(input), "/etc/modqos-stats/config.yml");
        }
        assert_eq!(config_path(Some("custom.yml".into())), "custom.yml");
    }

    #[test]
    fn yaml_subset() {
        let config = parse(indoc! {"
            # private HTTP endpoint
            url: 'http://www.example:8080/server-qos?auto' # comment
            timeout: \"2\"
        "})
        .unwrap();
        assert_eq!(config.url.host, "www.example");
        assert_eq!(config.url.port, 8080);
        assert_eq!(config.url.path, "/server-qos?auto");
        assert_eq!(config.timeout.as_secs(), 2);
        assert_eq!(
            parse("url: http://www.example\n")
                .unwrap()
                .timeout
                .as_secs(),
            5
        );
        assert_eq!(
            parse("url: 'http://www.example/a # b'\n").unwrap_err(),
            "config line 1: invalid HTTP URL"
        );
    }

    #[test]
    fn invalid_yaml_has_line_number() {
        for line in [
            " url: http://www.example",
            "\turl: http://www.example",
            "timeout:0",
            "unknown: foo",
            "url: ",
            "timeout: 0",
            "timeout: -1",
            "timeout: +1",
            "timeout: 1.5",
            "timeout: 18446744073709551616",
            "url: 'unclosed",
            "url: [http://www.example]",
            "url: http://www.example #comment",
        ] {
            let error = parse(&format!("# comment\n{line}\n")).unwrap_err();
            assert!(error.contains("line 2"), "{line}: {error}");
        }
        for key in ["url: http://www.example", "timeout: 1"] {
            assert!(parse(&format!("{key}\n{key}\n"))
                .unwrap_err()
                .contains("line 2"));
        }
        assert!(parse("timeout: 1\n").unwrap_err().contains("missing url"));
    }

    #[test]
    fn url_boundaries() {
        for (input, path, port) in [
            ("http://www.example", "/", 80),
            ("http://www.example?auto", "/?auto", 80),
            ("http://www.example:65535/a?x=1", "/a?x=1", 65535),
            (
                "http://www.example/a?email=a@b&name='x'",
                "/a?email=a@b&name='x'",
                80,
            ),
        ] {
            let url = Url::parse(input).unwrap();
            assert_eq!(url.path, path);
            assert_eq!(url.port, port);
        }
        assert!(Url::parse("https://www.example/")
            .unwrap_err()
            .contains("TLS is not supported"));
        for input in [
            "ftp://www.example",
            "http://",
            "http:///a",
            "http://www.example:0",
            "http://www.example:65536",
            "http://www.example:no",
            "http://www.example:+80",
            "http://www.example:\r\n",
            "http://user@www.example/",
            "http://www.example/a\r\nInjected: x",
            "http://www.example/#fragment",
        ] {
            assert!(Url::parse(input).is_err(), "{input:?}");
        }
    }
}
