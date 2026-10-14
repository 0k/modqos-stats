use crate::config::unsigned;
use crate::qos::Page;
use std::collections::HashMap;

pub const DEF: &str = "conn all_conn:GAUGE:0:U\nqos current:GAUGE:0:U limit:GAUGE:0:U\n";
const MAX_SEGMENT: usize = 200;
const SEPARATOR: &str = "    ";
const INDENT: &str = "  ";
const WHITE: &str = "\x1b[1;37m";
const GRAY: &str = "\x1b[1;30m";
const NORMAL: &str = "\x1b[0m";

fn safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'
}

pub fn encode(input: &str) -> Result<String, String> {
    let mut encoded = String::new();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in input.bytes() {
        if safe(byte) && !matches!(input, "." | "..") {
            encoded.push(char::from(byte));
        } else {
            encoded.push('_');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    if encoded.is_empty() {
        return Err("empty encoded segment".into());
    }
    if encoded.len() > MAX_SEGMENT {
        return Err("encoded segment exceeds 200 bytes".into());
    }
    Ok(encoded)
}

pub fn decode(input: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut iter = input.bytes();
    while let Some(byte) = iter.next() {
        if safe(byte) {
            bytes.push(byte);
        } else if byte == b'_' {
            let high = iter.next().and_then(hex).ok_or("invalid encoded escape")?;
            let low = iter.next().and_then(hex).ok_or("invalid encoded escape")?;
            bytes.push(high * 16 + low);
        } else {
            return Err("invalid encoded character".into());
        }
    }
    let decoded = String::from_utf8(bytes).map_err(|_| "encoded segment is not UTF-8")?;
    if encode(&decoded)? != input {
        return Err("non-canonical encoded segment".into());
    }
    Ok(decoded)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub fn collect(page: &Page) -> Result<String, String> {
    let connections = page
        .connections
        .map_or_else(|| "U".into(), |value| value.to_string());
    let mut output = format!("conn connections {connections}\n");
    let mut paths = HashMap::new();
    for rule in &page.rules {
        let host = encode(&rule.host)?;
        let vhost = format!("{host}_{}", rule.port);
        if vhost.len() > MAX_SEGMENT {
            return Err("encoded host/port segment exceeds 200 bytes".into());
        }
        let pattern = encode(&rule.pattern)?;
        let path = format!("qos/{vhost}/{}/{pattern}", rule.kind);
        if let Some(first) = paths.insert(path.clone(), rule.line) {
            return Err(format!(
                "page line {}: duplicate qos path (first at page line {first})",
                rule.line
            ));
        }
        output.push_str(&format!("qos {path} {} {}\n", rule.current, rule.limit));
    }
    Ok(output)
}

fn value(input: &str) -> Result<(), String> {
    if input == "U" {
        Ok(())
    } else {
        unsigned(input).map(|_| ())
    }
}

/// Request-limit rule kind of a `qos` path; declaration order is the
/// `pretty` sort order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Loc,
    Match,
}

/// One validated, decoded `qos` record of the `pretty` input.
struct Row {
    host: String,
    port: u64,
    kind: Kind,
    pattern: String,
    current: String,
    limit: String,
}

fn row(path: &str, current: &str, limit: &str) -> Result<Row, String> {
    let fields: Vec<_> = path.split('/').collect();
    if fields.len() != 4 || fields[0] != "qos" || !matches!(fields[2], "loc" | "match") {
        return Err("bad qos path".into());
    }
    if fields[1].len() > MAX_SEGMENT {
        return Err("host/port segment exceeds 200 bytes".into());
    }
    let (host, port) = fields[1]
        .rsplit_once('_')
        .ok_or("missing port in qos path")?;
    let port = unsigned(port)?;
    let host = decode(host)?;
    let pattern = decode(fields[3])?;
    if host.chars().any(char::is_control) || pattern.chars().any(char::is_control) {
        return Err("control character in qos path".into());
    }
    value(current)?;
    value(limit)?;
    Ok(Row {
        host,
        port,
        kind: if fields[2] == "loc" {
            Kind::Loc
        } else {
            Kind::Match
        },
        pattern,
        current: current.into(),
        limit: limit.into(),
    })
}

/// Lay out `rows` (header row first) as columns separated by four spaces.
/// Each column is as wide as its widest cell, in characters; `right`
/// right-aligns a column, header included.  No padding trails a line.
fn table<const N: usize>(rows: &[[String; N]], right: [bool; N]) -> Vec<String> {
    let mut widths = [0; N];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                let padding = " ".repeat(widths[column] - cell.chars().count());
                if column > 0 {
                    line.push_str(SEPARATOR);
                }
                if right[column] {
                    line.push_str(&padding);
                    line.push_str(cell);
                } else {
                    line.push_str(cell);
                    if column + 1 < N {
                        line.push_str(&padding);
                    }
                }
            }
            line
        })
        .collect()
}

/// `text` wrapped in the ANSI color `code` when `color` is set.
fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("{code}{text}{NORMAL}")
    } else {
        text.into()
    }
}

