//! # Spout Input (Windows)
//!
//! GPU texture sharing input via Spout2 (DirectX shared surfaces).
//! This is the Windows equivalent of Syphon input.
//!
//! ## Implementation
//!
//! Spout senders register themselves in two Windows named shared-memory mappings:
//!   - `"SpoutSenderNames"` — flat array of `char[256]` name slots (no header)
//!   - `"<sender_name>"`    — per-sender `SharedTextureInfo` (280 bytes)
//!
//! The per-sender info contains the DXGI `GetSharedHandle` value (stored as
//! 32-bit via `HandleToLong`). We open the shared D3D11 texture with
//! `ID3D11Device::OpenSharedResource`, then either:
//!
//! - **GPU path** ([`SpoutInputReceiver::receive_gpu`], Vulkan): copy it on
//!   the GPU into a texture Vulkan imported, and sample that. No readback.
//! - **CPU path** ([`SpoutInputReceiver::try_receive_texture`]): copy to a
//!   staging texture, map it, and read BGRA pixels for the caller to upload.

#![cfg(target_os = "windows")]

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Query, ID3D11Texture2D,
    D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_QUERY_DESC,
    D3D11_QUERY_EVENT, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
    DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIKeyedMutex, IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE,
};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingA, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
    MEMORY_BASIC_INFORMATION,
};

// ---------------------------------------------------------------------------
// Spout2 shared-memory layout constants
// ---------------------------------------------------------------------------

/// Max bytes per sender name (including null terminator).
const SPOUT_MAX_NAME_LEN: usize = 256;

/// Default max senders (Spout2 reads this from the registry, fallback = 64).
const SPOUT_MAX_SENDERS: usize = 64;

/// Per-sender info struct — matches Spout2 SDK `SharedTextureInfo`.
///
/// ```text
/// offset  0: shareHandle  (u32)  — DXGI handle via HandleToLong()
/// offset  4: width        (u32)
/// offset  8: height       (u32)
/// offset 12: format       (u32)  — DXGI_FORMAT enum value
/// offset 16: usage        (u32)  — adapter index / usage
/// offset 20: description  [u8; 256] — sender description / exe path
/// offset 276: partnerId   (u32)
/// total: 280 bytes
/// ```
#[repr(C)]
struct SharedTextureInfo {
    share_handle: u32,
    width: u32,
    height: u32,
    format: u32,
    usage: u32,
    description: [u8; 256],
    partner_id: u32,
}

/// Information about an available Spout sender
#[derive(Debug, Clone)]
pub struct SpoutSenderInfo {
    /// Sender name as registered in `SpoutSenderNames` shared memory
    pub name: String,
    /// Width of the shared texture (0 = unavailable)
    pub width: u32,
    /// Height of the shared texture (0 = unavailable)
    pub height: u32,
}

/// Discovers active Spout senders on this machine by reading the
/// `SpoutSenderNames` Windows named shared-memory mapping.
pub struct SpoutDiscovery;

