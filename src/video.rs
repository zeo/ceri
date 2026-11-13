// the media decoders the desktop already has: media foundation on windows, gstreamer on linux;
// the engine runs each reader on a media element's own decode or sound thread

use ceri_engine::{
    AudioChunk, AudioInfo, AudioReader, MediaDecoder, MediaInfo, MediaSource, VideoFrame,
    VideoReader,
};

#[derive(Default)]
pub(crate) struct DesktopVideo;

impl MediaDecoder for DesktopVideo {
    fn open(&self, source: &MediaSource) -> Result<Box<dyn VideoReader>, String> {
        Ok(Box::new(platform::Reader::open(source)?))
    }

    fn open_audio(&self, source: &MediaSource) -> Result<Box<dyn AudioReader>, String> {
        Ok(Box::new(platform::Sound::open(source)?))
    }

    fn can_play(&self, mime_type: &str) -> &'static str {
        let essence = mime_type.split(';').next().unwrap_or_default().trim();
        if platform::TYPES
            .iter()
            .any(|known| known.eq_ignore_ascii_case(essence))
        {
            "maybe"
        } else {
            ""
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::mp4::{edit_shift, fragmented_end, headers};
    use super::{
        AudioChunk, AudioInfo, AudioReader, MediaInfo, MediaSource, VideoFrame, VideoReader,
    };
    use ceri_engine::VideoSurface;
    use std::mem::ManuallyDrop;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, RECT};
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
        D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
        D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
        D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE, D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE,
        D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
        D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VIEWPORT, D3D11_VPIV_DIMENSION_TEXTURE2D,
        D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Buffer, ID3D11Device,
        ID3D11DeviceContext, ID3D11Multithread, ID3D11PixelShader, ID3D11Texture2D,
        ID3D11VertexShader, ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor,
        ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorEnumerator1,
        ID3D11VideoProcessorOutputView,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
        DXGI_COLOR_SPACE_TYPE, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601,
        DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIKeyedMutex, IDXGIResource1,
    };
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer, IMFAttributes, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaBuffer,
        IMFMediaType, IMFSample, IMFSourceReader, MF_BYTESTREAM_CONTENT_TYPE,
        MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
        MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MAX_LUMINANCE_LEVEL,
        MF_MT_MAX_MASTERING_LUMINANCE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_PIXEL_ASPECT_RATIO,
        MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX,
        MF_PD_DURATION, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ALL_STREAMS,
        MF_SOURCE_READER_D3D_MANAGER, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
        MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
        MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MF_VERSION, MFAudioFormat_Float,
        MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateMFByteStreamOnStream,
        MFCreateMediaType, MFCreateSourceReaderFromByteStream, MFCreateSourceReaderFromURL,
        MFMediaType_Audio, MFMediaType_Video, MFNominalRange_0_255, MFSTARTUP_LITE, MFStartup,
        MFVideoArea, MFVideoFormat_NV12, MFVideoFormat_P010, MFVideoFormat_RGB32,
        MFVideoTransFunc_2084, MFVideoTransFunc_HLG, MFVideoTransferFunction,
        MFVideoTransferMatrix, MFVideoTransferMatrix_BT601, MFVideoTransferMatrix_BT709,
        MFVideoTransferMatrix_BT2020_10, MFVideoTransferMatrix_BT2020_12,
    };
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::System::Threading::INFINITE;
    use windows::Win32::System::Variant::{VT_I8, VT_UI8};
    use windows::Win32::UI::Shell::SHCreateMemStream;
    use windows::core::{GUID, Interface, PCSTR, PCWSTR, s};

    pub(super) const TYPES: &[&str] = &[
        "video/mp4",
        "video/webm",
        "video/x-matroska",
        "audio/mp4",
        "audio/mpeg",
        "audio/aac",
    ];

    const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
    // media foundation keeps time in units of 100 nanoseconds
    const TICKS: f64 = 10_000_000.0;

    pub(super) struct Reader {
        reader: IMFSourceReader,
        info: MediaInfo,
        // the decoded buffer's size and row stride, and the part of it that is picture
        coded: (u32, u32),
        stride: i32,
        picture: Area,
        // set when the decoder hands frames over as gpu surfaces
        gpu: Option<&'static Gpu>,
        converter: Option<Converter>,
        // the first frame, decoded while open checked that the gpu path works
        pending: Option<VideoFrame>,
        // seconds the mp4 edit list moves pictures by
        shift: f64,
        // declared last so the reader is released before the apartment closes
        _apartment: Apartment,
    }

    // SAFETY: the engine opens, reads and drops a reader on one decode thread, the apartment
    // open joined
    unsafe impl Send for Reader {}

    // one d3d11 device that every hardware decoder in the process shares through the device
    // manager, as browsers do, so each video costs no device of its own
    pub(super) struct Gpu {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        video: ID3D11VideoDevice,
        playback: ID3D11VideoContext1,
        manager: IMFDXGIDeviceManager,
        multithread: ID3D11Multithread,
        shaders: OnceLock<Result<(ID3D11VertexShader, ID3D11PixelShader), String>>,
    }

    // SAFETY: d3d11 devices are free threaded, the immediate context is multithread protected
    // before it is shared, and the device manager serializes its own callers
    unsafe impl Send for Gpu {}
    unsafe impl Sync for Gpu {}

    pub(super) fn gpu() -> Option<&'static Gpu> {
        static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
        // SAFETY: the out pointers refer to live locals for each call
        GPU.get_or_init(|| unsafe { Gpu::new() }).as_ref()
    }

    impl Gpu {
        unsafe fn new() -> Option<Self> {
            unsafe {
                let mut device = None;
                let mut context = None;
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
                .ok()?;
                let (device, context): (ID3D11Device, ID3D11DeviceContext) = (device?, context?);
                let multithread = context.cast::<ID3D11Multithread>().ok()?;
                let _ = multithread.SetMultithreadProtected(true);
                let mut token = 0;
                let mut manager = None;
                MFCreateDXGIDeviceManager(&mut token, &mut manager).ok()?;
                let manager = manager?;
                manager.ResetDevice(&device, token).ok()?;
                Some(Self {
                    video: device.cast().ok()?,
                    playback: context.cast().ok()?,
                    device,
                    context,
                    manager,
                    multithread,
                    shaders: OnceLock::new(),
                })
            }
        }

        unsafe fn texture(
            &self,
            (width, height): (u32, u32),
            format: DXGI_FORMAT,
            usage: D3D11_USAGE,
            bind: u32,
            access: u32,
            misc: u32,
        ) -> Result<ID3D11Texture2D, String> {
            let description = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: usage,
                BindFlags: bind,
                CPUAccessFlags: access,
                MiscFlags: misc,
            };
            let mut texture = None;
            unsafe {
                self.device
                    .CreateTexture2D(&description, None, Some(&mut texture))
                    .map_err(text)?;
            }
            texture.ok_or_else(|| "Direct3D returned no texture".to_string())
        }

        // a processor drawing the shown part of coded sized frames in the input colour space
        // over a whole picture in sdr
        unsafe fn processor(
            &self,
            coded: (u32, u32),
            shown: Area,
            input: DXGI_COLOR_SPACE_TYPE,
        ) -> Result<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor), String> {
            unsafe {
                let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputWidth: coded.0,
                    InputHeight: coded.1,
                    OutputWidth: shown.width,
                    OutputHeight: shown.height,
                    Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                    ..Default::default()
                };
                let enumerator = self
                    .video
                    .CreateVideoProcessorEnumerator(&content)
                    .map_err(text)?;
                let processor = self
                    .video
                    .CreateVideoProcessor(&enumerator, 0)
                    .map_err(text)?;
                let source = RECT {
                    left: shown.left as i32,
                    top: shown.top as i32,
                    right: (shown.left + shown.width) as i32,
                    bottom: (shown.top + shown.height) as i32,
                };
                let whole = RECT {
                    left: 0,
                    top: 0,
                    right: shown.width as i32,
                    bottom: shown.height as i32,
                };
                let playback = &self.playback;
                playback.VideoProcessorSetStreamFrameFormat(
                    &processor,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                playback.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
                playback.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&whole));
                playback.VideoProcessorSetOutputTargetRect(&processor, true, Some(&whole));
                // drivers would otherwise sharpen or denoise, which the software path never does
                playback.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
                playback.VideoProcessorSetStreamColorSpace1(&processor, 0, input);
                playback.VideoProcessorSetOutputColorSpace1(&processor, SDR);
                Ok((enumerator, processor))
            }
        }

        unsafe fn output_view(
            &self,
            texture: &ID3D11Texture2D,
            enumerator: &ID3D11VideoProcessorEnumerator,
        ) -> Result<ID3D11VideoProcessorOutputView, String> {
            let described = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut view = None;
            unsafe {
                self.video
                    .CreateVideoProcessorOutputView(
                        texture,
                        enumerator,
                        &described,
                        Some(&mut view),
                    )
                    .map_err(text)?;
            }
            view.ok_or_else(|| "Direct3D returned no output view".to_string())
        }

        // a slice of the input texture drawn over the output view's picture
        unsafe fn blt(
            &self,
            (enumerator, processor): (&ID3D11VideoProcessorEnumerator, &ID3D11VideoProcessor),
            input: &ID3D11Texture2D,
            slice: u32,
            output: &ID3D11VideoProcessorOutputView,
        ) -> Result<(), String> {
            let described = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: slice,
                    },
                },
            };
            let mut view = None;
            unsafe {
                self.video
                    .CreateVideoProcessorInputView(input, enumerator, &described, Some(&mut view))
                    .map_err(text)?;
                let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                    Enable: true.into(),
                    pInputSurface: ManuallyDrop::new(view),
                    ..Default::default()
                };
                let blitted = self.playback.VideoProcessorBlt(
                    processor,
                    output,
                    0,
                    std::slice::from_ref(&stream),
                );
                drop(ManuallyDrop::into_inner(stream.pInputSurface));
                blitted.map_err(text)
            }
        }

        unsafe fn constants(&self, values: &[[f32; 4]]) -> Result<ID3D11Buffer, String> {
            let described = D3D11_BUFFER_DESC {
                ByteWidth: size_of_val(values) as u32,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let data = D3D11_SUBRESOURCE_DATA {
                pSysMem: values.as_ptr().cast(),
                ..Default::default()
            };
            let mut buffer = None;
            unsafe {
                self.device
                    .CreateBuffer(&described, Some(&data), Some(&mut buffer))
                    .map_err(text)?;
            }
            buffer.ok_or_else(|| "Direct3D returned no buffer".to_string())
        }

        // an hdr10 texture drawn into an sdr one through the curve, as chromium tone maps
        unsafe fn tone(
            &self,
            curve: &ID3D11Buffer,
            source: &ID3D11Texture2D,
            target: &ID3D11Texture2D,
            (width, height): (u32, u32),
        ) -> Result<(), String> {
            let (vertices, pixels) = self
                .shaders
                .get_or_init(|| unsafe { shaders(&self.device) })
                .as_ref()
                .map_err(Clone::clone)?;
            let (mut read, mut drawn) = (None, None);
            unsafe {
                self.device
                    .CreateShaderResourceView(source, None, Some(&mut read))
                    .map_err(text)?;
                self.device
                    .CreateRenderTargetView(target, None, Some(&mut drawn))
                    .map_err(text)?;
                let context = &self.context;
                // decoders on other threads share the context, so the draw's state is held whole
                self.multithread.Enter();
                context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                context.IASetInputLayout(None);
                context.VSSetShader(vertices, None);
                context.PSSetShader(pixels, None);
                context.PSSetConstantBuffers(0, Some(&[Some(curve.clone())]));
                context.PSSetShaderResources(0, Some(&[read]));
                context.OMSetRenderTargets(Some(&[drawn]), None);
                context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    Width: width as f32,
                    Height: height as f32,
                    MaxDepth: 1.0,
                    ..Default::default()
                }]));
                context.Draw(3, 0);
                context.PSSetShaderResources(0, Some(&[None]));
                context.OMSetRenderTargets(None, None);
                self.multithread.Leave();
            }
            Ok(())
        }
    }

    // pq in bt.2020 to linear light with 203 nits as 1, the curve's gain on the brightest
    // channel, then bt.709 primaries and the srgb curve
    const TONE_MAP: &str = r"
