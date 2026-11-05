//! Drawing allocates nothing once the scratch buffers are warm.
#![allow(missing_docs)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use common::Mock;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::MemLines;
use rift_tui::App;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counter has no effect on the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same layout as the caller passed.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: `p` came from `System.alloc` with layout `l`.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `p` came from `System.alloc` with layout `l`.
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn warm_frames_do_not_allocate() {
    let m = Mock::large(2000, true);
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let keys = ['n', ' ', 'p', 'b'].map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    let probe = ALLOCS.load(Ordering::Relaxed);
    drop(std::hint::black_box(vec![0u8; 64]));
    assert!(ALLOCS.load(Ordering::Relaxed) > probe, "counter is live");
    let mut frame = |app: &mut App<'_>| app.draw(&mut term.get_frame());
    // Warm-up covers every row kind and the minimap rebuild.
    for k in keys.iter().cycle().take(200) {
        app.handle_key(*k);
        frame(&mut app);
    }
    let before = ALLOCS.load(Ordering::Relaxed);
    for k in keys.iter().cycle().take(2000) {
        app.handle_key(*k);
        frame(&mut app);
    }
    let n = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(n, 0, "{n} allocations in 2000 frames");
}