impl SpoutDiscovery {
    /// Return a list of all active Spout senders.
    ///
    /// The `SpoutSenderNames` map is a flat array of `char[256]` name slots
    /// with **no count header**. An empty (first byte == 0) slot marks the
    /// end of the list.
    pub fn list_senders() -> Vec<SpoutSenderInfo> {
        unsafe {
            let map_name = windows::core::s!("SpoutSenderNames");
            let Ok(hmap) = OpenFileMappingA(FILE_MAP_READ.0, false, map_name) else {
                // Mapping doesn't exist → no active senders
                return Vec::new();
            };

            let view = MapViewOfFile(hmap, FILE_MAP_READ, 0, 0, 0);
            if view.Value.is_null() {
                CloseHandle(hmap).ok();
                return Vec::new();
            }

            // Determine the actual mapped region size via VirtualQuery so we
            // never read past the end of the mapping.
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            let mbi_size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
            let queried = VirtualQuery(Some(view.Value), &mut mbi, mbi_size);
            let mapped_size: usize = if queried == mbi_size {
                mbi.RegionSize
            } else {
                SPOUT_MAX_SENDERS * SPOUT_MAX_NAME_LEN // conservative fallback
            };

            let base = view.Value as *const u8;
            let max_slots = (mapped_size / SPOUT_MAX_NAME_LEN).min(SPOUT_MAX_SENDERS);

            let mut senders = Vec::new();
            for i in 0..max_slots {
                let slot_offset = i * SPOUT_MAX_NAME_LEN;
                if slot_offset + SPOUT_MAX_NAME_LEN > mapped_size {
                    break;
                }
                let slot = std::slice::from_raw_parts(base.add(slot_offset), SPOUT_MAX_NAME_LEN);
                // First byte == 0 means end of list
                if slot[0] == 0 {
                    break;
                }
                let null_pos = slot
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(SPOUT_MAX_NAME_LEN);
                let name = String::from_utf8_lossy(&slot[..null_pos]).into_owned();
                if name.is_empty() {
                    continue;
                }
                let (width, height) = read_sender_dimensions(&name);
                log::debug!("[Spout]   sender[{}]: '{}' {}x{}", i, name, width, height);
                senders.push(SpoutSenderInfo {
                    name,
                    width,
                    height,
                });
            }

            log::debug!(
                "[Spout] Discovery: {} sender(s) in SpoutSenderNames (mapped={}B, max_slots={})",
                senders.len(),
                mapped_size,
                max_slots,
            );

            UnmapViewOfFile(view).ok();
            CloseHandle(hmap).ok();
            senders
        }
    }
}

/// Read width/height from the per-sender named shared memory block.
unsafe fn read_sender_dimensions(name: &str) -> (u32, u32) {
    let Ok(cname) = std::ffi::CString::new(name) else {
        return (0, 0);
    };
    let Ok(hmap) = OpenFileMappingA(
        FILE_MAP_READ.0,
        false,
        windows::core::PCSTR(cname.as_ptr() as *const u8),
    ) else {
        return (0, 0);
    };

    let view = MapViewOfFile(hmap, FILE_MAP_READ, 0, 0, 0);
    let result = if !view.Value.is_null() {
        // Validate mapped region is large enough before dereferencing
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let mbi_size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
        let queried = VirtualQuery(Some(view.Value), &mut mbi, mbi_size);
        let mapped_size: usize = if queried == mbi_size {
            mbi.RegionSize
        } else {
            0
        };
        if mapped_size >= std::mem::size_of::<SharedTextureInfo>() {
            let info = &*(view.Value as *const SharedTextureInfo);
            let dims = (info.width, info.height);
            UnmapViewOfFile(view).ok();
            dims
        } else {
            log::warn!(
                "[Spout] Sender '{}' shared memory too small ({} < {})",
                name,
                mapped_size,
                std::mem::size_of::<SharedTextureInfo>()
            );
            UnmapViewOfFile(view).ok();
            (0, 0)
        }
    } else {
        (0, 0)
    };
    CloseHandle(hmap).ok();
    result
}

/// Read share handle and dimensions from a sender's named shared-memory block.
unsafe fn read_sender_info(name: &str) -> anyhow::Result<(HANDLE, u32, u32)> {
    let cname = std::ffi::CString::new(name)?;
    let hmap = OpenFileMappingA(
        FILE_MAP_READ.0,
        false,
        windows::core::PCSTR(cname.as_ptr() as *const u8),
    )
    .map_err(|_| anyhow::anyhow!("[Spout] sender '{}' not in shared memory", name))?;

    let view = MapViewOfFile(hmap, FILE_MAP_READ, 0, 0, 0);
    if view.Value.is_null() {
        CloseHandle(hmap).ok();
        return Err(anyhow::anyhow!(
            "[Spout] MapViewOfFile failed for sender '{}'",
            name
        ));
    }

    // Validate mapped region is large enough before dereferencing
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    let mbi_size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    let queried = VirtualQuery(Some(view.Value), &mut mbi, mbi_size);
    let mapped_size: usize = if queried == mbi_size {
        mbi.RegionSize
    } else {
        0
    };
    if mapped_size < std::mem::size_of::<SharedTextureInfo>() {
        UnmapViewOfFile(view).ok();
        CloseHandle(hmap).ok();
        return Err(anyhow::anyhow!(
            "[Spout] Sender '{}' shared memory too small ({} < {})",
            name,
            mapped_size,
            std::mem::size_of::<SharedTextureInfo>()
        ));
    }

    let info = &*(view.Value as *const SharedTextureInfo);
    // Spout2 stores the HANDLE as 32-bit via HandleToLong (actually HandleToULong).
    // Zero-extend to usize, then convert to handle pointer.
    let handle = HANDLE(info.share_handle as usize as *mut _);
    const MAX_SPOUT_DIM: u32 = 16384;
    let width = info.width.min(MAX_SPOUT_DIM);
    let height = info.height.min(MAX_SPOUT_DIM);

    log::debug!(
        "[Spout] Sender '{}': handle=0x{:08x}, {}x{}, fmt={}",
        name,
        info.share_handle,
        width,
        height,
        info.format
    );

    UnmapViewOfFile(view).ok();
    CloseHandle(hmap).ok();
    Ok((handle, width, height))
}

