//! One composite slot, two different source textures.
//!
//! The deck slot is fed by deck A's output, deck B's output, or the transition
//! pass, depending only on where the crossfader sits — the slot index never
//! changes, and neither does the mixer's generation counter. A bind-group cache
//! keyed on the slot alone therefore kept drawing whichever texture it was first
//! handed, which is what "the crossfader does nothing" looked like on screen.
//!
//! GPU test: opt in with `RUSTJAY_GPU_TESTS=1`, like the ISF pixel tests.

use rustjay_core::Vertex;
use rustjay_mixer::{BlendMode, CompositePipeline, KeyParams};
use rustjay_render::Texture;

const SIZE: u32 = 64;
const ROW: u32 = 256; // copy_texture_to_buffer wants a 256-byte aligned row

fn gpu_enabled() -> bool {
    std::env::var("RUSTJAY_GPU_TESTS").as_deref() == Ok("1")
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    quad_vb: wgpu::Buffer,
}

fn init_gpu() -> Option<Gpu> {
    if !gpu_enabled() {
        eprintln!("RUSTJAY_GPU_TESTS != 1 — skipping composite cache GPU test");
        return None;
    }
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .expect("no wgpu adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                label: Some("Composite Cache Test Device"),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .expect("no wgpu device")
    });
    let quad_vb = wgpu::util::DeviceExt::create_buffer_init(
        &device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("Composite Cache Quad VB"),
            contents: bytemuck::cast_slice(&Vertex::quad_vertices()),
            usage: wgpu::BufferUsages::VERTEX,
        },
    );
    Some(Gpu {
        device,
        queue,
        quad_vb,
    })
}

/// A solid, fully opaque source texture. The same byte in all three colour
/// channels, so the test says nothing about BGRA/RGBA ordering.
fn solid(gpu: &Gpu, label: &str, level: u8) -> Texture {
    let tex = Texture::create_render_target(&gpu.device, SIZE, SIZE, label);
    let px: Vec<u8> = [level, level, level, 0xFF]
        .iter()
        .copied()
        .cycle()
        .take((SIZE * SIZE * 4) as usize)
        .collect();
    tex.update(&gpu.queue, &px);
    tex
}

/// Blend `source` over `dest` into `out` at `slot`, then read `out`'s first
/// pixel back.
#[allow(clippy::too_many_arguments)]
fn blend_and_read(
    gpu: &Gpu,
    composite: &CompositePipeline,
    generation: u64,
    slot: usize,
    source: &Texture,
    dest: &Texture,
    out: &Texture,
) -> [u8; 4] {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    composite.blend(
        &gpu.device,
        &gpu.queue,
        &mut encoder,
        generation,
        slot,
        true,
        source,
        &dest.view,
        &out.view,
        1.0,
        BlendMode::Normal,
        KeyParams::default(),
        &gpu.quad_vb,
    );
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (ROW * SIZE) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &out.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ROW),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&done);
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |res| {
        res.expect("map_async");
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    while !done.load(std::sync::atomic::Ordering::SeqCst) {
        gpu.device.poll(wgpu::PollType::Poll).ok();
        std::thread::yield_now();
    }
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("buffer mapped by map_async");
    [data[0], data[1], data[2], data[3]]
}

/// Two different textures through one slot, at one generation, must not share a
/// bind group: the second blend has to show the second texture.
#[test]
fn a_slot_fed_a_second_texture_stops_drawing_the_first() {
    let Some(gpu) = init_gpu() else { return };
    let composite = CompositePipeline::new(&gpu.device, rustjay_core::working_format());

    let dark = solid(&gpu, "dark source", 0x20);
    let bright = solid(&gpu, "bright source", 0xC0);
    let dest = solid(&gpu, "dest", 0x00);
    let out = Texture::create_render_target(&gpu.device, SIZE, SIZE, "out");

    let first = blend_and_read(&gpu, &composite, 7, 3, &dark, &dest, &out);
    assert_eq!(
        first[0], 0x20,
        "first blend should show the dark source, got {first:?}"
    );

    // Same slot, same generation — only the source texture differs, exactly as
    // the deck slot does when the crossfader leaves an end.
    let second = blend_and_read(&gpu, &composite, 7, 3, &bright, &dest, &out);
    assert_eq!(
        second[0], 0xC0,
        "second blend still shows the first source: the bind-group cache served \
         a stale texture, got {second:?}"
    );
}
