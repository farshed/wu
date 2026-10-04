// Display-link registry mutations must run on the main thread, so this is a harness-less test.
#[cfg(target_os = "macos")]
#[path = "../src/display_link.rs"]
mod display_link;

// Overrides CoreVideo's start so every start fails deterministically.
#[cfg(target_os = "macos")]
static START_ATTEMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(target_os = "macos")]
#[unsafe(export_name = "CVDisplayLinkStart")]
extern "C" fn fail_display_link_start(_: *mut std::ffi::c_void) -> i32 {
    START_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    -6660 // kCVReturnError
}

fn main() {
    #[cfg(target_os = "macos")]
    {
        use core_graphics::display::CGMainDisplayID;
        use display_link::WindowFrameSource;
        use std::{
            ffi::c_void,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };

        extern "C" fn unused_callback(_: *mut c_void) {}
        let requested = Arc::new(AtomicBool::new(true));
        let mut source =
            WindowFrameSource::new(std::ptr::null_mut(), unused_callback, requested.clone());

        source.stop();
        assert!(!source.is_running());
        assert!(
            !requested.swap(true, Ordering::AcqRel),
            "stopping a source must allow invalidation to queue another restart"
        );

        // Exercise the full failed-subscription path, including registry cleanup.
        let display_id = unsafe { CGMainDisplayID() };
        for _ in 0..2 {
            let error = source
                .start(display_id)
                .expect_err("injected CoreVideo start failure must propagate");
            assert!(!source.is_running());
            assert!(
                !requested.swap(true, Ordering::AcqRel),
                "a failed start must allow the next invalidation to retry: {error}"
            );
        }
        assert_eq!(START_ATTEMPTS.load(Ordering::Relaxed), 2);
        println!("PASS: stopped and failed frame sources allow redraw requests to retry");
    }
}
