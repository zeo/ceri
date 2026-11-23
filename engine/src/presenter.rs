// frames reach the screen through a DirectComposition swapchain: the part of the software
// canvas that changed is uploaded into a texture holding the whole frame, copied into the
// premultiplied BGRA back buffer and presented on the display's vsync; video frames go to
// swapchains of their own in visuals under the canvas, which the GPU scales into the boxes
// the canvas leaves transparent, and a frame the decoder left on the GPU is copied there
// through a shared texture without reaching memory; while the window is on a display in HDR,
// frames of HDR video reach theirs as HDR10, as Chromium shows them

use crate::image::Image;
use crate::paint::{Plane, PlaneFrame, Revision};
use crate::video::VideoSurface;
use std::sync::{Arc, Weak};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR, DCOMPOSITION_BORDER_MODE_HARD,
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE, DXGI_ALPHA_MODE_IGNORE, DXGI_ALPHA_MODE_PREMULTIPLIED,
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_PRESENT, DXGI_PRESENT_PARAMETERS, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory1,
    IDXGIFactory2, IDXGIKeyedMutex, IDXGIOutput6, IDXGISwapChain1, IDXGISwapChain3,
};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::core::Interface;

const BUFFER_COUNT: u32 = 2;
// the most bytes of upload buffer kept from one present to the next
const STAGING_KEPT: usize = 4 << 20;

fn failed(step: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("{step}: {error}")
}

pub(crate) struct Presenter {
    hwnd: isize,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    factory: IDXGIFactory2,
    swapchain: IDXGISwapChain1,
    composition: IDCompositionDevice,
    // the target keeps the tree on the window; its root holds the planes under the canvas
    _target: IDCompositionTarget,
    root: IDCompositionVisual,
    _canvas: IDCompositionVisual,
    planes: Vec<ShownPlane>,
    size: (u32, u32),
    // the whole frame at the surface size, which a back buffer copies from
    frame: Option<ID3D11Texture2D>,
    // the canvas revision the frame texture holds
    shown: Option<u64>,
    // a back buffer comes back holding the frame before the last, so the box the last present
    // changed is copied again along with the new one
    last_box: [u32; 4],
    // the page's canvas colour, swizzled, which fills the window past a frame made for a
    // smaller size
    edge: [u8; 4],
    // the uploaded box swizzled to BGRA, kept to avoid an allocation per frame
    staging: Vec<u8>,
    // decoder surfaces opened on this device, kept while the decoder's pool keeps them
    opened: Vec<(Weak<dyn VideoSurface>, ID3D11Texture2D, IDXGIKeyedMutex)>,
    // a factory that sees the displays as they are now, and whether the window's is in HDR
    outputs: IDXGIFactory1,
    display: Option<(HMONITOR, bool)>,
    // set once an HDR10 frame could not be shared or its colour space was refused, after
    // which HDR video is shown as the decoder maps it to SDR, as on an SDR display
    sdr_only: bool,
}

