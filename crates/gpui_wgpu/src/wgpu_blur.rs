//! The backdrop blur (`BackdropBlur`): at one, the renderer ends its pass; this copies the region
//! under the box (grown by twice the radius) out of the target, blurs it by dual Kawase, two passes
//! down to a quarter and one up to a half, and draws the last pass up back into the target inside
//! the box's corners and clip. The renderer then begins a new pass. Its textures, bind groups and
//! uniform slots are kept from frame to frame. A target without `COPY_SRC` is left unblurred.

use bytemuck::{Pod, Zeroable};
use gpui::BackdropBlur;

const WGSL: &str = r#"
struct P {
  // The region copied out: its top left in the target, and its size, device px.
  region: vec4f,
  bounds: vec4f,
  mask: vec4f,
  // top left, top right, bottom right, bottom left
  radii: vec4f,
  // How many full-size px a texel of the source and of the destination is; the sample offset.
  src_div: f32,
  dst_div: f32,
  offset: f32,
  pad: f32,
}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;

struct V { @builtin(position) pos: vec4f }

@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
  let xy = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  return V(vec4f(xy.x * 2.0 - 1.0, 1.0 - xy.y * 2.0, 0.0, 1.0));
}

// A destination pixel's place in the source texture, both regions starting at texel 0.
fn src_uv(region_px: vec2f) -> vec2f {
  return region_px / p.src_div / vec2f(textureDimensions(src));
}

fn down(uv: vec2f) -> vec4f {
  let h = p.offset / vec2f(textureDimensions(src));
  var c = textureSample(src, smp, uv) * 4.0;
  c += textureSample(src, smp, uv - h);
  c += textureSample(src, smp, uv + h);
  c += textureSample(src, smp, uv + vec2f(h.x, -h.y));
  c += textureSample(src, smp, uv - vec2f(h.x, -h.y));
  return c / 8.0;
}

fn up(uv: vec2f) -> vec4f {
  let h = p.offset / vec2f(textureDimensions(src));
  var c = textureSample(src, smp, uv + vec2f(-h.x * 2.0, 0.0));
  c += textureSample(src, smp, uv + vec2f(-h.x, h.y)) * 2.0;
  c += textureSample(src, smp, uv + vec2f(0.0, h.y * 2.0));
  c += textureSample(src, smp, uv + vec2f(h.x, h.y)) * 2.0;
  c += textureSample(src, smp, uv + vec2f(h.x * 2.0, 0.0));
  c += textureSample(src, smp, uv + vec2f(h.x, -h.y)) * 2.0;
  c += textureSample(src, smp, uv + vec2f(0.0, -h.y * 2.0));
  c += textureSample(src, smp, uv + vec2f(-h.x, -h.y)) * 2.0;
  return c / 12.0;
}

@fragment fn fs_down(v: V) -> @location(0) vec4f {
  return down(src_uv(v.pos.xy * p.dst_div));
}

@fragment fn fs_up(v: V) -> @location(0) vec4f {
  return up(src_uv(v.pos.xy * p.dst_div));
}

// Back into the target: inside the box's rounded corners, antialiased, and its clip.
@fragment fn fs_back(v: V) -> @location(0) vec4f {
  let c = up(src_uv(v.pos.xy - p.region.xy));
  let half = p.bounds.zw / 2.0;
  let d = v.pos.xy - (p.bounds.xy + half);
  var r = select(select(p.radii.x, p.radii.y, d.x > 0.0), select(p.radii.w, p.radii.z, d.x > 0.0), d.y > 0.0);
  r = min(r, min(half.x, half.y));
  let q = abs(d) - half + r;
  let sdf = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
  let inside = all(v.pos.xy >= p.mask.xy) && all(v.pos.xy < p.mask.xy + p.mask.zw);
  let cover = saturate(0.5 - sdf) * select(0.0, 1.0, inside);
  return c * cover;
}
"#;

/// `P` in the shader.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct Params {
    region: [f32; 4],
    bounds: [f32; 4],
    mask: [f32; 4],
    radii: [f32; 4],
    src_div: f32,
    dst_div: f32,
    offset: f32,
    pad: f32,
}

/// Slots per blur: down, down, up, back.
const PASSES: u64 = 4;

pub(crate) struct Backdrop {
    layout: wgpu::BindGroupLayout,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    back: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    /// Bytes per uniform slot: the device's dynamic offset alignment.
    slot: u64,
    slots: u64,
    uniforms: wgpu::Buffer,
    /// Full size, half, quarter, half again; and a bind group reading each.
    textures: Vec<wgpu::Texture>,
    reads: Vec<wgpu::BindGroup>,
    size: (u32, u32),
    /// The next free blur of this frame.
    next: u64,
}

