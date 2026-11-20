//! On macOS, compiles GPUI's WGSL shaders to a metallib at build time, so a window's first frame
//! does not pay Metal's shader compile, which wgpu otherwise runs on the source naga writes. The
//! renderer hands the metallib to wgpu as a passthrough shader (`wgpu_renderer.rs`).
//!
//! A passthrough shader is taken as is, so its resource slots must be the ones wgpu's Metal backend
//! gives each pipeline layout: `slots` repeats `create_pipeline_layout` in
//! `wgpu-hal/src/metal/device.rs` over the layouts `create_bind_group_layouts` makes, and the
//! renderer's test `metallib_draws_as_the_wgsl_does` holds the two equal.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rerun-if-changed=src/shaders.wgsl");
    println!("cargo:rerun-if-changed=src/shaders_storage.wgsl");
    println!("cargo:rerun-if-changed=src/shaders_subpixel.wgsl");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");

    let read = |name: &str| std::fs::read_to_string(Path::new("src").join(name)).unwrap();
    let storage = format!("{}{}", read("shaders.wgsl"), read("shaders_storage.wgsl"));
    let subpixel = format!(
        "enable dual_source_blending;\n{}{}{}",
        read("shaders.wgsl"),
        read("shaders_storage.wgsl"),
        read("shaders_subpixel.wgsl")
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    compile(&storage, naga::valid::Capabilities::empty(), &out_dir, "gpui_shaders");
    compile(
        &subpixel,
        naga::valid::Capabilities::DUAL_SOURCE_BLENDING,
        &out_dir,
        "gpui_subpixel_shaders",
    );
}

/// The bind groups of each pipeline, in order, as `create_pipelines` lays them out.
fn layout_of(entry_point: &str) -> Vec<Group> {
    let data = if entry_point.ends_with("_surface") {
        SURFACES
    } else {
        INSTANCES
    };
    let textured = [
        "_path",
        "_mono_sprite",
        "_subpixel_sprite",
        "_poly_sprite",
    ]
    .iter()
    .any(|suffix| entry_point.ends_with(suffix));
    let mut groups = vec![GLOBALS, data];
    if textured {
        groups.push(TEXTURE);
    }
    groups
}

#[derive(Clone, Copy)]
enum Kind {
    Buffer,
    Texture,
    Sampler,
}

#[derive(Clone, Copy)]
struct Entry {
    binding: u32,
    kind: Kind,
    vertex: bool,
    fragment: bool,
}

type Group = &'static [Entry];

const fn entry(binding: u32, kind: Kind, vertex: bool) -> Entry {
    Entry {
        binding,
        kind,
        vertex,
        fragment: true,
    }
}

// `create_bind_group_layouts`: every entry is seen by the fragment stage, and by the vertex stage
// where the third argument says so.
const GLOBALS: Group = &[entry(0, Kind::Buffer, true), entry(1, Kind::Buffer, false)];
const INSTANCES: Group = &[entry(0, Kind::Buffer, true)];
const TEXTURE: Group = &[entry(0, Kind::Texture, true), entry(1, Kind::Sampler, true)];
const SURFACES: Group = &[
    entry(0, Kind::Buffer, true),
    entry(1, Kind::Texture, false),
    entry(2, Kind::Texture, false),
    entry(3, Kind::Sampler, false),
];

/// `create_pipeline_layout` for one stage: each group's entries in order take the stage's next
/// buffer, texture or sampler slot; the sizes buffer takes the buffer slot after them.
fn slots(groups: &[Group], stage: naga::ShaderStage) -> naga::back::msl::EntryPointResources {
    use naga::back::msl::{BindSamplerTarget, BindTarget};
    let (mut buffers, mut textures, mut samplers) = (0u8, 0u8, 0u8);
    let mut resources = naga::back::msl::BindingMap::default();
    for (group, entries) in groups.iter().enumerate() {
        for entry in entries.iter() {
            let visible = match stage {
                naga::ShaderStage::Vertex => entry.vertex,
                naga::ShaderStage::Fragment => entry.fragment,
                _ => false,
            };
            if !visible {
                continue;
            }
            let mut target = BindTarget::default();
            match entry.kind {
                Kind::Buffer => {
                    target.buffer = Some(buffers);
                    buffers += 1;
                }
                Kind::Texture => {
                    target.texture = Some(textures);
                    textures += 1;
                }
                Kind::Sampler => {
                    target.sampler = Some(BindSamplerTarget::Resource(samplers));
                    samplers += 1;
                }
            }
            resources.insert(
                naga::ResourceBinding {
                    group: group as u32,
                    binding: entry.binding,
                },
                target,
            );
        }
    }
    naga::back::msl::EntryPointResources {
        resources,
        immediates_buffer: None,
        sizes_buffer: Some(buffers),
    }
}

