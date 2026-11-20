#![cfg(target_os = "macos")]
//! macOS platform implementation for GPUI.
//!
//! macOS screens have a y axis that goes up from the bottom of the screen and
//! an origin at the bottom left of the main display.

mod display;
mod display_link;
mod events;
mod keyboard;
mod pasteboard;
mod system_notifications;

#[cfg(feature = "screen-capture")]
mod screen_capture;

#[cfg(feature = "metal-renderer")]
use gpui_apple::metal_renderer as renderer;
#[cfg(not(feature = "metal-renderer"))]
mod wgpu_renderer;
#[cfg(not(feature = "metal-renderer"))]
use wgpu_renderer as renderer;

/// The headless renderer of the windows' own renderer, for tests and benchmarks.
#[cfg(all(
    not(feature = "metal-renderer"),
    any(feature = "test-support", feature = "bench-support")
))]
pub use gpui_wgpu::WgpuHeadlessRenderer;

pub mod metal_renderer {
    pub use gpui_apple::metal_renderer::{PathRasterizationVertex, PathSprite, SurfaceBounds};

    #[cfg(any(test, feature = "bench-support", feature = "test-support"))]
    pub use gpui_apple::metal_renderer::MetalHeadlessRenderer;
}

mod platform;
mod window;
mod window_appearance;

use objc2::{
    Encode, Encoding, RefEncode,
    rc::Retained,
    runtime::{AnyObject, Bool},
};
use objc2_foundation::{NSNotFound, NSString, NSUInteger};
use std::{ffi::CStr, ops::Range};

pub(crate) use display::*;
pub(crate) use display_link::*;
pub(crate) use gpui_apple::{AppleActivity, AppleDispatcher, main_thread_marker};
pub(crate) use keyboard::*;
pub(crate) use platform::*;
pub(crate) use window::*;

pub use platform::MacPlatform;

trait BoolExt {
    fn to_objc(self) -> Bool;
}

impl BoolExt for bool {
    fn to_objc(self) -> Bool {
        Bool::new(self)
    }
}

trait NSStringExt {
    unsafe fn to_str(&self) -> &str;
}

impl NSStringExt for *mut AnyObject {
    unsafe fn to_str(&self) -> &str {
        unsafe {
            let Some(string) = self.cast::<NSString>().as_ref() else {
                return "";
            };
            let cstr = string.UTF8String();
            if cstr.is_null() {
                ""
            } else {
                CStr::from_ptr(cstr).to_str().unwrap()
            }
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct NSRange {
    pub location: NSUInteger,
    pub length: NSUInteger,
}

impl NSRange {
    fn invalid() -> Self {
        Self {
            location: NSNotFound as NSUInteger,
            length: 0,
        }
    }

    fn is_valid(&self) -> bool {
        self.location != NSNotFound as NSUInteger
    }

    fn to_range(self) -> Option<Range<usize>> {
        if self.is_valid() {
            let start = self.location;
            let end = start + self.length;
            Some(start..end)
        } else {
            None
        }
    }
}

impl From<Range<usize>> for NSRange {
    fn from(range: Range<usize>) -> Self {
        NSRange {
            location: range.start as NSUInteger,
            length: range.len() as NSUInteger,
        }
    }
}

unsafe impl Encode for NSRange {
    const ENCODING: Encoding = objc2_foundation::NSRange::ENCODING;
}

unsafe impl RefEncode for NSRange {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

fn ns_string(string: &str) -> *mut AnyObject {
    Retained::autorelease_ptr(NSString::from_str(string)).cast()
}

#[cfg(test)]
mod renderer_start {
    use gpui::{Bounds, ContentMask, DevicePixels, Hsla, Quad, ScaledPixels, Scene, point, size};
    use std::time::Instant;

    /// Where a window's first frame spends its time, by renderer: run with
    /// `cargo test -p gpui_macos renderer_start -- --ignored --nocapture`, with
    /// and without `--features metal-renderer`.
    #[test]
    #[ignore]
    fn times_the_first_frame() {
        let started = Instant::now();
        let mut renderer = super::renderer::new_renderer(Default::default(), false, false).unwrap();
        let created = started.elapsed();
        renderer.update_drawable_size(size(DevicePixels(1200), DevicePixels(800)));
        let mut scene = Scene::default();
        let bounds = Bounds::new(
            point(ScaledPixels(0.), ScaledPixels(0.)),
            size(ScaledPixels(600.), ScaledPixels(400.)),
        );
        scene.insert_primitive(Quad {
            bounds,
            content_mask: ContentMask { bounds },
            background: Hsla::red().into(),
            border_color: Hsla::red(),
            ..Default::default()
        });
        scene.finish();
        let drawing = Instant::now();
        renderer.draw(&scene);
        let first = drawing.elapsed();
        let drawing = Instant::now();
        renderer.draw(&scene);
        let second = drawing.elapsed();
        println!(
            "renderer start: create {:.1} ms, first draw {:.1} ms, second draw {:.1} ms",
            created.as_secs_f64() * 1e3,
            first.as_secs_f64() * 1e3,
            second.as_secs_f64() * 1e3
        );
    }
}
