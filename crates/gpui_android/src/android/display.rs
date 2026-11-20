//! The one display an Android activity draws on.

use std::time::Duration;

use anyhow::Result;
use gpui::{Bounds, DisplayId, Pixels, PlatformDisplay, Point, px, size};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub(crate) struct AndroidDisplay {
    /// Its size in logical pixels.
    pub width: f32,
    pub height: f32,
}

impl AndroidDisplay {
    pub(crate) fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

impl PlatformDisplay for AndroidDisplay {
    fn id(&self) -> DisplayId {
        DisplayId::new(0)
    }

    fn uuid(&self) -> Result<Uuid> {
        Ok(Uuid::from_u128(0x6770_7569_5f61_6e64_726f_6964_0000_0000))
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds::new(Point::default(), size(px(self.width), px(self.height)))
    }

    fn refresh_interval(&self) -> Option<Duration> {
        // Choreographer paces frames; GPUI asks only for a hint.
        Some(Duration::from_micros(16_667))
    }
}
