//! 原生 Media Foundation → D3D11 / D3D12 零拷贝 GPU 路径（仅 Windows）。
//!
//! nokhwa 0.10 的 MSMF 后端把 `IMFMediaBuffer` 锁进 CPU 内存并拷贝成字节，
//! 无法提供 GPU 纹理。这里自己建 `IMFSourceReader` + `IMFDXGIDeviceManager`：
//!
//! ```text
//! MF 摄像头 → IMFSample → IMFDXGIBuffer::GetResource → ID3D11Texture2D
//!                                                      ├─ 直接作为 D3D11 纹理使用（零 CPU 拷贝）
//!                                                      └─ IDXGIResource1::CreateSharedHandle
//!                                                           → ID3D12Device::OpenSharedHandle
//!                                                           → ID3D12Resource（跨 API 零拷贝）
//! ```
//!
//! 对应 Chromium `media/capture/video/win/video_capture_device_mf_win.cc`
//! 的 `GetTextureFromMFBuffer()` 与 `DeliverExternalBufferToClient()`。
//!
//! # 安全边界
//!
//! 本模块是 crate 里唯一允许 `unsafe` 的地方（COM / D3D 互操作）。
//! 所有裸指针都封装在 RAII 类型内，对外只暴露安全 API。
//! 设备侧不支持 NV12 纹理或无法导出共享句柄时会返回错误，不会退回静默 CPU 拷贝。