fn compile(source: &str, capabilities: naga::valid::Capabilities, out_dir: &Path, name: &str) {
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{name}: {}", error.emit_to_string(source)));
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), capabilities)
        .validate(&module)
        .unwrap_or_else(|error| panic!("{name}: {error:?}"));

    let per_entry_point_map = module
        .entry_points
        .iter()
        .map(|ep| (ep.name.clone(), slots(&layout_of(&ep.name), ep.stage)))
        .collect();
    // A passthrough shader gets no buffer of runtime array sizes from wgpu, so these shaders are
    // written without the bounds checks that read one; GPUI indexes its instance arrays only by
    // `instance_index`, within the draw's count.
    let unchecked = naga::proc::BoundsCheckPolicy::Unchecked;
    let options = naga::back::msl::Options {
        lang_version: (2, 4),
        per_entry_point_map,
        fake_missing_bindings: false,
        bounds_check_policies: naga::proc::BoundsCheckPolicies {
            index: unchecked,
            buffer: unchecked,
            image_load: unchecked,
            binding_array: unchecked,
        },
        force_loop_bounding: true,
        ..Default::default()
    };
    let pipeline_options = naga::back::msl::PipelineOptions {
        entry_point: None,
        allow_and_force_point_size: false,
        vertex_pulling_transform: true,
        vertex_buffer_mappings: Vec::new(),
        binding_array_length_map: Default::default(),
    };
    let (msl, translation) =
        naga::back::msl::write_string(&module, &info, &options, &pipeline_options)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
    for (ep, translated) in module.entry_points.iter().zip(&translation.entry_point_names) {
        let translated = translated.as_ref().unwrap();
        assert_eq!(
            &ep.name, translated,
            "{name}: wgpu finds a passthrough entry point by its WGSL name"
        );
    }
    // Naga threads a buffer of runtime array sizes through every function that touches an
    // instance array, but without bounds checks never reads it, so Metal leaves the argument
    // inactive and wgpu's not binding it is sound.
    assert!(
        !msl.contains("_buffer_sizes."),
        "{name}: the MSL reads a sizes buffer, which wgpu does not bind for a passthrough shader"
    );

    // As gpui_apple's build does: compile a copy in OUT_DIR, from OUT_DIR, so the metallib
    // records no checkout path.
    let metal = out_dir.join(format!("{name}.metal"));
    let air = out_dir.join(format!("{name}.air"));
    let metallib = out_dir.join(format!("{name}.metallib"));
    std::fs::write(&metal, msl).unwrap();
    let sdk = env::var("SDKROOT").unwrap_or_else(|_| "macosx".to_string());
    run(Command::new("xcrun")
        .current_dir(out_dir)
        .args(["-sdk", &sdk, "metal", "-std=macos-metal2.4", "-mmacosx-version-min=12.0", "-c"])
        .arg(&metal)
        .arg("-o")
        .arg(&air));
    run(Command::new("xcrun")
        .current_dir(out_dir)
        .args(["-sdk", &sdk, "metallib"])
        .arg(&air)
        .arg("-o")
        .arg(&metallib));
    std::fs::remove_file(&air).unwrap();
}

fn run(command: &mut Command) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{command:?}: {error}"));
    if !output.status.success() {
        println!(
            "cargo::error={command:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::process::exit(1);
    }
}