cbuffer Curve : register(b0) {
    float4 points[8];
    float4 count;
};
Texture2D<float4> source : register(t0);

float4 vertices(uint id : SV_VertexID) : SV_Position {
    float2 at = float2((id << 1) & 2, id & 2);
    return float4(at * float2(2, -2) + float2(-1, 1), 0, 1);
}

float gain(float x) {
    int last = (int)count.x - 1;
    if (x <= points[0].x) return points[0].y;
    if (x >= points[last].x) return points[last].y + log2(points[last].x / x);
    int i = 0;
    [unroll] for (int j = 1; j < 7; j++) if (j < last && x >= points[j].x) i = j;
    float4 a = points[i];
    float4 b = points[i + 1];
    float h = b.x - a.x;
    float ma = a.z * h;
    float mb = b.z * h;
    float t = (x - a.x) / h;
    float c3 = 2 * a.y + ma - 2 * b.y + mb;
    float c2 = -3 * a.y + 3 * b.y - 2 * ma - mb;
    return ((c3 * t + c2) * t + ma) * t + a.y;
}

float4 pixels(float4 at : SV_Position) : SV_Target {
    float3 e = pow(saturate(source.Load(int3(at.xy, 0)).rgb), 1 / 78.84375);
    float3 c = pow(max(e - 0.8359375, 0) / (18.8515625 - 18.6875 * e), 1 / 0.1593017578125);
    c *= 10000.0 / 203.0;
    if (count.x > 0) c *= exp2(gain(max(max(c.r, c.g), c.b)));
    c = saturate(mul(float3x3(
        1.660491, -0.587641, -0.072850,
        -0.124550, 1.132900, -0.008349,
        -0.018151, -0.100579, 1.118730), c));
    c = c <= 0.0031308 ? 12.92 * c : 1.055 * pow(c, 1 / 2.4) - 0.055;
    return float4(c, 1);
}
";

    unsafe fn shaders(
        device: &ID3D11Device,
    ) -> Result<(ID3D11VertexShader, ID3D11PixelShader), String> {
        let compiled = |entry: PCSTR, target: PCSTR| {
            let (mut code, mut errors) = (None, None);
            let compiled = unsafe {
                D3DCompile(
                    TONE_MAP.as_ptr().cast(),
                    TONE_MAP.len(),
                    PCSTR::null(),
                    None,
                    None,
                    entry,
                    target,
                    0,
                    0,
                    &mut code,
                    Some(&mut errors),
                )
            };
            let bytes = |blob: &ID3DBlob| unsafe {
                std::slice::from_raw_parts(
                    blob.GetBufferPointer().cast::<u8>(),
                    blob.GetBufferSize(),
                )
                .to_vec()
            };
            match (compiled, code) {
                (Ok(()), Some(code)) => Ok(bytes(&code)),
                (compiled, _) => Err(errors.as_ref().map_or_else(
                    || compiled.err().map(text).unwrap_or_default(),
                    |errors| String::from_utf8_lossy(&bytes(errors)).into_owned(),
                )),
            }
        };
        let (mut vertices, mut pixels) = (None, None);
        unsafe {
            device
                .CreateVertexShader(
                    &compiled(s!("vertices"), s!("vs_4_0"))?,
                    None,
                    Some(&mut vertices),
                )
                .map_err(text)?;
            device
                .CreatePixelShader(
                    &compiled(s!("pixels"), s!("ps_4_0"))?,
                    None,
                    Some(&mut pixels),
                )
                .map_err(text)?;
        }
        vertices
            .zip(pixels)
            .ok_or_else(|| "Direct3D returned no shader".to_string())
    }

    // skia's tone map for pq and hlg that carry no agtm metadata, the reference white tone
    // mapping operator of smpte st 2094-50, as chromium applies it for an sdr target: hermite
    // control points of a log2 gain over maxrgb, with 203 nits as 1 and the peak from the
    // stream's metadata or 1000 nits; none when the peak is no brighter than white
    fn sdr_curve(peak: f32) -> [[f32; 4]; 9] {
        let mut curve = [[0.0; 4]; 9];
        let headroom = (peak / 203.0).log2().min(6.0);
        if headroom.is_nan() || headroom <= 0.0 {
            return curve;
        }
        let white = 1.0 - 0.5 * (headroom / (1000.0f32 / 203.0).log2()).min(1.0);
        let kappa = 0.65;
        let (knee, top) = ((1.0, white), (headroom.exp2(), 1.0f32));
        let middle = (
            (1.0 - kappa) * knee.0 + kappa * knee.0 * top.1 / knee.1,
            (1.0 - kappa) * knee.1 + kappa * top.1,
        );
        let a = (
            knee.0 - 2.0 * middle.0 + top.0,
            knee.1 - 2.0 * middle.1 + top.1,
        );
        let b = (2.0 * (middle.0 - knee.0), 2.0 * (middle.1 - knee.1));
        for (index, point) in curve[..8].iter_mut().enumerate() {
            let t = index as f32 / 7.0;
            let x = knee.0 + t * (b.0 + t * a.0);
            let y = knee.1 + t * (b.1 + t * a.1);
            let slope = (2.0 * a.1 * t + b.1) / (2.0 * a.0 * t + b.0);
            *point = [
                x,
                (y / x).log2(),
                (x * slope - y) / (std::f32::consts::LN_2 * x * y),
                0.0,
            ];
        }
        curve[8][0] = 8.0;
        curve
    }

    const SDR: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
    const HDR10: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;

    // turns the decoder's nv12 or p010 surfaces into bgra pictures on the gpu, or into hdr10
    // ones for an hdr source while the presenter shows it on a display in hdr
    struct Converter {
        enumerator: ID3D11VideoProcessorEnumerator,
        processor: ID3D11VideoProcessor,
        // pictures the engine or presenter may still hold; one only the pool holds is free
        pictures: Vec<Arc<Picture>>,
        readback: Arc<Mutex<Readback>>,
        size: (u32, u32),
        // for an hdr source the processor gives hdr10, which this tone map takes to sdr
        curve: Option<ID3D11Buffer>,
        staged: Option<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
        // what the presenter last asked for, and what the pool's pictures hold
        wanted: Arc<AtomicBool>,
        hdr10: bool,
    }

    // a converted frame in a texture the presenter opens by its handle; the keyed mutex orders
    // the decoder's writes and reads against the presenter's copies
    struct Picture {
        gpu: &'static Gpu,
        texture: ID3D11Texture2D,
        view: ID3D11VideoProcessorOutputView,
        mutex: IDXGIKeyedMutex,
        handle: HANDLE,
        hdr10: bool,
        curve: Option<ID3D11Buffer>,
        wanted: Arc<AtomicBool>,
        readback: Arc<Mutex<Readback>>,
        size: (u32, u32),
    }

    // what a converter's pictures take turns with to be read back, made when one is first
    // painted: a staging texture, and for hdr10 pictures a target the tone map draws sdr into
    #[derive(Default)]
    struct Readback {
        staging: Option<ID3D11Texture2D>,
        toned: Option<ID3D11Texture2D>,
    }

    // SAFETY: d3d11 resources are free threaded, and the one context they meet is multithread
    // protected
    unsafe impl Send for Picture {}
    unsafe impl Sync for Picture {}

    impl Drop for Picture {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateSharedHandle and is closed only here
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    impl VideoSurface for Picture {
        fn shared_handle(&self) -> isize {
            self.handle.0 as isize
        }

        fn read(&self) -> Result<Vec<u8>, String> {
            let mut kept = self.readback.lock().unwrap_or_else(PoisonError::into_inner);
            let Readback { staging, toned } = &mut *kept;
            let context = &self.gpu.context;
            // SAFETY: the staging texture and the tone target are the picture's size in bgra,
            // the keyed mutex is held while the picture is read, and each row read lies inside
            // the mapped pitch
            unsafe {
                let staging = match staging {
                    Some(made) => made,
                    None => staging.insert(self.gpu.texture(
                        self.size,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        D3D11_USAGE_STAGING,
                        0,
                        D3D11_CPU_ACCESS_READ.0 as u32,
                        0,
                    )?),
                };
                let toned = match (self.curve.as_ref().filter(|_| self.hdr10), toned) {
                    (Some(curve), Some(made)) => Some((curve, &*made)),
                    (Some(curve), toned) => Some((
                        curve,
                        &*toned.insert(self.gpu.texture(
                            self.size,
                            DXGI_FORMAT_B8G8R8A8_UNORM,
                            D3D11_USAGE_DEFAULT,
                            D3D11_BIND_RENDER_TARGET.0 as u32,
                            0,
                            0,
                        )?),
                    )),
                    (None, _) => None,
                };
                self.mutex.AcquireSync(0, INFINITE).map_err(text)?;
                let copied = match toned {
                    Some((curve, toned)) => self
                        .gpu
                        .tone(curve, &self.texture, toned, self.size)
                        .map(|()| context.CopyResource(&*staging, toned)),
                    None => {
                        context.CopyResource(&*staging, &self.texture);
                        Ok(())
                    }
                };
                self.mutex.ReleaseSync(0).map_err(text)?;
                copied?;
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context
                    .Map(&*staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(text)?;
                let row = self.size.0 as usize * 4;
                let mut bgra = Vec::with_capacity(row * self.size.1 as usize);
                for y in 0..self.size.1 as usize {
                    let start = mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize);
                    bgra.extend_from_slice(std::slice::from_raw_parts(start, row));
                }
                context.Unmap(&*staging, 0);
                Ok(bgra)
            }
        }

        fn hdr10(&self) -> bool {
            self.hdr10
        }

        fn show_hdr(&self, display: bool) {
            self.wanted.store(display, Ordering::Relaxed);
        }
    }

    impl Converter {
        unsafe fn new(
            gpu: &Gpu,
            kind: &IMFMediaType,
            native: Option<&IMFMediaType>,
            picture: Area,
            coded: (u32, u32),
        ) -> Result<Self, String> {
            unsafe {
                let input = color_space(kind, native, picture.height);
                let (enumerator, processor) = gpu.processor(coded, picture, input)?;
                let decoded = match kind.GetGUID(&MF_MT_SUBTYPE) {
                    Ok(subtype) if subtype == MFVideoFormat_P010 => DXGI_FORMAT_P010,
                    _ => DXGI_FORMAT_NV12,
                };
                let hdr_source = [
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020,
                ]
                .contains(&input)
                    && enumerator
                        .cast::<ID3D11VideoProcessorEnumerator1>()
                        .and_then(|enumerator| {
                            enumerator.CheckVideoProcessorFormatConversion(
                                decoded,
                                input,
                                DXGI_FORMAT_R10G10B10A2_UNORM,
                                HDR10,
                            )
                        })
                        .is_ok_and(|supported| supported.as_bool());
                let curve = match hdr_source {
                    true => {
                        let peak = [&MF_MT_MAX_LUMINANCE_LEVEL, &MF_MT_MAX_MASTERING_LUMINANCE]
                            .into_iter()
                            .flat_map(|key| {
                                [Some(kind), native]
                                    .into_iter()
                                    .flatten()
                                    .map(move |kind| kind.GetUINT32(key))
                            })
                            .find_map(|value| value.ok().filter(|&nits| nits > 0))
                            .map_or(1000.0, |nits| nits as f32);
                        gpu.playback
                            .VideoProcessorSetOutputColorSpace1(&processor, HDR10);
                        Some(gpu.constants(&sdr_curve(peak))?)
                    }
                    false => None,
                };
                Ok(Self {
                    enumerator,
                    processor,
                    pictures: Vec::new(),
                    readback: Arc::default(),
                    size: (picture.width, picture.height),
                    curve,
                    staged: None,
                    wanted: Arc::default(),
                    hdr10: false,
                })
            }
        }

        unsafe fn picture(&mut self, gpu: &'static Gpu) -> Result<Arc<Picture>, String> {
            if let Some(free) = self
                .pictures
                .iter()
                .find(|picture| Arc::strong_count(picture) == 1)
            {
                return Ok(free.clone());
            }
            let format = if self.hdr10 {
                DXGI_FORMAT_R10G10B10A2_UNORM
            } else {
                DXGI_FORMAT_B8G8R8A8_UNORM
            };
            unsafe {
                let texture = gpu.texture(
                    self.size,
                    format,
                    D3D11_USAGE_DEFAULT,
                    (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    0,
                    (D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0
                        | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
                )?;
                let view = gpu.output_view(&texture, &self.enumerator)?;
                let handle = texture
                    .cast::<IDXGIResource1>()
                    .and_then(|resource| {
                        resource.CreateSharedHandle(
                            None,
                            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                            PCWSTR::null(),
                        )
                    })
                    .map_err(text)?;
                let picture = Arc::new(Picture {
                    gpu,
                    mutex: texture.cast().map_err(text)?,
                    texture,
                    view,
                    handle,
                    hdr10: self.hdr10,
                    curve: self.curve.clone(),
                    wanted: self.wanted.clone(),
                    readback: self.readback.clone(),
                    size: self.size,
                });
                self.pictures.push(picture.clone());
                Ok(picture)
            }
        }

        // the sample's surface drawn into a free picture; a frame in system memory means no
        // gpu decoder
        unsafe fn convert(
            &mut self,
            gpu: &'static Gpu,
            sample: &IMFSample,
        ) -> Result<Arc<Picture>, String> {
            unsafe {
                let hdr10 = self.curve.is_some() && self.wanted.load(Ordering::Relaxed);
                if hdr10 != self.hdr10 {
                    // pictures still held keep their format, and the pool starts over
                    self.pictures.clear();
                    self.hdr10 = hdr10;
                }
                let picture = self.picture(gpu)?;
                let surface: IMFDXGIBuffer = sample
                    .GetBufferByIndex(0)
                    .and_then(|buffer| buffer.cast())
                    .map_err(|_| "the decoder gave a frame outside the gpu".to_string())?;
                let mut texture: Option<ID3D11Texture2D> = None;
                surface
                    .GetResource(&ID3D11Texture2D::IID, (&raw mut texture).cast())
                    .map_err(text)?;
                let texture = texture.ok_or("the decoder gave no texture")?;
                let slice = surface.GetSubresourceIndex().map_err(text)?;
                let processing = (&self.enumerator, &self.processor);
                let Some(curve) = self.curve.as_ref().filter(|_| !hdr10) else {
                    picture.mutex.AcquireSync(0, INFINITE).map_err(text)?;
                    let blitted = gpu.blt(processing, &texture, slice, &picture.view);
                    let released = picture.mutex.ReleaseSync(0).map_err(text);
                    blitted.and(released)?;
                    return Ok(picture);
                };
                // an hdr source on its way to sdr stops in hdr10 for the tone map
                let (staged, view) = match &mut self.staged {
                    Some(staged) => &*staged,
                    staged => {
                        let texture = gpu.texture(
                            self.size,
                            DXGI_FORMAT_R10G10B10A2_UNORM,
                            D3D11_USAGE_DEFAULT,
                            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                            0,
                            0,
                        )?;
                        let view = gpu.output_view(&texture, &self.enumerator)?;
                        &*staged.insert((texture, view))
                    }
                };
                gpu.blt(processing, &texture, slice, view)?;
                picture.mutex.AcquireSync(0, INFINITE).map_err(text)?;
                let toned = gpu.tone(curve, staged, &picture.texture, self.size);
                let released = picture.mutex.ReleaseSync(0).map_err(text);
                toned.and(released)?;
                Ok(picture)
            }
        }
    }

    // the decoder's matrix, range and transfer as a dxgi colour space, taken from the stream
    // where the decoder's output type leaves them out; with no matrix named, hd pictures count
    // as bt.709 and smaller ones as bt.601, as players commonly guess
    unsafe fn color_space(
        kind: &IMFMediaType,
        native: Option<&IMFMediaType>,
        height: u32,
    ) -> DXGI_COLOR_SPACE_TYPE {
        let value = |key| {
            [Some(kind), native]
                .into_iter()
                .flatten()
                .find_map(|kind| unsafe { kind.GetUINT32(key) }.ok())
                .map(|value| value as i32)
        };
        let full = value(&MF_MT_VIDEO_NOMINAL_RANGE) == Some(MFNominalRange_0_255.0);
        let matrix = value(&MF_MT_YUV_MATRIX).map(MFVideoTransferMatrix);
        let transfer = value(&MF_MT_TRANSFER_FUNCTION).map(MFVideoTransferFunction);
        let wide = [
            MFVideoTransferMatrix_BT2020_10,
            MFVideoTransferMatrix_BT2020_12,
        ]
        .map(Some)
        .contains(&matrix);
        if wide && transfer == Some(MFVideoTransFunc_2084) {
            return DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020;
        }
        if wide && transfer == Some(MFVideoTransFunc_HLG) {
            return DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020;
        }
        let [studio, whole] = if wide {
            [
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020,
                DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020,
            ]
        } else if matrix == Some(MFVideoTransferMatrix_BT709)
            || matrix != Some(MFVideoTransferMatrix_BT601) && height >= 720
        {
            [
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
            ]
        } else {
            [
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
                DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601,
            ]
        };
        if full { whole } else { studio }
    }

    struct Apartment;

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: balances the CoInitializeEx in Reader::open on this same thread
            unsafe { CoUninitialize() };
        }
    }

    fn text(error: windows::core::Error) -> String {
        error.message()
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain([0]).collect()
    }

    // the source resolver refuses the verbatim paths canonicalize gives
    fn resolvable(path: &std::path::Path) -> std::ffi::OsString {
        match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
            Some(rest) => match rest.strip_prefix(r"UNC\") {
                Some(share) => format!(r"\\{share}").into(),
                None => rest.into(),
            },
            None => path.into(),
        }
    }

    fn started() -> Result<(), String> {
        static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
        STARTED
            // SAFETY: media foundation stays started for the life of the process
            .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_err(text))
            .clone()
    }

    fn apartment() -> Result<Apartment, String> {
        // SAFETY: the decode or sound thread is new, so this is its first COM initialization
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(text)?;
        let apartment = Apartment;
        started()?;
        Ok(apartment)
    }

    // a source reader with only the given stream selected, or the missing message without one
    unsafe fn source_reader(
        source: &MediaSource,
        attributes: Option<&IMFAttributes>,
        stream: u32,
        missing: &str,
    ) -> Result<IMFSourceReader, String> {
        unsafe {
            let reader = match source {
                MediaSource::File(path) => {
                    let path = wide(&resolvable(path));
                    MFCreateSourceReaderFromURL(PCWSTR(path.as_ptr()), attributes)
                }
                MediaSource::Bytes { body, mime_type } => {
                    let stream = SHCreateMemStream(Some(body)).ok_or("could not hold the media")?;
                    let bytes = MFCreateMFByteStreamOnStream(&stream).map_err(text)?;
                    let mime_type = wide(mime_type.as_ref());
                    bytes
                        .cast::<IMFAttributes>()
                        .and_then(|described| {
                            described
                                .SetString(&MF_BYTESTREAM_CONTENT_TYPE, PCWSTR(mime_type.as_ptr()))
                        })
                        .map_err(text)?;
                    MFCreateSourceReaderFromByteStream(&bytes, attributes)
                }
            }
            .map_err(text)?;
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(text)?;
            reader
                .SetStreamSelection(stream, true)
                .map_err(|_| missing.to_string())?;
            Ok(reader)
        }
    }

    unsafe fn duration(reader: &IMFSourceReader) -> f64 {
        unsafe {
            reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .filter(|value| value.Anonymous.Anonymous.vt == VT_UI8)
                .map_or(f64::NAN, |value| {
                    value.Anonymous.Anonymous.Anonymous.uhVal as f64 / TICKS
                })
        }
    }

    // the next sample of a stream and its time, none at the end; a changed output type is
    // reported first so the caller reads it before the sample's data
    fn read_sample(
        reader: &IMFSourceReader,
        stream: u32,
        mut changed: impl FnMut() -> Result<(), String>,
    ) -> Result<Option<(IMFSample, f64)>, String> {
        loop {
            let mut flags = 0;
            let mut time = 0;
            let mut sample = None;
            // SAFETY: a synchronous read into locals that outlive the call
            unsafe {
                reader.ReadSample(
                    stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
            }
            .map_err(text)?;
            let flags = flags as i32;
            if flags & MF_SOURCE_READERF_ERROR.0 != 0 {
                return Err("the media stream failed".to_string());
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 != 0 {
                changed()?;
            }
            if let Some(sample) = sample {
                return Ok(Some((sample, time as f64 / TICKS)));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 != 0 {
                return Ok(None);
            }
        }
    }

    fn seek(reader: &IMFSourceReader, seconds: f64) -> Result<(), String> {
        let mut position = PROPVARIANT::default();
        // SAFETY: fills the eight byte integer member that VT_I8 names
        unsafe {
            let value = &mut position.Anonymous.Anonymous;
            value.vt = VT_I8;
            value.Anonymous.hVal = (seconds.max(0.0) * TICKS) as i64;
            reader
                .SetCurrentPosition(&GUID::zeroed(), &position)
                .map_err(text)
        }
    }

    fn split(packed: u64) -> (u32, u32) {
        ((packed >> 32) as u32, packed as u32)
    }

    #[derive(Clone, Copy)]
    struct Area {
        left: u32,
        top: u32,
        width: u32,
        height: u32,
    }

    // the visible part of a frame, which codecs padding to whole macroblocks keep smaller
    // than the coded size, as 1080 rows of a 1088 row h.264 picture
    unsafe fn picture(kind: &IMFMediaType) -> Result<(Area, (u32, u32)), String> {
        unsafe {
            let coded = split(kind.GetUINT64(&MF_MT_FRAME_SIZE).map_err(text)?);
            let mut aperture = MFVideoArea::default();
            let described = kind
                .GetBlob(
                    &MF_MT_MINIMUM_DISPLAY_APERTURE,
                    std::slice::from_raw_parts_mut(
                        (&raw mut aperture).cast::<u8>(),
                        size_of::<MFVideoArea>(),
                    ),
                    None,
                )
                .is_ok();
            let area = Area {
                left: aperture.OffsetX.value.max(0) as u32,
                top: aperture.OffsetY.value.max(0) as u32,
                width: aperture.Area.cx.max(0) as u32,
                height: aperture.Area.cy.max(0) as u32,
            };
            let inside = area.width > 0
                && area.height > 0
                && area.left + area.width <= coded.0
                && area.top + area.height <= coded.1;
            let whole = Area {
                left: 0,
                top: 0,
                width: coded.0,
                height: coded.1,
            };
            Ok((if described && inside { area } else { whole }, coded))
        }
    }

    // chromium's natural size: anamorphic pixels widen the picture, or make it taller
    pub(super) fn natural_size(width: u32, height: u32, aspect: (u32, u32)) -> (u32, u32) {
        let (across, down) = aspect;
        if across == 0 || down == 0 || across == down {
            (width, height)
        } else if across > down {
            (
                (u64::from(width) * u64::from(across) / u64::from(down)) as u32,
                height,
            )
        } else {
            (
                width,
                (u64::from(height) * u64::from(down) / u64::from(across)) as u32,
            )
        }
    }

    impl Reader {
        // a hardware decoder that fails anywhere up to its first frame leaves the media to
        // the software one
        pub(super) fn open(source: &MediaSource) -> Result<Self, String> {
            gpu()
                .and_then(|gpu| Self::open_on(source, Some(gpu)).ok())
                .map_or_else(|| Self::open_on(source, None), Ok)
        }

        pub(super) fn open_on(
            source: &MediaSource,
            gpu: Option<&'static Gpu>,
        ) -> Result<Self, String> {
            let apartment = apartment()?;
            // SAFETY: every call below follows the source reader's documented contract, on
            // interfaces this function created and still holds
            unsafe {
                let mut attributes = None;
                MFCreateAttributes(&mut attributes, 2).map_err(text)?;
                let attributes: IMFAttributes =
                    attributes.ok_or("media foundation gave no attribute store")?;
                match gpu {
                    Some(gpu) => {
                        attributes
                            .SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &gpu.manager)
                            .map_err(text)?;
                        attributes
                            .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                            .map_err(text)?;
                    }
                    None => attributes
                        // the advanced processor would also resample to the frame rate the
                        // container claims, which webm files often get wrong
                        .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                        .map_err(text)?,
                }
                let reader = source_reader(
                    source,
                    Some(&attributes),
                    VIDEO,
                    "the media has no video stream",
                )?;
                let native = reader
                    .GetNativeMediaType(VIDEO, 0)
                    .map_err(|_| "the media has no video stream".to_string())?;
                let (shown, _) = picture(&native)?;
                let aspect = native
                    .GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO)
                    .map_or((1, 1), split);
                let (width, height) = natural_size(shown.width, shown.height, aspect);
                let headers = headers(source);
                let shift = edit_shift(&headers, b"vide").unwrap_or(0.0);
                let duration = fragmented_end(&headers, b"vide")
                    .map_or_else(|| duration(&reader), |end| end + shift);
                let output = MFCreateMediaType().map_err(text)?;
                output
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                    .map_err(text)?;
                let deliver = |subtype| {
                    output.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                    reader.SetCurrentMediaType(VIDEO, None, &output)
                };
                let hdr = [MFVideoTransFunc_2084.0, MFVideoTransFunc_HLG.0]
                    .map(|transfer| Ok(transfer as u32))
                    .contains(&native.GetUINT32(&MF_MT_TRANSFER_FUNCTION));
                match gpu {
                    // ten bit decoders give p010 only, and others would cut pq to eight bits
                    Some(_) if hdr => {
                        deliver(&MFVideoFormat_P010).or_else(|_| deliver(&MFVideoFormat_NV12))
                    }
                    Some(_) => {
                        deliver(&MFVideoFormat_NV12).or_else(|_| deliver(&MFVideoFormat_P010))
                    }
                    None => deliver(&MFVideoFormat_RGB32),
                }
                .map_err(text)?;
                let mut opened = Self {
                    reader,
                    info: MediaInfo {
                        duration,
                        width,
                        height,
                    },
                    coded: (0, 0),
                    stride: 0,
                    picture: shown,
                    gpu,
                    converter: None,
                    pending: None,
                    shift,
                    _apartment: apartment,
                };
                opened.take_format()?;
                if gpu.is_some() {
                    opened.pending = opened.next()?;
                }
                Ok(opened)
            }
        }

        fn take_format(&mut self) -> Result<(), String> {
            // SAFETY: reads attributes of the type the reader now delivers
            unsafe {
                let current = self.reader.GetCurrentMediaType(VIDEO).map_err(text)?;
                (self.picture, self.coded) = picture(&current)?;
                self.stride = current
                    .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                    .map_or(self.coded.0 as i32 * 4, |stride| stride as i32);
                if let Some(gpu) = self.gpu {
                    let native = self.reader.GetNativeMediaType(VIDEO, 0).ok();
                    self.converter = Some(Converter::new(
                        gpu,
                        &current,
                        native.as_ref(),
                        self.picture,
                        self.coded,
                    )?);
                }
            }
            Ok(())
        }

        fn next(&mut self) -> Result<Option<VideoFrame>, String> {
            let reader = self.reader.clone();
            let Some((sample, time)) = read_sample(&reader, VIDEO, || self.take_format())? else {
                return Ok(None);
            };
            // SAFETY: the sample came from the read above and is still held
            let (bgra, surface) = unsafe {
                match (self.gpu, self.converter.as_mut()) {
                    (Some(gpu), Some(converter)) => {
                        let picture: Arc<dyn VideoSurface> = converter.convert(gpu, &sample)?;
                        (Vec::new(), Some(picture))
                    }
                    _ => (
                        self.copy(&sample.ConvertToContiguousBuffer().map_err(text)?)?,
                        None,
                    ),
                }
            };
            Ok(Some(VideoFrame {
                time: time + self.shift,
                width: self.picture.width,
                height: self.picture.height,
                bgra,
                surface,
            }))
        }

        fn copy(&self, buffer: &IMFMediaBuffer) -> Result<Vec<u8>, String> {
            let Area {
                left,
                top,
                width,
                height,
            } = self.picture;
            let row = width as usize * 4;
            let (skip, rows) = (left as usize * 4, height as usize);
            let coded = (self.coded.0 as usize * 4, self.coded.1 as usize);
            let mut bgra = Vec::with_capacity(row * rows);
            // SAFETY: the lock pins the buffer while the rows are read, each row read lies
            // inside the pitch and length the lock reported, and every lock is unlocked
            unsafe {
                if let Ok(planar) = buffer.cast::<IMF2DBuffer>() {
                    let mut first = std::ptr::null_mut();
                    let mut pitch = 0;
                    planar.Lock2D(&mut first, &mut pitch).map_err(text)?;
                    if pitch.unsigned_abs() as usize >= coded.0 {
                        for y in top as usize..top as usize + rows {
                            let start = first.offset(y as isize * pitch as isize).add(skip);
                            bgra.extend_from_slice(std::slice::from_raw_parts(start, row));
                        }
                    }
                    planar.Unlock2D().map_err(text)?;
                } else {
                    let mut start = std::ptr::null_mut();
                    let mut length = 0;
                    buffer
                        .Lock(&mut start, None, Some(&mut length))
                        .map_err(text)?;
                    let pitch = self.stride.unsigned_abs() as usize;
                    if pitch >= coded.0 && length as usize >= pitch * coded.1 {
                        let bytes = std::slice::from_raw_parts(start, length as usize);
                        for y in top as usize..top as usize + rows {
                            // a negative stride stores the bottom row first
                            let line = if self.stride < 0 { coded.1 - 1 - y } else { y };
                            bgra.extend_from_slice(&bytes[line * pitch + skip..][..row]);
                        }
                    }
                    buffer.Unlock().map_err(text)?;
                }
            }
            if bgra.is_empty() {
                return Err("the decoder gave a frame narrower than its picture".to_string());
            }
            Ok(bgra)
        }
    }

    impl VideoReader for Reader {
        fn info(&self) -> MediaInfo {
            self.info
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            self.pending = None;
            seek(&self.reader, seconds - self.shift)
        }

        fn read(&mut self) -> Result<Option<VideoFrame>, String> {
            match self.pending.take() {
                Some(frame) => Ok(Some(frame)),
                None => self.next(),
            }
        }
    }

    pub(super) struct Sound {
        reader: IMFSourceReader,
        info: AudioInfo,
        // seconds the mp4 edit list moves the sound by
        shift: f64,
        // declared last so the reader is released before the apartment closes
        _apartment: Apartment,
    }

    // SAFETY: the engine opens, reads and drops a sound on one thread, the apartment open joined
    unsafe impl Send for Sound {}

    // the rate and channels the decoder gives, as 32 bit floats
    fn layout(reader: &IMFSourceReader) -> Result<(u32, u16), String> {
        // SAFETY: reads attributes of the type the reader now delivers
        unsafe {
            let current = reader.GetCurrentMediaType(AUDIO).map_err(text)?;
            let bits = current
                .GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)
                .map_err(text)?;
            let rate = current
                .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
                .map_err(text)?;
            let channels = current.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(text)?;
            match (bits, u16::try_from(channels)) {
                (32, Ok(channels)) => Ok((rate, channels)),
                _ => Err(format!(
                    "the decoder gave {channels} channels of {bits} bit sound"
                )),
            }
        }
    }

    impl Sound {
        pub(super) fn open(source: &MediaSource) -> Result<Self, String> {
            let apartment = apartment()?;
            // SAFETY: every call below follows the source reader's documented contract, on
            // interfaces this function created and still holds
            unsafe {
                let reader = source_reader(source, None, AUDIO, "the media has no sound")?;
                let output = MFCreateMediaType().map_err(text)?;
                output
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                    .map_err(text)?;
                // the rate and channels stay the source's, the engine fits them to the speakers
                output
                    .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)
                    .map_err(text)?;
                reader
                    .SetCurrentMediaType(AUDIO, None, &output)
                    .map_err(text)?;
                let (rate, channels) = layout(&reader)?;
                let headers = headers(source);
                let shift = edit_shift(&headers, b"soun").unwrap_or(0.0);
                Ok(Self {
                    info: AudioInfo {
                        rate,
                        channels,
                        duration: fragmented_end(&headers, b"soun")
                            .map_or_else(|| duration(&reader), |end| end + shift),
                    },
                    reader,
                    shift,
                    _apartment: apartment,
                })
            }
        }
    }

    impl AudioReader for Sound {
        fn info(&self) -> AudioInfo {
            self.info
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            seek(&self.reader, seconds - self.shift)
        }

        fn read(&mut self) -> Result<Option<AudioChunk>, String> {
            let expected = (self.info.rate, self.info.channels);
            let changed = || match layout(&self.reader)? {
                layout if layout == expected => Ok(()),
                _ => Err("the media's sound changed its rate or channels".to_string()),
            };
            let Some((sample, time)) = read_sample(&self.reader, AUDIO, changed)? else {
                return Ok(None);
            };
            let mut samples = Vec::new();
            // SAFETY: the lock pins the buffer while its bytes are read, and is unlocked
            unsafe {
                let buffer = sample.ConvertToContiguousBuffer().map_err(text)?;
                let mut start = std::ptr::null_mut();
                let mut length = 0;
                buffer
                    .Lock(&mut start, None, Some(&mut length))
                    .map_err(text)?;
                let bytes = std::slice::from_raw_parts(start, length as usize);
                samples.extend(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|&sample| f32::from_ne_bytes(sample)),
                );
                buffer.Unlock().map_err(text)?;
            }
            Ok(Some(AudioChunk {
                time: time + self.shift,
                samples,
            }))
        }
    }
}

