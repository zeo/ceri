// frames reach the screen through a DirectComposition swapchain: the software canvas is
// uploaded into a premultiplied BGRA back buffer and presented on the display's vsync

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2,
    IDXGISwapChain1,
};
use windows::core::Interface;

const BUFFER_COUNT: u32 = 2;

fn failed(step: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("{step}: {error}")
}

pub(crate) struct Presenter {
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    // the composition tree keeps the swapchain on the window while these live
    _composition: (
        IDCompositionDevice,
        IDCompositionTarget,
        IDCompositionVisual,
    ),
    size: (u32, u32),
    // the canvas swizzled to BGRA, kept to avoid an allocation per frame
    staging: Vec<u8>,
}

impl Presenter {
    pub(crate) fn new(hwnd: isize, width: u32, height: u32) -> Result<Self, String> {
        let (width, height) = (width.max(1), height.max(1));
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: the out pointers refer to live locals for the duration of the call
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
            )
        }
        .map_err(failed("the Direct3D device could not be created"))?;
        let device = device.ok_or("Direct3D returned no device")?;
        let context = context.ok_or("Direct3D returned no device context")?;
        // SAFETY: plain COM calls on interfaces this function owns
        unsafe {
            let dxgi: IDXGIDevice = device.cast().map_err(failed("no DXGI device"))?;
            let factory: IDXGIFactory2 = dxgi
                .GetAdapter()
                .and_then(|adapter| adapter.GetParent())
                .map_err(failed("no DXGI factory"))?;
            let description = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: BUFFER_COUNT,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swapchain = factory
                .CreateSwapChainForComposition(&device, &description, None)
                .map_err(failed("the composition swapchain could not be created"))?;
            let composition: IDCompositionDevice =
                DCompositionCreateDevice(&dxgi).map_err(failed("no composition device"))?;
            let target = composition
                .CreateTargetForHwnd(HWND(hwnd as *mut _), true)
                .map_err(failed("the window refused a composition target"))?;
            let visual = composition
                .CreateVisual()
                .map_err(failed("no composition visual"))?;
            visual
                .SetContent(&swapchain)
                .map_err(failed("the swapchain could not be shown"))?;
            target
                .SetRoot(&visual)
                .map_err(failed("the visual could not be attached"))?;
            composition
                .Commit()
                .map_err(failed("the composition could not be committed"))?;
            Ok(Self {
                context,
                swapchain,
                _composition: (composition, target, visual),
                size: (width, height),
                staging: Vec::new(),
            })
        }
    }

    // uploads premultiplied RGBA pixels and presents them on the next vsync
    pub(crate) fn present(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        surface: (u32, u32),
    ) -> Result<(), String> {
        let surface = (surface.0.max(1), surface.1.max(1));
        if surface != self.size {
            // SAFETY: no back buffer reference is held across the resize
            unsafe {
                self.swapchain.ResizeBuffers(
                    BUFFER_COUNT,
                    surface.0,
                    surface.1,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
            }
            .map_err(|error| format!("the swapchain could not resize: {error}"))?;
            self.size = surface;
        }
        if width == 0 || height == 0 || rgba.len() < width as usize * height as usize * 4 {
            return Err("the frame has no pixels to present".to_string());
        }
        // a canvas a pixel short of the surface after rounding repeats its edge, since flip
        // model back buffers start undefined
        let (columns, rows) = surface;
        let row_bytes = columns as usize * 4;
        self.staging.clear();
        self.staging.resize(row_bytes * rows as usize, 0);
        for (row, target) in self.staging.chunks_exact_mut(row_bytes).enumerate() {
            let source_row = row.min(height as usize - 1) * width as usize * 4;
            let source = &rgba[source_row..source_row + width as usize * 4];
            for (column, to) in target.chunks_exact_mut(4).enumerate() {
                let from = &source[column.min(width as usize - 1) * 4..][..4];
                to.copy_from_slice(&[from[2], from[1], from[0], from[3]]);
            }
        }
        let region = windows::Win32::Graphics::Direct3D11::D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: columns,
            bottom: rows,
            back: 1,
        };
        // SAFETY: the staging buffer holds rows * row_bytes bytes for the boxed region
        unsafe {
            let buffer: ID3D11Texture2D = self
                .swapchain
                .GetBuffer(0)
                .map_err(|error| format!("no back buffer: {error}"))?;
            self.context.UpdateSubresource(
                &buffer,
                0,
                Some(&region),
                self.staging.as_ptr().cast(),
                row_bytes as u32,
                0,
            );
            self.swapchain
                .Present(1, DXGI_PRESENT(0))
                .ok()
                .map_err(|error| format!("the frame could not be presented: {error}"))
        }
    }
}