/// Slots the GPU path cycles through. One copy is in flight at a time and the
/// renderer samples the newest finished one, so a slot is only rewritten three
/// copies after it was last shown — past the engine's two frames in flight.
const BRIDGE_SLOTS: usize = 4;

/// The GPU path: Spout senders share their texture by a legacy (KMT) D3D11
/// handle, which wgpu-hal can't import. So each frame D3D11 copies it, on the
/// GPU, into one of our own textures shared by NT handle, each imported into
/// Vulkan once. That replaces the staging readback, a CPU copy and an upload.
struct GpuBridge {
    slots: Vec<BridgeSlot>,
    /// Width, height and DXGI format the slots were made for.
    key: (u32, u32, i32),
    /// Newest slot whose copy has finished: what the renderer samples.
    ready: Option<usize>,
    /// Slot with a copy in flight.
    pending: Option<usize>,
    next: usize,
}

struct BridgeSlot {
    d3d: ID3D11Texture2D,
    /// Signals when the copy into `d3d` has finished on the GPU.
    query: ID3D11Query,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The NT handle the import used; Vulkan doesn't take ownership of it.
    /// Kept as its value, not a `HANDLE`, which isn't `Send` — sources move
    /// between threads.
    handle: usize,
}

impl Drop for BridgeSlot {
    fn drop(&mut self) {
        // SAFETY: our own handle from CreateSharedHandle, closed exactly once.
        unsafe { CloseHandle(HANDLE(self.handle as *mut _)).ok() };
    }
}

/// The wgpu format for a sender's DXGI format; `None` sends it down the CPU path.
fn bridge_format(format: DXGI_FORMAT) -> Option<wgpu::TextureFormat> {
    Some(match format {
        DXGI_FORMAT_B8G8R8A8_UNORM => wgpu::TextureFormat::Bgra8Unorm,
        DXGI_FORMAT_R8G8B8A8_UNORM => wgpu::TextureFormat::Rgba8Unorm,
        DXGI_FORMAT_R16G16B16A16_FLOAT => wgpu::TextureFormat::Rgba16Float,
        DXGI_FORMAT_R10G10B10A2_UNORM => wgpu::TextureFormat::Rgb10a2Unorm,
        _ => return None,
    })
}

