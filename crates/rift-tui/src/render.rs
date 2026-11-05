//! Drawing: minimap, hunk rows, status line.

use std::fmt::{self, Write as _};
use std::ops::Range;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use rift_core::{DiffView, Edit, HunkState, LineAccess, LineNo, Segment, SegmentStatus};

use crate::app::{App, Mode, Pos, Scratch, rows_in};
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

    /// Draws as much of `run` as fits and returns the rows drawn.
    fn run(&mut self, buf: &mut Buffer, run: &Run) -> u64 {
        let n = run.len.min(u64::from(self.end - self.row));
        let (file, first) = match run.kind {
            Kind::Insert => (self.files[1], run.b),
            _ => (self.files[0], run.a),
        };
        let read = file.read_lines(LineNo(first)..LineNo(first + n), &mut self.scratch.lines);
        let (mark, color) = match (run.kind, run.approx) {
            (Kind::Equal, _) => (' ', None),
            (kind, true) => (
                if kind == Kind::Delete { '-' } else { '+' },
                Some(Color::Yellow),
            ),
            (Kind::Delete, false) => ('-', Some(Color::Red)),
            (Kind::Insert, false) => ('+', Some(Color::Green)),
        };
        let style = color.map_or_else(Style::default, |c| Style::default().fg(c));
        let digits = self.digits;
        let room = usize::from(self.width).saturating_sub(digits * 2 + 7);
        for i in 0..n {
            let pre = &mut self.scratch.prefix;
            pre.clear();
            let _ = write!(pre, "{}{mark} ", if run.approx { '~' } else { ' ' });
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
            let txt = &mut self.scratch.text;
            txt.clear();
            match &read {
                Ok(()) => sanitize(
                    self.scratch.lines.line(usize::try_from(i).expect("small")),
                    txt,
                    room,
                ),
                Err(e) => {
                    let _ = write!(txt, "<{e}>");
                }
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
        self.show(seg.a.start.0..seg.a.end.0);
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
                self.show(cursor..cursor);
            }
            done += self.run(buf, &run);
        }
        done
    }
}

/// Appends at most `max` chars of `line` to `out`: no terminator, tabs as
/// spaces, control bytes and invalid UTF-8 replaced. Reuses `out`.
fn sanitize(line: &[u8], out: &mut String, max: usize) {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut left = max;
    for chunk in line.utf8_chunks() {
        for c in chunk.valid().chars() {
            if left == 0 {
                return;
            }
            left -= 1;
            out.push(match c {
                '\t' => ' ',
                c if c.is_control() => '·',
                c => c,
            });
        }
        if !chunk.invalid().is_empty() && left > 0 {
            left -= 1;
            out.push('\u{FFFD}');
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
        self.draw_status(buf, Rect::new(area.x, area.bottom() - 1, area.width, 1));
        self.drawn_progress = Some(self.view.progress());
        self.pending_shown = info.pending;
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
            };
        }
        // The first segment's hunk is fetched here and reused by the loop.
        let start_seg = self.pos.seg;
        let mut first =
            (segs[start_seg].status != SegmentStatus::Equal).then(|| view.hunk(start_seg));
        let rows_here = rows_in(&segs[start_seg], first.as_ref());
        self.pos.off = self.pos.off.min(rows_here - 1);
        let (len_a, len_b) = (view.len_a(), view.len_b());
        let mut painter = Painter {
            files: [self.a, self.b],
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
        } else if info.vis.end <= info.vis.start && info.vis.start < len_a {
            info.vis.end = info.vis.start + 1;
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

    fn draw_status(&mut self, buf: &mut Buffer, area: Rect) {
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
            if !self.message.is_empty() {
                let _ = write!(sc.status, "  {}", self.message);
            }
        }
        buf.set_stringn(area.x, area.y, &sc.status, area.width.into(), style);

        let pr = self.view.progress();
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
