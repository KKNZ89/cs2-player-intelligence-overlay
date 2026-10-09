//! One frame of a single window, through Windows Graphics Capture: the same system API screen recorders
//! use. Only that window's own pixels are captured (windows above it, such as this app's overlay, are not),
//! and nothing touches the game process.

use std::path::Path;

/// A captured frame, 4 bytes per pixel in B, G, R, A order, rows packed without padding.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Frame {
    /// (r, g, b) at a pixel; black outside the frame.
    pub fn rgb(&self, x: u32, y: u32) -> (u8, u8, u8) {
        if x >= self.width || y >= self.height {
            return (0, 0, 0);
        }
        let i = ((y * self.width + x) * 4) as usize;
        (self.bgra[i + 2], self.bgra[i + 1], self.bgra[i])
    }

    pub fn save_png(&self, file: &Path) -> Result<(), String> {
        let out = std::fs::File::create(file).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(out), self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        let rgb: Vec<u8> = self.bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        writer.write_image_data(&rgb).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
pub use native::{capture_window, find_window};

#[cfg(windows)]
mod native {
    use super::Frame;
    use std::time::{Duration, Instant};
    use windows::core::Interface;
    use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::{HMODULE, HWND, LPARAM};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsIconic, IsWindowVisible};
    use windows_core::BOOL;

    /// The first visible, non-minimised top-level window whose title starts with `prefix`.
    pub fn find_window(prefix: &str) -> Option<isize> {
        struct Search {
            prefix: String,
            found: Option<isize>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
            // SAFETY: `data` is the &mut Search passed to EnumWindows below, alive for the whole call.
            let search = unsafe { &mut *(data.0 as *mut Search) };
            let mut title = [0u16; 256];
            // SAFETY: hwnd comes from EnumWindows; the buffer is ours.
            let len = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
            let visible = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() };
            if visible && String::from_utf16_lossy(&title[..len]).starts_with(&search.prefix) {
                search.found = Some(hwnd.0 as isize);
                return BOOL(0);
            }
            BOOL(1)
        }
        let mut search = Search { prefix: prefix.to_string(), found: None };
        // SAFETY: the callback only reads window titles and writes into `search`, which outlives the call.
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
        search.found
    }

    /// Captures one frame of the window. Runs on its own thread (WinRT needs a multithreaded apartment).
    pub fn capture_window(hwnd: isize, timeout: Duration) -> Result<Frame, String> {
        std::thread::spawn(move || capture(HWND(hwnd as _), timeout)).join().map_err(|_| "The capture thread failed.".to_string())?
    }

    fn capture(hwnd: HWND, timeout: Duration) -> Result<Frame, String> {
        let e = |error: windows_core::Error| error.message();
        // SAFETY: plain API calls on objects created here; every pointer passed in is to a local.
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
            if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
                return Err("Window capture is not supported on this version of Windows.".into());
            }
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
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
            )
            .map_err(e)?;
            let (device, context) = (device.ok_or("No Direct3D device.")?, context.ok_or("No Direct3D context.")?);
            let dxgi: IDXGIDevice = device.cast().map_err(e)?;
            let d3d: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).map_err(e)?.cast().map_err(e)?;

            let interop = windows_core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(e)?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd).map_err(e)?;
            let pool =
                Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, item.Size().map_err(e)?).map_err(e)?;
            let session = pool.CreateCaptureSession(&item).map_err(e)?;
            // Not available on every Windows version; a short yellow capture border is the fallback.
            let _ = session.SetIsCursorCaptureEnabled(false);
            let _ = session.SetIsBorderRequired(false);
            session.StartCapture().map_err(e)?;

            let started = Instant::now();
            let frame = loop {
                if let Ok(frame) = pool.TryGetNextFrame() {
                    break frame;
                }
                if started.elapsed() > timeout {
                    let _ = session.Close();
                    let _ = pool.Close();
                    return Err("The window did not deliver a frame in time.".into());
                }
                std::thread::sleep(Duration::from_millis(15));
            };
            let size = frame.ContentSize().map_err(e)?;
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface().map_err(e)?.cast().map_err(e)?;
            let texture: ID3D11Texture2D = access.GetInterface().map_err(e)?;

            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.MiscFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut staging: Option<ID3D11Texture2D> = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging)).map_err(e)?;
            let staging = staging.ok_or("No staging texture.")?;
            context.CopyResource(&staging, &texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(e)?;
            let width = (size.Width.max(0) as u32).min(desc.Width);
            let height = (size.Height.max(0) as u32).min(desc.Height);
            let mut bgra = Vec::with_capacity((width * height * 4) as usize);
            for row in 0..height as usize {
                let start = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
                bgra.extend_from_slice(std::slice::from_raw_parts(start, width as usize * 4));
            }
            context.Unmap(&staging, 0);
            let _ = frame.Close();
            let _ = session.Close();
            let _ = pool.Close();
            Ok(Frame { width, height, bgra })
        }
    }
}