use std::mem::MaybeUninit;
use std::ptr;
use std::sync::Once;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::core::{Interface, GUID};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_RESOURCE_MISC_FLAG, D3D11_RESOURCE_MISC_RESTRICT_SHARED_RESOURCE,
    D3D11_RESOURCE_MISC_RESTRICT_SHARED_RESOURCE_DRIVER, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
};
use windows::Win32::Graphics::Direct3D12::{D3D12CreateDevice, ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice2, IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE,
};
use windows::Win32::Media::MediaFoundation::{
    IMFDXGIBuffer, IMFDXGIDeviceManager, IMFExtendedCameraControl, IMFExtendedCameraController,
    IMFGetService, IMFMediaBuffer, IMFMediaSource, IMFSourceReader, MFCreateAttributes,
    MFCreateDXGIDeviceManager, MFCreateDeviceSource, MFCreateSourceReaderFromMediaSource,
    MFStartup, MFVideoFormat_NV12, MFSTARTUP_LITE, MF_CAPTURE_ENGINE_MEDIASOURCE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_E_NO_MORE_TYPES, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_SUBTYPE, MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_D3D_MANAGER,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::controls::{ExtendedControl, ExtendedControlSetting};
use crate::device::{
    CameraError, DeviceCapabilities, DeviceInfo, FormatInfo, PixelFormat, Resolution, Result,
};

/// `MF_SOURCE_READER_FIRST_VIDEO_STREAM` 是 -4；MF API 以 `u32` 接收该常量。
const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

fn dx_error(context: &str, error: windows::core::Error) -> CameraError {
    CameraError::Dx(format!("{context}: {error}"))
}

/// 一帧原生 NV12 GPU 纹理（零 CPU 拷贝）。
pub struct DxFrame {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    timestamp: Duration,
    shared_handle: Option<HANDLE>,
    /// 纹理是否带跨进程共享所需的标志（Chromium `GetTextureInfo()` 的检查）。
    cross_api_shareable: bool,
}

impl DxFrame {
    /// 底层 D3D11 纹理。NV12 是两个子资源：子资源 0 = Y，子资源 1 = 交错 UV。
    #[must_use]
    pub const fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// MF 源端时间戳（100ns 单位换算为 `Duration`）。
    #[must_use]
    pub const fn timestamp(&self) -> Duration {
        self.timestamp
    }

    /// Y 平面子资源索引（NV12 固定为 0）。
    #[must_use]
    pub const fn y_subresource() -> u32 {
        0
    }

    /// UV 平面子资源索引（NV12 固定为 1）。
    #[must_use]
    pub const fn uv_subresource() -> u32 {
        1
    }

    /// 纹理是否带 `SHARED_NTHANDLE` 且未被 `RESTRICT_SHARED_RESOURCE` 限制，
    /// 即能否跨 API / 跨进程共享给 D3D12（Chromium `GetTextureInfo()` 的判定）。
    #[must_use]
    pub const fn is_cross_api_shareable(&self) -> bool {
        self.cross_api_shareable
    }

    /// 等待 D3D11 侧对该纹理的所有 GPU 操作完成。
    ///
    /// Chromium `CopyTextureToGpuMemoryBuffer()` (mf_win.cc:730-743) 用
    /// `IDXGIDevice2::EnqueueSetEvent` + `WaitForSingleObject` 做跨设备屏障——
    /// D3D11Fence 在跨设备/跨进程场景下不可靠，必须用事件同步。
    pub fn wait_for_gpu_completion(&self) -> Result<()> {
        let d3d11_device = unsafe { self.texture.GetDevice() }
            .map_err(|error| dx_error("ID3D11Texture2D::GetDevice", error))?;
        let dxgi_device: IDXGIDevice2 = d3d11_device
            .cast()
            .map_err(|error| dx_error("query IDXGIDevice2", error))?;
        let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map_err(|error| dx_error("CreateEventW", error))?;
        let wait_result = unsafe {
            dxgi_device
                .EnqueueSetEvent(event)
                .map_err(|error| dx_error("EnqueueSetEvent", error))?;
            WaitForSingleObject(event, u32::MAX)
        };
        let _ = unsafe { CloseHandle(event) };
        if wait_result != WAIT_OBJECT_0 {
            return Err(CameraError::Dx(format!(
                "D3D11 GPU completion wait failed: {wait_result:?}"
            )));
        }
        Ok(())
    }

    /// 导出 NT 共享句柄（惰性，缓存复用）。D3D12 导入用。
    pub fn shared_handle(&mut self) -> Result<HANDLE> {
        if let Some(handle) = self.shared_handle {
            return Ok(handle);
        }
        let resource: IDXGIResource1 = self
            .texture
            .cast()
            .map_err(|error| dx_error("query IDXGIResource1", error))?;
        let access = DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0;
        let handle =
            unsafe { resource.CreateSharedHandle(None, access, windows::core::PCWSTR::null()) }
                .map_err(|error| dx_error("CreateSharedHandle", error))?;
        self.shared_handle = Some(handle);
        Ok(handle)
    }

    /// 把同一块显存导入为 D3D12 资源（不拷贝像素）。
    ///
    /// 导入前会先做 GPU 完成屏障（`wait_for_gpu_completion`），确保 D3D11 侧
    /// 不再写这块纹理，避免 D3D12 侧读到半成品。
    pub fn import_to_dx12(&mut self, device: &ID3D12Device) -> Result<ID3D12Resource> {
        if !self.cross_api_shareable {
            return Err(CameraError::Dx(
                "texture lacks D3D11_RESOURCE_MISC_SHARED_NTHANDLE or is restricted from sharing"
                    .to_owned(),
            ));
        }
        self.wait_for_gpu_completion()?;
        let handle = self.shared_handle()?;
        let mut resource: Option<ID3D12Resource> = None;
        unsafe { device.OpenSharedHandle(handle, &mut resource) }
            .map_err(|error| dx_error("D3D12 OpenSharedHandle", error))?;
        resource.ok_or_else(|| CameraError::Dx("D3D12 OpenSharedHandle returned null".to_owned()))
    }
}

impl Drop for DxFrame {
    fn drop(&mut self) {
        if let Some(handle) = self.shared_handle {
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

/// D3D11 设备 + 与之绑定的 `IMFDXGIDeviceManager`，可选附带 D3D12 设备。
pub struct DxDevice {
    d3d11: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
    reset_token: u32,
    d3d12: Option<ID3D12Device>,
    com_initialized: bool,
}

pub(crate) static MF_INIT: Once = Once::new();

impl DxDevice {
    /// 创建 D3D11 视频设备并注册给 Media Foundation。
    /// `with_dx12 = true` 时同时创建 D3D12 设备，供 `DxFrame::import_to_dx12` 使用。
    pub fn new(with_dx12: bool) -> Result<Self> {
        MF_INIT.call_once(|| {
            let _ = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) };
        });
        let com_initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();

        let mut device = None;
        let mut context = None;
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let flags = D3D11_CREATE_DEVICE_FLAG(
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0 | D3D11_CREATE_DEVICE_BGRA_SUPPORT.0,
        );
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|error| dx_error("D3D11CreateDevice", error))?;
        let d3d11 = device.ok_or_else(|| CameraError::Dx("D3D11 device is null".to_owned()))?;
        let context = context.ok_or_else(|| CameraError::Dx("D3D11 context is null".to_owned()))?;

        // MF 的 FrameServer 线程会并发调用，必须开多线程保护（与 Chromium 一致）。
        if let Ok(multithread) =
            d3d11.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>()
        {
            let _ = unsafe { multithread.SetMultithreadProtected(true) };
        }

        let mut reset_token = 0u32;
        let mut manager = None;
        unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager) }
            .map_err(|error| dx_error("MFCreateDXGIDeviceManager", error))?;
        let manager =
            manager.ok_or_else(|| CameraError::Dx("DXGI device manager is null".to_owned()))?;
        unsafe { manager.ResetDevice(&d3d11, reset_token) }
            .map_err(|error| dx_error("IMFDXGIDeviceManager::ResetDevice", error))?;

        let d3d12 = if with_dx12 {
            let mut dx12 = None;
            let result: windows::core::Result<()> =
                unsafe { D3D12CreateDevice(&d3d11, D3D_FEATURE_LEVEL_11_0, &mut dx12) };
            result.map_err(|error| dx_error("D3D12CreateDevice", error))?;
            dx12
        } else {
            None
        };

        Ok(Self {
            d3d11,
            context,
            manager,
            reset_token,
            d3d12,
            com_initialized,
        })
    }

    #[must_use]
    pub const fn d3d11(&self) -> &ID3D11Device {
        &self.d3d11
    }

    #[must_use]
    pub const fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    #[must_use]
    pub const fn d3d12(&self) -> Option<&ID3D12Device> {
        self.d3d12.as_ref()
    }

    #[must_use]
    pub const fn reset_token(&self) -> u32 {
        self.reset_token
    }
}

