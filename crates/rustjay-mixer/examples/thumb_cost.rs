//! What do the per-layer preview thumbnails actually cost?
//!
//! `Thumbnails::update` blits every layer's output into a 160x90 texture once
//! per frame, so the previews scale with layer count. The pixel work is
//! trivially small — 160x90 is well under a percent of a 1080p layer — but each
//! blit is its own render pass and allocates a bind group, and that per-layer
//! overhead is the part worth knowing before deciding whether previews should
//! be switchable.
//!
//! Times N blits per frame against a full-resolution source, the same shape
//! `Thumbnails::update` does, and prints one line per layer count.
//!
//! Run: cargo run --release -p rustjay-mixer --example thumb_cost

use std::sync::Arc;
use std::time::Instant;

use rustjay_mixer::blit::BlitPipeline;

const SRC_W: u32 = 1920;
const SRC_H: u32 = 1080;
const THUMB_W: u32 = 160;
const THUMB_H: u32 = 90;
const WARMUP: u32 = 20;
const FRAMES: u32 = 200;

fn main() {
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("no adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("no device")
    });

    let target = |w: u32, h: u32, label: &str| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    };

    // One shared source stands in for a layer's output: the blit samples it,
    // so what matters is its size, not what is in it.
    let source = target(SRC_W, SRC_H, "source");
    let pipeline = BlitPipeline::new(&device, wgpu::TextureFormat::Bgra8Unorm);
    let quad = wgpu::util::DeviceExt::create_buffer_init(
        &device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("quad"),
            contents: bytemuck::cast_slice(&rustjay_core::Vertex::quad_vertices()),
            usage: wgpu::BufferUsages::VERTEX,
        },
    );

    println!("blit {SRC_W}x{SRC_H} -> {THUMB_W}x{THUMB_H}, {FRAMES} frames each\n");
    println!(
        "{:>6}  {:>10}  {:>12}  {:>16}",
        "layers", "ms/frame", "us/layer", "% of 60fps frame"
    );

    for layers in [1usize, 2, 4, 8, 12, 16] {
        let thumbs: Vec<wgpu::TextureView> = (0..layers)
            .map(|i| target(THUMB_W, THUMB_H, &format!("thumb {i}")))
            .collect();

        let run = |frames: u32| {
            for _ in 0..frames {
                let mut encoder =
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                for view in &thumbs {
                    pipeline.blit(&device, &mut encoder, &source, view, &quad);
                }
                queue.submit(std::iter::once(encoder.finish()));
                // Block until the GPU is done, or this times command recording
                // rather than the work itself.
                let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let flag = Arc::clone(&done);
                queue.on_submitted_work_done(move || {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                });
                while !done.load(std::sync::atomic::Ordering::SeqCst) {
                    device.poll(wgpu::PollType::Poll).ok();
                    std::thread::yield_now();
                }
            }
        };

        run(WARMUP);
        let start = Instant::now();
        run(FRAMES);
        let ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES);
        println!(
            "{layers:>6}  {ms:>10.3}  {:>12.1}  {:>15.2}%",
            ms * 1000.0 / layers as f64,
            ms / 16.667 * 100.0
        );
    }
}
