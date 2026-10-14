use crate::config::unsigned;

#[derive(Debug)]
pub struct Rule {
    pub line: usize,
    pub host: String,
    pub port: u64,
    pub kind: &'static str,
    pub pattern: String,
    pub current: u64,
    pub limit: u64,
}

#[derive(Debug, Default)]
pub struct Page {
    pub connections: Option<u64>,
    pub rules: Vec<Rule>,
    pub warnings: Vec<String>,
}

pub fn parse(body: &str) -> Result<Page, String> {
    if !body.is_empty() && !body.ends_with('\n') {
        return Err("page truncated: missing final newline".into());
    }
    let mut page = Page::default();
    for (index, line) in body.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        parse_line(line, index + 1, &mut page)
            .map_err(|error| format!("page line {}: {error}", index + 1))?;
    }
    Ok(page)
}

fn parse_line(line: &str, line_number: usize, page: &mut Page) -> Result<(), String> {
    let mut fields = line.splitn(4, ';');
    match fields.next() {
        Some("b" | "v") => (),
        _ => return Err("bad counter prefix (expected b or v)".into()),
    }
    let host = fields
        .next()
        .filter(|host| !host.is_empty())
        .ok_or("missing hostname")?;
    let port = unsigned(fields.next().ok_or("missing port")?)?;
    let record = fields.next().ok_or("missing counter name")?;
    let end = record.find([';', ':']).ok_or("missing counter value")?;
    let name = &record[..end];
    if name.is_empty() {
        return Err("missing counter name".into());
    }
    match name {
        "QS_AllConn" => {
            if page.connections.is_some() {
                return Err("duplicate QS_AllConn".into());
            }
            let value = record
                .strip_prefix("QS_AllConn: ")
                .ok_or("malformed QS_AllConn")?;
            page.connections = Some(unsigned(value)?);
        }
        "QS_LocRequestLimit" | "QS_LocRequestLimitMatch" => {
            let rule = record[end..]
                .strip_prefix(';')
                .ok_or("missing request limit")?;
            let open = rule.find('[').ok_or("missing [ after limit")?;
            let close = rule.rfind("]: ").ok_or("missing ]: delimiter")?;
            if close <= open + 1 {
                return Err("empty or malformed pattern".into());
            }
            page.rules.push(Rule {
                line: line_number,
                host: host.into(),
                port,
                kind: if name == "QS_LocRequestLimit" {
                    "loc"
                } else {
                    "match"
                },
                pattern: rule[open + 1..close].into(),
                current: unsigned(&rule[close + 3..])?,
                limit: unsigned(&rule[..open])?,
            });
        }
        _ => {
            let (_, value) = record.rsplit_once(": ").ok_or("missing counter value")?;
            unsigned(value)?;
            if !page.warnings.iter().any(|warning| warning == name) {
                page.warnings.push(name.into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn counters_and_regex_delimiters() {
        let page = parse(indoc! {"
            b;www.example;0;QS_AllConn: 91
            v;www.example;443;QS_LocRequestLimit;25[/]: 0
            v;git.example;80;QS_LocRequestLimitMatch;15[^/[a-z];x]]: 2
        "})
        .unwrap();
        assert_eq!(page.connections, Some(91));
        assert_eq!(page.rules.len(), 2);
        assert_eq!(page.rules[0].kind, "loc");
        assert_eq!(page.rules[0].limit, 25);
        assert_eq!(page.rules[1].kind, "match");
        assert_eq!(page.rules[1].pattern, "^/[a-z];x]");
        assert_eq!(page.rules[1].current, 2);
    }

    #[test]
    fn empty_and_unknown_counters() {
        assert_eq!(parse("\n").unwrap().connections, None);
        let page = parse("v;www.example;80;QS_ClientPrefer: 1\nv;www.example;80;QS_ClientPrefer: 2\nv;www.example;80;QS_EventRequestLimit;1[x]: 3\n").unwrap();
        assert_eq!(page.warnings, ["QS_ClientPrefer", "QS_EventRequestLimit"]);
        assert!(page.rules.is_empty());
    }

    #[test]
    fn truncated_page() {
        assert_eq!(
            parse("b;www.example;0;QS_AllConn: 1").unwrap_err(),
            "page truncated: missing final newline"
        );
        assert!(parse("").is_ok());
        assert!(parse("b;www.example;0;QS_AllConn: 1\n").is_ok());
    }

    #[test]
    fn malformed_lines() {
        for line in [
            "bad",
            "x;www.example;80;QS_AllConn: 1",
            "b;;80;QS_AllConn: 1",
            "b;www.example;x;QS_AllConn: 1",
            "b;www.example;-1;QS_AllConn: 1",
            "b;www.example;0;QS_AllConn: +1",
            "b;www.example;0;QS_AllConn: 18446744073709551616",
            "v;www.example;80;QS_LocRequestLimit;1[/]: -1",
            "v;www.example;80;QS_LocRequestLimit;-1[/]: 1",
            "v;www.example;80;QS_LocRequestLimit;1[/]",
            "v;www.example;80;QS_LocRequestLimit;1[]: 1",
            "v;www.example;80;QS_ClientPrefer: nope",
        ] {
            assert!(
                parse(&format!("\n{line}\n"))
                    .unwrap_err()
                    .contains("line 2"),
                "{line}"
            );
        }
        assert!(
            parse("b;www.example;0;QS_AllConn: 1\nb;www.example;0;QS_AllConn: 1\n")
                .unwrap_err()
                .contains("line 2")
        );
    }
}
