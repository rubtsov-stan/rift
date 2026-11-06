//! Drawing: minimap, hunk rows, status line.

use std::fmt::{self, Write as _};
use std::ops::Range;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use rift_core::moves::MovedBlock;
use rift_core::{DiffView, Edit, HunkState, LineAccess, LineNo, Progress, Segment, SegmentStatus};

use crate::app::{App, Mode, Pos, Scratch, anchor_at, row_of, rows_in};
use crate::cache::LineCache;
use crate::hidden_table::HIDDEN;
use crate::minimap;

const GLYPHS: [char; 9] = ['·', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const MINIMAP_ROWS: u16 = 3;
const LABEL_COLS: u16 = 2;
const MIN_WIDTH: u16 = 24;
const MIN_HEIGHT: u16 = 3;
/// Rows needed before the minimap is worth its three lines.
const MINIMAP_MIN_HEIGHT: u16 = 9;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Equal,
    Delete,
    Insert,
}

/// 1-based inclusive line range, or a dash for an empty one.
struct Lines<'r>(&'r Range<LineNo>);

impl fmt::Display for Lines<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.start == self.0.end {
            f.write_str("-")
        } else {
            write!(f, "{}..{}", self.0.start.0 + 1, self.0.end.0)
        }
    }
}

/// What a pass over the rows learned.
struct Info {
    /// A lines on screen; inserted lines count with the A line they follow.
    vis: Range<u64>,
    /// Last row drawn.
    last: Pos,
    pending: bool,
    /// The first moved row: its block and whether it is the inserted side.
    moved: Option<(usize, bool)>,
}

/// `len` consecutive rows of one kind; `a` and `b` are the first line
/// numbers (0-based) on each side.
struct Run {
    kind: Kind,
    a: u64,
    b: u64,
    len: u64,
    approx: bool,
}

struct Painter<'s> {
    files: [&'s dyn LineAccess; 2],
    moved: &'s [MovedBlock],
    scratch: &'s mut Scratch,
    /// Digits of the widest line number.
    digits: usize,
    x: u16,
    width: u16,
    /// Next screen row to fill, and the first row below the area.
    row: u16,
    end: u16,
    info: Info,
}

