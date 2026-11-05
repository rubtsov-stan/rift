//! Line prefixes of the rows on screen, kept from one frame to the next.
//!
//! Reading a prefix costs time linear in the line's whole length, so a
//! redraw must not repeat it. The cache holds exactly the lines of the last
//! frame, each cut to the bytes a row can show: at most rows times that cap
//! in bytes. A line that leaves the screen is dropped.

use rift_core::{Bytes, LineAccess, LineBuf, LineNo, Result};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Entry {
    /// 0 for file A, 1 for file B.
    side: u8,
    line: u64,
    /// Cap the prefix was read with.
    max: usize,
    /// Length of the whole line, terminator included.
    pub full: u64,
    start: usize,
    end: usize,
}

/// Two generations, swapped each frame; buffers keep their capacity.
#[derive(Debug, Default)]
pub(crate) struct LineCache {
    prev: Vec<Entry>,
    prev_bytes: Vec<u8>,
    cur: Vec<Entry>,
    cur_bytes: Vec<u8>,
}

impl LineCache {
    /// Starts a frame: what was drawn last becomes the lookup side.
    pub(crate) fn begin(&mut self) {
        std::mem::swap(&mut self.prev, &mut self.cur);
        std::mem::swap(&mut self.prev_bytes, &mut self.cur_bytes);
        self.cur.clear();
        self.cur_bytes.clear();
    }

    /// The first `max` bytes of `line` of file `side`, reusing the last
    /// frame's read when it covers `max`. `scratch` receives fresh reads.
    pub(crate) fn get(
        &mut self,
        file: &dyn LineAccess,
        side: u8,
        line: u64,
        max: usize,
        scratch: &mut LineBuf,
    ) -> Result<Entry> {
        let start = self.cur_bytes.len();
        let hit = self.prev.iter().find(|e| {
            e.side == side && e.line == line && (e.full <= (e.end - e.start) as u64 || e.max >= max)
        });
        let entry = if let Some(e) = hit {
            self.cur_bytes
                .extend_from_slice(&self.prev_bytes[e.start..e.end]);
            Entry {
                start,
                end: self.cur_bytes.len(),
                ..*e
            }
        } else {
            let full = file.read_line_prefix(LineNo(line), Bytes(max as u64), scratch)?;
            self.cur_bytes.extend_from_slice(scratch.line(0));
            Entry {
                side,
                line,
                max,
                full: full.0,
                start,
                end: self.cur_bytes.len(),
            }
        };
        self.cur.push(entry);
        Ok(entry)
    }

    /// The bytes of an entry returned by `get` this frame.
    pub(crate) fn bytes(&self, e: &Entry) -> &[u8] {
        &self.cur_bytes[e.start..e.end]
    }

    /// Whether the entry holds less than the whole line.
    pub(crate) fn is_cut(e: &Entry) -> bool {
        e.full > (e.end - e.start) as u64
    }
}
