//! A window's renderer on `gpui_wgpu`, drawing on Metal: the names the window calls, over
//! `WgpuRenderer`. `gpui_apple`'s `MetalRenderer` takes its place under the `metal-renderer`
//! feature.
//!
//! The renderer owns the view's backing `CAMetalLayer`, as `MetalRenderer` does, and wgpu draws
//! into it. While a `CAMetalDisplayLink` drives the layer, each frame is drawn into the drawable
//! the link hands out, so a draw never waits in `nextDrawable`; detached (a live resize's
//! synchronous draws), wgpu takes one from the layer itself.

use anyhow::Result;
use gpui::{DevicePixels, Scene, Size, size};
use gpui_wgpu::{GpuContext, WgpuAtlas, WgpuRenderer, WgpuSurfaceConfig, wgpu};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_quartz_core::{CAAutoresizingMask, CAMetalDrawable, CAMetalLayer};
use std::sync::Arc;

/// One GPU device, shared by every window.
pub type Context = GpuContext;
pub type Renderer = WgpuMacRenderer;

pub fn new_renderer(context: Context, transparent: bool, use_display_p3: bool) -> Result<Renderer> {
    WgpuMacRenderer::new(context, transparent, use_display_p3)
}

pub struct WgpuMacRenderer {
    renderer: WgpuRenderer,
    display_link_mode: DisplayLinkMode,
    presents_with_transaction: bool,
    // Dropped after `renderer`, whose surface draws into it.
    layer: Retained<CAMetalLayer>,
}

impl WgpuMacRenderer {
    fn new(context: Context, transparent: bool, use_display_p3: bool) -> Result<Self> {
        let layer = CAMetalLayer::new();
        layer.setNeedsDisplayOnBoundsChange(true);
        layer.setAutoresizingMask(
            CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable,
        );
        let config = WgpuSurfaceConfig {
            // The window sets the real size once the view is laid out.
            size: size(DevicePixels(1), DevicePixels(1)),
            transparent,
            preferred_present_mode: None,
        };
        // Safety: the layer is a `CAMetalLayer`, and this struct drops the renderer before it.
        let mut renderer = unsafe {
            WgpuRenderer::new_from_layer(
                context,
                Retained::as_ptr(&layer).cast_mut().cast(),
                config,
            )?
        };
        if use_display_p3 {
            renderer.set_color_space(wgpu::SurfaceColorSpace::DisplayP3);
        }
        Ok(Self {
            renderer,
            display_link_mode: DisplayLinkMode::Detached,
            presents_with_transaction: false,
            layer,
        })
    }

    /// The layer a `CAMetalDisplayLink` drives, handing out its drawables.
    pub fn layer(&self) -> Option<&CAMetalLayer> {
        Some(&self.layer)
    }

    /// The view's backing layer.
    pub fn layer_ptr(&self) -> *mut CAMetalLayer {
        Retained::as_ptr(&self.layer).cast_mut()
    }

    pub fn sprite_atlas(&self) -> &Arc<WgpuAtlas> {
        self.renderer.sprite_atlas()
    }

    /// Draw the next frame into `drawable`, one the display link handed out. Pass `None` once
    /// the frame is done, so a drawable the frame did not use goes back to the layer.
    ///
    /// Returns the drawable this replaces: after a frame, `Some` means the frame neither drew
    /// nor presented.
    pub fn set_frame_drawable(
        &mut self,
        drawable: Option<Retained<ProtocolObject<dyn CAMetalDrawable>>>,
    ) -> Option<Retained<ProtocolObject<dyn CAMetalDrawable>>> {
        match &mut self.display_link_mode {
            DisplayLinkMode::Attached { drawable: current } => std::mem::replace(current, drawable),
            DisplayLinkMode::Detached => {
                debug_assert!(
                    drawable.is_none(),
                    "received a display link drawable while detached"
                );
                None
            }
        }
    }