#[cfg(any(target_os = "windows", test))]
mod mp4 {
    use super::MediaSource;

    // the movie and fragment headers of an mp4, in file order, read until the boxes stop parsing
    pub(super) fn headers(source: &MediaSource) -> Vec<([u8; 4], Vec<u8>)> {
        use std::io::{Cursor, Read, Seek, SeekFrom};

        // a header this large is none worth reading whole
        const LIMIT: u64 = 64 << 20;

        fn walk(mut file: impl Read + Seek) -> Vec<([u8; 4], Vec<u8>)> {
            let mut found = Vec::new();
            let mut next = || -> Option<()> {
                let mut head = [0; 8];
                file.read_exact(&mut head).ok()?;
                let mut size = u64::from(u32::from_be_bytes(head[..4].try_into().ok()?));
                let mut header = 8;
                if size == 1 {
                    let mut large = [0; 8];
                    file.read_exact(&mut large).ok()?;
                    size = u64::from_be_bytes(large);
                    header = 16;
                }
                let body = size.checked_sub(header)?;
                let kind: [u8; 4] = head[4..].try_into().ok()?;
                if [*b"moov", *b"moof"].contains(&kind) {
                    if body > LIMIT {
                        return None;
                    }
                    let mut data = vec![0; body as usize];
                    file.read_exact(&mut data).ok()?;
                    found.push((kind, data));
                } else {
                    file.seek(SeekFrom::Current(i64::try_from(body).ok()?))
                        .ok()?;
                }
                Some(())
            };
            while next().is_some() {}
            found
        }

        match source {
            MediaSource::File(path) => std::fs::File::open(path).map_or_else(|_| Vec::new(), walk),
            MediaSource::Bytes { body, .. } => walk(Cursor::new(&body[..])),
        }
    }

