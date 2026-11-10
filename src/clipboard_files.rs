//! Files copied in Explorer (Ctrl+C on a file) arrive on the clipboard as a
//! file list, which egui does not read: its paste only carries text. The
//! launcher reads that list itself when Ctrl+V is pressed in its window.

use std::path::PathBuf;

/// Detects a fresh Ctrl+V press (not a held key) while the window has focus.
#[derive(Default)]
pub struct PasteKey {
    down: bool,
}

impl PasteKey {
    pub fn pressed(&mut self) -> bool {
        let down = key_down(0x11) && key_down(0x56); // VK_CONTROL + V
        let fresh = down && !self.down;
        self.down = down;
        fresh
    }
}

#[cfg(windows)]
fn key_down(key: i32) -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetAsyncKeyState(key: i32) -> i16;
    }
    // A plain query of the keyboard state; no input is consumed.
    (unsafe { GetAsyncKeyState(key) } as u16 & 0x8000) != 0
}

#[cfg(not(windows))]
fn key_down(_: i32) -> bool {
    false
}

/// Paths of the files on the clipboard, if it holds a file list.
#[cfg(windows)]
pub fn files() -> Vec<PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, ptr::null_mut};
    const CF_HDROP: u32 = 15;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn OpenClipboard(owner: *mut std::ffi::c_void) -> i32;
        fn GetClipboardData(format: u32) -> *mut std::ffi::c_void;
        fn CloseClipboard() -> i32;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn DragQueryFileW(
            drop: *mut std::ffi::c_void,
            index: u32,
            file: *mut u16,
            size: u32,
        ) -> u32;
    }
    let mut paths = Vec::new();
    // SAFETY: the clipboard is opened and closed in this block; the HDROP
    // handle is only read while it is open and buffers are sized by the API.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) == 0 || OpenClipboard(null_mut()) == 0 {
            return paths;
        }
        let drop = GetClipboardData(CF_HDROP);
        if !drop.is_null() {
            let count = DragQueryFileW(drop, u32::MAX, null_mut(), 0);
            for index in 0..count.min(256) {
                let length = DragQueryFileW(drop, index, null_mut(), 0);
                let mut buffer = vec![0u16; length as usize + 1];
                let written = DragQueryFileW(drop, index, buffer.as_mut_ptr(), buffer.len() as u32);
                buffer.truncate(written as usize);
                paths.push(PathBuf::from(OsString::from_wide(&buffer)));
            }
        }
        CloseClipboard();
    }
    paths
}

#[cfg(not(windows))]
pub fn files() -> Vec<PathBuf> {
    Vec::new()
}
