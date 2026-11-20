//! Native Android platform support for GPUI.
//!
//! This crate connects GPUI to an Android `GameActivity`: its `SurfaceView` is the window, drawn
//! through `gpui_wgpu` on Vulkan (GL when Vulkan is missing), its looper thread runs GPUI's
//! foreground executor, and its lifecycle makes the surface again on resume. Text is shaped by
//! `gpui_wgpu`'s cosmic-text system over the fonts in /system/fonts.

#[cfg(target_os = "android")]
pub mod android;

#[cfg(target_os = "android")]
pub use android::{AndroidPlatform, current_platform, set_android_app};