impl GpuBridge {
    /// # Safety
    /// `d3d` must be on the same GPU as `device`.
    unsafe fn new(
        d3d: &ID3D11Device,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
    ) -> anyhow::Result<Self> {
        let wgpu_format = bridge_format(format)
            .ok_or_else(|| anyhow::anyhow!("sender format {format:?} has no wgpu equivalent"))?;
        // SAFETY: the guard is dropped before `device` is.
        let hal = unsafe { device.as_hal::<wgpu_hal::api::Vulkan>() }
            .ok_or_else(|| anyhow::anyhow!("not a Vulkan device"))?;
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let mut slots = Vec::with_capacity(BRIDGE_SLOTS);
        for _ in 0..BRIDGE_SLOTS {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                    as u32,
            };
            let mut d3d_tex = None;
            unsafe { d3d.CreateTexture2D(&desc, None, Some(&mut d3d_tex)) }?;
            let d3d_tex =
                d3d_tex.ok_or_else(|| anyhow::anyhow!("CreateTexture2D (bridge) returned None"))?;
            let mut query = None;
            let query_desc = D3D11_QUERY_DESC {
                Query: D3D11_QUERY_EVENT,
                MiscFlags: 0,
            };
            unsafe { d3d.CreateQuery(&query_desc, Some(&mut query)) }?;
            let query = query.ok_or_else(|| anyhow::anyhow!("CreateQuery returned None"))?;
            let handle = unsafe {
                d3d_tex.cast::<IDXGIResource1>()?.CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    windows::core::PCWSTR::null(),
                )
            }?;
            let hal_desc = wgpu_hal::TextureDescriptor {
                label: Some("Spout bridge"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu_format,
                // COPY_SRC so a frame can be read back (tests, snapshots).
                usage: wgpu::wgt::TextureUses::RESOURCE | wgpu::wgt::TextureUses::COPY_SRC,
                memory_flags: wgpu_hal::MemoryFlags::empty(),
                view_formats: Vec::new(),
            };
            // SAFETY: `handle` names a live texture made to match `hal_desc`.
            let hal_texture = match unsafe { hal.texture_from_d3d11_shared_handle(handle, &hal_desc) } {
                Ok(t) => t,
                Err(e) => {
                    unsafe { CloseHandle(handle).ok() };
                    anyhow::bail!("Vulkan import failed: {e:?}");
                }
            };
            // SAFETY: made for this device just above. D3D11 writes it and
            // wgpu only samples it, so it starts (and stays) a resource.
            let texture = unsafe {
                device.create_texture_from_hal::<wgpu_hal::api::Vulkan>(
                    hal_texture,
                    &wgpu::TextureDescriptor {
                        label: Some("Spout bridge"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu_format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    },
                    wgpu::wgt::TextureUses::RESOURCE,
                )
            };
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            slots.push(BridgeSlot {
                d3d: d3d_tex,
                query,
                texture,
                view,
                handle: handle.0 as usize,
            });
        }
        Ok(Self {
            slots,
            key: (width, height, format.0),
            ready: None,
            pending: None,
            next: 0,
        })
    }

    /// Promote the copy in flight once it has landed, then start the next.
    unsafe fn step(&mut self, ctx: &ID3D11DeviceContext, shared: &ID3D11Texture2D) {
        if let Some(pending) = self.pending {
            let mut done = windows::core::BOOL(0);
            // S_FALSE (still running) is Ok as well; only `done` tells.
            let polled = unsafe {
                ctx.GetData(
                    &self.slots[pending].query,
                    Some((&raw mut done).cast()),
                    std::mem::size_of_val(&done) as u32,
                    D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                )
            };
            if polled.is_err() || !done.as_bool() {
                return;
            }
            self.ready = Some(pending);
            self.pending = None;
        }

        let slot = self.next;
        // The sender's keyed mutex, when it has one — as on the CPU path.
        let keyed = shared.cast::<IDXGIKeyedMutex>().ok();
        if let Some(k) = &keyed
            && unsafe { k.AcquireSync(0, 1000) }.is_err()
        {
            return;
        }
        unsafe { ctx.CopyResource(&self.slots[slot].d3d, shared) };
        if let Some(k) = &keyed {
            unsafe { k.ReleaseSync(0) }.ok();
        }
        unsafe {
            ctx.End(&self.slots[slot].query);
            ctx.Flush();
        }
        self.pending = Some(slot);
        self.next = (slot + 1) % self.slots.len();
    }
}

/// Receives frames from a Spout sender as CPU pixel bytes → wgpu texture.
///
/// Opens the sender's D3D11 shared texture via its DXGI handle, copies to a
/// staging texture each frame, and exposes BGRA bytes via [`take_pixels`].
pub struct SpoutInputReceiver {
    d3d_device: ID3D11Device,
    d3d_context: ID3D11DeviceContext,
    /// Name of the connected sender (None = disconnected)
    sender_name: Option<String>,
    /// The shared D3D11 texture from the sender
    shared_texture: Option<ID3D11Texture2D>,
    /// CPU-readable staging copy
    staging_texture: Option<ID3D11Texture2D>,
    /// Current resolution of the shared texture
    resolution: (u32, u32),
    /// BGRA pixel buffer filled by `try_receive_texture()`
    pixel_buffer: Vec<u8>,
    /// The GPU path, once [`Self::receive_gpu`] has set it up.
    gpu: Option<GpuBridge>,
    /// Set once the GPU path has proved impossible, so it isn't retried.
    gpu_unavailable: bool,
}

