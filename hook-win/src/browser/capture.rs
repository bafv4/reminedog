//! Copies what the browser draws into [`super::FRAME`].
//!
//! The WebView draws into a Windows.UI.Composition visual that is in no window. Windows'
//! screen capture (`GraphicsCaptureItem::CreateFromVisual`) hands each new picture of that
//! visual over as a Direct3D 11 texture, which is copied to a staging texture and from there
//! into memory, where the game's thread picks it up.

use std::time::Instant;

use windows::Foundation::TimeSpan;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, IDXGIDevice,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows_core::Interface;
use windows_numerics::Vector2;

use super::{FRAME, WM_APP_FRAME, frame_changed, lock, post};

/// The shortest time between two pictures (a 60 Hz cap, in 100 ns units). Windows before
/// 11 24H2 ignores it and hands over a picture on every refresh of the screen.
const MIN_UPDATE_INTERVAL: TimeSpan = TimeSpan { Duration: 166_667 };
/// Times the Direct3D device is made again after the GPU removed or reset it (a driver
/// update, a GPU switch) before the capture gives up.
const MAX_DEVICE_RESETS: u32 = 3;

pub(super) struct Capture {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    _compositor: Compositor,
    /// The visual the WebView draws into.
    pub(super) root: ContainerVisual,
    session: Option<Session>,
    staging: Option<Staging>,
    /// The picture being copied out of the staging texture; swapped with [`FRAME`]'s, so the
    /// game's thread never waits for the copy.
    back: Vec<u8>,
    /// Which browser the pictures belong to ([`super::FrameSlot::generation`]).
    generation: u64,
    /// The thread's window, woken with [`WM_APP_FRAME`] when a picture arrives.
    window: usize,
    frames: u64,
    started: Option<Instant>,
    /// The size captured at ([`Capture::start`]).
    size: Option<[u32; 2]>,
    device_resets: u32,
}

struct Session {
    // Kept alive while capturing.
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// A hardware Direct3D 11 device for the capture, as WinRT wants it too.
fn create_device() -> windows_core::Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: plain out parameters.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let (Some(device), Some(context)) = (device, context) else {
        return Err(windows_core::Error::from_hresult(
            windows::Win32::Foundation::E_POINTER,
        ));
    };
    let dxgi: IDXGIDevice = device.cast()?;
    // SAFETY: a valid DXGI device.
    let winrt_device: IDirect3DDevice =
        unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?.cast()?;
    Ok((device, context, winrt_device))
}

/// Whether `error` says the GPU removed or reset the device.
fn is_device_lost(error: &windows_core::Error) -> bool {
    error.code() == DXGI_ERROR_DEVICE_REMOVED || error.code() == DXGI_ERROR_DEVICE_RESET
}

struct Staging {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl Capture {
    /// Needs a DispatcherQueue on this thread (the compositor and the frame pool use it).
    pub(super) fn new(generation: u64, window: HWND) -> windows_core::Result<Self> {
        let (device, context, winrt_device) = create_device()?;
        let compositor = Compositor::new()?;
        let root = compositor.CreateContainerVisual()?;
        Ok(Self {
            device,
            context,
            winrt_device,
            _compositor: compositor,
            root,
            session: None,
            staging: None,
            back: Vec::new(),
            generation,
            window: window.0 as usize,
            frames: 0,
            started: None,
            size: None,
            device_resets: 0,
        })
    }

