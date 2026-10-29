//! JSON Lines server loop over any reader/writer pair.
//!
//! One request per input line, exactly one response per request, flushed immediately.
//! Lines are read with a hard size limit, so an oversized or endless line cannot exhaust
//! memory; it is skipped up to its newline and answered with `request_too_large`.

use std::io::{self, BufRead, Read, Write};

use crate::protocol::{ErrorBody, ErrorCode, MAX_REQUEST_BYTES, ResponseEnvelope};
use crate::session::Host;

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
    host: &mut Host,
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
                while matches!(buffer.last(), Some(b'\n' | b'\r')) {
                    buffer.pop();
                }
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

enum LineRead {
    Eof,
    Line,
    TooLong,
}

/// Reads one line of at most `limit` bytes (plus newline) into `buffer`.
fn read_bounded_line<R: BufRead>(
    input: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> io::Result<LineRead> {
    let read = Read::take(&mut *input, limit as u64 + 1).read_until(b'\n', buffer)?;
    if read == 0 {
        return Ok(LineRead::Eof);
    }
    if buffer.last() == Some(&b'\n') || buffer.len() <= limit {
        return Ok(LineRead::Line);
    }
    // Over the limit without a newline: discard the rest of the line.
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            break;
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => {
                input.consume(index + 1);
                break;
            }
            None => {
                let length = available.len();
                input.consume(length);
            }
        }
    }
    Ok(LineRead::TooLong)
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