    /// Whether a `CAMetalDisplayLink` drives the layer. While one does, frames are drawn only
    /// into the drawables it hands out.
    pub fn set_display_link_attached(&mut self, attached: bool) {
        self.display_link_mode = if attached {
            DisplayLinkMode::Attached { drawable: None }
        } else {
            DisplayLinkMode::Detached
        };
    }

    /// During a live resize the window draws synchronously, and the frame must reach the
    /// screen in the same Core Animation transaction as the new window size; wgpu reads this
    /// flag off the layer when it presents.
    pub fn set_presents_with_transaction(&mut self, presents_with_transaction: bool) {
        self.presents_with_transaction = presents_with_transaction;
        self.layer
            .setPresentsWithTransaction(presents_with_transaction);
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        self.renderer.update_drawable_size(size);
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        self.renderer.update_transparency(transparent);
    }

    pub fn destroy(&mut self) {
        self.renderer.destroy();
    }

    pub fn draw(&mut self, scene: &Scene) {
        match &mut self.display_link_mode {
            DisplayLinkMode::Attached { drawable } => {
                let Some(drawable) = drawable.take() else {
                    log::error!("skipped a frame drawn outside its display link's update");
                    return;
                };
                self.renderer
                    .draw_to_drawable(scene, &drawable, self.presents_with_transaction);
            }
            DisplayLinkMode::Detached => {
                self.renderer.draw(scene);
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn render_to_image(&mut self, scene: &Scene) -> Result<image::RgbaImage> {
        self.renderer.render_to_image(scene)
    }
}

enum DisplayLinkMode {
    Detached,
    /// The drawable the link handed out for the frame under way, until the frame draws it.
    Attached {
        drawable: Option<Retained<ProtocolObject<dyn CAMetalDrawable>>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, ContentMask, DevicePixels, Hsla, Quad, ScaledPixels, point, size};

    #[test]
    fn draws_a_scene_into_an_image_of_the_drawable_size() {
        let mut renderer = new_renderer(Default::default(), false, false).unwrap();
        renderer.update_drawable_size(size(DevicePixels(64), DevicePixels(32)));
        let mut scene = Scene::default();
        let bounds = Bounds::new(
            point(ScaledPixels(0.), ScaledPixels(0.)),
            size(ScaledPixels(64.), ScaledPixels(32.)),
        );
        scene.insert_primitive(Quad {
            bounds,
            content_mask: ContentMask { bounds },
            background: Hsla::red().into(),
            border_color: Hsla::red(),
            ..Default::default()
        });
        scene.finish();
        let image = renderer.render_to_image(&scene).unwrap();
        assert_eq!((image.width(), image.height()), (64, 32));
        let pixel = image.get_pixel(32, 16);
        assert!(
            pixel[0] > 200 && pixel[1] < 50 && pixel[2] < 50,
            "a red quad fills the frame: {pixel:?}"
        );
    }

    #[test]
    fn draws_into_the_display_links_drawable_and_hands_back_an_unused_one() {
        let mut renderer = new_renderer(Default::default(), false, false).unwrap();
        renderer.update_drawable_size(size(DevicePixels(64), DevicePixels(64)));
        let drawable = renderer
            .layer()
            .and_then(|layer| layer.nextDrawable())
            .expect("the layer gives a drawable");

        renderer.set_display_link_attached(true);
        assert!(renderer.set_frame_drawable(Some(drawable.clone())).is_none());
        renderer.draw(&Scene::default());
        assert!(
            matches!(
                renderer.display_link_mode,
                DisplayLinkMode::Attached { drawable: None }
            ),
            "a frame that drew consumed its drawable"
        );
        assert!(renderer.set_frame_drawable(None).is_none());

        renderer.set_frame_drawable(Some(drawable));
        assert!(
            renderer.set_frame_drawable(None).is_some(),
            "a frame that did not draw hands its drawable back"
        );
        renderer.set_display_link_attached(false);
        assert!(matches!(renderer.display_link_mode, DisplayLinkMode::Detached));
    }
}