    /// Sizes the visual (in pixels) and captures it at that size from now on.
    pub(super) fn start(&mut self, size: [u32; 2]) -> windows_core::Result<()> {
        self.stop();
        self.size = Some(size);
        let [width, height] = size.map(|v| v.max(1));
        self.root
            .SetSize(Vector2::new(width as f32, height as f32))?;
        let item = GraphicsCaptureItem::CreateFromVisual(&self.root.cast::<Visual>()?)?;
        let pool = Direct3D11CaptureFramePool::Create(
            &self.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            SizeInt32 {
                Width: width as i32,
                Height: height as i32,
            },
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Neither is in every version of Windows; the defaults only cost a yellow frame
        // around the picture (it is not on the screen) and the cursor (not over the visual).
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetMinUpdateInterval(MIN_UPDATE_INTERVAL);
        let window = self.window;
        // Raised on this thread (the pool was made with its DispatcherQueue); the message
        // lets the loop pick the picture up with the capture at hand.
        pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            post(window, WM_APP_FRAME);
            Ok(())
        }))?;
        session.StartCapture()?;
        self.session = Some(Session {
            _item: item,
            pool,
            session,
        });
        self.started.get_or_insert_with(Instant::now);
        log::debug!("browser: capturing {width}x{height}");
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        self.session = None;
        self.size = None;
    }

    /// Copies the newest picture into [`FRAME`] (older ones still waiting are dropped).
    pub(super) fn on_frame(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let mut newest: Option<Direct3D11CaptureFrame> = None;
        while let Ok(frame) = session.pool.TryGetNextFrame() {
            if let Some(older) = newest.replace(frame) {
                let _ = older.Close();
            }
        }
        let Some(frame) = newest else {
            return;
        };
        let copied = self.copy(&frame);
        let _ = frame.Close();
        if let Err(e) = copied {
            if is_device_lost(&e) {
                self.reset_device(&e);
            } else {
                log::debug!("browser: copying a picture failed: {e}");
            }
        }
    }

    /// The GPU removed or reset the device (a driver update, a GPU switch): nothing is copied
    /// with it again. A new device takes over, and the capture starts again on it.
    fn reset_device(&mut self, error: &windows_core::Error) {
        let Some(size) = self.size else {
            return;
        };
        self.session = None;
        self.staging = None;
        if self.device_resets >= MAX_DEVICE_RESETS {
            log::warn!("browser: the Direct3D device was lost again ({error}); no more pictures");
            return;
        }
        self.device_resets += 1;
        log::warn!("browser: the Direct3D device was lost ({error}); making a new one");
        let restarted = create_device().and_then(|(device, context, winrt_device)| {
            self.device = device;
            self.context = context;
            self.winrt_device = winrt_device;
            self.start(size)
        });
        if let Err(e) = restarted {
            log::warn!("browser: capturing again failed: {e}");
        }
    }

    fn copy(&mut self, frame: &Direct3D11CaptureFrame) -> windows_core::Result<()> {
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the surface is a D3D11 texture made on our device.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a valid out parameter.
        unsafe { texture.GetDesc(&mut desc) };
        // While the size changes, the content can be smaller than the pool's textures.
        let content = frame.ContentSize()?;
        let width = (content.Width.max(0) as u32).min(desc.Width);
        let height = (content.Height.max(0) as u32).min(desc.Height);
        if width == 0 || height == 0 {
            return Ok(());
        }
        let staging = self.staging(&desc)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures are on our device, same size and format; the staging texture
        // is CPU-readable and only this thread touches the context.
        unsafe {
            self.context.CopyResource(&staging, &texture);
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let pitch = mapped.RowPitch as usize;
        let row = width as usize * 4;
        self.back.resize(row * height as usize, 0);
        for (y, dst) in self.back.chunks_exact_mut(row).enumerate() {
            // SAFETY: the mapping holds `RowPitch` bytes for each of the texture's rows, and a
            // row of `width` pixels fits in one.
            let src = unsafe {
                std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(y * pitch), row)
            };
            dst.copy_from_slice(src);
        }
        // SAFETY: mapped above.
        unsafe { self.context.Unmap(&staging, 0) };
        {
            let mut slot = lock(&FRAME);
            if slot.generation == self.generation {
                std::mem::swap(&mut slot.bgra, &mut self.back);
                slot.size = [width, height];
                slot.seq += 1;
                frame_changed();
            }
        }
        self.frames += 1;
        if self.frames == 1 {
            let ms = self.started.map_or(0, |t| t.elapsed().as_millis());
            log::info!("browser: first picture {width}x{height} after {ms} ms");
        }
        Ok(())
    }

    /// The staging texture for pictures like `desc`, made again when the size changes.
    fn staging(&mut self, desc: &D3D11_TEXTURE2D_DESC) -> windows_core::Result<ID3D11Texture2D> {
        if let Some(staging) = &self.staging
            && staging.width == desc.Width
            && staging.height == desc.Height
        {
            return Ok(staging.texture.clone());
        }
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: desc.SampleDesc,
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: a valid description and out parameter.
        unsafe {
            self.device
                .CreateTexture2D(&staging_desc, None, Some(&mut texture))?;
        }
        let texture = texture.ok_or_else(|| {
            windows_core::Error::from_hresult(windows::Win32::Foundation::E_POINTER)
        })?;
        self.staging = Some(Staging {
            texture: texture.clone(),
            width: desc.Width,
            height: desc.Height,
        });
        Ok(texture)
    }
}
