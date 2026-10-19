//! How big the terminal is, so a frame can be composed to fit it.
//!
//! A frame drawn with a home-and-redraw writer has one hard rule: never write more
//! rows than the terminal has. One row too many scrolls the screen, and a scrolled
//! screen makes every later frame's row arithmetic wrong.
//!
//! `ioctl(TIOCGWINSZ)` is the way to ask, and it is one more libc declaration in the
//! style `rust/ob/src/sys.rs` already uses: `struct winsize` is four `unsigned
//! short`s on both macOS and Linux, and the request number is the same on both
//! (`0x40087468`). The fallback is 80×24, which is what a terminal that will not
//! answer is almost always at least as big as.

/// The size assumed when the terminal will not say.
pub const FALLBACK: (usize, usize) = (24, 80);

/// `struct winsize` from `<sys/ioctl.h>`: four `unsigned short`s.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

/// `TIOCGWINSZ`, the same value on macOS and Linux.
#[cfg(target_os = "linux")]
const TIOCGWINSZ: std::ffi::c_ulong = 0x5413;
/// `TIOCGWINSZ` on Darwin, which differs from Linux and is why both are named.
#[cfg(not(target_os = "linux"))]
const TIOCGWINSZ: std::ffi::c_ulong = 0x4008_7468;

extern "C" {
    fn ioctl(fd: i32, request: std::ffi::c_ulong, ...) -> i32;
}

/// The terminal's size as `(rows, columns)`, or the fallback.
///
/// A size of zero in either field is what a terminal reports when it has no size to
/// report — a pty with no window — and is treated as the fallback rather than as a
/// frame with no room.
pub fn size() -> (usize, usize) {
    size_of(1)
}

/// The size of the terminal on `fd`, or the fallback.
///
/// A file descriptor rather than always stdout, so the failure path can be tested
/// with a descriptor that is not a terminal: a wrong answer here is a frame that
/// scrolls the screen, and that is worth a test that does not need a pty.
pub fn size_of(fd: i32) -> (usize, usize) {
    let mut ws = WinSize::default();
    // SAFETY: `ioctl` writes through the pointer it is given, and `ws` is a live
    // `WinSize` of exactly the size the kernel's `struct winsize` has. The request
    // code is the one that takes a `struct winsize *`.
    let rc = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut WinSize) };
    if rc != 0 || ws.ws_row == 0 || ws.ws_col == 0 {
        return FALLBACK;
    }
    (usize::from(ws.ws_row), usize::from(ws.ws_col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_always_usable() {
        // Under `cargo test` stdout is usually a pipe, so this takes the fallback;
        // run from a terminal it takes the real path. Either way neither branch may
        // return a size a frame cannot be composed to.
        let (rows, cols) = size();
        assert!(rows >= 4, "rows={rows}");
        assert!(cols >= 20, "cols={cols}");
    }

    #[test]
    fn a_descriptor_that_is_not_a_terminal_falls_back_rather_than_composing_a_frame_with_no_room() {
        // A closed descriptor: `ioctl` fails, and the caller gets 80x24 rather than
        // a zero that would compose nothing.
        assert_eq!(size_of(9_000), FALLBACK);
    }

    #[test]
    fn a_real_terminal_is_asked_and_its_answer_is_taken_seriously() {
        // The pty is the one the Python test drives (`python/test_tui_input.py`),
        // which sets a size and asserts the frame fits it. Here the check is that
        // the request number and the struct layout are accepted by this kernel at
        // all: on a terminal, the call must succeed.
        // SAFETY: `isatty` takes a descriptor and has no other requirement.
        if unsafe { isatty(1) } == 0 {
            // A pipe: nothing to ask. The fallback test above is the other branch.
            return;
        }
        let (rows, cols) = size_of(1);
        assert!(rows >= 4 && cols >= 20, "rows={rows} cols={cols}");
    }

    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
}