impl Backdrop {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop_blur"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop_blur"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("backdrop_blur"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let down = pipeline("fs_down", None);
        let up = pipeline("fs_up", None);
        // Premultiplied, as the shaders' colours: what is under the box is replaced where it is covered.
        let back = pipeline("fs_back", Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("backdrop_blur"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let slot = (device.limits().min_uniform_buffer_offset_alignment as u64)
            .max(size_of::<Params>() as u64);
        let slots = 4 * PASSES;
        let uniforms = Self::buffer(device, slot * slots);
        Self {
            layout,
            down,
            up,
            back,
            sampler,
            slot,
            slots,
            uniforms,
            textures: Vec::new(),
            reads: Vec::new(),
            size: (0, 0),
            next: 0,
        }
    }

    fn buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("backdrop_blur"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Room for `count` blurs this frame, and textures for a `width` x `height` target.
    pub(crate) fn begin(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        count: usize,
        width: u32,
        height: u32,
    ) {
        self.next = 0;
        let count = count as u64;
        if count * PASSES > self.slots {
            self.slots = (count * PASSES).max(self.slots * 2);
            self.uniforms = Self::buffer(device, self.slot * self.slots);
            self.reads.clear();
        }
        if self.size != (width, height) || self.reads.is_empty() {
            let make = |div: u32| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("backdrop_blur"),
                    size: wgpu::Extent3d {
                        width: width.div_ceil(div).max(1),
                        height: height.div_ceil(div).max(1),
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };
            self.textures = vec![make(1), make(2), make(4), make(2)];
            self.reads = self
                .textures
                .iter()
                .map(|texture| {
                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("backdrop_blur"),
                        layout: &self.layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer: &self.uniforms,
                                    offset: 0,
                                    size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                                }),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::Sampler(&self.sampler),
                            },
                        ],
                    })
                })
                .collect();
            self.size = (width, height);
        }
    }

    /// Blurs `blur` in `target`, which needs `COPY_SRC`. The caller's pass is ended before and
    /// begun again after.
    pub(crate) fn run(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        target_view: &wgpu::TextureView,
        blur: &BackdropBlur,
    ) {
        let slot = self.next;
        if (slot + 1) * PASSES > self.slots || self.textures.len() != 4 {
            return;
        }
        self.next += 1;
        let (tw, th) = (target.width() as f32, target.height() as f32);
        let b = blur.bounds;
        let (bx, by, bw, bh) = (b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0);
        let m = blur.content_mask.bounds;
        let (mx, my, mw, mh) = (m.origin.x.0, m.origin.y.0, m.size.width.0, m.size.height.0);
        let radius = blur.radius.0;
        // The region: the box grown by twice the radius, within the target.
        let grow = 2.0 * radius;
        let x0 = (bx - grow).floor().max(0.0);
        let y0 = (by - grow).floor().max(0.0);
        let x1 = (bx + bw + grow).ceil().min(tw);
        let y1 = (by + bh + grow).ceil().min(th);
        // Its part that is drawn back: the box within its clip and the target.
        let sx0 = bx.max(mx).floor().max(x0);
        let sy0 = by.max(my).floor().max(y0);
        let sx1 = (bx + bw).min(mx + mw).ceil().min(x1);
        let sy1 = (by + bh).min(my + mh).ceil().min(y1);
        if radius <= 0.0 || x1 <= x0 || y1 <= y0 || sx1 <= sx0 || sy1 <= sy0 {
            return;
        }
        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let r = blur.corner_radii;
        let mut params = Params {
            region: [x0, y0, w as f32, h as f32],
            bounds: [bx, by, bw, bh],
            mask: [mx, my, mw, mh],
            radii: [r.top_left.0, r.top_right.0, r.bottom_right.0, r.bottom_left.0],
            // Dual Kawase's offset for this radius: a CSS blur's radius is its sigma.
            offset: (radius / 6.0).max(0.5),
            ..Params::default()
        };
        // down: full to half, half to quarter; up: quarter to half; back: half to the target.
        let divs = [(1.0, 2.0), (2.0, 4.0), (4.0, 2.0), (2.0, 1.0)];
        for (pass, (src, dst)) in divs.into_iter().enumerate() {
            params.src_div = src;
            params.dst_div = dst;
            queue.write_buffer(
                &self.uniforms,
                (slot * PASSES + pass as u64) * self.slot,
                bytemuck::bytes_of(&params),
            );
        }
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x0 as u32, y: y0 as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.textures[0],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let offset = |pass: u64| ((slot * PASSES + pass) * self.slot) as u32;
        let steps = [(&self.down, 0usize, 1usize, 0u64, 2u32), (&self.down, 1, 2, 1, 4), (&self.up, 2, 3, 2, 2)];
        for (pipeline, read, into, pass, div) in steps {
            let view = self.textures[into].create_view(&wgpu::TextureViewDescriptor::default());
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop_blur"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            p.set_pipeline(pipeline);
            p.set_bind_group(0, &self.reads[read], &[offset(pass)]);
            p.set_viewport(0.0, 0.0, w.div_ceil(div) as f32, h.div_ceil(div) as f32, 0.0, 1.0);
            p.draw(0..3, 0..1);
        }
        let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("backdrop_blur_back"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
        p.set_pipeline(&self.back);
        p.set_bind_group(0, &self.reads[3], &[offset(3)]);
        p.set_scissor_rect(sx0 as u32, sy0 as u32, (sx1 - sx0) as u32, (sy1 - sy0) as u32);
        p.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::WGSL;

    #[test]
    fn blur_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(WGSL).expect("shader should parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("shader should validate");
    }

    /// Patterns, dotted borders and the blur, drawn by the headless renderer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod pixels {
        use crate::WgpuHeadlessRenderer;
        use gpui::{
            BackdropBlur, BorderStyle, Bounds, ContentMask, Corners, DevicePixels, Edges, Hsla,
            PatternLines, PlatformHeadlessRenderer, Point, Quad, ScaledPixels, Scene, Size,
            pattern_dots, pattern_lines,
        };

        fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<ScaledPixels> {
            Bounds {
                origin: Point { x: x.into(), y: y.into() },
                size: Size { width: w.into(), height: h.into() },
            }
        }

        fn quad(bounds: Bounds<ScaledPixels>, background: gpui::Background) -> Quad {
            Quad {
                order: 0,
                border_style: BorderStyle::Solid,
                bounds,
                content_mask: ContentMask { bounds, ..Default::default() },
                background,
                border_color: Hsla::transparent_black(),
                corner_radii: Corners::default(),
                border_widths: Edges::default(),
                pad: [0; 2],
            }
        }

        fn draw(scene: &mut Scene, w: i32, h: i32) -> image::RgbaImage {
            scene.finish();
            let mut renderer = WgpuHeadlessRenderer::new().expect("a headless renderer");
            renderer
                .render_scene_to_image(scene, Size { width: DevicePixels(w), height: DevicePixels(h) })
                .expect("a frame")
        }

        fn red(image: &image::RgbaImage, x: u32, y: u32) -> u8 {
            image.get_pixel(x, y).0[0]
        }

        #[test]
        fn dots_sit_at_each_tiles_centre() {
            let mut scene = Scene::default();
            scene.insert_primitive(quad(rect(0., 0., 32., 32.), pattern_dots(gpui::red(), 16., 2.)));
            let image = draw(&mut scene, 32, 32);
            assert!(red(&image, 8, 8) > 240 && red(&image, 24, 24) > 240, "a dot at each centre");
            assert!(red(&image, 0, 0) < 15 && red(&image, 15, 8) < 15, "nothing between them");
        }

        #[test]
        fn grid_lines_run_from_the_corner() {
            let mut scene = Scene::default();
            scene.insert_primitive(quad(rect(0., 0., 32., 32.), pattern_lines(gpui::red(), 16., 1., PatternLines::Grid)));
            scene.insert_primitive(quad(rect(0., 40., 32., 32.), pattern_lines(gpui::red(), 16., 1., PatternLines::Horizontal)));
            let image = draw(&mut scene, 32, 72);
            assert!(red(&image, 0, 5) > 240 && red(&image, 16, 5) > 240 && red(&image, 5, 16) > 240, "both ways");
            assert!(red(&image, 5, 5) < 15, "nothing inside a tile");
            assert!(red(&image, 5, 56) > 240 && red(&image, 0, 45) < 15, "horizontal only");
        }

        #[test]
        fn a_dotted_border_is_round_dots_with_gaps() {
            let mut scene = Scene::default();
            let mut q = quad(rect(0., 0., 64., 16.), Hsla::transparent_black().into());
            q.border_style = BorderStyle::Dotted;
            q.border_color = gpui::red();
            q.border_widths = Edges { top: 4.0.into(), ..Default::default() };
            scene.insert_primitive(q);
            let image = draw(&mut scene, 64, 16);
            let row = |y: u32| (0..64).map(|x| red(&image, x, y) as u32).collect::<Vec<_>>();
            let (edge, middle) = (row(0), row(2));
            assert!(middle.iter().any(|&r| r > 200) && middle.iter().any(|&r| r < 15), "dots and gaps: {middle:?}");
            assert!(edge.iter().sum::<u32>() < middle.iter().sum::<u32>(), "round: thinner at the edge than the middle");
        }

        #[test]
        fn a_backdrop_blur_softens_only_inside_its_box() {
            let mut scene = Scene::default();
            // White on the left half, black on the right; a box over the edge blurs it.
            scene.insert_primitive(quad(rect(0., 0., 32., 64.), gpui::white().into()));
            scene.insert_primitive(BackdropBlur {
                order: 0,
                radius: 8.0.into(),
                bounds: rect(16., 16., 32., 32.),
                content_mask: ContentMask { bounds: rect(0., 0., 64., 64.), ..Default::default() },
                corner_radii: Corners::default(),
            });
            let image = draw(&mut scene, 64, 64);
            let (inside_white, inside_black) = (red(&image, 30, 32), red(&image, 34, 32));
            assert!(inside_white < 245 && inside_black > 10, "the edge is soft in the box: {inside_white} {inside_black}");
            assert!(red(&image, 31, 8) > 250 && red(&image, 32, 8) < 5, "and sharp outside it");
            assert!(red(&image, 17, 32) > 245, "far from the edge the white stays");
        }
    }
}
