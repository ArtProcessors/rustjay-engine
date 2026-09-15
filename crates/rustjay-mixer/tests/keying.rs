//! Keying inside a group.
//!
//! A grouped layer blends into its group's accumulator, an ungrouped one
//! straight into the master, and only the master pass used to read the
//! channel's key parameters. Every layer on a deck is grouped, so keying was
//! silently a no-op for all of them. Both paths must key the same way.
//!
//! GPU test: opt in with `RUSTJAY_GPU_TESTS=1`, like the ISF pixel tests.

use rustjay_core::{EffectInput, EffectInstance, EngineState, RenderCtx, RenderTarget, Vertex};
use rustjay_mixer::{BlitPipeline, Channel, Mixer};
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
        eprintln!("RUSTJAY_GPU_TESTS != 1 — skipping keying GPU test");
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
                label: Some("Keying Test Device"),
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
            label: Some("Keying Test Quad VB"),
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

/// A layer source that paints one opaque grey level. The same byte in all
/// three colour channels, so the test says nothing about BGRA/RGBA ordering.
struct SolidSource {
    tex: Texture,
    blit: BlitPipeline,
}

impl SolidSource {
    fn new(gpu: &Gpu, level: u8) -> Self {
        let tex = Texture::create_render_target(&gpu.device, SIZE, SIZE, "solid source");
        let px: Vec<u8> = [level, level, level, 0xFF]
            .iter()
            .copied()
            .cycle()
            .take((SIZE * SIZE * 4) as usize)
            .collect();
        tex.update(&gpu.queue, &px);
        Self {
            tex,
            blit: BlitPipeline::new(&gpu.device, rustjay_core::working_format()),
        }
    }
}

impl EffectInstance for SolidSource {
    fn render_to(
        &mut self,
        ctx: &mut RenderCtx<'_>,
        _inputs: &[EffectInput<'_>],
        target: RenderTarget<'_>,
        _engine: &EngineState,
    ) {
        self.blit
            .blit(ctx.device, ctx.encoder, &self.tex.view, target.view, ctx.vertex_buffer);
    }
}

/// Render the mixer once and read the first output pixel back.
fn render_and_read(gpu: &Gpu, mixer: &mut Mixer, engine: &EngineState) -> [u8; 4] {
    let out = Texture::create_render_target(&gpu.device, SIZE, SIZE, "out");
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut ctx = RenderCtx {
            device: &gpu.device,
            queue: &gpu.queue,
            encoder: &mut encoder,
            vertex_buffer: &gpu.quad_vb,
        };
        mixer.render_to(
            &mut ctx,
            &[],
            RenderTarget {
                view: &out.view,
                size: [SIZE, SIZE],
            },
            engine,
        );
    }
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

/// A mid-grey layer luma-keyed above a threshold it does not reach must
/// vanish, leaving the white layer beneath — whether the two sit in the
/// master stack or inside one group.
#[test]
fn a_grouped_layer_is_keyed_like_an_ungrouped_one() {
    let Some(gpu) = init_gpu() else { return };
    let engine = EngineState::new();

    for grouped in [false, true] {
        let mut mixer = Mixer::new();
        mixer.use_crossfader = false;
        let base = Channel::new("base", "base", Box::new(SolidSource::new(&gpu, 0xFF)));
        let mut top = Channel::new("top", "top", Box::new(SolidSource::new(&gpu, 0x80)));
        // Luma key: pixels darker than the threshold are cut. Grey 0x80 is
        // luma 0.5, so at 0.9 the whole layer goes.
        top.key_mode = 2;
        top.key_threshold = 0.9;
        top.key_smoothness = 0.0;
        mixer.add_channel(base).unwrap();
        mixer.add_channel(top).unwrap();
        if grouped {
            mixer
                .group_channels("g", "Group", &["base".to_string(), "top".to_string()])
                .expect("two layers make a group");
        }

        let px = render_and_read(&gpu, &mut mixer, &engine);
        assert_eq!(
            px[0], 0xFF,
            "grouped={grouped}: the keyed-out grey layer must not cover the white one, got {px:?}"
        );
    }
}
