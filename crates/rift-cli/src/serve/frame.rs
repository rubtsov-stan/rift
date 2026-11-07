//! LSP's framing: a header block with `Content-Length`, a blank line, the
//! body. Reads through fixed buffers, so a hostile header or length costs
//! no memory.

use std::io::{self, BufRead, Read, Write};

/// Longest request body read; a longer one is skipped. Requests carry two
/// paths at most.
pub const MAX_BODY: usize = 64 << 10;
/// Longest header line kept; the rest of a longer one is skipped.
const MAX_HEADER_LINE: usize = 1 << 10;

/// What the next message on the input was.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// A body, now in the caller's buffer.
    Body,
    /// The header block had no usable `Content-Length`.
    NoLength,
    /// A body longer than [`MAX_BODY`], skipped.
    TooLarge(u64),
    /// The input ended.
    End,
}

/// Reads one message into `body`. An input that ends inside a message is
/// [`Frame::End`].
///
/// # Errors
///
/// A read error of `input`.
pub fn read(input: &mut impl BufRead, body: &mut Vec<u8>) -> io::Result<Frame> {
    let mut len = None;
    let mut bad = false;
    let mut line = Vec::with_capacity(MAX_HEADER_LINE);
    let mut lines = 0;
    loop {
        line.clear();
        let (n, whole) = read_line(input, &mut line)?;
        if n == 0 {
            return Ok(Frame::End);
        }
        let text = line.trim_ascii();
        if text.is_empty() && whole {
            // Blank lines before any header are separators a client may
            // send between messages.
            if lines == 0 {
                continue;
            }
            break;
        }
        lines += 1;
        // Other headers, and lines that are not headers, are ignored.
        let header = std::str::from_utf8(text)
            .ok()
            .and_then(|t| t.split_once(':'));
        if let Some((name, value)) = header
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            match value.trim().parse::<u64>() {
                Ok(n) if whole => len = Some(n),
                _ => bad = true,
            }
        }
    }
    let Some(len) = len.filter(|_| !bad) else {
        return Ok(Frame::NoLength);
    };
    if len > MAX_BODY as u64 {
        let skipped = io::copy(&mut Read::take(&mut *input, len), &mut io::sink())?;
        return Ok(if skipped < len {
            Frame::End
        } else {
            Frame::TooLarge(len)
        });
    }
    body.clear();
    body.resize(usize::try_from(len).expect("at most MAX_BODY"), 0);
    match input.read_exact(body) {
        Ok(()) => Ok(Frame::Body),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(Frame::End),
        Err(e) => Err(e),
    }
}

/// Appends one header line to `line`, at most [`MAX_HEADER_LINE`] bytes of
/// it, and skips the rest. Returns the bytes consumed and whether the line
/// fit.
fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<(usize, bool)> {
    let mut consumed = 0;
    let mut whole = true;
    loop {
        let buf = match input.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok((consumed, whole));
        }
        let (take, done) = match memchr::memchr(b'\n', buf) {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        let room = MAX_HEADER_LINE - line.len();
        if take > room {
            whole = false;
        }
        line.extend_from_slice(&buf[..take.min(room)]);
        input.consume(take);
        consumed += take;
        if done {
            return Ok((consumed, whole));
        }
    }
}

/// Counts what is written to it.
#[derive(Debug, Default)]
pub struct Counter(pub u64);

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Writes one message whose body `body` writes, twice: once to count its
/// length for the header, then for real. The body is never held whole.
///
/// # Errors
///
/// A write error of `out` or of `body`.
pub fn write(
    out: &mut impl Write,
    body: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let mut count = Counter::default();
    body(&mut count)?;
    write!(out, "Content-Length: {}\r\n\r\n", count.0)?;
    body(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(input: &[u8]) -> Vec<(Frame, Vec<u8>)> {
        let mut input = input;
        let mut out = Vec::new();
        loop {
            let mut body = Vec::new();
            let f = read(&mut input, &mut body).unwrap();
            if f == Frame::End {
                return out;
            }
            out.push((f, body));
        }
    }

    #[test]
    fn reads_bodies_by_their_length() {
        let got = frames(b"Content-Length: 2\r\n\r\n{}content-length:3\nX-Other: y\n\n[1]");
        assert_eq!(
            got,
            [
                (Frame::Body, b"{}".to_vec()),
                (Frame::Body, b"[1]".to_vec())
            ]
        );
    }

    #[test]
    fn a_header_block_without_a_length_is_reported() {
        let got = frames(b"Content-Type: x\r\n\r\nContent-Length: nope\r\n\r\n");
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|(f, _)| *f == Frame::NoLength));
    }

    #[test]
    fn a_long_body_or_header_is_skipped() {
        let mut input = format!("Content-Length: {}\r\n\r\n", MAX_BODY + 1).into_bytes();
        input.resize(input.len() + MAX_BODY + 1, b' ');
        input.extend(b"X: ");
        input.resize(input.len() + 3 * MAX_HEADER_LINE, b'h');
        input.extend(b"\r\nContent-Length: 1\r\n\r\n1");
        let got = frames(&input);
        assert_eq!(got[0].0, Frame::TooLarge(MAX_BODY as u64 + 1));
        assert_eq!(
            got[1],
            (Frame::Body, b"1".to_vec()),
            "a long other header is ignored"
        );
    }

    #[test]
    fn input_that_ends_inside_a_message_is_the_end() {
        assert_eq!(frames(b"Content-Length: 10\r\n\r\n{\"a\"").len(), 0);
        assert_eq!(frames(b"Content-Len").len(), 0);
    }

    #[test]
    fn writes_the_length_of_what_it_writes() {
        let mut out = Vec::new();
        write(&mut out, &mut |w| w.write_all("é{}".as_bytes())).unwrap();
        assert_eq!(out, "Content-Length: 4\r\n\r\né{}".as_bytes());
    }
}