impl Drop for DxDevice {
    fn drop(&mut self) {
        if self.com_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

/// 从设备唯一 ID（MSMF 符号链接）打开摄像头并输出 GPU 纹理的采集器。
pub struct DxCapture {
    reader: IMFSourceReader,
    /// 仅用于保活：`IMFSourceReader` 引用的媒体源不能在 reader 之前释放。
    #[allow(dead_code)]
    source: IMFMediaSource,
    device: DxDevice,
    device_info: DeviceInfo,
    format: FormatInfo,
}

/// 把 MF 的 `MF_MT_FRAME_SIZE` UINT64 解包为 (宽, 高)。
fn unpack_size(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, (value & 0xFFFF_FFFF) as u32)
}

/// 把 `MF_MT_FRAME_RATE` 的分子/分母解包为整数帧率（分母为 0 时返回 0）。
fn unpack_fps(value: u64) -> u32 {
    let numerator = (value >> 32) as u32;
    let denominator = (value & 0xFFFF_FFFF) as u32;
    numerator.checked_div(denominator).unwrap_or(0)
}

/// 列出 reader 上所有原生 NV12 媒体类型。
fn enumerate_nv12_formats(reader: &IMFSourceReader) -> Result<Vec<FormatInfo>> {
    let mut formats = Vec::new();
    let mut index = 0u32;
    loop {
        let media_type = match unsafe { reader.GetNativeMediaType(VIDEO_STREAM, index) } {
            Ok(media_type) => media_type,
            Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
            Err(error) => return Err(dx_error("GetNativeMediaType", error)),
        };
        index += 1;
        let subtype = match unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) } {
            Ok(subtype) => subtype,
            Err(_) => continue,
        };
        if subtype != MFVideoFormat_NV12 {
            continue;
        }
        let size = match unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) } {
            Ok(size) => size,
            Err(_) => continue,
        };
        let (width, height) = unpack_size(size);
        let rate = unsafe { media_type.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(0);
        formats.push(FormatInfo::new(
            Resolution::new(width, height),
            PixelFormat::Nv12,
            unpack_fps(rate),
        ));
    }
    Ok(formats)
}

