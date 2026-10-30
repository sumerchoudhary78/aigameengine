//! JSON Lines server loop over any reader/writer pair.
//!
//! One request per input line, exactly one response per request, flushed immediately.
//! Lines are read with a hard size limit, so an oversized or endless line cannot exhaust
//! memory; it is skipped up to its newline and answered with `request_too_large`.

use std::io::{self, BufRead, Write};

use crate::lines::{LineRead, read_bounded_line};
use crate::protocol::{ErrorBody, ErrorCode, MAX_REQUEST_BYTES, ResponseEnvelope};
use crate::session::Host;

/// A local session served by the shared bounded JSON Lines transport.
pub trait Session {
    /// Current monotonic authoring revision.
    fn revision(&self) -> u64;
    /// Handles one input line and returns its correlated protocol response.
    fn handle_line(&mut self, line: &str) -> ResponseEnvelope;
}
impl Session for Host {
    fn revision(&self) -> u64 {
        Host::revision(self)
    }
    fn handle_line(&mut self, line: &str) -> ResponseEnvelope {
        Host::handle_line(self, line)
    }
}

/// Counters reported when the input ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeStats {
    /// Requests answered.
    pub requests: u64,
    /// Requests answered with an error.
    pub errors: u64,
}

/// Serves requests from `input` until end of input, writing responses to `output`.
/// Returns an error only if reading or writing the streams themselves fails.
pub fn serve<R: BufRead, W: Write>(
    host: &mut impl Session,
    mut input: R,
    mut output: W,
) -> io::Result<ServeStats> {
    let mut stats = ServeStats::default();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let response = match read_bounded_line(&mut input, &mut buffer, MAX_REQUEST_BYTES)? {
            LineRead::Eof => return Ok(stats),
            LineRead::TooLong => ResponseEnvelope::error(
                None,
                None,
                host.revision(),
                ErrorBody {
                    code: ErrorCode::RequestTooLarge,
                    message: format!(
                        "request line exceeds {MAX_REQUEST_BYTES} bytes and was skipped"
                    ),
                    details: serde_json::Map::new(),
                    hint: Some(
                        "Split large batches into several scene.apply_batch requests.".to_owned(),
                    ),
                },
            ),
            LineRead::Line => {
                if buffer.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match std::str::from_utf8(&buffer) {
                    Ok(line) => host.handle_line(line),
                    Err(error) => ResponseEnvelope::error(
                        None,
                        None,
                        host.revision(),
                        ErrorBody {
                            code: ErrorCode::MalformedJson,
                            message: format!("request is not valid UTF-8: {error}"),
                            details: serde_json::Map::new(),
                            hint: None,
                        },
                    ),
                }
            }
        };
        stats.requests += 1;
        if response.error.is_some() {
            stats.errors += 1;
        }
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ResponseStatus;

    fn run(input: &[u8]) -> Vec<ResponseEnvelope> {
        let mut host = Host::author();
        let mut output = Vec::new();
        serve(&mut host, input, &mut output).expect("in-memory streams do not fail");
        String::from_utf8(output)
            .expect("utf-8 output")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each output line is a response"))
            .collect()
    }

    #[test]
    fn oversized_lines_are_skipped_and_the_session_continues() {
        let mut input = vec![b'x'; MAX_REQUEST_BYTES + 10];
        input.push(b'\n');
        input.extend_from_slice(
            br#"{"request_id":"after","protocol_version":1,"operation":"session.status"}"#,
        );
        input.push(b'\n');
        let responses = run(&input);
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0].error.as_ref().map(|e| e.code),
            Some(ErrorCode::RequestTooLarge)
        );
        assert_eq!(responses[1].request_id.as_deref(), Some("after"));
        assert_eq!(responses[1].status, ResponseStatus::Ok);
    }

    /// Regression: the limit excludes the line terminator, but a "\r\n" terminator used to
    /// count one byte against it.
    #[test]
    fn the_limit_excludes_both_newline_styles() {
        for terminator in [&b"\n"[..], &b"\r\n"[..]] {
            let mut at_limit = vec![b' '; MAX_REQUEST_BYTES - 2];
            at_limit.splice(0..0, *b"{}");
            at_limit.extend_from_slice(terminator);
            let responses = run(&at_limit);
            assert_eq!(
                responses[0].error.as_ref().map(|e| e.code),
                Some(ErrorCode::InvalidEnvelope),
                "a line of exactly the limit is read and parsed"
            );

            let mut over = vec![b' '; MAX_REQUEST_BYTES + 1];
            over.extend_from_slice(terminator);
            let responses = run(&over);
            assert_eq!(
                responses[0].error.as_ref().map(|e| e.code),
                Some(ErrorCode::RequestTooLarge)
            );
        }
    }

    #[test]
    fn blank_lines_are_ignored_and_invalid_utf8_is_reported() {
        let responses = run(b"\n   \r\n\xff\xfe\n");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            responses[0].error.as_ref().map(|e| e.code),
            Some(ErrorCode::MalformedJson)
        );
    }

    #[test]
    fn a_final_line_without_newline_is_still_served() {
        let responses =
            run(br#"{"request_id":"last","protocol_version":1,"operation":"discover"}"#);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].status, ResponseStatus::Ok);
    }
}
