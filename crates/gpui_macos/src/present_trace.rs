//! A QA trace of each frame's way to the screen (F6 after-f6-render, D131): with `GPUI_PRESENT_LOG=<file>` every
//! presented drawable appends `<presented> <waited> <asked> p<transaction>`, the frame log's shape (ensemble ADR-0168,
//! so deskqa's `watch --app-log` reads it): when the drawable reached the screen (Unix ms, from its `presentedTime`; 0
//! when it never did), how long `nextDrawable` held it (ms), when the renderer asked for it (Unix ms) and whether it
//! went with a Core Animation transaction. Lines that start with `#` mark the way there (deskqa reads no frame from them):
//! `#e <arrival> <type> <made>` an event the view got, `#w <unix ms> <wake>` the frame waker's answer to new work,
//! `#s <unix ms>` a frame's step. Off (no variable), a frame and an event cost one read of a `OnceLock`.

use objc::{msg_send, sel, sel_impl};
use std::{
    fs::File,
    io::Write as _,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    fn CACurrentMediaTime() -> f64;
}

fn log() -> Option<&'static Mutex<File>> {
    static LOG: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = std::env::var_os("GPUI_PRESENT_LOG")?;
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(Mutex::new)
    })
    .as_ref()
}

fn unix_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0., |d| d.as_secs_f64() * 1000.)
}

pub(crate) fn watch(drawable: &metal::MetalDrawableRef, waited: Duration, transaction: bool) {
    if log().is_none() {
        return;
    }
    let asked = unix_ms() - waited.as_secs_f64() * 1000.;
    let waited = waited.as_secs_f64() * 1000.;
    let block = block::ConcreteBlock::new(move |drawable: &metal::DrawableRef| {
        let presented = drawable.presented_time();
        let at = if presented > 0. {
            // SAFETY: a QuartzCore function with no argument.
            let now = unsafe { CACurrentMediaTime() };
            unix_ms() - (now - presented) * 1000.
        } else {
            0.
        };
        if let Some(log) = log()
            && let Ok(mut file) = log.lock()
        {
            file.write_all(format!("{at:.3} {waited:.3} {asked:.3} p{}\n", transaction as u8).as_bytes()).ok();
        }
    });
    let block = block.copy();
    let drawable: &metal::DrawableRef = drawable;
    drawable.add_presented_handler(&block);
}

/// A mark in the log, `#<what> <unix ms> <words>` (deskqa skips a line that is no frame).
pub(crate) fn mark(what: &str, words: &str) {
    if let Some(log) = log()
        && let Ok(mut file) = log.lock()
    {
        file.write_all(format!("#{what} {:.3} {words}\n", unix_ms()).as_bytes()).ok();
    }
}

/// An event as the view got it: `#e <arrival unix ms> <type> <made unix ms>` (its `timestamp`, the system's uptime).
pub(crate) fn event(native_event: cocoa::base::id) {
    if log().is_none() {
        return;
    }
    // SAFETY: an NSEvent the view was handed; both are plain getters.
    let (kind, made): (u64, f64) = unsafe {
        (msg_send![native_event, type], msg_send![native_event, timestamp])
    };
    // SAFETY: a QuartzCore function with no argument.
    let now = unsafe { CACurrentMediaTime() };
    mark("e", &format!("{kind} {:.3}", unix_ms() - (now - made) * 1000.));
}