impl Painter<'_> {
    fn show(&mut self, lines: Range<u64>) {
        self.info.vis.start = self.info.vis.start.min(lines.start);
        self.info.vis.end = self.info.vis.end.max(lines.end);
    }

    /// Inserted lines count with the A line they follow, `at - 1`; at the top
    /// of the file they follow nothing, so the first line stands in.
    fn show_insert(&mut self, at: u64) {
        self.show(at.saturating_sub(1)..at.max(1));
    }

    /// The moved block holding a deleted line `a`, or an inserted line `b`.
    fn moved_at(&self, kind: Kind, line: u64) -> Option<usize> {
        let line = LineNo(line);
        match kind {
            Kind::Equal => None,
            Kind::Delete => {
                let k = self.moved.partition_point(|m| m.a.end <= line);
                self.moved
                    .get(k)
                    .is_some_and(|m| m.a.contains(&line))
                    .then_some(k)
            }
            // Linear in the blocks per inserted row, since they are
            // ordered by A; index them by B if a diff has very many.
            Kind::Insert => self.moved.iter().position(|m| m.b.contains(&line)),
        }
    }

    /// Draws as much of `run` as fits and returns the rows drawn.
    fn run(&mut self, buf: &mut Buffer, run: &Run) -> u64 {
        let n = run.len.min(u64::from(self.end - self.row));
        let (file, first) = match run.kind {
            Kind::Insert => (self.files[1], run.b),
            _ => (self.files[0], run.a),
        };
        let side = u8::from(run.kind == Kind::Insert);
        let (mark, color) = match (run.kind, run.approx) {
            (Kind::Equal, _) => (' ', None),
            (kind, true) => (
                if kind == Kind::Delete { '-' } else { '+' },
                Some(Color::Yellow),
            ),
            (Kind::Delete, false) => ('-', Some(Color::Red)),
            (Kind::Insert, false) => ('+', Some(Color::Green)),
        };
        let digits = self.digits;
        let room = usize::from(self.width).saturating_sub(digits * 2 + 7);
        for i in 0..n {
            let moved = self.moved_at(run.kind, first + i);
            if let Some(k) = moved
                && self.info.moved.is_none()
            {
                self.info.moved = Some((k, run.kind == Kind::Insert));
            }
            let color = if moved.is_some() {
                Some(Color::Magenta)
            } else {
                color
            };
            let style = color.map_or_else(Style::default, |c| Style::default().fg(c));
            let flag = match (run.approx, moved) {
                (true, _) => '~',
                (false, Some(_)) => 'm',
                (false, None) => ' ',
            };
            let pre = &mut self.scratch.prefix;
            pre.clear();
            let _ = write!(pre, "{flag}{mark} ");
            if run.kind == Kind::Insert {
                let _ = write!(pre, "{:digits$}", "");
            } else {
                let _ = write!(pre, "{:digits$}", run.a + i + 1);
            }
            pre.push(' ');
            if run.kind == Kind::Delete {
                let _ = write!(pre, "{:digits$}", "");
            } else {
                let _ = write!(pre, "{:digits$}", run.b + i + 1);
            }
            pre.push_str(" │ ");
            self.scratch.text.clear();
            // A row shows at most `room` characters of at most 4 bytes, so
            // this prefix is enough however long the line is.
            let mut left = room;
            let sc = &mut *self.scratch;
            let txt = &mut sc.text;
            let e = sc
                .cache
                .get(file, side, first + i, room * 4 + 2, &mut sc.lines);
            let bytes = sc.cache.bytes(&e);
            if e.failed {
                // Built from a `String` by the cache.
                push_clean(txt, &String::from_utf8_lossy(bytes), &mut left);
            } else {
                sanitize(bytes, LineCache::is_cut(&e), txt, &mut left);
            }
            let y = self.row + u16::try_from(i).expect("rows fit the screen");
            let (x, _) = buf.set_stringn(self.x, y, &self.scratch.prefix, self.width.into(), style);
            buf.set_stringn(x, y, &self.scratch.text, room, style);
        }
        match run.kind {
            Kind::Insert => {}
            _ => self.show(run.a..run.a + n),
        }
        self.row += u16::try_from(n).expect("n <= rows left");
        n
    }

    /// Draws the rows of an Equal segment after skipping `skip` of them.
    /// Returns rows consumed, skipped ones included.
    fn equal(&mut self, buf: &mut Buffer, seg: &Segment, skip: u64) -> u64 {
        let skip = skip.min(seg.len_a());
        let run = Run {
            kind: Kind::Equal,
            a: seg.a.start.0 + skip,
            b: seg.b.start.0 + skip,
            len: seg.len_a() - skip,
            approx: false,
        };
        skip + self.run(buf, &run)
    }

    fn pending(&mut self, buf: &mut Buffer, seg: &Segment) -> u64 {
        self.info.pending = true;
        let txt = &mut self.scratch.text;
        txt.clear();
        let _ = write!(txt, "  ? pending  A {}  B {}", Lines(&seg.a), Lines(&seg.b));
        let style = Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC);
        buf.set_stringn(
            self.x,
            self.row,
            &self.scratch.text,
            self.width.into(),
            style,
        );
        if seg.a.is_empty() {
            self.show_insert(seg.a.start.0);
        } else {
            self.show(seg.a.start.0..seg.a.end.0);
        }
        self.row += 1;
        1
    }

    /// Draws the rows of a refined hunk after skipping `skip` of them.
    /// Returns rows consumed, skipped ones included. O(edits).
    fn hunk(
        &mut self,
        buf: &mut Buffer,
        seg: &Segment,
        hunk: &rift_core::Hunk,
        approx: bool,
        skip: u64,
    ) -> u64 {
        let approx = approx || hunk.approx;
        let mut left = skip;
        let mut done = skip;
        let mut cursor = seg.a.start.0;
        for edit in &hunk.edits {
            if self.row >= self.end {
                break;
            }
            let (kind, a, b, len) = match *edit {
                Edit::Equal { a, b, len } => (Kind::Equal, a.0, b.0, len),
                Edit::Delete { a, len } => (Kind::Delete, a.0, 0, len),
                Edit::Insert { b, len } => (Kind::Insert, 0, b.0, len),
            };
            if kind != Kind::Insert {
                cursor = a + len;
            }
            let cut = left.min(len);
            left -= cut;
            if cut == len {
                continue;
            }
            let run = Run {
                kind,
                a: a + cut,
                b: b + cut,
                len: len - cut,
                approx,
            };
            if kind == Kind::Insert {
                self.show_insert(cursor);
            }
            done += self.run(buf, &run);
        }
        done
    }
}

/// Appends `line` to `out` while `left` columns remain: no terminator, tabs
/// as a space, invalid UTF-8 as U+FFFD, everything else that could change
/// the terminal or the reading order as a visible `\u{..}` escape.
pub fn sanitize(line: &[u8], cut: bool, out: &mut String, left: &mut usize) {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut chunks = line.utf8_chunks().peekable();
    while let Some(chunk) = chunks.next() {
        push_clean(out, chunk.valid(), left);
        // A cut can end inside a character; that is not invalid text.
        let torn = cut && chunks.peek().is_none();
        if !chunk.invalid().is_empty() && !torn && *left > 0 {
            *left -= 1;
            out.push('\u{FFFD}');
        }
    }
}

