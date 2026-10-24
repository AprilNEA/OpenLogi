//! A display-refresh clock for synthetic scroll output.
//!
//! macOS generates trackpad and Magic Mouse momentum in step with the
//! display: one event per frame. Output posted on a free-running timer drifts
//! against the frames instead, so a browser draws some frames with no scroll
//! event and some with two, and the page stutters. A [`DisplayLink`] calls
//! back once per refresh of the active displays, so output can follow the
//! same beat.
//!
//! On other platforms [`DisplayLink::new`] answers `None` and callers keep
//! their timer.

/// Calls a function once per display refresh while started.
pub struct DisplayLink {
    #[cfg(target_os = "macos")]
    inner: macos::Link,
}

impl DisplayLink {
    /// A stopped display link that will call `on_frame` on a CoreVideo
    /// thread once per refresh. `None` where there is no display clock.
    #[must_use]
    pub fn new(on_frame: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            macos::Link::new(Box::new(on_frame)).map(|inner| Self { inner })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = on_frame;
            None
        }
    }

    /// Start calling back; harmless when already started. Returns whether
    /// the clock is running, so callers can retain their timer on failure.
    #[must_use]
    pub fn start(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.inner.start()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Stop calling back; harmless when already stopped.
    pub fn stop(&self) {
        #[cfg(target_os = "macos")]
        self.inner.stop();
    }
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CVDisplayLink creation and its C output callback are CoreVideo FFI"
)]
#[expect(
    deprecated,
    reason = "CVDisplayLink is deprecated for NSScreen/NSView display links, which \
              deliver on the main run loop; the scroll worker needs its tick off \
              the main thread, which only CVDisplayLink provides"
)]
mod macos {
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};

    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{CVDisplayLink, CVOptionFlags, CVReturn, CVTimeStamp, kCVReturnSuccess};

    type Callback = Box<dyn Fn() + Send + Sync>;

    pub(super) struct Link {
        link: CFRetained<CVDisplayLink>,
        /// The boxed callback CoreVideo holds a raw pointer to. Released only
        /// after the link is stopped and dropped, in [`Drop`].
        callback: *mut Callback,
    }

    // SAFETY: CVDisplayLink is documented thread-safe to start and stop from
    // any thread, and the callback it holds is `Send + Sync`.
    unsafe impl Send for Link {}

    unsafe extern "C-unwind" fn on_output(
        _link: NonNull<CVDisplayLink>,
        _now: NonNull<CVTimeStamp>,
        _output_time: NonNull<CVTimeStamp>,
        _flags_in: CVOptionFlags,
        _flags_out: NonNull<CVOptionFlags>,
        user_info: *mut c_void,
    ) -> CVReturn {
        // SAFETY: `user_info` is the `*mut Callback` set in `Link::new`,
        // alive until `Drop` has stopped the link.
        let callback = unsafe { &*user_info.cast::<Callback>() };
        callback();
        kCVReturnSuccess
    }

    impl Link {
        pub(super) fn new(callback: Callback) -> Option<Self> {
            let mut raw: *mut CVDisplayLink = ptr::null_mut();
            // SAFETY: `raw` is a valid out-pointer; on success it holds a +1
            // retained display link.
            let status =
                unsafe { CVDisplayLink::create_with_active_cg_displays(NonNull::from(&mut raw)) };
            let raw = NonNull::new(raw).filter(|_| status == kCVReturnSuccess)?;
            // SAFETY: created above with a +1 retain that this takes over.
            let link = unsafe { CFRetained::from_raw(raw) };
            let callback = Box::into_raw(Box::new(callback));
            // SAFETY: `on_output` matches the callback type; `callback` stays
            // valid until `Drop`, which stops the link before freeing it.
            let status = unsafe { link.set_output_callback(Some(on_output), callback.cast()) };
            if status != kCVReturnSuccess {
                // SAFETY: never handed to a running link.
                drop(unsafe { Box::from_raw(callback) });
                return None;
            }
            Some(Self { link, callback })
        }

        pub(super) fn start(&self) -> bool {
            if !self.link.is_running() {
                let _ = self.link.start();
            }
            self.link.is_running()
        }

        pub(super) fn stop(&self) {
            if self.link.is_running() {
                let _ = self.link.stop();
            }
        }
    }

    impl Drop for Link {
        fn drop(&mut self) {
            // CVDisplayLinkStop waits for an in-flight callback to return, so
            // nothing uses `callback` after this.
            let _ = self.link.stop();
            // SAFETY: created by `Box::into_raw` in `new`; the link is stopped.
            drop(unsafe { Box::from_raw(self.callback) });
        }
    }
}
