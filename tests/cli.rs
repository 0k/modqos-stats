use indoc::indoc;
use similar::TextDiff;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_modqos-stats");
const PAGE: &str = indoc! {"
    b;www.example;0;QS_AllConn: 91
    v;git.example;443;QS_LocRequestLimit;25[/]: 1
    v;www.example;443;QS_LocRequestLimit;15[/longpolling]: 0
"};
const COLLECT: &str = indoc! {"
    conn connections 91
    qos qos/git.example_443/loc/_2f 1 25
    qos qos/www.example_443/loc/_2flongpolling 0 15
"};
const PRETTY: &str = indoc! {"
    connections: 91
    requests:
      location                       current    limit
      git.example:443/                     1       25
      www.example:443/longpolling          0       15
"};
// The README.org § pretty example.
const EXAMPLE: &str = indoc! {"
    conn connections 72
    qos qos/git.example_443/loc/_2f 1 25
    qos qos/www.example_443/loc/_2f 0 25
    qos qos/www.example_443/loc/_2flongpolling 0 15
    qos qos/www.example_8080/match/_5e_2fapi_2f._2a_24 3 10
"};
const EXAMPLE_PRETTY: &str = indoc! {"
    connections: 72
    requests:
      location                       current    limit
      git.example:443/                     1       25
      www.example:443/                     0       25
      www.example:443/longpolling          0       15
      www.example:8080 ~^/api/.*$          3       10
"};
const EXAMPLE_PRETTY_COLOR: &str = indoc! {"
    \x1b[1;37mconnections\x1b[0m: 72
    \x1b[1;37mrequests\x1b[0m:
      \x1b[1;30mlocation                       current    limit\x1b[0m
      git.example:443/                     1       25
      www.example:443/                     0       25
      www.example:443/longpolling          0       15
      www.example:8080 ~^/api/.*$          3       10
"};

fn assert_output_eq(actual: &str, expected: &str) {
    if actual != expected {
        for line in TextDiff::from_lines(expected, actual)
            .unified_diff()
            .header("expected", "actual")
            .to_string()
            .lines()
        {
            let color = match line.chars().next() {
                Some('-') => "31",
                Some('+') => "32",
                Some('@') => "36",
                _ => "0",
            };
            eprintln!("\x1b[{color}m{line}\x1b[0m");
        }
        panic!("output mismatch");
    }
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

fn run(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(BIN)
        .args(args)
        .env("MODQOS_STATS_CONFIG", dir.path().join("missing-config.yml"))
        .output()
        .unwrap()
}

fn pretty(input: &[u8]) -> Output {
    pretty_with(input, &[])
}

fn pretty_with(input: &[u8], environment: &[(&str, &str)]) -> Output {
    let mut child = Command::new(BIN)
        .arg("pretty")
        // The test runner's own color settings must not reach the binary.
        .env_remove("VPS_STATS_COLOR")
        .env_remove("NO_COLOR")
        .envs(environment.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn assert_pretty(input: &str, environment: &[(&str, &str)], expected: &str) {
    let output = pretty_with(input.as_bytes(), environment);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_output_eq(text(&output.stdout), expected);
    assert_output_eq(text(&output.stderr), "");
}

fn error(output: &Output, code: i32, message: &str) {
    assert_eq!(output.status.code(), Some(code));
    assert_output_eq(text(&output.stdout), "");
    assert!(text(&output.stderr).starts_with("modqos-stats: error: "));
    assert!(
        text(&output.stderr).contains(message),
        "{}",
        text(&output.stderr)
    );
}

fn configured(config: &str) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yml");
    std::fs::write(&path, config).unwrap();
    Command::new(BIN)
        .arg("collect")
        .env("MODQOS_STATS_CONFIG", path)
        .output()
        .unwrap()
}

fn served(response: Vec<u8>, stall: bool) -> (Output, String) {
    served_with_drip(response, stall, false)
}

fn served_with_drip(response: Vec<u8>, stall: bool, drip: bool) -> (Output, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "client never connected");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        if stall {
            // Wait for the client to close after its read timeout, not a fixed sleep.
            assert_eq!(socket.read(&mut byte).unwrap(), 0);
        } else if drip {
            socket.write_all(b"HTTP/1.0 200 OK\r\n\r\n").unwrap();
            for byte in response {
                if socket.write_all(&[byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(300));
            }
        } else {
            if let Err(error) = socket.write_all(&response) {
                assert!(response.len() > 1024 * 1024);
                assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ));
            }
        }
        String::from_utf8(request).unwrap()
    });
    let output = configured(&format!(
        "url: http://127.0.0.1:{port}/server-qos?auto\ntimeout: 1\n"
    ));
    let request = server.join().unwrap();
    (output, request)
}

fn page(body: &str) -> Output {
    served(format!("HTTP/1.0 200 OK\r\n\r\n{body}").into_bytes(), false).0
}

#[test]
fn def_without_config() {
    let output = run(&["def"]);
    assert!(output.status.success());
    assert_output_eq(text(&output.stderr), "");
    assert_output_eq(
        text(&output.stdout),
        "conn all_conn:GAUGE:0:U\nqos current:GAUGE:0:U limit:GAUGE:0:U\n",
    );
}

#[test]
fn cli_contract() {
    for args in [&[][..], &["def", "collect"], &["--unknown"], &["-"]] {
        error(&run(args), 2, "usage");
    }
    error(&run(&["graph"]), 3, "action not implemented");
    for flag in ["-V", "--version"] {
        let output = run(&[flag]);
        assert!(output.status.success());
        assert_output_eq(
            text(&output.stdout),
            concat!("modqos-stats ", env!("CARGO_PKG_VERSION"), "\n"),
        );
        assert_output_eq(text(&output.stderr), "");
    }
    for flag in ["-h", "--help"] {
        let output = run(&[flag]);
        assert!(output.status.success());
        assert_output_eq(text(&output.stdout), "Usage: modqos-stats <def|collect|pretty>\n       modqos-stats <-h|--help|-V|--version>\n");
        assert_output_eq(text(&output.stderr), "");
    }
}

#[test]
fn collect_and_pretty() {
    let (output, request) = served(
        format!("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n{PAGE}").into_bytes(),
        false,
    );
    assert!(output.status.success());
    assert_output_eq(text(&output.stdout), COLLECT);
    assert_output_eq(text(&output.stderr), "");
    let host = request
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("Host: ")
        .unwrap();
    assert!(host.starts_with("127.0.0.1:"));
    assert_output_eq(&request, &format!("GET /server-qos?auto HTTP/1.0\r\nHost: {host}\r\nUser-Agent: modqos-stats/{}\r\nConnection: close\r\n\r\n", env!("CARGO_PKG_VERSION")));
    let rendered = pretty(&output.stdout);
    assert!(rendered.status.success());
    assert_output_eq(text(&rendered.stdout), PRETTY);
    assert_output_eq(text(&rendered.stderr), "");
}

#[test]
fn connection_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    error(
        &configured(&format!("url: http://127.0.0.1:{port}/\n")),
        1,
        "HTTP",
    );
}

#[cfg(unix)]
#[test]
fn stdout_write_failure() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let output = Command::new(BIN)
        .arg("def")
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    error(&output, 1, "stdout:");
}

#[test]
fn pretty_path_rejections() {
    for (segment, message) in [
        ("_00".to_string(), "control character in qos path"),
        ("_7f".to_string(), "control character in qos path"),
        ("a".repeat(201), "encoded segment exceeds 200 bytes"),
    ] {
        error(
            &pretty(
                format!("conn connections 1\nqos qos/www.example_80/loc/{segment} 0 1\n")
                    .as_bytes(),
            ),
            1,
            message,
        );
    }
    error(
        &pretty(
            format!(
                "conn connections 1\nqos qos/{}_80/loc/_2f 0 1\n",
                "a".repeat(198)
            )
            .as_bytes(),
        ),
        1,
        "host/port segment exceeds 200 bytes",
    );
}

#[test]
fn quoted_config_trailing_text() {
    error(
        &configured("url: 'http://www.example/' garbage\n"),
        1,
        "config line 1: unexpected content after quoted value",
    );
}

#[test]
fn empty_counter_name() {
    error(
        &page("v;www.example;80;: 1\n"),
        1,
        "page line 1: missing counter name",
    );
}

#[test]
fn runtime_errors() {
    error(&run(&["collect"]), 1, "missing-config.yml");
    error(
        &configured("url: https://www.example/server-qos?auto\n"),
        1,
        "TLS is not supported",
    );
    error(
        &configured("url: http://www.example\nunknown: value\n"),
        1,
        "line 2",
    );
}

#[test]
fn malformed_pages() {
    for bad in [
        "x;www.example;80;QS_AllConn: 1",
        "b;www.example;x;QS_AllConn: 1",
        "b;www.example;0;QS_AllConn: -1",
        "v;www.example;443;QS_LocRequestLimit;25[/] 1",
        "v;www.example;443;QS_LocRequestLimit;25[]: 1",
        "v;www.example;443;QS_LocRequestLimit;x[/]: 1",
    ] {
        error(&page(&format!("{PAGE}{bad}\n")), 1, "line 4");
    }
    error(
        &page(&format!("{PAGE}b;www.example;0;QS_AllConn: 2\n")),
        1,
        "line 4",
    );
    let oversized = format!(
        "v;www.example;443;QS_LocRequestLimit;25[{}]: 1\n",
        "a".repeat(201)
    );
    error(&page(&oversized), 1, "200");
}

#[test]
fn duplicate_paths() {
    let output = page(&format!(
        "{PAGE}v;git.example;443;QS_LocRequestLimit;30[/]: 2\n"
    ));
    error(
        &output,
        1,
        "page line 4: duplicate qos path (first at page line 2)",
    );
}

#[test]
fn empty_page_and_warning_deduplication() {
    let empty = page("");
    assert!(empty.status.success());
    assert_output_eq(text(&empty.stdout), "conn connections U\n");
    let output = page("v;www.example;443;QS_ClientPrefer: 1\nv;git.example;443;QS_ClientPrefer: 2\nv;www.example;443;QS_EventRequestLimit;5[foo]: 1\n");
    assert!(output.status.success());
    assert_output_eq(text(&output.stdout), "conn connections U\n");
    assert_output_eq(text(&output.stderr), "modqos-stats: warning: unsupported counter QS_ClientPrefer\nmodqos-stats: warning: unsupported counter QS_EventRequestLimit\n");
}

#[test]
fn http_failures() {
    for response in [
        b"HTTP/1.0 503 Busy\r\n\r\n".to_vec(),
        b"HTTP/1.0 200 OK\r\n\r\n\xff".to_vec(),
        b"HTTP/1.0 200 OK\r\n".to_vec(),
    ] {
        let output = served(response, false).0;
        error(&output, 1, "HTTP");
    }
}

#[test]
fn truncated_http_body() {
    let response = format!(
        "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{PAGE}",
        PAGE.len() + 1
    );
    error(
        &served(response.into_bytes(), false).0,
        1,
        &format!(
            "HTTP body length mismatch: got {} of {} bytes",
            PAGE.len(),
            PAGE.len() + 1
        ),
    );
}

#[test]
fn overlong_http_body() {
    let response = format!(
        "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{PAGE}",
        PAGE.len() - 1
    );
    error(
        &served(response.into_bytes(), false).0,
        1,
        &format!(
            "HTTP body length mismatch: got {} of {} bytes",
            PAGE.len(),
            PAGE.len() - 1
        ),
    );
}

#[test]
fn truncated_page_without_content_length() {
    error(&page(PAGE.trim_end_matches('\n')), 1, "page truncated");
}

#[test]
fn oversized_http_response() {
    let mut response = b"HTTP/1.0 200 OK\r\n\r\n".to_vec();
    response.resize(1024 * 1024 + 1, b' ');
    error(&served(response, false).0, 1, "1 MiB");
}

#[test]
fn config_read_failure() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .arg("collect")
        .env("MODQOS_STATS_CONFIG", dir.path())
        .output()
        .unwrap();
    error(&output, 1, dir.path().to_str().unwrap());
}

#[test]
fn match_pattern_and_host_encoding() {
    let output = page("b;a_b.example;0;QS_LocRequestLimitMatch;15[^/[a-z];x]]: 2\n");
    assert!(output.status.success());
    assert_output_eq(
        text(&output.stdout),
        "conn connections U\nqos qos/a_5fb.example_0/match/_5e_2f_5ba-z_5d_3bx_5d 2 15\n",
    );
    assert_output_eq(text(&output.stderr), "");
    let rendered = pretty(&output.stdout);
    assert!(rendered.status.success());
    assert_output_eq(
        text(&rendered.stdout),
        indoc! {"
            connections: U
            requests:
              location                     current    limit
              a_b.example:0 ~^/[a-z];x]          2       15
        "},
    );
}

#[test]
fn overall_deadline_with_dripping_response() {
    let start = Instant::now();
    let output = served_with_drip(vec![b'\n'; 12], false, true).0;
    error(&output, 1, "HTTP timeout:");
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert!(start.elapsed() < Duration::from_millis(2500));
}

#[test]
fn timeout() {
    let start = Instant::now();
    error(&served(Vec::new(), true).0, 1, "timeout");
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert!(start.elapsed() < Duration::from_secs(4));
}

#[test]
fn dot_segments() {
    for (plain, encoded) in [(".", "_2e"), ("..", "_2e_2e")] {
        let output = page(&format!(
            "v;www.example;80;QS_LocRequestLimit;1[{plain}]: 0\n"
        ));
        assert!(output.status.success());
        assert_output_eq(
            text(&output.stdout),
            &format!("conn connections U\nqos qos/www.example_80/loc/{encoded} 0 1\n"),
        );
        assert_output_eq(text(&output.stderr), "");
        assert_pretty(text(&output.stdout), &[], &format!("connections: U\nrequests:\n  location{}current    limit\n  www.example:80{plain}          0        1\n", " ".repeat(10 + plain.len())));
        error(
            &pretty(
                format!("conn connections U\nqos qos/www.example_80/loc/{plain} 0 1\n").as_bytes(),
            ),
            1,
            "non-canonical encoded segment",
        );
    }
}

#[test]
fn duplicate_connections_message() {
    let output = pretty(b"conn connections 1\nconn connections 2\n");
    error(&output, 1, "duplicate connections record");
    assert_output_eq(
        text(&output.stderr),
        "modqos-stats: error: stdin line 2: duplicate connections record\n",
    );
}

#[test]
fn http_status_diagnostic_text() {
    for (status, escaped) in [
        (
            "HTTP/1.0 503 \x1b\t\x7f\rOops".to_string(),
            "HTTP/1.0 503 \\x1b\\x09\\x7f\\x0dOops".to_string(),
        ),
        (
            format!("HTTP/1.0 503 {}", "é".repeat(201)),
            format!("HTTP/1.0 503 {}...", "é".repeat(187)),
        ),
    ] {
        let output = served(format!("{status}\r\n\r\n").into_bytes(), false).0;
        error(&output, 1, "HTTP status:");
        assert_output_eq(
            text(&output.stderr),
            &format!("modqos-stats: error: HTTP status: {escaped}\n"),
        );
    }
}

#[test]
fn counter_warning_diagnostic_text() {
    for (name, escaped) in [
        (
            "QS_\x1b\t\x7f\rOops".to_string(),
            "QS_\\x1b\\x09\\x7f\\x0dOops".to_string(),
        ),
        ("é".repeat(201), format!("{}...", "é".repeat(200))),
    ] {
        let output = page(&format!("v;www.example;80;{name}: 1\n"));
        assert!(output.status.success());
        assert_output_eq(text(&output.stdout), "conn connections U\n");
        assert_output_eq(
            text(&output.stderr),
            &format!("modqos-stats: warning: unsupported counter {escaped}\n"),
        );
    }
}

#[test]
fn pretty_invalid_input() {
    for input in [
        "",
        "other x 1\n",
        "conn connections\n",
        "conn connections -1\n",
        "qos qos/www.example_443/loc/_2f 1 25\n",
        "conn connections 1\nqos qos/www.example_443/other/_2f 1 25\n",
        "conn connections 1\nqos qos/www.example_443/loc/_zz 1 25\n",
        "conn connections 1\nqos qos/www.example_443/loc/_2f X 25\n",
        "conn connections 1\nqos qos/www.example_443/loc/_2f 1 25 extra\n",
    ] {
        error(&pretty(input.as_bytes()), 1, "line");
    }
    error(&pretty(b"\xff"), 1, "stdin");
}

#[test]
fn pretty_color_decision() {
    let cases: [(&[(&str, &str)], &str); 6] = [
        (&[], EXAMPLE_PRETTY),
        (&[("VPS_STATS_COLOR", "yes")], EXAMPLE_PRETTY_COLOR),
        (
            &[("VPS_STATS_COLOR", "yes"), ("NO_COLOR", "")],
            EXAMPLE_PRETTY_COLOR,
        ),
        (
            &[("VPS_STATS_COLOR", "yes"), ("NO_COLOR", "1")],
            EXAMPLE_PRETTY,
        ),
        (&[("VPS_STATS_COLOR", "no")], EXAMPLE_PRETTY),
        (&[("VPS_STATS_COLOR", "true")], EXAMPLE_PRETTY),
    ];
    for (environment, expected) in cases {
        eprintln!("environment: {environment:?}");
        assert_pretty(EXAMPLE, environment, expected);
    }
}

#[test]
fn pretty_connections_only() {
    for (input, expected) in [
        ("conn connections 72\n", "connections: 72\n"),
        ("conn connections U\n", "connections: U\n"),
    ] {
        assert_pretty(input, &[], expected);
    }
}

#[test]
fn pretty_sorting() {
    // Reverse of the expected order: host bytes ("Z" < "a"), then port as a
    // number (80 < 443), then kind (loc before match), then pattern.
    let input = indoc! {"
        conn connections 6
        qos qos/a.example_443/match/_2fb 0 6
        qos qos/a.example_443/match/_2fa 0 5
        qos qos/a.example_443/loc/_2fc 0 4
        qos qos/a.example_443/loc/_2fb 0 3
        qos qos/a.example_80/loc/_2f 0 2
        qos qos/Z.example_443/loc/_2f 0 1
    "};
    assert_pretty(
        input,
        &[],
        indoc! {"
            connections: 6
            requests:
              location             current    limit
              Z.example:443/             0        1
              a.example:80/              0        2
              a.example:443/b            0        3
              a.example:443/c            0        4
              a.example:443 ~/a          0        5
              a.example:443 ~/b          0        6
        "},
    );
}