/// Characters that are control or invisible: they are drawn as escapes.
/// Cc, the line and paragraph separators, and the generated table of Cf and
/// `Default_Ignorable_Code_Point`.
fn is_hidden(c: char) -> bool {
    let cp = c as u32;
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}')
        || HIDDEN
            .binary_search_by(|&(lo, hi)| {
                if hi < cp {
                    std::cmp::Ordering::Less
                } else if lo > cp {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
}

/// Appends valid text, escaping hidden characters, while `left` columns
/// remain. An escape that does not fit ends the text.
fn push_clean(out: &mut String, text: &str, left: &mut usize) {
    for c in text.chars() {
        if c == '\t' {
            if *left == 0 {
                return;
            }
            *left -= 1;
            out.push(' ');
        } else if is_hidden(c) {
            let digits = (32 - (c as u32).leading_zeros()).div_ceil(4).max(1) as usize;
            let width = digits + 4;
            if *left < width {
                *left = 0;
                return;
            }
            *left -= width;
            let _ = write!(out, "\\u{{{:X}}}", c as u32);
        } else {
            if *left == 0 {
                return;
            }
            *left -= 1;
            out.push(c);
        }
    }
}

impl App<'_> {
    /// Draws one frame and tells the view which A lines are visible.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        self.render(frame.buffer_mut(), area);
    }

    pub(crate) fn render(&mut self, buf: &mut Buffer, area: Rect) {
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            buf.set_stringn(
                area.x,
                area.y,
                "terminal too small",
                area.width.into(),
                Style::default(),
            );
            return;
        }
        let mini = if area.height >= MINIMAP_MIN_HEIGHT {
            MINIMAP_ROWS
        } else {
            0
        };
        let rows = Rect::new(area.x, area.y + mini, area.width, area.height - mini - 1);
        self.rows = rows.height;
        let info = self.draw_rows(buf, rows);
        if mini > 0 {
            self.draw_minimap(buf, Rect::new(area.x, area.y, area.width, mini), &info);
        }
        // One reading feeds both the bar and the next redraw decision.
        let progress = self.view.progress();
        self.draw_status(
            buf,
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
            progress,
        );
        self.drawn_progress = Some(progress);
        self.pending_shown = info.pending;
        self.moved_shown = info.moved;
        let vis = LineNo(info.vis.start)..LineNo(info.vis.end);
        if self.reported.as_ref() != Some(&vis) {
            self.view.set_visible(vis.clone());
            self.reported = Some(vis);
        }
    }

    fn draw_rows(&mut self, buf: &mut Buffer, area: Rect) -> Info {
        let view: &dyn DiffView = self.view;
        let segs = view.segments();
        if segs.is_empty() {
            buf.set_stringn(
                area.x,
                area.y,
                "  both files are empty",
                area.width.into(),
                Style::default(),
            );
            return Info {
                vis: 0..0,
                last: self.pos,
                pending: false,
                moved: None,
            };
        }
        // The first segment's hunk is fetched here and reused by the loop.
        let start_seg = self.pos.seg;
        let mut first =
            (segs[start_seg].status != SegmentStatus::Equal).then(|| view.hunk(start_seg));
        let rows_here = rows_in(&segs[start_seg], first.as_ref());
        // A refined hunk at the top shows the anchored line, and the
        // anchor follows the top row from then on; a pending one keeps it
        // until it is refined.
        if let Some(HunkState::Ready(h) | HunkState::Approx(h)) = &first {
            if let Some(off) = self.anchor.and_then(|a| row_of(h, a)) {
                self.pos.off = off;
            }
            self.anchor = Some(anchor_at(h, self.pos.off.min(rows_here - 1)));
        } else if first.is_none() {
            self.anchor = None;
        }
        self.pos.off = self.pos.off.min(rows_here - 1);
        let (len_a, len_b) = (view.len_a(), view.len_b());
        self.scratch.cache.begin();
        let mut painter = Painter {
            files: [self.a, self.b],
            moved: view.moved_blocks(),
            scratch: &mut self.scratch,
            digits: (len_a.max(len_b).max(1).ilog10() + 1) as usize,
            x: area.x,
            width: area.width,
            row: area.y,
            end: area.bottom(),
            info: Info {
                vis: Range {
                    start: u64::MAX,
                    end: 0,
                }, // empty until a row reports
                last: self.pos,
                pending: false,
                moved: None,
            },
        };
        let mut skip = self.pos.off;
        for (idx, seg) in segs.iter().enumerate().skip(self.pos.seg) {
            if painter.row >= painter.end {
                break;
            }
            let done = if seg.status == SegmentStatus::Equal {
                painter.equal(buf, seg, skip)
            } else {
                let state = first.take().filter(|_| idx == start_seg);
                match state.unwrap_or_else(|| view.hunk(idx)) {
                    HunkState::Pending if skip == 0 => painter.pending(buf, seg),
                    HunkState::Pending => 1,
                    HunkState::Ready(h) => painter.hunk(buf, seg, &h, false, skip),
                    HunkState::Approx(h) => painter.hunk(buf, seg, &h, true, skip),
                }
            };
            painter.info.last = Pos {
                seg: idx,
                off: done.saturating_sub(1),
            };
            skip = 0;
        }
        let mut info = painter.info;
        if info.vis.start == u64::MAX {
            info.vis = 0..0;
        } else {
            info.vis.end = info.vis.end.min(len_a);
            info.vis.start = info.vis.start.min(info.vis.end);
        }
        info
    }

    fn draw_minimap(&mut self, buf: &mut Buffer, area: Rect, info: &Info) {
        let bar = usize::from(area.width - LABEL_COLS);
        let total = self.weight_total;
        let segs = self.view.segments();
        let mm = &mut self.scratch.minimap;
        if mm.width() != bar {
            mm.rebuild(segs, total, bar);
        }
        let labels = [('D', Color::Red), ('~', Color::Yellow), ('v', Color::Reset)];
        let (c0, c1) = if total == 0 {
            (0, 0)
        } else {
            let mut at = |p: Pos| {
                let w = minimap::weight(&segs[p.seg]);
                self.before.sum_to(segs, p.seg, minimap::weight) + p.off.min(w.saturating_sub(1))
            };
            (mm.cell(at(self.pos), total), mm.cell(at(info.last), total))
        };
        for (r, (label, color)) in labels.into_iter().enumerate() {
            let y = area.y + u16::try_from(r).expect("three rows");
            buf[(area.x, y)].set_char(label).set_fg(color);
            for c in 0..bar {
                let x = area.x + LABEL_COLS + u16::try_from(c).expect("below width");
                let (ch, st) = if r == 2 {
                    if (c0..=c1).contains(&c) {
                        ('▀', Style::default())
                    } else {
                        ('·', Style::default().add_modifier(Modifier::DIM))
                    }
                } else {
                    let l = if total == 0 {
                        0
                    } else {
                        mm.level(r == 1, c, total)
                    };
                    (GLYPHS[l], Style::default().fg(color))
                };
                buf[(x, y)].set_char(ch).set_style(st);
            }
        }
    }

    fn draw_status(&mut self, buf: &mut Buffer, area: Rect, pr: Progress) {
        let style = Style::default().add_modifier(Modifier::REVERSED);
        buf.set_style(area, style);
        let sc = &mut self.scratch;
        sc.status.clear();
        if let Mode::Goto(n) = self.mode {
            let _ = write!(sc.status, " go to line: ");
            if let Some(n) = n {
                let _ = write!(sc.status, "{n}");
            }
            sc.status.push_str("_  Enter/Esc");
        } else {
            let segs = self.view.segments();
            let seen = if segs.is_empty() {
                0
            } else {
                self.seen.sum_to(segs, self.pos.seg + 1, minimap::is_diff)
            };
            let _ = write!(
                sc.status,
                " A {}  B {}  diff {}/{}",
                self.view.len_a(),
                self.view.len_b(),
                seen,
                self.diff_total
            );
            if self.diff_total == 0 {
                sc.status.push_str("  files are identical");
            }
            let moved = self.view.moved_blocks().len();
            if moved > 0 {
                let _ = write!(sc.status, "  moved {moved}");
            }
            if !self.message.is_empty() {
                let _ = write!(sc.status, "  {}", self.message);
            }
        }
        buf.set_stringn(area.x, area.y, &sc.status, area.width.into(), style);

        if pr.total == 0 {
            return;
        }
        let t = &mut sc.text;
        t.clear();
        if pr.done >= pr.total {
            let _ = write!(t, "refined {}/{} ", pr.done, pr.total);
        } else {
            let filled = usize::try_from(pr.done * 10 / pr.total).unwrap_or(10);
            let _ = write!(t, "refining {}/{} [", pr.done, pr.total);
            t.extend((0..10).map(|i| if i < filled { '█' } else { '░' }));
            t.push_str("] ");
        }
        let cols = u16::try_from(t.chars().count()).unwrap_or(u16::MAX);
        // Overwrites the right end of the status text on a narrow terminal.
        let x = area.right().saturating_sub(cols).max(area.x);
        buf.set_stringn(x, area.y, &sc.text, cols.into(), style);
    }
}