pub fn pretty(input: &str, color: bool) -> Result<String, String> {
    let mut connections = None;
    let mut rows = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let fail = |error: String| format!("stdin line {}: {error}", index + 1);
        let fields: Vec<_> = line.split(' ').collect();
        match fields.as_slice() {
            ["conn", "connections", number] => {
                if connections.is_some() {
                    return Err(fail("duplicate connections record".into()));
                }
                value(number).map_err(&fail)?;
                connections = Some(*number);
            }
            ["qos", path, current, limit] => rows.push(row(path, current, limit).map_err(&fail)?),
            _ => return Err(fail("unknown schema or wrong field count".into())),
        }
    }
    let connections =
        connections.ok_or("stdin line 1: missing connections record (or empty stdin)")?;
    let mut output = format!("{}: {connections}\n", paint(color, WHITE, "connections"));
    if rows.is_empty() {
        return Ok(output);
    }
    rows.sort_by(|a, b| {
        (&a.host, a.port, &a.kind, &a.pattern).cmp(&(&b.host, b.port, &b.kind, &b.pattern))
    });
    output.push_str(&format!("{}:\n", paint(color, WHITE, "requests")));
    let mut cells = vec![["location", "current", "limit"].map(String::from)];
    cells.extend(rows.into_iter().map(|row| {
        let location = match row.kind {
            Kind::Loc => format!("{}:{}{}", row.host, row.port, row.pattern),
            Kind::Match => format!("{}:{} ~{}", row.host, row.port, row.pattern),
        };
        [location, row.current, row.limit]
    }));
    for (index, line) in table(&cells, [false, true, true]).into_iter().enumerate() {
        let line = if index == 0 {
            paint(color, GRAY, &line)
        } else {
            line
        };
        output.push_str(&format!("{INDENT}{line}\n"));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn encoding_round_trip_and_boundaries() {
        for (plain, encoded) in [
            ("/", "_2f"),
            ("/longpolling", "_2flongpolling"),
            ("_", "_5f"),
            ("a Zé", "a_20Z_c3_a9"),
            ("A-z.09", "A-z.09"),
            ("[];", "_5b_5d_3b"),
        ] {
            assert_eq!(encode(plain).unwrap(), encoded);
            assert_eq!(decode(encoded).unwrap(), plain);
        }
        assert!(encode(&"a".repeat(200)).is_ok());
        assert!(encode(&"a".repeat(201)).is_err());
        assert!(encode(&"/".repeat(67)).is_err());
        for encoded in ["_", "_2", "_gg", "_2F", "/", "_ff", "_61", ""] {
            assert!(decode(encoded).is_err(), "{encoded}");
        }
        for byte in 0..=127 {
            let plain = char::from(byte).to_string();
            assert_eq!(decode(&encode(&plain).unwrap()).unwrap(), plain);
        }
        for plain in ["中文", "😀", "a_b.example", "^/[a-z];x]"] {
            assert_eq!(decode(&encode(plain).unwrap()).unwrap(), plain);
        }
    }

    #[test]
    fn dot_segments() {
        for (plain, encoded) in [(".", "_2e"), ("..", "_2e_2e")] {
            assert_eq!(encode(plain).unwrap(), encoded);
            assert_eq!(decode(encoded).unwrap(), plain);
            assert_eq!(decode(plain).unwrap_err(), "non-canonical encoded segment");
        }
    }

    #[test]
    fn duplicate_paths() {
        let page = crate::qos::parse(indoc! {"
            v;www.example;80;QS_LocRequestLimit;1[/]: 0

            v;www.example;80;QS_LocRequestLimit;2[/]: 1
        "})
        .unwrap();
        assert_eq!(
            collect(&page).unwrap_err(),
            "page line 3: duplicate qos path (first at page line 1)"
        );
        let page = crate::qos::parse(indoc! {"
            v;www.example;80;QS_LocRequestLimit;1[/]: 0
            v;www.example;80;QS_LocRequestLimitMatch;2[/]: 1
        "})
        .unwrap();
        assert_eq!(collect(&page).unwrap(), "conn connections U\nqos qos/www.example_80/loc/_2f 0 1\nqos qos/www.example_80/match/_2f 1 2\n");
    }

    #[test]
    fn collect_filename_boundaries() {
        for (length, accepted) in [(197, true), (198, false)] {
            let page = crate::qos::parse(&format!(
                "v;{};80;QS_LocRequestLimit;1[/]: 0\n",
                "a".repeat(length)
            ))
            .unwrap();
            assert_eq!(collect(&page).is_ok(), accepted);
        }
        for (length, accepted) in [(200, true), (201, false)] {
            let page = crate::qos::parse(&format!(
                "v;www.example;80;QS_LocRequestLimit;1[{}]: 0\n",
                "a".repeat(length)
            ))
            .unwrap();
            assert_eq!(collect(&page).is_ok(), accepted);
        }
    }

    #[test]
    fn pretty_unknown_and_match() {
        let output = pretty(
            "conn connections 0\nqos qos/a_5fb.example_80/match/_5ba-z_5d 0 U\n",
            false,
        );
        assert_eq!(
            output.unwrap(),
            indoc! {"
                connections: 0
                requests:
                  location                 current    limit
                  a_b.example:80 ~[a-z]          0        U
            "}
        );
    }

    #[test]
    fn pretty_path_and_value_validation() {
        for line in [
            "conn connections U extra",
            "conn other 1",
            "qos qos/www.example_+0/loc/_2f 1 2",
            "qos qos/www.example_18446744073709551616/loc/_2f 1 2",
            "qos qos/www.example_x/loc/_2f 1 2",
            "qos qos/www.example_80/loc/ 1 2",
            "qos qos/_80/loc/_2f 1 2",
            "qos qos/www.example_80/loc/_2f/extra 1 2",
            "qos other/www.example_80/loc/_2f 1 2",
            "qos qos/www.example_80/loc/_2f +1 2",
            "qos qos/www.example_80/loc/_2f 1 -2",
            "conn connections 1",
        ] {
            assert!(
                pretty(&format!("conn connections 1\n{line}\n"), false)
                    .unwrap_err()
                    .contains("line 2"),
                "{line}"
            );
        }
    }
}
