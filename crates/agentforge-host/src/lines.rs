//! Bounded line reading shared by the JSON Lines and MCP transports.
//!
//! Both transports use newline-delimited JSON with a hard size limit, so an endless or
//! oversized line cannot exhaust memory. The reader either returns one line, reports end of
//! input, or consumes the rest of an over-long line and reports how to answer it.

use std::io::{self, BufRead, Read};

/// Outcome of reading one bounded line.
pub(crate) enum LineRead {
    /// The input ended before any byte was read.
    Eof,
    /// One complete line (without its terminator) is in the buffer.
    Line,
    /// The line exceeded the limit; the rest of it has been discarded.
    TooLong,
}

/// Reads one line of at most `limit` bytes (plus newline) into `buffer`.
pub(crate) fn read_bounded_line<R: BufRead>(
    input: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> io::Result<LineRead> {
    // Up to `limit` content bytes plus a two-byte "\r\n" terminator.
    let read = Read::take(&mut *input, limit as u64 + 2).read_until(b'\n', buffer)?;
    if read == 0 {
        return Ok(LineRead::Eof);
    }
    let terminated = buffer.last() == Some(&b'\n');
    if !terminated && read == limit + 2 {
        // Over the limit and the line goes on: discard the rest of it.
        discard_line(input)?;
        return Ok(LineRead::TooLong);
    }
    if terminated {
        buffer.pop();
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
    }
    if buffer.len() > limit {
        return Ok(LineRead::TooLong);
    }
    Ok(LineRead::Line)
}

fn discard_line<R: BufRead>(input: &mut R) -> io::Result<()> {
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => {
                input.consume(index + 1);
                return Ok(());
            }
            None => {
                let length = available.len();
                input.consume(length);
            }
        }
    }
}
