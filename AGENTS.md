# modqos-stats

Executable `vps stats` plugin: reads Apache mod_qos counters from the
`qos-viewer` handler's machine-readable page (`?auto`) over plain HTTP
and prints them in the plugin line protocol (`def` / `collect` /
`pretty`) so `vps stats` can store them in RRD files.  The contract is
specified in `README.org` § Plugin contract; that section is the source
of truth for the output formats and exit codes.

## BLOCKING rules

- **Smallest possible footprint is a product requirement.**  The
  release binary is ONE statically linked executable built for
  `x86_64-unknown-linux-musl` (`bin/build`).  `[dependencies]` in
  `Cargo.toml` stays EMPTY: everything (HTTP, config parsing, argument
  handling) uses `std` only.  Adding a runtime dependency needs the
  user's explicit approval.  Dev-dependencies (tests only) are fine.
- **Deliberate exception to our usual Rust CLI conventions**: no
  `clap`, no `log`/`fern`/`kal-log`, no `colored`.  Arguments are a
  single action word matched by hand; diagnostics are plain `stderr`
  lines prefixed `modqos-stats: error:` / `modqos-stats: warning:`.
- **stdout carries protocol data only.**  On any error, stdout stays
  EMPTY (output is fully built in memory, printed only on success) so
  `vps stats` never stores a partial run.
- **Plain HTTP only.**  `https://` URLs are refused: TLS would need a
  dependency.  The page is meant to be reached on a private address
  restricted by `Require ip`.
- **No host-specific values** (our servers, IPs, vhosts) in code, tests
  or `README.org`: they belong in the config file of the host.  Tests
  use reserved names (`*.example`, `192.0.2.x`) and loopback.

## Project map

- `src/main.rs` — argument dispatch, exit codes.
- `src/config.rs` — config file loading (strict flat-YAML subset).
- `src/http.rs` — one HTTP/1.0 GET with timeouts.
- `src/qos.rs` — parsing of the mod_qos `?auto` text.
- `src/plugin.rs` — `def` / `collect` / `pretty` output, path encoding.
- `tests/` — integration tests running the binary against an
  in-process HTTP server.
- `bin/build` — static musl release build + checks (static, version).
- `bin/test`, `test/` — sunit tests of `bin/build`.

## Checks before declaring work done

1. `cargo fmt`
2. `cargo clippy --all-targets --all-features` (no warnings)
3. `cargo test`
4. `shellcheck bin/*`
5. `bin/test`
