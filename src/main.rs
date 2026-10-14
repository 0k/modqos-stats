#![forbid(unsafe_code)]

mod config;
mod http;
mod plugin;
mod qos;

use std::io::{self, Read, Write};
use std::process::ExitCode;

const USAGE: &str =
    "Usage: modqos-stats <def|collect|pretty>\n       modqos-stats <-h|--help|-V|--version>\n";

fn run() -> Result<String, (u8, String)> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err((2, "usage: exactly one action is required".into()));
    }
    let action = args[0]
        .to_str()
        .ok_or((2, "usage: argument is not UTF-8".into()))?;
    let result = match action {
        "-h" | "--help" => Ok(USAGE.into()),
        "-V" | "--version" => Ok(format!("modqos-stats {}\n", env!("CARGO_PKG_VERSION"))),
        "def" => Ok(plugin::DEF.into()),
        "collect" => collect(),
        "pretty" => {
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .map_err(|error| (1, format!("invalid stdin: {error}")))?;
            plugin::pretty(&input, color())
        }
        word if word.starts_with('-') => return Err((2, format!("usage: unknown option {word}"))),
        word => return Err((3, format!("action not implemented: {word}"))),
    };
    result.map_err(|error| (1, error))
}

/// `vps stats` reads `pretty` through a pipe, so it passes its own terminal
/// color decision in `VPS_STATS_COLOR`; a non-empty `NO_COLOR` always wins.
fn color() -> bool {
    std::env::var_os("VPS_STATS_COLOR").is_some_and(|value| value == "yes")
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
}

fn collect() -> Result<String, String> {
    let config = config::load()?;
    let body = http::get(&config)?;
    let page = qos::parse(&body)?;
    let output = plugin::collect(&page)?;
    for name in page.warnings {
        eprintln!(
            "modqos-stats: warning: unsupported counter {}",
            diagnostic_text(&name)
        );
    }
    Ok(output)
}

/// Bound and escape untrusted server text before displaying it on stderr.
fn diagnostic_text(input: &str) -> String {
    let mut output = String::new();
    let mut length = 0;
    for character in input.chars() {
        let escaped = if character.is_control() {
            let mut buffer = [0; 4];
            character
                .encode_utf8(&mut buffer)
                .bytes()
                .map(|byte| format!("\\x{byte:02x}"))
                .collect::<String>()
        } else {
            character.to_string()
        };
        let count = escaped.chars().count();
        if length + count > 200 {
            output.push_str("...");
            break;
        }
        output.push_str(&escaped);
        length += count;
    }
    output
}

fn main() -> ExitCode {
    match run() {
        Ok(output) => match io::stdout().lock().write_all(output.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("modqos-stats: error: stdout: {error}");
                ExitCode::from(1)
            }
        },
        Err((code, error)) => {
            eprintln!("modqos-stats: error: {error}");
            ExitCode::from(code)
        }
    }
}