impl SpoutInputReceiver {
    /// Create an unconnected receiver and initialise the D3D11 device.
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "[Spout] SpoutInputReceiver: D3D11CreateDevice failed: {}",
                    e
                )
            })?;

            let d3d_device = device
                .ok_or_else(|| anyhow::anyhow!("[Spout] D3D11CreateDevice returned no device"))?;
            let d3d_context = context
                .ok_or_else(|| anyhow::anyhow!("[Spout] D3D11CreateDevice returned no context"))?;

            log::info!("[Spout] SpoutInputReceiver: D3D11 device created");
            Ok(Self {
                d3d_device,
                d3d_context,
                sender_name: None,
                shared_texture: None,
                staging_texture: None,
                resolution: (0, 0),
                pixel_buffer: Vec::new(),
                gpu: None,
                gpu_unavailable: false,
            })
        }
    }

    /// Connect to the named Spout sender and open its shared D3D11 texture.
    pub fn connect(&mut self, sender_name: &str) -> anyhow::Result<()> {
        self.disconnect();
        self.sender_name = Some(sender_name.to_string());
        self.open_shared_texture()?;
        log::info!("[Spout] Connected to sender: {}", sender_name);
        Ok(())
    }

    /// Disconnect from the current sender and release D3D11 resources.
    pub fn disconnect(&mut self) {
        self.shared_texture = None;
        self.staging_texture = None;
        self.resolution = (0, 0);
        self.pixel_buffer.clear();
        self.gpu = None;
        if let Some(ref name) = self.sender_name {
            log::info!("[Spout] Disconnected from '{}'", name);
        }
        self.sender_name = None;
    }

    /// Open (or re-open) the shared D3D11 texture for the connected sender.
    fn open_shared_texture(&mut self) -> anyhow::Result<()> {
        let sender_name = self
            .sender_name
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("[Spout] not connected to any sender"))?
            .to_string();

        unsafe {
            let (handle, width, height) = read_sender_info(&sender_name)?;

            if width == 0 || height == 0 {
                return Err(anyhow::anyhow!(
                    "[Spout] sender '{}' has zero dimensions",
                    sender_name
                ));
            }
            if handle.0.is_null() {
                return Err(anyhow::anyhow!(
                    "[Spout] sender '{}' has null share handle",
                    sender_name
                ));
            }

            // Open the shared texture on our D3D11 device
            let mut shared_tex: Option<ID3D11Texture2D> = None;
            self.d3d_device
                .OpenSharedResource(handle, &mut shared_tex)?;
            let shared_tex = shared_tex
                .ok_or_else(|| anyhow::anyhow!("[Spout] OpenSharedResource returned None"))?;

            // Create a CPU-readable staging texture of the same size
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            self.d3d_device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            let staging = staging.ok_or_else(|| {
                anyhow::anyhow!("[Spout] CreateTexture2D (staging) returned None")
            })?;

            log::info!(
                "[Spout] Opened shared texture {}x{} from '{}' (handle={:?})",
                width,
                height,
                sender_name,
                handle
            );

            self.shared_texture = Some(shared_tex);
            self.staging_texture = Some(staging);
            self.resolution = (width, height);
        }
        Ok(())
    }

    /// Poll for a new frame.
    ///
    /// Copies the sender's shared D3D11 texture to the staging buffer and
    /// reads the BGRA pixels. Returns `true` when new pixels are ready.
    /// Call [`take_pixels`](Self::take_pixels) to move them out.
    pub fn try_receive_texture(&mut self) -> bool {
        if self.sender_name.is_none() {
            return false;
        }

        // (Re-)open if not connected yet or sender restarted
        if self.shared_texture.is_none() {
            if let Err(e) = self.open_shared_texture() {
                log::error!("[Spout Input] Failed to open texture: {}", e);
                return false;
            }
            log::info!(
                "[Spout Input] Opened {}x{} texture from '{}'",
                self.resolution.0,
                self.resolution.1,
                self.sender_name.as_deref().unwrap_or("?")
            );
        }

        let (w, h) = self.resolution;
        if w == 0 || h == 0 {
            return false;
        }

        unsafe {
            let shared_tex = match self.shared_texture.as_ref() {
                Some(t) => t,
                None => return false,
            };
            let staging_tex = match self.staging_texture.as_ref() {
                Some(t) => t,
                None => return false,
            };

            // Copy under keyed mutex if present (sender uses key=0)
            let use_keyed_mutex = match shared_tex.cast::<IDXGIKeyedMutex>() {
                Ok(keyed_mutex) => match keyed_mutex.AcquireSync(0, 1000) {
                    Ok(_) => {
                        self.d3d_context.CopyResource(staging_tex, shared_tex);
                        self.d3d_context.Flush();
                        keyed_mutex.ReleaseSync(0).ok();
                        true
                    }
                    Err(e) => {
                        log::warn!("[Spout Input] AcquireSync failed: {:?}", e);
                        false
                    }
                },
                Err(_) => false,
            };

            if !use_keyed_mutex {
                self.d3d_context.CopyResource(staging_tex, shared_tex);
                self.d3d_context.Flush();
            }

            // Map staging texture and read BGRA bytes
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            if let Err(e) =
                self.d3d_context
                    .Map(staging_tex, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            {
                log::error!("[Spout Input] Map failed: {:?}", e);
                return false;
            }

            let needed = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(4))
                .filter(|&n| n <= 32 * 1024 * 1024) // reject > 32 MB (8K max)
                .ok_or_else(|| {
                    log::error!("[Spout Input] Dimensions out of range: {}x{}", w, h);
                })
                .unwrap_or(0);
            if needed == 0 {
                self.d3d_context.Unmap(staging_tex, 0);
                return false;
            }
            if self.pixel_buffer.len() != needed {
                self.pixel_buffer.resize(needed, 0);
            }

            let src = mapped.pData as *const u8;
            let row_pitch = mapped.RowPitch as usize;
            let dst_row_bytes = (w * 4) as usize;

            if row_pitch == dst_row_bytes {
                std::ptr::copy_nonoverlapping(src, self.pixel_buffer.as_mut_ptr(), needed);
            } else {
                for row in 0..h as usize {
                    let src_row =
                        std::slice::from_raw_parts(src.add(row * row_pitch), dst_row_bytes);
                    self.pixel_buffer[row * dst_row_bytes..(row + 1) * dst_row_bytes]
                        .copy_from_slice(src_row);
                }
            }

            self.d3d_context.Unmap(staging_tex, 0);
        }

        true
    }

    /// Receive onto `device` without leaving the GPU; see [`GpuBridge`].
    ///
    /// Returns `false` when the caller should use the CPU path
    /// ([`Self::try_receive_texture`]) instead: not a Vulkan device, no
    /// `VULKAN_EXTERNAL_MEMORY_WIN32`, or the import failed (logged once).
    /// The newest landed frame is [`Self::gpu_frame`]; it trails the sender
    /// by a frame, the price of never waiting on the copy.
    pub fn receive_gpu(&mut self, device: &wgpu::Device) -> bool {
        if self.gpu_unavailable || self.sender_name.is_none() {
            return false;
        }
        if !device
            .features()
            .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32)
        {
            log::info!(
                "[Spout] Zero-copy input needs Vulkan with external memory; using the CPU path"
            );
            self.gpu_unavailable = true;
            return false;
        }
        if self.shared_texture.is_none()
            && let Err(e) = self.open_shared_texture()
        {
            log::error!("[Spout Input] Failed to open texture: {}", e);
            return true;
        }
        let Some(shared) = self.shared_texture.clone() else {
            return true;
        };

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { shared.GetDesc(&mut desc) };
        let key = (desc.Width, desc.Height, desc.Format.0);
        if self.gpu.as_ref().is_none_or(|g| g.key != key) {
            // The old slots go before the new ones are made.
            self.gpu = None;
            // SAFETY: both devices are on the default adapter, as the
            // sender's texture must be for OpenSharedResource to work.
            match unsafe {
                GpuBridge::new(&self.d3d_device, device, desc.Width, desc.Height, desc.Format)
            } {
                Ok(bridge) => {
                    log::info!(
                        "[Spout] Zero-copy input: {}x{} {:?}",
                        desc.Width,
                        desc.Height,
                        desc.Format
                    );
                    self.gpu = Some(bridge);
                }
                Err(e) => {
                    log::warn!("[Spout] Zero-copy input unavailable ({e}); using the CPU path");
                    self.gpu_unavailable = true;
                    return false;
                }
            }
        }
        if let Some(bridge) = self.gpu.as_mut() {
            unsafe { bridge.step(&self.d3d_context, &shared) };
        }
        true
    }

    /// The newest frame [`Self::receive_gpu`] has landed, if one has yet.
    pub fn gpu_frame(&self) -> Option<(&wgpu::Texture, &wgpu::TextureView)> {
        let bridge = self.gpu.as_ref()?;
        let slot = &bridge.slots[bridge.ready?];
        Some((&slot.texture, &slot.view))
    }

    /// Move the pixel buffer out of the receiver.
    ///
    /// Returns `Some(Vec<u8>)` (BGRA, row-major) when a frame was received.
    /// Leaves the internal buffer empty until the next `try_receive_texture()`.
    pub fn take_pixels(&mut self) -> Option<Vec<u8>> {
        if self.pixel_buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pixel_buffer))
        }
    }

    /// Borrow the pixel buffer without moving it.
    ///
    /// Returns `Some(&[u8])` (BGRA, row-major) when a frame is available.
    /// The buffer is reused in-place on the next `try_receive_texture()`,
    /// avoiding a per-frame reallocation.
    pub fn pixels(&self) -> Option<&[u8]> {
        if self.pixel_buffer.is_empty() {
            None
        } else {
            Some(&self.pixel_buffer)
        }
    }

    /// The output texture — always `None` (CPU path, use `take_pixels()` instead).
    pub fn output_texture(&self) -> Option<&wgpu::Texture> {
        None
    }

    /// Current resolution of the shared texture
    pub fn resolution(&self) -> (u32, u32) {
        self.resolution
    }
}