/// CS2's five teammate colours (radar, scoreboard and chat, `cl_teammate_colors_show 1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeammateColour {
    Yellow,
    Purple,
    Green,
    Blue,
    Orange,
}

impl TeammateColour {
    pub fn name(self) -> &'static str {
        match self {
            Self::Yellow => "yellow",
            Self::Purple => "purple",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Orange => "orange",
        }
    }
}

/// Reference colours from CS2's `panorama/styles/csgostyles.vcss`: the five `color-player-*` values, and
/// the CT and T interface tints (`color-CT`, `color-T`), which are close to blue and yellow but are not
/// teammate colours.
const REFERENCES: [(Option<TeammateColour>, [u8; 3]); 7] = [
    (Some(TeammateColour::Yellow), [248, 246, 45]),
    (Some(TeammateColour::Purple), [192, 54, 153]),
    (Some(TeammateColour::Green), [29, 162, 132]),
    (Some(TeammateColour::Blue), [136, 206, 245]),
    (Some(TeammateColour::Orange), [255, 155, 37]),
    (None, [181, 212, 238]),
    (None, [234, 209, 138]),
];

/// Chromaticity (each channel's share of the total), which stays the same when a colour is drawn darker
/// at anti-aliased edges.
fn chroma([r, g, b]: [u8; 3]) -> [f32; 3] {
    let sum = (r as f32 + g as f32 + b as f32).max(1.0);
    [r as f32 / sum, g as f32 / sum, b as f32 / sum]
}

/// Which teammate colour a pixel shows: the nearest reference by chromaticity, if it is close enough and
/// the pixel is not too dark to judge.
pub fn classify(r: u8, g: u8, b: u8) -> Option<TeammateColour> {
    if r.max(g).max(b) < 90 {
        return None;
    }
    let pixel = chroma([r, g, b]);
    let (colour, distance) = REFERENCES
        .iter()
        .map(|(colour, reference)| {
            let c = chroma(*reference);
            (*colour, ((pixel[0] - c[0]).powi(2) + (pixel[1] - c[1]).powi(2) + (pixel[2] - c[2]).powi(2)).sqrt())
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    colour.filter(|_| distance < 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_five_teammate_colours_and_ignores_greys() {
        // CS2's exact values, and darker shades of them (anti-aliased edges).
        assert_eq!(classify(248, 246, 45), Some(TeammateColour::Yellow));
        assert_eq!(classify(192, 54, 153), Some(TeammateColour::Purple));
        assert_eq!(classify(29, 162, 132), Some(TeammateColour::Green));
        assert_eq!(classify(136, 206, 245), Some(TeammateColour::Blue));
        assert_eq!(classify(255, 155, 37), Some(TeammateColour::Orange));
        assert_eq!(classify(68, 103, 122), Some(TeammateColour::Blue), "half-bright blue");
        assert_eq!(classify(124, 123, 22), Some(TeammateColour::Yellow), "half-bright yellow");
        // The CT and T interface tints are not teammate colours.
        assert_eq!(classify(181, 212, 238), None, "color-CT");
        assert_eq!(classify(234, 209, 138), None, "color-T");
        assert_eq!(classify(20, 20, 20), None, "dark background");
        assert_eq!(classify(200, 200, 200), None, "white text");
        assert_eq!(classify(90, 110, 140), None, "muted UI blue");
        assert_eq!(TeammateColour::Blue.name(), "blue");
    }

    #[test]
    fn frame_pixels_and_png() {
        let frame = Frame { width: 2, height: 1, bgra: vec![0, 230, 255, 255, 255, 140, 40, 255] };
        assert_eq!(frame.rgb(0, 0), (255, 230, 0));
        assert_eq!(frame.rgb(1, 0), (40, 140, 255));
        assert_eq!(frame.rgb(5, 5), (0, 0, 0));
        let dir = tempfile::tempdir().unwrap();
        frame.save_png(&dir.path().join("f.png")).unwrap();
        assert!(dir.path().join("f.png").metadata().unwrap().len() > 0);
    }
}
