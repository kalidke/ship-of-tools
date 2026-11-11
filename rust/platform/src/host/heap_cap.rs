//! The capped allocator (guarantee 18): wraps `System`, and once armed counts the bytes requested and ends the process
//! with a code past the cap. It never returns null, because a null allocation is an abort (a core file and a crash
//! report). The `#[global_allocator]` static is the binary's: `#[global_allocator] static HEAP: HeapCap = HeapCap::new();`
//! then `HEAP.arm(cap, code)` before the process reads its input.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{fence, AtomicI32, AtomicIsize, AtomicUsize, Ordering::*};

use super::bound::exit_now;

/// The `limit` of a disarmed cap: the allocator then costs one relaxed load and the `System` call.
const DISARMED: usize = usize::MAX;

pub struct HeapCap {
    limit: AtomicUsize,
    code: AtomicI32,
    /// Signed: a block freed after arming may have been allocated before it.
    used: AtomicIsize,
    peak: AtomicIsize,
}

impl HeapCap {
    pub const fn new() -> Self {
        Self { limit: AtomicUsize::new(DISARMED), code: AtomicI32::new(0), used: AtomicIsize::new(0), peak: AtomicIsize::new(0) }
    }

    /// From now on the process ends with `code` (`exit_now`) at the first request that takes the requested bytes held
    /// since this call past `cap`, and at one request larger than `cap`. A `realloc` counts its growth. Arm once,
    /// before any other thread allocates: arming again starts a new count and can lose a concurrent request's. A
    /// block allocated before arming and freed after it credits its size, so the bound is `cap` plus what was held
    /// at arming.
    pub fn arm(&self, cap: usize, code: i32) {
        self.code.store(code, Relaxed);
        self.used.store(0, Relaxed);
        self.peak.store(0, Relaxed);
        self.limit.store(cap.min(isize::MAX as usize), Release);
    }

    /// The most requested bytes held at once since arming (zero while disarmed): the unit `arm`'s cap is in.
    pub fn high_water(&self) -> usize {
        self.peak.load(Relaxed).max(0) as usize
    }

    fn end(&self) -> ! {
        fence(Acquire); // the code stored before the limit was
        exit_now(self.code.load(Relaxed))
    }

    fn charge(&self, delta: isize) {
        let limit = self.limit.load(Relaxed);
        if limit == DISARMED {
            return;
        }
        // A request past the cap by itself ends the process before it is added: with the count held to the cap plus
        // what is in flight, the sum cannot overflow (a wrapped count would switch the cap off).
        if delta > limit as isize {
            self.end();
        }
        let now = self.used.fetch_add(delta, Relaxed).wrapping_add(delta);
        if now > limit as isize {
            self.end();
        }
        self.peak.fetch_max(now, Relaxed);
    }

    fn refund(&self, size: usize) {
        if self.limit.load(Relaxed) != DISARMED {
            self.used.fetch_sub(size as isize, Relaxed);
        }
    }
}

// SAFETY: every call is `System`'s own, after an accounting step that touches only atomics.
unsafe impl GlobalAlloc for HeapCap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.charge(layout.size() as isize);
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.charge(layout.size() as isize);
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.refund(layout.size());
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        self.charge(new_size as isize - layout.size() as isize);
        System.realloc(ptr, layout, new_size)
    }
}

#[cfg(test)]
#[path = "heap_cap_tests.rs"]
mod tests;