// a video frame's swapchain, in a visual that places and scales it inside one that clips it
// to the box it shows in
struct ShownPlane {
    clip: IDCompositionVisual,
    content: IDCompositionVisual,
    swapchain: IDXGISwapChain1,
    plane: Plane,
    frame: Arc<Image>,
    format: DXGI_FORMAT,
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
            let swapchain = swapchain_for(
                &factory,
                &device,
                (width, height),
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            )?;
            let composition: IDCompositionDevice =
                DCompositionCreateDevice(&dxgi).map_err(failed("no composition device"))?;
            let target = composition
                .CreateTargetForHwnd(HWND(hwnd as *mut _), true)
                .map_err(failed("the window refused a composition target"))?;
            let root = composition
                .CreateVisual()
                .map_err(failed("no composition visual"))?;
            let canvas = composition
                .CreateVisual()
                .map_err(failed("no composition visual"))?;
            canvas
                .SetContent(&swapchain)
                .map_err(failed("the swapchain could not be shown"))?;
            root.AddVisual(&canvas, false, None)
                .map_err(failed("the canvas visual could not be attached"))?;
            target
                .SetRoot(&root)
                .map_err(failed("the visual could not be attached"))?;
            composition
                .Commit()
                .map_err(failed("the composition could not be committed"))?;
            Ok(Self {
                hwnd,
                device,
                context,
                factory,
                swapchain,
                composition,
                _target: target,
                root,
                _canvas: canvas,
                planes: Vec::new(),
                size: (width, height),
                frame: None,
                shown: None,
                last_box: [0, 0, width, height],
                edge: [0, 0, 0, 255],
                staging: Vec::new(),
                opened: Vec::new(),
                outputs: CreateDXGIFactory1().map_err(failed("no DXGI factory"))?,
                display: None,
                sdr_only: false,
            })
        }
    }

    // uploads premultiplied RGBA pixels and presents them on the next vsync; with the canvas
    // revision only the boxes changed since the shown one are uploaded
    pub(crate) fn present(
        &mut self,
        rgba: &[u8],
        (width, height): (u32, u32),
        surface: (u32, u32),
        revision: Option<Revision>,
        planes: &[PlaneFrame],
    ) -> Result<(), String> {
        self.show_planes(planes)?;
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
        let uploads = match revision {
            Some(revision) if self.shown == Some(revision.id) => return Ok(()),
            Some(Revision {
                changed: Some((base, boxes)),
                ..
            }) if self.shown == Some(base) => boxes
                .iter()
                .map(|[left, top, right, bottom]| {
                    // a box reaching the canvas edge reaches the edge pixels repeated past it
                    let right = if right >= width { columns } else { right };
                    let bottom = if bottom >= height { rows } else { bottom };
                    [left, top, right.min(columns), bottom.min(rows)]
                })
                .filter(|[left, top, right, bottom]| left < right && top < bottom)
                .collect(),
            _ => vec![whole],
        };
        self.shown = revision.map(|revision| revision.id);
        let Some(upload) = uploads.iter().copied().reduce(|all, next| {
            [
                all[0].min(next[0]),
                all[1].min(next[1]),
                all[2].max(next[2]),
                all[3].max(next[3]),
            ]
        }) else {
            return Ok(());
        };
        let region = |[left, top, right, bottom]: [u32; 4]| D3D11_BOX {
            left,
            top,
            front: 0,
            right,
            bottom,
            back: 1,
        };
        for &[left, top, right, bottom] in &uploads {
            // a canvas a pixel short of the surface after rounding repeats its edge, since
            // flip model back buffers start undefined
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
            // SAFETY: the staging buffer holds the boxed rows at row_bytes each, and the box
            // lies inside the frame texture, which has the surface size
            unsafe {
                self.context.UpdateSubresource(
                    &frame,
                    0,
                    Some(&region([left, top, right, bottom])),
                    self.staging.as_ptr().cast(),
                    row_bytes as u32,
                    0,
                );
            }
        }
        // a whole upload leaves a buffer the size of the frame, which the boxes that follow
        // have no use for
        if self.staging.capacity() > STAGING_KEPT {
            self.staging = Vec::new();
        }
        // the back buffer is two frames old, so it takes the boxes of the last frame too
        let last = self.last_box;
        let copied = [
            upload[0].min(last[0]),
            upload[1].min(last[1]),
            upload[2].max(last[2]),
            upload[3].max(last[3]),
        ];
        self.last_box = upload;
        let mut dirty = RECT {
            left: copied[0] as i32,
            top: copied[1] as i32,
            right: copied[2] as i32,
            bottom: copied[3] as i32,
        };
        // SAFETY: the copied box lies inside textures of the surface size, and the dirty rect
        // outlives the present
        unsafe {
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

    // the colour the window takes past a frame of an earlier size: the page's canvas colour
    pub(crate) fn set_edge(&mut self, [red, green, blue]: [u8; 3]) {
        self.edge = [blue, green, red, 255];
    }

    // keeps the window covered as it resizes, before a frame of the new size is made: the
    // last frame stays where it was and the rest takes its edge colour, where the window
    // would otherwise show through blank
    pub(crate) fn extend(&mut self, surface: (u32, u32)) -> Result<(), String> {
        let surface = (surface.0.max(1), surface.1.max(1));
        let Some(old) = self.frame.clone() else {
            return Ok(());
        };
        if surface == self.size {
            return Ok(());
        }
        let kept = [0, 0, self.size.0.min(surface.0), self.size.1.min(surface.1)];
        let frame = self.frame_texture(surface)?;
        let (columns, rows) = surface;
        // SAFETY: no back buffer reference is held across the resize, every box lies inside
        // the textures it names, and the staging rows hold the boxes they are copied into
        unsafe {
            self.context.CopySubresourceRegion(
                &frame,
                0,
                0,
                0,
                0,
                &old,
                0,
                Some(&D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: kept[2],
                    bottom: kept[3],
                    back: 1,
                }),
            );
            for [left, top, right, bottom] in
                [[kept[2], 0, columns, rows], [0, kept[3], kept[2], rows]]
            {
                if left >= right || top >= bottom {
                    continue;
                }
                let row_bytes = (right - left) as usize * 4;
                self.staging.clear();
                self.staging.extend(
                    std::iter::repeat_n(self.edge, row_bytes / 4 * (bottom - top) as usize)
                        .flatten(),
                );
                self.context.UpdateSubresource(
                    &frame,
                    0,
                    Some(&D3D11_BOX {
                        left,
                        top,
                        front: 0,
                        right,
                        bottom,
                        back: 1,
                    }),
                    self.staging.as_ptr().cast(),
                    row_bytes as u32,
                    0,
                );
            }
            self.swapchain
                .ResizeBuffers(
                    BUFFER_COUNT,
                    columns,
                    rows,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
                .map_err(|error| format!("the swapchain could not resize: {error}"))?;
            let buffer: ID3D11Texture2D = self
                .swapchain
                .GetBuffer(0)
                .map_err(|error| format!("no back buffer: {error}"))?;
            self.context.CopyResource(&buffer, &frame);
            drop(buffer);
            self.swapchain
                .Present(1, DXGI_PRESENT(0))
                .ok()
                .map_err(|error| format!("the frame could not be presented: {error}"))?;
        }
        self.size = surface;
        self.frame = Some(frame);
        // the frame no longer holds any canvas revision, and both back buffers need it whole
        self.shown = None;
        self.last_box = [0, 0, columns, rows];
        if self.staging.capacity() > STAGING_KEPT {
            self.staging = Vec::new();
        }
        Ok(())
    }

    // the visuals follow the planes, each new frame is uploaded whole and presented, and the
    // tree is committed when a visual came, went or moved
    fn show_planes(&mut self, planes: &[PlaneFrame]) -> Result<(), String> {
        let surfaces = || planes.iter().filter_map(|(_, _, surface)| surface.as_ref());
        let hdr = surfaces().next().is_some() && !self.sdr_only && self.display_hdr();
        for surface in surfaces() {
            surface.show_hdr(hdr);
        }
        let mut moved = false;
        let mut kept = Vec::with_capacity(planes.len());
        for (at, shown) in std::mem::take(&mut self.planes).into_iter().enumerate() {
            if planes.get(at).is_some_and(|(plane, _, surface)| {
                plane.size == shown.plane.size && format_of(surface.as_ref(), hdr) == shown.format
            }) {
                kept.push(shown);
            } else {
                // SAFETY: the clip visual is a child of the root
                unsafe { self.root.RemoveVisual(&shown.clip) }
                    .map_err(failed("a video visual could not be removed"))?;
                moved = true;
            }
        }
        self.planes = kept;
        for (at, (plane, frame, surface)) in planes.iter().enumerate() {
            let fresh = at >= self.planes.len();
            if fresh {
                let format = format_of(surface.as_ref(), hdr);
                let shown = self.plane_visual(plane, frame.clone(), format)?;
                self.planes.push(shown);
            }
            let shown = &mut self.planes[at];
            if fresh || shown.plane != *plane {
                place(shown, plane)?;
                shown.plane = plane.clone();
                moved = true;
            }
            if fresh || !Arc::ptr_eq(&shown.frame, frame) {
                shown.frame = frame.clone();
                let (swapchain, format) = (shown.swapchain.clone(), shown.format);
                match surface {
                    Some(surface) => self.copy_surface(&swapchain, format, surface, plane.size)?,
                    None => upload(&self.context, &swapchain, frame)?,
                }
            }
        }
        if moved {
            // SAFETY: a plain COM call on the device this presenter owns
            unsafe { self.composition.Commit() }
                .map_err(failed("the composition could not be committed"))?;
        }
        Ok(())
    }

    // whether the display holding most of the window is in HDR, looked up again when the
    // window reaches another display or DXGI says the displays changed
    fn display_hdr(&mut self) -> bool {
        // SAFETY: plain calls on the window this presenter draws into and a factory it owns
        unsafe {
            let monitor = MonitorFromWindow(HWND(self.hwnd as *mut _), MONITOR_DEFAULTTONEAREST);
            if !self.outputs.IsCurrent().as_bool() {
                let Ok(outputs) = CreateDXGIFactory1() else {
                    return false;
                };
                self.outputs = outputs;
                self.display = None;
            }
            if let Some((known, hdr)) = self.display
                && known == monitor
            {
                return hdr;
            }
            let hdr = (0..)
                .map_while(|at| self.outputs.EnumAdapters1(at).ok())
                .flat_map(|adapter| (0..).map_while(move |at| adapter.EnumOutputs(at).ok()))
                .filter_map(|output| output.cast::<IDXGIOutput6>().ok()?.GetDesc1().ok())
                .any(|output| {
                    output.Monitor == monitor
                        && output.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
                });
            self.display = Some((monitor, hdr));
            hdr
        }
    }

    // the surface's texture goes to the back buffer on the GPU; when this device cannot open
    // it, or it is HDR10 and the swapchain is not, the decoder reads it back in SDR and its
    // rows are uploaded instead
    fn copy_surface(
        &mut self,
        swapchain: &IDXGISwapChain1,
        format: DXGI_FORMAT,
        surface: &Arc<dyn VideoSurface>,
        (width, height): (u32, u32),
    ) -> Result<(), String> {
        let hdr10 = format == DXGI_FORMAT_R10G10B10A2_UNORM;
        let at = if surface.hdr10() == hdr10 {
            self.opened_at(surface)
        } else {
            None
        };
        if at.is_none() && hdr10 {
            // the frame is dropped and the next ones go to a BGRA swapchain
            self.sdr_only = true;
            return Ok(());
        }
        // SAFETY: the back buffer and the opened texture are both the frame's size in one
        // format, and the keyed mutex is held while the copy is queued
        unsafe {
            let buffer: ID3D11Texture2D = swapchain
                .GetBuffer(0)
                .map_err(failed("no video back buffer"))?;
            match at {
                Some(at) => {
                    let (_, texture, mutex) = &self.opened[at];
                    // windows-rs turns a timeout into success, so the wait is unbounded
                    mutex
                        .AcquireSync(0, u32::MAX)
                        .map_err(failed("a video frame stayed locked"))?;
                    self.context.CopyResource(&buffer, texture);
                    mutex
                        .ReleaseSync(0)
                        .map_err(failed("a video frame could not be unlocked"))?;
                }
                None => {
                    let bgra = surface.read()?;
                    if bgra.len() < width as usize * height as usize * 4 {
                        return Err("a video frame is short of its size".to_string());
                    }
                    self.context.UpdateSubresource(
                        &buffer,
                        0,
                        None,
                        bgra.as_ptr().cast(),
                        width * 4,
                        0,
                    );
                }
            }
            swapchain
                .Present(1, DXGI_PRESENT(0))
                .ok()
                .map_err(failed("a video frame could not be presented"))
        }
    }

    fn opened_at(&mut self, surface: &Arc<dyn VideoSurface>) -> Option<usize> {
        self.opened.retain(|(kept, ..)| kept.strong_count() > 0);
        let known = self
            .opened
            .iter()
            .position(|(kept, ..)| std::ptr::addr_eq(kept.as_ptr(), Arc::as_ptr(surface)));
        known.or_else(|| {
            let opened = self.open_surface(surface)?;
            self.opened.push(opened);
            Some(self.opened.len() - 1)
        })
    }

    fn open_surface(
        &self,
        surface: &Arc<dyn VideoSurface>,
    ) -> Option<(Weak<dyn VideoSurface>, ID3D11Texture2D, IDXGIKeyedMutex)> {
        // SAFETY: the handle stays open while the surface lives, which outlasts the call
        unsafe {
            let texture: ID3D11Texture2D = self
                .device
                .cast::<ID3D11Device1>()
                .ok()?
                .OpenSharedResource1(HANDLE(surface.shared_handle() as *mut _))
                .ok()?;
            let mutex = texture.cast().ok()?;
            Some((Arc::downgrade(surface), texture, mutex))
        }
    }

    fn plane_visual(
        &mut self,
        plane: &Plane,
        frame: Arc<Image>,
        format: DXGI_FORMAT,
    ) -> Result<ShownPlane, String> {
        let mut format = format;
        let mut swapchain = swapchain_for(
            &self.factory,
            &self.device,
            plane.size,
            format,
            DXGI_ALPHA_MODE_IGNORE,
        )?;
        // RGB10 swapchains count as sRGB until told otherwise, and some drivers refuse HDR10
        if format == DXGI_FORMAT_R10G10B10A2_UNORM && !shows_hdr10(&swapchain) {
            self.sdr_only = true;
            format = DXGI_FORMAT_B8G8R8A8_UNORM;
            swapchain = swapchain_for(
                &self.factory,
                &self.device,
                plane.size,
                format,
                DXGI_ALPHA_MODE_IGNORE,
            )?;
        }
        // SAFETY: plain COM calls on interfaces this presenter owns or just made
        unsafe {
            let clip = self
                .composition
                .CreateVisual()
                .map_err(failed("no composition visual"))?;
            let content = self
                .composition
                .CreateVisual()
                .map_err(failed("no composition visual"))?;
            content
                .SetContent(&swapchain)
                .map_err(failed("a video swapchain could not be shown"))?;
            content
                .SetBitmapInterpolationMode(DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR)
                .map_err(failed("a video visual refused its filtering"))?;
            // hard edges clamp the frame where it meets its box, as the canvas does
            clip.SetBorderMode(DCOMPOSITION_BORDER_MODE_HARD)
                .map_err(failed("a video visual refused its edges"))?;
            clip.AddVisual(&content, false, None)
                .map_err(failed("a video visual could not be attached"))?;
            // above no sibling, so under the canvas
            self.root
                .AddVisual(&clip, true, None)
                .map_err(failed("a video visual could not be attached"))?;
            Ok(ShownPlane {
                clip,
                content,
                swapchain,
                plane: plane.clone(),
                frame,
                format,
            })
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

// pixels come as RGBA and decoder surfaces as BGRA, or as HDR10 for a swapchain to match
// while the display shows HDR
fn format_of(surface: Option<&Arc<dyn VideoSurface>>, hdr: bool) -> DXGI_FORMAT {
    match surface {
        None => DXGI_FORMAT_R8G8B8A8_UNORM,
        Some(surface) if surface.hdr10() && hdr => DXGI_FORMAT_R10G10B10A2_UNORM,
        Some(_) => DXGI_FORMAT_B8G8R8A8_UNORM,
    }
}

fn shows_hdr10(swapchain: &IDXGISwapChain1) -> bool {
    // SAFETY: plain COM calls on a swapchain the caller owns
    unsafe {
        swapchain.cast::<IDXGISwapChain3>().is_ok_and(|swapchain| {
            swapchain
                .CheckColorSpaceSupport(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
                .is_ok_and(|support| {
                    support & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT.0 as u32 != 0
                })
                && swapchain
                    .SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
                    .is_ok()
        })
    }
}

fn swapchain_for(
    factory: &IDXGIFactory2,
    device: &ID3D11Device,
    (width, height): (u32, u32),
    format: DXGI_FORMAT,
    alpha: DXGI_ALPHA_MODE,
) -> Result<IDXGISwapChain1, String> {
    let description = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: BUFFER_COUNT,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: alpha,
        ..Default::default()
    };
    // SAFETY: the description outlives the call
    unsafe { factory.CreateSwapChainForComposition(device, &description, None) }
        .map_err(failed("the composition swapchain could not be created"))
}

// the clip visual takes the device box and the content visual maps frame pixels onto the
// device pixels the canvas fitted them to
fn place(shown: &ShownPlane, plane: &Plane) -> Result<(), String> {
    let [left, top, right, bottom] = plane.bounds.map(|edge| edge as f32);
    let matrix = [
        plane.scale.0,
        0.0,
        0.0,
        plane.scale.1,
        plane.origin.0,
        plane.origin.1,
    ];
    // SAFETY: the matrix is six f32s in the row order of a repr(C) Matrix3x2, and both live
    // for the calls
    unsafe {
        shown
            .clip
            .SetClip2(&D2D_RECT_F {
                left,
                top,
                right,
                bottom,
            })
            .map_err(failed("a video visual refused its clip"))?;
        shown
            .content
            .SetTransform2(matrix.as_ptr().cast())
            .map_err(failed("a video visual refused its transform"))
    }
}

// colours are four bytes in red, green, blue, alpha order, the swapchain's own layout
fn upload(
    context: &ID3D11DeviceContext,
    swapchain: &IDXGISwapChain1,
    frame: &Image,
) -> Result<(), String> {
    if frame.pixels.len() < frame.width as usize * frame.height as usize {
        return Err("a video frame is short of its size".to_string());
    }
    // SAFETY: the back buffer is the frame's size, and its pixels hold height rows of width
    // four-byte colours
    unsafe {
        let buffer: ID3D11Texture2D = swapchain
            .GetBuffer(0)
            .map_err(failed("no video back buffer"))?;
        context.UpdateSubresource(
            &buffer,
            0,
            None,
            frame.pixels.as_ptr().cast(),
            frame.width * 4,
            0,
        );
        swapchain
            .Present(1, DXGI_PRESENT(0))
            .ok()
            .map_err(failed("a video frame could not be presented"))
    }
}