    fn children(mut data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
        std::iter::from_fn(move || {
            let size = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
            let (kind, body) = (data.get(4..8)?, data.get(8..size)?);
            data = &data[size..];
            Some((kind, body))
        })
    }

    fn child<'a>(data: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
        children(data)
            .find(|(kind, _)| *kind == name)
            .map(|(_, body)| body)
    }

    fn number(data: &[u8], at: usize, wide: bool) -> Option<i64> {
        Some(if wide {
            i64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
        })
    }

    fn unsigned(data: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
    }

    // mvhd and mdhd put the timescale after a version byte, flags and two dates
    fn timescale(header: &[u8]) -> Option<f64> {
        let scale = unsigned(header, if header.first()? == &1 { 20 } else { 12 })?;
        (scale > 0).then_some(f64::from(scale))
    }

    fn movie(headers: &[([u8; 4], Vec<u8>)]) -> Option<&[u8]> {
        headers
            .iter()
            .find(|(kind, _)| kind == b"moov")
            .map(|(_, body)| &body[..])
    }

    fn track<'a>(moov: &'a [u8], handler: &[u8; 4]) -> Option<&'a [u8]> {
        children(moov)
            .filter(|(kind, _)| *kind == b"trak")
            .find(|(_, trak)| {
                child(trak, b"mdia")
                    .and_then(|mdia| child(mdia, b"hdlr"))
                    .and_then(|hdlr| hdlr.get(8..12))
                    == Some(handler)
            })
            .map(|(_, trak)| trak)
    }

    // media foundation's mp4 source ignores a track's edit list, which ffmpeg and chromium apply; an
    // x264 file with b-frames would otherwise start its pictures two frames late, and aac its sound
    // 1024 samples of encoder priming late
    pub(super) fn edit_shift(headers: &[([u8; 4], Vec<u8>)], handler: &[u8; 4]) -> Option<f64> {
        let moov = movie(headers)?;
        let movie_scale = timescale(child(moov, b"mvhd")?)?;
        let track = track(moov, handler)?;
        let media_scale = timescale(child(child(track, b"mdia")?, b"mdhd")?)?;
        let list = child(child(track, b"edts")?, b"elst")?;
        let wide = list.first()? == &1;
        let count = unsigned(list, 4)? as usize;
        let entry = if wide { 20 } else { 12 };
        let mut delay = 0.0;
        for index in 0..count {
            let at = 8 + index * entry;
            let duration = number(list, at, wide)?;
            let media_time = number(list, at + if wide { 8 } else { 4 }, wide)?;
            if media_time == -1 {
                delay += duration as f64 / movie_scale;
            } else {
                return Some(delay - media_time as f64 / media_scale);
            }
        }
        None
    }

    // media foundation dates a fragmented mp4's end at its last fragment's start plus one sample, so
    // the track's end is summed from its fragments as ffmpeg does
    pub(super) fn fragmented_end(headers: &[([u8; 4], Vec<u8>)], handler: &[u8; 4]) -> Option<f64> {
        let moov = movie(headers)?;
        let extends = child(moov, b"mvex")?;
        let track = track(moov, handler)?;
        let tkhd = child(track, b"tkhd")?;
        let id = unsigned(tkhd, if tkhd.first()? == &1 { 20 } else { 12 })?;
        let scale = timescale(child(child(track, b"mdia")?, b"mdhd")?)?;
        let fallback = children(extends)
            .filter(|(kind, _)| *kind == b"trex")
            .find(|(_, trex)| unsigned(trex, 4) == Some(id))
            .and_then(|(_, trex)| unsigned(trex, 12))
            .unwrap_or(0);
        let mut end = 0u64;
        let fragments = headers.iter().filter(|(kind, _)| kind == b"moof");
        for (_, moof) in fragments {
            for (_, traf) in children(moof).filter(|(kind, _)| *kind == b"traf") {
                let tfhd = child(traf, b"tfhd")?;
                if unsigned(tfhd, 4)? != id {
                    continue;
                }
                let flags = unsigned(tfhd, 0)? & 0xff_ffff;
                let at = 8 + 8 * usize::from(flags & 1 != 0) + 4 * usize::from(flags & 2 != 0);
                let default = if flags & 8 != 0 {
                    unsigned(tfhd, at)?
                } else {
                    fallback
                };
                if let Some(tfdt) = child(traf, b"tfdt") {
                    end = if tfdt.first()? == &1 {
                        number(tfdt, 4, true)?.max(0) as u64
                    } else {
                        u64::from(unsigned(tfdt, 4)?)
                    };
                }
                for (_, trun) in children(traf).filter(|(kind, _)| *kind == b"trun") {
                    let flags = unsigned(trun, 0)? & 0xff_ffff;
                    let count = u64::from(unsigned(trun, 4)?);
                    if flags & 0x100 == 0 {
                        end = end.saturating_add(count * u64::from(default));
                        continue;
                    }
                    let first =
                        8 + 4 * usize::from(flags & 1 != 0) + 4 * usize::from(flags & 4 != 0);
                    let stride = 4 * (flags & 0xf00).count_ones() as usize;
                    for sample in 0..count as usize {
                        let duration = unsigned(trun, first + sample * stride)?;
                        end = end.saturating_add(u64::from(duration));
                    }
                }
            }
        }
        (end > 0).then(|| end as f64 / scale)
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{
        AudioChunk, AudioInfo, AudioReader, MediaInfo, MediaSource, VideoFrame, VideoReader,
    };
    use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    pub(super) const TYPES: &[&str] = &[
        "video/mp4",
        "video/webm",
        "video/ogg",
        "video/x-matroska",
        "audio/mp4",
        "audio/mpeg",
        "audio/webm",
        "audio/ogg",
    ];

    const RTLD_NOW: c_int = 2;
    const STATE_NULL: c_int = 1;
    const STATE_PAUSED: c_int = 3;
    const STATE_PLAYING: c_int = 4;
    const STATE_CHANGE_FAILURE: c_int = 0;
    const FORMAT_TIME: c_int = 3;
    const SEEK_FLUSH: c_int = 1;
    const SEEK_ACCURATE: c_int = 2;
    const MAP_READ: c_int = 1;
    const MESSAGE_ERROR: c_int = 1 << 1;
    const CLOCK_TIME_NONE: u64 = u64::MAX;
    const NANOSECONDS: f64 = 1e9;
    // how long a pipeline may sit without a frame, an end or an error before it counts as stuck
    const STALL: Duration = Duration::from_secs(10);
    const POLL: u64 = 100_000_000;

    // videoscale squares anamorphic pixels, so the sink's size is the natural one
    const PICTURES: &CStr = c"filesrc name=source ! decodebin ! videoconvert ! videoscale ! \
        video/x-raw,format=BGRx,pixel-aspect-ratio=1/1 ! \
        appsink name=sink sync=false max-buffers=3 enable-last-sample=false";
    // the rate and channels stay the source's, the engine fits them to the speakers
    const SOUND: &CStr = c"filesrc name=source ! decodebin ! audioconvert ! \
        audio/x-raw,format=F32LE,layout=interleaved ! \
        appsink name=sink sync=false max-buffers=8 enable-last-sample=false";
    const SIZE: [&CStr; 2] = [c"width", c"height"];
    const LAYOUT: [&CStr; 2] = [c"rate", c"channels"];

    #[repr(C)]
    struct MiniObject {
        kind: usize,
        refcount: c_int,
        lockstate: c_int,
        flags: c_uint,
        copy: *mut c_void,
        dispose: *mut c_void,
        free: *mut c_void,
        priv_uint: c_uint,
        priv_pointer: *mut c_void,
    }

    // the head of GstBuffer, which gstreamer 1.x keeps stable
    #[repr(C)]
    struct Buffer {
        object: MiniObject,
        pool: *mut c_void,
        pts: u64,
    }

    #[repr(C)]
    struct MapInfo {
        memory: *mut c_void,
        flags: c_int,
        data: *const u8,
        size: usize,
        maxsize: usize,
        user_data: [*mut c_void; 4],
        reserved: [*mut c_void; 4],
    }

    #[repr(C)]
    struct GError {
        domain: u32,
        code: c_int,
        message: *const c_char,
    }

    type Object = *mut c_void;

    // libgstreamer and libgstapp, loaded at first use so a system without them still runs,
    // and only its videos fail; each name is the C function the field calls
    struct Gst {
        gst_init_check: unsafe extern "C" fn(*mut c_int, *mut c_void, *mut *mut GError) -> c_int,
        gst_parse_launch: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> Object,
        gst_bin_get_by_name: unsafe extern "C" fn(Object, *const c_char) -> Object,
        g_object_set: unsafe extern "C" fn(Object, *const c_char, ...),
        g_error_free: unsafe extern "C" fn(*mut GError),
        g_free: unsafe extern "C" fn(*mut c_void),
        gst_object_unref: unsafe extern "C" fn(Object),
        gst_mini_object_unref: unsafe extern "C" fn(Object),
        gst_element_set_state: unsafe extern "C" fn(Object, c_int) -> c_int,
        gst_element_get_state: unsafe extern "C" fn(Object, *mut c_int, *mut c_int, u64) -> c_int,
        gst_element_query_duration: unsafe extern "C" fn(Object, c_int, *mut i64) -> c_int,
        gst_element_seek_simple: unsafe extern "C" fn(Object, c_int, c_int, i64) -> c_int,
        gst_element_get_bus: unsafe extern "C" fn(Object) -> Object,
        gst_element_get_static_pad: unsafe extern "C" fn(Object, *const c_char) -> Object,
        gst_pad_get_current_caps: unsafe extern "C" fn(Object) -> Object,
        gst_bus_pop_filtered: unsafe extern "C" fn(Object, c_int) -> Object,
        gst_message_parse_error: unsafe extern "C" fn(Object, *mut *mut GError, *mut *mut c_char),
        gst_app_sink_try_pull_sample: unsafe extern "C" fn(Object, u64) -> Object,
        gst_app_sink_is_eos: unsafe extern "C" fn(Object) -> c_int,
        gst_sample_get_buffer: unsafe extern "C" fn(Object) -> *mut Buffer,
        gst_sample_get_caps: unsafe extern "C" fn(Object) -> Object,
        gst_caps_get_structure: unsafe extern "C" fn(Object, c_uint) -> Object,
        gst_structure_get_int: unsafe extern "C" fn(Object, *const c_char, *mut c_int) -> c_int,
        gst_buffer_map: unsafe extern "C" fn(*mut Buffer, *mut MapInfo, c_int) -> c_int,
        gst_buffer_unmap: unsafe extern "C" fn(*mut Buffer, *mut MapInfo),
    }

    unsafe extern "C" {
        fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(library: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    macro_rules! load {
        ($library:expr, $($name:ident),* $(,)?) => {{
            let library = $library;
            Gst {
                $($name: {
                    let symbol = dlsym(library, concat!(stringify!($name), "\0").as_ptr().cast());
                    if symbol.is_null() {
                        return None;
                    }
                    // the field the struct declares gives the signature
                    #[allow(clippy::missing_transmute_annotations)]
                    let function = std::mem::transmute::<*mut c_void, _>(symbol);
                    function
                },)*
            }
        }};
    }

    fn gst() -> Result<&'static Gst, String> {
        static GST: OnceLock<Option<Gst>> = OnceLock::new();
        GST.get_or_init(|| {
            // SAFETY: the libraries stay loaded for the life of the process, dlsym on gstapp
            // also finds what it links (gstreamer, gobject, glib), and each symbol is cast to
            // the signature its header declares
            unsafe {
                if dlopen(c"libgstreamer-1.0.so.0".as_ptr(), RTLD_NOW).is_null() {
                    return None;
                }
                let library = dlopen(c"libgstapp-1.0.so.0".as_ptr(), RTLD_NOW);
                if library.is_null() {
                    return None;
                }
                let gst = load!(
                    library,
                    gst_init_check,
                    gst_parse_launch,
                    gst_bin_get_by_name,
                    g_object_set,
                    g_error_free,
                    g_free,
                    gst_object_unref,
                    gst_mini_object_unref,
                    gst_element_set_state,
                    gst_element_get_state,
                    gst_element_query_duration,
                    gst_element_seek_simple,
                    gst_element_get_bus,
                    gst_element_get_static_pad,
                    gst_pad_get_current_caps,
                    gst_bus_pop_filtered,
                    gst_message_parse_error,
                    gst_app_sink_try_pull_sample,
                    gst_app_sink_is_eos,
                    gst_sample_get_buffer,
                    gst_sample_get_caps,
                    gst_caps_get_structure,
                    gst_structure_get_int,
                    gst_buffer_map,
                    gst_buffer_unmap,
                );
                let mut error = std::ptr::null_mut();
                if (gst.gst_init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) == 0
                {
                    if !error.is_null() {
                        (gst.g_error_free)(error);
                    }
                    return None;
                }
                Some(gst)
            }
        })
        .as_ref()
        .ok_or_else(|| "gstreamer is not installed".to_string())
    }

    // a pipeline from the source to one appsink, prerolled so the sink's caps are settled
    struct Pipeline {
        gst: &'static Gst,
        pipeline: Object,
        sink: Object,
        bus: Object,
        // bytes handed over in memory, written out for filesrc and removed with the pipeline
        spilled: Option<PathBuf>,
    }

    // SAFETY: gstreamer objects are thread safe, and the engine uses a reader on one thread
    unsafe impl Send for Pipeline {}

    impl Pipeline {
        fn open(source: &MediaSource, description: &CStr) -> Result<Self, String> {
            let gst = gst()?;
            let (path, spilled) = match source {
                MediaSource::File(path) => (path.clone(), None),
                MediaSource::Bytes { body, .. } => {
                    static SPILLS: AtomicU64 = AtomicU64::new(0);
                    let path = std::env::temp_dir().join(format!(
                        "ceri-media-{}-{}",
                        std::process::id(),
                        SPILLS.fetch_add(1, Ordering::Relaxed)
                    ));
                    std::fs::write(&path, body).map_err(|error| error.to_string())?;
                    (path.clone(), Some(path))
                }
            };
            let location = CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| "the media path holds a nul byte".to_string())?;
            // SAFETY: each object is checked before use and released by Drop, which runs as
            // soon as the pipeline exists
            unsafe {
                let mut error = std::ptr::null_mut();
                let pipeline = (gst.gst_parse_launch)(description.as_ptr(), &mut error);
                if !error.is_null() {
                    let message = take_error(gst, error);
                    if !pipeline.is_null() {
                        (gst.gst_object_unref)(pipeline);
                    }
                    if let Some(path) = &spilled {
                        let _ = std::fs::remove_file(path);
                    }
                    return Err(message);
                }
                let opened = Self {
                    gst,
                    pipeline,
                    sink: (gst.gst_bin_get_by_name)(pipeline, c"sink".as_ptr()),
                    bus: (gst.gst_element_get_bus)(pipeline),
                    spilled,
                };
                let source = (gst.gst_bin_get_by_name)(pipeline, c"source".as_ptr());
                if source.is_null() || opened.sink.is_null() || opened.bus.is_null() {
                    return Err("gstreamer could not build the pipeline".to_string());
                }
                (gst.g_object_set)(
                    source,
                    c"location".as_ptr(),
                    location.as_ptr(),
                    std::ptr::null::<c_char>(),
                );
                (gst.gst_object_unref)(source);
                if (gst.gst_element_set_state)(pipeline, STATE_PAUSED) == STATE_CHANGE_FAILURE
                    || (gst.gst_element_get_state)(
                        pipeline,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        STALL.as_nanos() as u64,
                    ) == STATE_CHANGE_FAILURE
                {
                    return Err(opened
                        .error()
                        .unwrap_or_else(|| "gstreamer could not open the media".to_string()));
                }
                Ok(opened)
            }
        }

        // two whole fields of the caps the sink agreed on
        fn negotiated(&self, names: [&CStr; 2]) -> Option<(u32, u32)> {
            let gst = self.gst;
            // SAFETY: the sink is alive until Drop, and the pad and caps are released
            unsafe {
                let pad = (gst.gst_element_get_static_pad)(self.sink, c"sink".as_ptr());
                if pad.is_null() {
                    return None;
                }
                let caps = (gst.gst_pad_get_current_caps)(pad);
                (gst.gst_object_unref)(pad);
                if caps.is_null() {
                    return None;
                }
                let fields = caps_fields(gst, caps, names);
                (gst.gst_mini_object_unref)(caps);
                fields
            }
        }

        fn duration(&self) -> f64 {
            let mut duration = 0;
            // SAFETY: the pipeline is alive until Drop
            let known = unsafe {
                (self.gst.gst_element_query_duration)(self.pipeline, FORMAT_TIME, &mut duration)
            };
            if known != 0 && duration >= 0 {
                duration as f64 / NANOSECONDS
            } else {
                f64::NAN
            }
        }

        fn play(&self) -> Result<(), String> {
            // SAFETY: the pipeline is alive until Drop
            if unsafe { (self.gst.gst_element_set_state)(self.pipeline, STATE_PLAYING) }
                == STATE_CHANGE_FAILURE
            {
                return Err(self
                    .error()
                    .unwrap_or_else(|| "gstreamer could not play the media".to_string()));
            }
            Ok(())
        }

        fn error(&self) -> Option<String> {
            let gst = self.gst;
            // SAFETY: the bus is alive until Drop, and the message and its parts are released
            unsafe {
                let message = (gst.gst_bus_pop_filtered)(self.bus, MESSAGE_ERROR);
                if message.is_null() {
                    return None;
                }
                let mut error = std::ptr::null_mut();
                let mut debug = std::ptr::null_mut();
                (gst.gst_message_parse_error)(message, &mut error, &mut debug);
                if !debug.is_null() {
                    (gst.g_free)(debug.cast());
                }
                (gst.gst_mini_object_unref)(message);
                Some(take_error(gst, error))
            }
        }

        fn seek(&self, seconds: f64) -> Result<(), String> {
            let nanoseconds = (seconds.max(0.0) * NANOSECONDS) as i64;
            // SAFETY: the pipeline is alive until Drop
            let sought = unsafe {
                (self.gst.gst_element_seek_simple)(
                    self.pipeline,
                    FORMAT_TIME,
                    SEEK_FLUSH | SEEK_ACCURATE,
                    nanoseconds,
                )
            };
            if sought == 0 {
                return Err("gstreamer could not seek".to_string());
            }
            Ok(())
        }

        // the next sample's caps, time and bytes, none at the end
        fn pull<T>(
            &self,
            take: impl FnOnce(Object, Option<f64>, &[u8]) -> Result<T, String>,
        ) -> Result<Option<T>, String> {
            let gst = self.gst;
            let started = Instant::now();
            // SAFETY: the sink and bus are alive until Drop, each sample is released, and the
            // mapped bytes are read only while mapped
            unsafe {
                loop {
                    let sample = (gst.gst_app_sink_try_pull_sample)(self.sink, POLL);
                    if !sample.is_null() {
                        let buffer = (gst.gst_sample_get_buffer)(sample);
                        let mut map = std::mem::zeroed::<MapInfo>();
                        let taken = if buffer.is_null()
                            || (gst.gst_buffer_map)(buffer, &mut map, MAP_READ) == 0
                        {
                            Err("gstreamer gave a sample it could not map".to_string())
                        } else {
                            let pts = (*buffer).pts;
                            let time = (pts != CLOCK_TIME_NONE).then(|| pts as f64 / NANOSECONDS);
                            let bytes = match map.size {
                                0 => &[][..],
                                size => std::slice::from_raw_parts(map.data, size),
                            };
                            let taken = take((gst.gst_sample_get_caps)(sample), time, bytes);
                            (gst.gst_buffer_unmap)(buffer, &mut map);
                            taken
                        };
                        (gst.gst_mini_object_unref)(sample);
                        return taken.map(Some);
                    }
                    if (gst.gst_app_sink_is_eos)(self.sink) != 0 {
                        return Ok(None);
                    }
                    if let Some(error) = self.error() {
                        return Err(error);
                    }
                    if started.elapsed() > STALL {
                        return Err("gstreamer stopped giving samples".to_string());
                    }
                }
            }
        }
    }

    impl Drop for Pipeline {
        fn drop(&mut self) {
            // SAFETY: each object is released once, after the pipeline stops using them
            unsafe {
                if !self.pipeline.is_null() {
                    (self.gst.gst_element_set_state)(self.pipeline, STATE_NULL);
                }
                for object in [self.sink, self.bus, self.pipeline] {
                    if !object.is_null() {
                        (self.gst.gst_object_unref)(object);
                    }
                }
            }
            if let Some(path) = &self.spilled {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    // two whole, positive fields of a caps' first structure
    unsafe fn caps_fields(gst: &Gst, caps: Object, names: [&CStr; 2]) -> Option<(u32, u32)> {
        unsafe {
            if caps.is_null() {
                return None;
            }
            let structure = (gst.gst_caps_get_structure)(caps, 0);
            if structure.is_null() {
                return None;
            }
            let mut values = [0; 2];
            for (name, value) in names.iter().zip(&mut values) {
                if (gst.gst_structure_get_int)(structure, name.as_ptr(), value) == 0 || *value <= 0
                {
                    return None;
                }
            }
            Some((values[0] as u32, values[1] as u32))
        }
    }

    unsafe fn take_error(gst: &Gst, error: *mut GError) -> String {
        unsafe {
            if error.is_null() {
                return "gstreamer failed without saying why".to_string();
            }
            let message = CStr::from_ptr((*error).message)
                .to_string_lossy()
                .into_owned();
            (gst.g_error_free)(error);
            message
        }
    }

    pub(super) struct Reader {
        pipeline: Pipeline,
        info: MediaInfo,
        last: f64,
    }

    impl Reader {
        pub(super) fn open(source: &MediaSource) -> Result<Self, String> {
            let pipeline = Pipeline::open(source, PICTURES)?;
            let (width, height) = pipeline
                .negotiated(SIZE)
                .ok_or("the media has no video stream gstreamer can decode")?;
            let info = MediaInfo {
                duration: pipeline.duration(),
                width,
                height,
            };
            pipeline.play()?;
            Ok(Self {
                pipeline,
                info,
                last: 0.0,
            })
        }
    }

    impl VideoReader for Reader {
        fn info(&self) -> MediaInfo {
            self.info
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            self.pipeline.seek(seconds)
        }

        fn read(&mut self) -> Result<Option<VideoFrame>, String> {
            let gst = self.pipeline.gst;
            let last = &mut self.last;
            self.pipeline.pull(|caps, time, bytes| {
                // SAFETY: the caps belong to the sample pull still holds
                let (width, height) = unsafe { caps_fields(gst, caps, SIZE) }
                    .ok_or("gstreamer gave a frame without a size")?;
                if let Some(time) = time {
                    *last = time;
                }
                let row = width as usize * 4;
                let rows = height as usize;
                let pitch = bytes.len().checked_div(rows).unwrap_or(0);
                if pitch < row {
                    return Err("gstreamer gave a frame smaller than its picture".to_string());
                }
                let mut bgra = Vec::with_capacity(row * rows);
                for line in bytes.chunks(pitch).take(rows) {
                    bgra.extend_from_slice(&line[..row]);
                }
                Ok(VideoFrame {
                    time: *last,
                    width,
                    height,
                    bgra,
                    surface: None,
                })
            })
        }
    }

    pub(super) struct Sound {
        pipeline: Pipeline,
        info: AudioInfo,
        // where the next chunk starts when its buffer carries no time
        next: f64,
    }

    impl Sound {
        pub(super) fn open(source: &MediaSource) -> Result<Self, String> {
            let pipeline = Pipeline::open(source, SOUND)?;
            let (rate, channels) = pipeline
                .negotiated(LAYOUT)
                .ok_or("the media has no sound gstreamer can decode")?;
            let info = AudioInfo {
                rate,
                channels: u16::try_from(channels)
                    .map_err(|_| format!("the media's sound has {channels} channels"))?,
                duration: pipeline.duration(),
            };
            pipeline.play()?;
            Ok(Self {
                pipeline,
                info,
                next: 0.0,
            })
        }
    }

    impl AudioReader for Sound {
        fn info(&self) -> AudioInfo {
            self.info
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            self.pipeline.seek(seconds)
        }

        fn read(&mut self) -> Result<Option<AudioChunk>, String> {
            let gst = self.pipeline.gst;
            let info = self.info;
            let next = &mut self.next;
            self.pipeline.pull(|caps, time, bytes| {
                // SAFETY: the caps belong to the sample pull still holds
                let layout = unsafe { caps_fields(gst, caps, LAYOUT) };
                if layout != Some((info.rate, u32::from(info.channels))) {
                    return Err("the media's sound changed its rate or channels".to_string());
                }
                let time = time.unwrap_or(*next);
                let samples: Vec<f32> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&sample| f32::from_le_bytes(sample))
                    .collect();
                *next = time
                    + (samples.len() / usize::from(info.channels)) as f64 / f64::from(info.rate);
                Ok(AudioChunk { time, samples })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DesktopVideo;
    use ceri_engine::{MediaDecoder, MediaSource};
    use std::path::PathBuf;

    // ten 64x32 frames at ten a second, key frames at 0 and 0.5 s, frame n grey at luma
    // 16 + 10n, so its rgb level is about 11.6n
    fn reel() -> PathBuf {
        fixture("reel")
    }

    fn fixture(stem: &str) -> PathBuf {
        let extension = if cfg!(target_os = "windows") {
            "mp4"
        } else {
            "webm"
        };
        [
            env!("CARGO_MANIFEST_DIR"),
            "tests",
            "fixtures",
            &format!("{stem}.{extension}"),
        ]
        .iter()
        .collect()
    }

    // the reel's pictures over a mono 48 kHz sound: half a second of silence, then a 440 Hz
    // tone at half scale, aac in the mp4 and opus in the webm
    fn voiced() -> PathBuf {
        fixture("voiced")
    }

    // each sample with its media time
    fn timed(reader: &mut dyn ceri_engine::AudioReader) -> Vec<(f64, f32)> {
        let rate = f64::from(reader.info().rate);
        let mut timed = Vec::new();
        while let Some(chunk) = reader.read().expect("sound should decode") {
            timed.extend(
                chunk
                    .samples
                    .iter()
                    .enumerate()
                    .map(|(index, &sample)| (chunk.time + index as f64 / rate, sample)),
            );
        }
        timed
    }

    fn loudness(timed: &[(f64, f32)], from: f64, to: f64) -> f32 {
        let window: Vec<f32> = timed
            .iter()
            .filter(|(time, _)| (from..to).contains(time))
            .map(|&(_, sample)| sample)
            .collect();
        assert!(!window.is_empty(), "no sound between {from} and {to}");
        (window.iter().map(|sample| sample * sample).sum::<f32>() / window.len() as f32).sqrt()
    }

    #[test]
    fn a_file_decodes_to_timed_sound_from_where_it_seeks() {
        let mut sound = DesktopVideo
            .open_audio(&MediaSource::File(voiced()))
            .expect("the fixture's sound should open");
        let info = sound.info();
        assert_eq!((info.rate, info.channels), (48_000, 1));
        assert!((info.duration - 1.0).abs() < 0.05, "{}", info.duration);
        let whole = timed(&mut *sound);
        // half scale passes a quarter of it 0.19 ms into the tone
        let onset = whole
            .iter()
            .find(|(_, sample)| sample.abs() > 0.25)
            .map_or(f64::NAN, |&(time, _)| time);
        assert!((onset - 0.5002).abs() < 0.002, "{onset}");
        assert!(loudness(&whole, 0.0, 0.45) < 0.02);
        let tone = loudness(&whole, 0.55, 0.95);
        assert!((tone - 0.354).abs() < 0.06, "{tone}");
        let end = whole.last().map_or(f64::NAN, |&(time, _)| time);
        assert!((end - 1.0).abs() < 0.03, "{end}");

        sound.seek(0.7).expect("the seek should land");
        let after = timed(&mut *sound);
        let start = after.first().map_or(f64::NAN, |&(time, _)| time);
        assert!((0.5..=0.7).contains(&start), "{start}");
        assert!((loudness(&after, 0.7, 0.9) - 0.354).abs() < 0.06);
    }

    #[test]
    fn pictures_decode_past_a_sound_and_no_sound_fails_fast() {
        let mut reader = DesktopVideo
            .open(&MediaSource::File(voiced()))
            .expect("the fixture's pictures should open");
        let mut frames = 0;
        while reader.read().expect("frames should decode").is_some() {
            frames += 1;
        }
        assert_eq!(frames, 10);
        let started = std::time::Instant::now();
        assert!(DesktopVideo.open_audio(&MediaSource::File(reel())).is_err());
        assert!(started.elapsed().as_secs_f64() < 2.0);
    }

    // a gpu decoder's frame is read back from its surface
    fn rows(frame: ceri_engine::VideoFrame) -> Vec<u8> {
        match frame.surface {
            Some(surface) => surface.read().expect("a surface should read back"),
            None => frame.bgra,
        }
    }

    // the files and app origins hand over canonical paths, verbatim on windows
    #[test]
    fn canonical_paths_open() {
        let canonical = |path: PathBuf| MediaSource::File(path.canonicalize().expect("a fixture"));
        assert!(DesktopVideo.open(&canonical(reel())).is_ok());
        assert!(DesktopVideo.open_audio(&canonical(voiced())).is_ok());
    }

    #[test]
    fn a_file_decodes_to_timed_frames_of_its_natural_size() {
        let mut reader = DesktopVideo
            .open(&MediaSource::File(reel()))
            .expect("the fixture should open");
        let info = reader.info();
        assert_eq!((info.width, info.height), (64, 32));
        assert!((info.duration - 1.0).abs() < 0.05, "{}", info.duration);
        let mut frames = Vec::new();
        while let Some(frame) = reader.read().expect("frames should decode") {
            let (time, size) = (frame.time, (frame.width, frame.height));
            let bgra = rows(frame);
            assert_eq!((size, bgra.len()), ((64, 32), 64 * 32 * 4));
            frames.push((time, bgra[(16 * 64 + 32) * 4 + 2]));
        }
        assert_eq!(frames.len(), 10, "{frames:?}");
        for (index, (time, level)) in frames.iter().enumerate() {
            assert!((time - index as f64 / 10.0).abs() < 0.01, "{frames:?}");
            let expected = index as f64 * 11.6;
            assert!((f64::from(*level) - expected).abs() < 8.0, "{frames:?}");
        }
    }

    #[test]
    fn a_seek_decodes_on_from_at_or_before_its_target() {
        let body = std::fs::read(reel()).expect("the fixture should read");
        let mut reader = DesktopVideo
            .open(&MediaSource::Bytes {
                body: body.into(),
                mime_type: "video/mp4".to_string(),
            })
            .expect("the fixture should open from memory");
        reader.seek(0.7).expect("the seek should land");
        let mut times = Vec::new();
        while let Some(frame) = reader.read().expect("frames should decode") {
            times.push(frame.time);
        }
        let near = |target: f64| times.iter().any(|time| (time - target).abs() < 0.01);
        assert!(times[0] <= 0.71 && near(0.7) && near(0.9), "{times:?}");
        assert!((times[times.len() - 1] - 0.9).abs() < 0.01, "{times:?}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn anamorphic_pixels_stretch_the_natural_size() {
        use super::platform::natural_size;

        assert_eq!(natural_size(720, 480, (32, 27)), (853, 480));
        assert_eq!(natural_size(720, 480, (8, 9)), (720, 540));
        assert_eq!(natural_size(64, 32, (0, 0)), (64, 32));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn gpu_frames_match_the_software_decoders() {
        use super::platform::{Reader, gpu};
        use ceri_engine::VideoReader;

        let Some(gpu) = gpu() else {
            return;
        };
        let frames = |gpu| {
            let mut reader =
                Reader::open_on(&MediaSource::File(reel()), gpu).expect("the fixture should open");
            std::iter::from_fn(|| reader.read().expect("frames should decode"))
                .map(|frame| (frame.time, rows(frame)))
                .collect::<Vec<_>>()
        };
        let (hardware, software) = (frames(Some(gpu)), frames(None));
        assert_eq!(hardware.len(), software.len());
        for ((time, gpu), (expected, cpu)) in hardware.iter().zip(&software) {
            assert!((time - expected).abs() < 1e-6, "{time} {expected}");
            // rgb32 leaves alpha undefined, the engine reads only the colour
            let apart = gpu
                .as_chunks::<4>()
                .0
                .iter()
                .zip(cpu.as_chunks::<4>().0)
                .flat_map(|(a, b)| (0..3).map(|channel| a[channel].abs_diff(b[channel])))
                .max()
                .unwrap_or(u8::MAX);
            assert!(apart <= 2, "{time}: {apart}");
        }
    }

    // ten 256x144 frames of hevc main10 in bt.2100 pq, eight upright bands 32 wide at 10-bit
    // luma 118, 195, 333, 509, 572, 635, 722 and 854: 0.1, 1, 10, 100, 203, 400, 1000 and
    // 4000 nits
    #[cfg(target_os = "windows")]
    #[test]
    fn hdr_frames_tone_map_to_sdr_as_chromium_does() {
        use super::platform::{Reader, gpu};
        use ceri_engine::VideoReader;

        let Some(gpu) = gpu() else {
            return;
        };
        let pq = MediaSource::File(
            [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "pq.mp4"]
                .iter()
                .collect(),
        );
        let Ok(mut reader) = Reader::open_on(&pq, Some(gpu)) else {
            // windows decodes hevc only with its video extension installed
            assert!(
                Reader::open_on(&pq, None).is_err(),
                "hevc decodes off the gpu only"
            );
            return;
        };
        let mut surface = || {
            let frame = reader
                .read()
                .expect("frames should decode")
                .expect("a frame");
            frame.surface.expect("a gpu frame comes as a surface")
        };
        let sdr = surface();
        assert!(!sdr.hdr10());
        sdr.show_hdr(true);
        let hdr = surface();
        assert!(
            hdr.hdr10(),
            "a pq source on a gpu that maps it hands over hdr10"
        );
        let bands = |bgra: Vec<u8>| -> Vec<[u8; 3]> {
            (0..8)
                .map(|band| {
                    let at = (72 * 256 + band * 32 + 16) * 4;
                    [bgra[at + 2], bgra[at + 1], bgra[at]]
                })
                .collect()
        };
        // edge reads the same bands back through a canvas as these, give or take its rounding
        let chromium = [1, 8, 45, 135, 185, 223, 254, 255];
        for picture in [sdr, hdr] {
            let read = bands(picture.read().expect("a picture reads back"));
            let near = read.iter().zip(chromium).all(|(band, expected)| {
                band.iter().all(|&channel| channel.abs_diff(expected) <= 4)
            });
            assert!(near, "hdr10 {}: {read:?}", picture.hdr10());
        }
    }

    #[test]
    fn an_edit_list_moves_pictures_back_to_zero() {
        let source = |body: &[u8]| MediaSource::Bytes {
            body: body.into(),
            mime_type: "video/mp4".to_string(),
        };
        let reel = std::fs::read(reel()).expect("the fixture should read");
        let edited = std::fs::read(
            [
                env!("CARGO_MANIFEST_DIR"),
                "tests",
                "fixtures",
                "edited.mp4",
            ]
            .iter()
            .collect::<PathBuf>(),
        )
        .expect("the fixture should read");
        let shift = super::mp4::edit_shift(&super::mp4::headers(&source(&edited)), b"vide")
            .expect("the file has an edit list");
        assert!((shift + 2.0 / 30.0).abs() < 1e-6, "{shift}");
        if cfg!(target_os = "windows") {
            assert_eq!(
                super::mp4::edit_shift(&super::mp4::headers(&source(&reel)), b"vide"),
                Some(0.0)
            );
            let mut reader = DesktopVideo
                .open(&source(&edited))
                .expect("the fixture should open");
            let first = reader
                .read()
                .expect("frames should decode")
                .expect("a frame");
            assert!(first.time.abs() < 1e-6, "{}", first.time);
        }
        assert_eq!(
            super::mp4::edit_shift(
                &super::mp4::headers(&source(b"\x1a\x45\xdf\xa3 not a movie")),
                b"vide"
            ),
            None
        );
    }

    // a second of the reel's first ten pictures at 64x48 in fragments of four, four and two
    #[test]
    fn a_fragmented_movie_lasts_to_its_last_fragments_end() {
        let fragmented = MediaSource::File(
            [
                env!("CARGO_MANIFEST_DIR"),
                "tests",
                "fixtures",
                "fragmented.mp4",
            ]
            .iter()
            .collect(),
        );
        let headers = super::mp4::headers(&fragmented);
        assert_eq!(super::mp4::fragmented_end(&headers, b"vide"), Some(1.0));
        assert_eq!(super::mp4::fragmented_end(&headers, b"soun"), None);
        let plain = super::mp4::headers(&MediaSource::File(reel()));
        assert_eq!(super::mp4::fragmented_end(&plain, b"vide"), None);
        if cfg!(target_os = "windows") {
            let duration = DesktopVideo
                .open(&fragmented)
                .expect("the fixture should open")
                .info()
                .duration;
            assert!((duration - 1.0).abs() < 1e-6, "{duration}");
        }
    }

    #[test]
    fn can_play_type_answers_maybe_for_containers_it_reads() {
        assert_eq!(
            DesktopVideo.can_play("video/mp4; codecs=\"avc1.42E01E\""),
            "maybe"
        );
        assert_eq!(DesktopVideo.can_play("VIDEO/WEBM"), "maybe");
        assert_eq!(DesktopVideo.can_play("audio/mpeg"), "maybe");
        assert_eq!(DesktopVideo.can_play("video/x-unknown"), "");
    }
}
