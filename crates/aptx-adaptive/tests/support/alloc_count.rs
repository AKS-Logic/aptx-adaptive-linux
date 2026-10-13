//! Test support: a global allocator that counts allocations, reallocations and frees made by
//! the current thread while [`count`] runs, so tests running in parallel don't interfere.
//! Used by `tests/no_alloc.rs` here and, through `#[path]`, by the plugin's unit tests. It
//! lives outside `src/` so the encoder library stays `forbid(unsafe_code)`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn note() {
    // `try_with`: thread-local storage may be gone while a thread exits.
    if ARMED.try_with(Cell::get).unwrap_or(false) {
        let _ = CALLS.try_with(|c| c.set(c.get() + 1));
    }
}

// SAFETY: forwards to the system allocator; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: caller contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        // SAFETY: caller contract.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: caller contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Runs `f` and returns how many allocations, reallocations and frees it made on this thread.
pub fn count(f: impl FnOnce()) -> usize {
    CALLS.set(0);
    ARMED.set(true);
    f();
    ARMED.set(false);
    CALLS.get()
}