impl DxCapture {
    /// 创建媒体源与 `IMFSourceReader`（绑定 D3D11 设备管理器，禁用格式转换）。
    fn create_reader(
        device_info: &DeviceInfo,
        device: &DxDevice,
    ) -> Result<(IMFMediaSource, IMFSourceReader)> {
        let mut source_attrs = None;
        unsafe { MFCreateAttributes(&mut source_attrs, 2) }
            .map_err(|error| dx_error("MFCreateAttributes(source)", error))?;
        let source_attrs =
            source_attrs.ok_or_else(|| CameraError::Dx("source attrs null".to_owned()))?;
        unsafe {
            source_attrs.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
        }
        .map_err(|error| dx_error("set source type", error))?;
        let path: Vec<u16> = device_info
            .unique_id()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            source_attrs.SetString(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                windows::core::PCWSTR(path.as_ptr()),
            )
        }
        .map_err(|error| dx_error("set symbolic link", error))?;
        let source = unsafe { MFCreateDeviceSource(&source_attrs) }
            .map_err(|error| dx_error("MFCreateDeviceSource", error))?;

        let mut reader_attrs = None;
        unsafe { MFCreateAttributes(&mut reader_attrs, 2) }
            .map_err(|error| dx_error("MFCreateAttributes(reader)", error))?;
        let reader_attrs =
            reader_attrs.ok_or_else(|| CameraError::Dx("reader attrs null".to_owned()))?;
        unsafe { reader_attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &device.manager) }
            .map_err(|error| dx_error("set MF_SOURCE_READER_D3D_MANAGER", error))?;
        unsafe { reader_attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1) }
            .map_err(|error| dx_error("set MF_READWRITE_DISABLE_CONVERTERS", error))?;
        let reader = unsafe { MFCreateSourceReaderFromMediaSource(&source, &reader_attrs) }
            .map_err(|error| dx_error("MFCreateSourceReaderFromMediaSource", error))?;
        Ok((source, reader))
    }

    /// 打开设备。要求设备提供目标分辨率的原生 NV12 媒体类型；找不到就报错，
    /// 不会静默改用 CPU 解码路径（那是 `CameraManager` 的职责）。
    pub fn open(
        device_info: &DeviceInfo,
        width: u32,
        height: u32,
        fps: u32,
        with_dx12: bool,
    ) -> Result<Self> {
        let format = FormatInfo::new(Resolution::new(width, height), PixelFormat::Nv12, fps);
        Self::open_format(device_info, &format, with_dx12)
    }

    /// 列出设备声明的所有原生 NV12 媒体类型（临时打开设备，读完释放）。
    pub fn capabilities(device_info: &DeviceInfo) -> Result<DeviceCapabilities> {
        let device = DxDevice::new(false)?;
        let (_, reader) = Self::create_reader(device_info, &device)?;
        Ok(DeviceCapabilities::new(enumerate_nv12_formats(&reader)?))
    }

    /// 用精确的媒体类型打开（多分辨率/帧率切换使用）。
    pub fn open_format(
        device_info: &DeviceInfo,
        format: &FormatInfo,
        with_dx12: bool,
    ) -> Result<Self> {
        if format.format != PixelFormat::Nv12 {
            return Err(CameraError::Dx(
                "GPU path supports only NV12 media types".to_owned(),
            ));
        }
        let device = DxDevice::new(with_dx12)?;
        let (source, reader) = Self::create_reader(device_info, &device)?;
        let formats = enumerate_nv12_formats(&reader)?;
        // fps = 0 表示该分辨率下取最高帧率。
        let chosen = formats
            .iter()
            .filter(|candidate| candidate.resolution == format.resolution)
            .filter(|candidate| format.fps == 0 || candidate.fps == format.fps)
            .max_by_key(|candidate| candidate.fps)
            .copied()
            .ok_or_else(|| {
                CameraError::Dx(format!("device has no native NV12 {}", format.label()))
            })?;
        // 找到对应媒体类型并设为当前类型（SetCurrentMediaType 需在启动流之前）。
        let mut index = 0u32;
        let mut selected = None;
        loop {
            let media_type = match unsafe { reader.GetNativeMediaType(VIDEO_STREAM, index) } {
                Ok(media_type) => media_type,
                Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
                Err(error) => return Err(dx_error("GetNativeMediaType", error)),
            };
            index += 1;
            if unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }.ok().as_ref()
                != Some(&MFVideoFormat_NV12)
            {
                continue;
            }
            let size = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
            let rate = unsafe { media_type.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(0);
            if unpack_size(size) == (chosen.width(), chosen.height())
                && unpack_fps(rate) == chosen.fps
            {
                selected = Some(media_type);
                break;
            }
        }
        let media_type = selected.ok_or_else(|| {
            CameraError::Dx(format!("NV12 media type {} not found", chosen.label()))
        })?;
        unsafe { reader.SetCurrentMediaType(VIDEO_STREAM, None, &media_type) }
            .map_err(|error| dx_error("SetCurrentMediaType", error))?;
        Ok(Self {
            reader,
            source,
            device,
            device_info: device_info.clone(),
            format: chosen,
        })
    }

    /// 设置扩展摄像头控制（背景虚化 / 眼神矫正 / 人脸取景 / HDR）。
    ///
    /// 对应 Chromium `SetAndCommitExtendedCameraControlFlags()`（mf_win.cc:1251）：
    /// 通过 `IMFGetService::GetService` 拿到 `IMFExtendedCameraController`，
    /// 再 `GetExtendedCameraControl(MF_CAPTURE_ENGINE_MEDIASOURCE, property_id)` →
    /// `SetFlags()` → `CommitSettings()`。并非所有设备支持，不支持时返回错误。
    pub fn set_extended_control(
        &self,
        control: ExtendedControl,
        setting: ExtendedControlSetting,
    ) -> Result<()> {
        let get_service: IMFGetService = self
            .source
            .cast()
            .map_err(|error| dx_error("query IMFGetService", error))?;
        let controller: IMFExtendedCameraController =
            unsafe { get_service.GetService(&GUID::zeroed()) }
                .map_err(|error| dx_error("GetService(IMFExtendedCameraController)", error))?;
        let camera_control: IMFExtendedCameraControl = unsafe {
            controller.GetExtendedCameraControl(
                MF_CAPTURE_ENGINE_MEDIASOURCE.0,
                control.ks_property_id() as u32,
            )
        }
        .map_err(|error| dx_error("GetExtendedCameraControl", error))?;
        let flags: u64 = match setting {
            ExtendedControlSetting::Off => 0,
            ExtendedControlSetting::On => 1,
            // Auto 对人脸取景是 AUTOFACEFRAMING；其他控制映射为设备自动模式。
            ExtendedControlSetting::Auto => 1,
        };
        unsafe {
            camera_control
                .SetFlags(flags)
                .map_err(|error| dx_error("SetFlags", error))?;
            camera_control.CommitSettings()
        }
        .map_err(|error| dx_error("commit extended control", error))
    }

    /// 切换到另一个精确媒体类型（重建整条 GPU 采集链）。
    pub fn set_format(&mut self, format: &FormatInfo) -> Result<()> {
        let with_dx12 = self.device.d3d12().is_some();
        let fresh = Self::open_format(&self.device_info, format, with_dx12)?;
        *self = fresh;
        Ok(())
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.format.width()
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.format.height()
    }

    #[must_use]
    pub const fn fps(&self) -> u32 {
        self.format.fps
    }

    #[must_use]
    pub const fn current_format(&self) -> Option<FormatInfo> {
        Some(self.format)
    }

    #[must_use]
    pub const fn device(&self) -> &DxDevice {
        &self.device
    }

    /// 取一帧 GPU 纹理。没有新样本时返回 `Ok(None)`。
    pub fn poll(&mut self) -> Result<Option<DxFrame>> {
        let mut sample = None;
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        unsafe {
            self.reader.ReadSample(
                VIDEO_STREAM,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
        }
        .map_err(|error| dx_error("ReadSample", error))?;
        let Some(sample) = sample else {
            return Ok(None);
        };

        let buffer: IMFMediaBuffer = unsafe { sample.GetBufferByIndex(0) }
            .map_err(|error| dx_error("GetBufferByIndex", error))?;
        let dxgi_buffer: IMFDXGIBuffer = buffer.cast().map_err(|error| {
            dx_error(
                "query IMFDXGIBuffer (device did not deliver a GPU surface)",
                error,
            )
        })?;
        let mut raw = ptr::null_mut();
        unsafe { dxgi_buffer.GetResource(&ID3D11Texture2D::IID, &mut raw) }
            .map_err(|error| dx_error("IMFDXGIBuffer::GetResource", error))?;
        if raw.is_null() {
            return Err(CameraError::Dx(
                "IMFDXGIBuffer::GetResource returned null".to_owned(),
            ));
        }
        let texture = unsafe { ID3D11Texture2D::from_raw(raw) };

        let mut desc = MaybeUninit::<D3D11_TEXTURE2D_DESC>::uninit();
        unsafe { texture.GetDesc(desc.as_mut_ptr()) };
        let desc = unsafe { desc.assume_init() };
        if desc.Format != DXGI_FORMAT_NV12 {
            return Err(CameraError::Dx(format!(
                "expected NV12 texture, got {:?}",
                desc.Format
            )));
        }

        // Chromium `GetTextureInfo()`：跨 API 共享要求 SHARED_NTHANDLE，且不能被
        // RESTRICT_SHARED_RESOURCE / RESTRICT_SHARED_RESOURCE_DRIVER 限制。
        let flags = D3D11_RESOURCE_MISC_FLAG(desc.MiscFlags as i32);
        let cross_api_shareable = flags.0 & D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 != 0
            && flags.0 & D3D11_RESOURCE_MISC_RESTRICT_SHARED_RESOURCE.0 == 0
            && flags.0 & D3D11_RESOURCE_MISC_RESTRICT_SHARED_RESOURCE_DRIVER.0 == 0;

        Ok(Some(DxFrame {
            texture,
            width: desc.Width,
            height: desc.Height,
            timestamp: Duration::from_nanos((timestamp.max(0) as u64).saturating_mul(100)),
            shared_handle: None,
            cross_api_shareable,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_mf_frame_size_and_rate() {
        // MF_MT_FRAME_SIZE: 高 32 位宽，低 32 位高。
        let packed = ((1280u64) << 32) | 720;
        assert_eq!(unpack_size(packed), (1280, 720));
        // MF_MT_FRAME_RATE: 分子/分母，30000/1001 ≈ 29.97 → 整数 29。
        assert_eq!(unpack_fps((30000u64 << 32) | 1001), 29);
        assert_eq!(unpack_fps((30u64 << 32) | 1), 30);
        assert_eq!(unpack_fps(0), 0);
    }

    #[test]
    fn nv12_subresource_indices_are_stable() {
        assert_eq!(DxFrame::y_subresource(), 0);
        assert_eq!(DxFrame::uv_subresource(), 1);
    }
}
