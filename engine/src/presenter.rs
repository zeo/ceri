// frames reach the screen through a DirectComposition swapchain: the part of the software
// canvas that changed is uploaded into a texture holding the whole frame, copied into the
// premultiplied BGRA back buffer and presented on the display's vsync

use crate::paint::Revision;
use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_PRESENT_PARAMETERS, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
};
use windows::core::Interface;

const BUFFER_COUNT: u32 = 2;

fn failed(step: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("{step}: {error}")
}

pub(crate) struct Presenter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    // the composition tree keeps the swapchain on the window while these live
    _composition: (
        IDCompositionDevice,
        IDCompositionTarget,
        IDCompositionVisual,
    ),
    size: (u32, u32),
    // the whole frame at the surface size, which a back buffer copies from
    frame: Option<ID3D11Texture2D>,
    // the canvas revision the frame texture holds
    shown: Option<u64>,
    // a back buffer comes back holding the frame before the last, so the box the last present
    // changed is copied again along with the new one
    last_box: [u32; 4],
    // the uploaded box swizzled to BGRA, kept to avoid an allocation per frame
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
                device,
                context,
                swapchain,
                _composition: (composition, target, visual),
                size: (width, height),
                frame: None,
                shown: None,
                last_box: [0, 0, width, height],
                staging: Vec::new(),
            })
        }
    }

    // uploads premultiplied RGBA pixels and presents them on the next vsync; with the canvas
    // revision only the box changed since the shown one is uploaded
    pub(crate) fn present(
        &mut self,
        rgba: &[u8],
        (width, height): (u32, u32),
        surface: (u32, u32),
        revision: Option<Revision>,
    ) -> Result<(), String> {
        let surface = (surface.0.max(1), surface.1.max(1));
        let (columns, rows) = surface;
        let whole = [0, 0, columns, rows];
        if surface != self.size {
            self.frame = None;
            // SAFETY: no back buffer reference is held across the resize
            unsafe {
                self.swapchain.ResizeBuffers(
                    BUFFER_COUNT,
                    columns,
                    rows,
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
        let frame = match &self.frame {
            Some(frame) => frame.clone(),
            None => {
                let frame = self.frame_texture(surface)?;
                self.shown = None;
                self.last_box = whole;
                self.frame = Some(frame.clone());
                frame
            }
        };
        let upload = match revision {
            Some(revision) if self.shown == Some(revision.id) => return Ok(()),
            Some(Revision {
                changed: Some((base, [left, top, right, bottom])),
                ..
            }) if self.shown == Some(base) => {
                // a box reaching the canvas edge reaches the edge pixels repeated past it
                let right = if right >= width { columns } else { right };
                let bottom = if bottom >= height { rows } else { bottom };
                [left, top, right.min(columns), bottom.min(rows)]
            }
            _ => whole,
        };
        self.shown = revision.map(|revision| revision.id);
        let [left, top, right, bottom] = upload;
        if left >= right || top >= bottom {
            return Ok(());
        }
        // a canvas a pixel short of the surface after rounding repeats its edge, since flip
        // model back buffers start undefined
        let row_bytes = (right - left) as usize * 4;
        self.staging.clear();
        self.staging.resize(row_bytes * (bottom - top) as usize, 0);
        for (row, target) in (top..bottom).zip(self.staging.chunks_exact_mut(row_bytes)) {
            let source_row = row.min(height - 1) as usize * width as usize * 4;
            let source = &rgba[source_row..source_row + width as usize * 4];
            for (column, to) in (left..right).zip(target.as_chunks_mut::<4>().0.iter_mut()) {
                let from = &source[column.min(width - 1) as usize * 4..][..4];
                *to = [from[2], from[1], from[0], from[3]];
            }
        }
        let last = self.last_box;
        let copied = [
            left.min(last[0]),
            top.min(last[1]),
            right.max(last[2]),
            bottom.max(last[3]),
        ];
        self.last_box = upload;
        let region = |[left, top, right, bottom]: [u32; 4]| D3D11_BOX {
            left,
            top,
            front: 0,
            right,
            bottom,
            back: 1,
        };
        let mut dirty = RECT {
            left: copied[0] as i32,
            top: copied[1] as i32,
            right: copied[2] as i32,
            bottom: copied[3] as i32,
        };
        // SAFETY: the staging buffer holds the boxed rows at row_bytes each, both boxes lie
        // inside textures of the surface size, and the dirty rect outlives the present
        unsafe {
            self.context.UpdateSubresource(
                &frame,
                0,
                Some(&region(upload)),
                self.staging.as_ptr().cast(),
                row_bytes as u32,
                0,
            );
            let buffer: ID3D11Texture2D = self
                .swapchain
                .GetBuffer(0)
                .map_err(|error| format!("no back buffer: {error}"))?;
            self.context.CopySubresourceRegion(
                &buffer,
                0,
                copied[0],
                copied[1],
                0,
                &frame,
                0,
                Some(&region(copied)),
            );
            self.swapchain
                .Present1(
                    1,
                    DXGI_PRESENT(0),
                    &DXGI_PRESENT_PARAMETERS {
                        DirtyRectsCount: 1,
                        pDirtyRects: &mut dirty,
                        pScrollRect: std::ptr::null_mut(),
                        pScrollOffset: std::ptr::null_mut(),
                    },
                )
                .ok()
                .map_err(|error| format!("the frame could not be presented: {error}"))
        }
    }

    fn frame_texture(&self, (width, height): (u32, u32)) -> Result<ID3D11Texture2D, String> {
        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            ..Default::default()
        };
        let mut frame = None;
        // SAFETY: the description and out pointer refer to live locals for the call
        unsafe {
            self.device
                .CreateTexture2D(&description, None, Some(&mut frame))
        }
        .map_err(failed("the frame texture could not be created"))?;
        frame.ok_or_else(|| "Direct3D returned no frame texture".to_string())
    }
}