impl Default for SpoutInputReceiver {
    fn default() -> Self {
        // This may panic if D3D11 is unavailable; prefer new() for fallible construction.
        Self::new().expect("SpoutInputReceiver::default() requires D3D11")
    }
}

impl Drop for SpoutInputReceiver {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod gpu_path_tests {
    //! The GPU path end to end on this machine's GPU. Gated like the other
    //! pixel tests: `RUSTJAY_GPU_TESTS=1`.
    use super::{SpoutDiscovery, SpoutInputReceiver};
    use crate::output::spout_output::SpoutOutput;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const TEST_SENDER: &str = "rustjay-gpu-path-test";

    fn vulkan_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        if std::env::var("RUSTJAY_GPU_TESTS").as_deref() != Ok("1") {
            eprintln!("RUSTJAY_GPU_TESTS != 1 — skipping");
            return None;
        }
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    ..Default::default()
                })
                .await
                .ok()?;
            let feature = wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32;
            if !adapter.features().contains(feature) {
                eprintln!("{} lacks {feature:?} — skipping", adapter.get_info().name);
                return None;
            }
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    required_features: feature,
                    required_limits: wgpu::Limits::default(),
                    label: Some("Spout GPU path test"),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                })
                .await
                .ok()
        })
    }

    /// The texture's pixels, rows tightly packed, 4 bytes each.
    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (tex.width(), tex.height());
        let row = w * 4;
        let padded = row.next_multiple_of(256);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(padded * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            tex.size(),
        );
        queue.submit([enc.finish()]);
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            r.expect("map_async");
            flag.store(true, Ordering::SeqCst);
        });
        while !done.load(Ordering::SeqCst) {
            device.poll(wgpu::PollType::Poll).ok();
            std::thread::yield_now();
        }
        let data = buf.slice(..).get_mapped_range().expect("mapped");
        data.chunks(padded as usize)
            .flat_map(|r| &r[..row as usize])
            .copied()
            .collect()
    }

    /// Drive `receive_gpu` until a landed frame's first pixel is `want`.
    fn first_pixel_becomes(
        rx: &mut SpoutInputReceiver,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        want: [u8; 4],
    ) -> [u8; 4] {
        let mut last = [0; 4];
        for _ in 0..200 {
            assert!(rx.receive_gpu(device), "GPU path declined to run");
            if let Some((tex, _)) = rx.gpu_frame() {
                last.copy_from_slice(&read_back(device, queue, tex)[..4]);
                if last == want {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        last
    }

    #[test]
    fn own_sender_reaches_vulkan_on_the_gpu_path() {
        let Some((device, queue)) = vulkan_device() else {
            return;
        };
        let mut sender = SpoutOutput::new(TEST_SENDER).expect("sender");
        let solid = |bgra: [u8; 4]| bgra.repeat(64 * 64);
        sender
            .submit_bytes(&solid([0, 0, 255, 255]), 64, 64)
            .expect("red frame");

        let mut rx = SpoutInputReceiver::new().expect("receiver");
        rx.connect(TEST_SENDER).expect("connect");
        let red = [0, 0, 255, 255];
        assert_eq!(first_pixel_becomes(&mut rx, &device, &queue, red), red, "red, as BGRA");

        // Later frames have to arrive too, not only the first.
        sender
            .submit_bytes(&solid([255, 0, 0, 255]), 64, 64)
            .expect("blue frame");
        let blue = [255, 0, 0, 255];
        assert_eq!(first_pixel_becomes(&mut rx, &device, &queue, blue), blue, "blue, as BGRA");
    }

    /// With a real sender running: saves what each path sees to
    /// `$SPOUT_SNAPSHOT_DIR` (opaque RGBA PNGs) and times a receive on each.
    #[test]
    fn live_sender_snapshots() {
        let Ok(dir) = std::env::var("SPOUT_SNAPSHOT_DIR") else {
            eprintln!("SPOUT_SNAPSHOT_DIR unset — skipping");
            return;
        };
        let Some((device, queue)) = vulkan_device() else {
            return;
        };
        let senders = SpoutDiscovery::list_senders();
        let live = senders
            .iter()
            .find(|s| s.name != TEST_SENDER)
            .expect("no live Spout sender");
        eprintln!("live sender '{}' {}x{}", live.name, live.width, live.height);

        let mut gpu = SpoutInputReceiver::new().unwrap();
        gpu.connect(&live.name).unwrap();
        let mut cpu = SpoutInputReceiver::new().unwrap();
        cpu.connect(&live.name).unwrap();
        for _ in 0..10 {
            gpu.receive_gpu(&device);
            cpu.try_receive_texture();
            std::thread::sleep(Duration::from_millis(16));
        }

        let t = Instant::now();
        for _ in 0..120 {
            assert!(gpu.receive_gpu(&device));
            std::thread::sleep(Duration::from_millis(1));
        }
        let gpu_ms = t.elapsed().as_secs_f64() * 1e3 / 120.0 - 1.0;
        let t = Instant::now();
        for _ in 0..120 {
            cpu.try_receive_texture();
            std::thread::sleep(Duration::from_millis(1));
        }
        let cpu_ms = t.elapsed().as_secs_f64() * 1e3 / 120.0 - 1.0;
        eprintln!(
            "per receive: GPU path ~{gpu_ms:.2} ms, CPU path ~{cpu_ms:.2} ms (before its upload)"
        );

        let opaque_rgba = |px: &mut [u8], bgra: bool| {
            for p in px.chunks_mut(4) {
                if bgra {
                    p.swap(0, 2);
                }
                p[3] = 255;
            }
        };
        let (tex, _) = gpu.gpu_frame().expect("the GPU path landed no frame");
        let format = tex.format();
        eprintln!("GPU frame: {}x{} {format:?}", tex.width(), tex.height());
        if matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm) {
            let mut px = read_back(&device, &queue, tex);
            opaque_rgba(&mut px, format == wgpu::TextureFormat::Bgra8Unorm);
            image::save_buffer(
                format!("{dir}/spout_gpu.png"),
                &px,
                tex.width(),
                tex.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        if let Some(px) = cpu.pixels() {
            let mut px = px.to_vec();
            opaque_rgba(&mut px, true);
            let (w, h) = cpu.resolution();
            image::save_buffer(format!("{dir}/spout_cpu.png"), &px, w, h, image::ColorType::Rgba8)
                .unwrap();
        }
    }
}
