//! 高分辨率静帧拍照（Windows Media Foundation `IMFCaptureEngine`）。
//!
//! Chromium 用独立的 **photo stream**（`MF_CAPTURE_ENGINE_STREAM_CATEGORY_PHOTO_INDEPENDENT`）
//! 在不打断视频预览的情况下抓高分辨率静帧（`video_capture_device_mf_win.cc:1672`）。
//! `IMFSourceReader` 做不到这一点，所以这里单独建一条 `IMFCaptureEngine` 管线。
//!
//! 用法：
//! ```text
//! let photo = DxPhotoCapture::new(&device_info)?;
//! photo.capture_to_file(&settings, Path::new("photo.jpg"))?;
//! ```
//!
//! 本模块是受控 unsafe 的 DX11 路径的一部分（`dx` feature）。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::core::{implement, Interface, Ref, GUID};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFCaptureEngineClassFactory, IMFCaptureEngine, IMFCaptureEngineOnEventCallback,
    IMFCaptureEngineOnEventCallback_Impl, IMFCapturePhotoSink, IMFMediaEvent,
    MF_CAPTURE_ENGINE_ERROR, MF_CAPTURE_ENGINE_INITIALIZED, MF_CAPTURE_ENGINE_PHOTO_TAKEN,
    MF_CAPTURE_ENGINE_SINK_TYPE_PHOTO, MF_CAPTURE_ENGINE_USE_VIDEO_DEVICE_ONLY,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::controls::PhotoSettings;
use crate::device::{CameraError, DeviceInfo, Result};
use crate::dx::MF_INIT;

fn photo_error(context: &str, error: windows::core::Error) -> CameraError {
    CameraError::Dx(format!("photo {context}: {error}"))
}

/// 简单的 Win32 事件包装（RAII）。
struct WinEvent(HANDLE);

// Win32 事件句柄是内核对象，可跨线程 Set/Wait；这里显式声明 Send/Sync。
// SAFETY: HANDLE 只在 WinEvent 内通过 CreateEventW/CloseHandle 管理，
// SetEvent/WaitForSingleObject 都是线程安全的；引用计数由 Arc 管理。
unsafe impl Send for WinEvent {}
unsafe impl Sync for WinEvent {}

impl WinEvent {
    fn new() -> Result<Self> {
        let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|error| photo_error("CreateEventW", error))?;
        Ok(Self(handle))
    }

    fn set(&self) {
        let _ = unsafe { windows::Win32::System::Threading::SetEvent(self.0) };
    }

    fn reset(&self) {
        let _ = unsafe { windows::Win32::System::Threading::ResetEvent(self.0) };
    }

    fn wait(&self, timeout: Duration) -> bool {
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let result = unsafe { WaitForSingleObject(self.0, ms) };
        result == windows::Win32::Foundation::WAIT_OBJECT_0
    }

    /// 非阻塞查询事件是否已置位（用零超时 wait）。
    fn is_set(&self) -> bool {
        self.wait(Duration::ZERO)
    }
}

impl Drop for WinEvent {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// 引擎事件回调：收到初始化完成或拍照完成事件时置位 Win32 事件。
#[implement(IMFCaptureEngineOnEventCallback)]
struct EngineCallback {
    initialized: Arc<WinEvent>,
    photo_taken: Arc<WinEvent>,
    failed: Arc<WinEvent>,
}

impl IMFCaptureEngineOnEventCallback_Impl for EngineCallback_Impl {
    fn OnEvent(&self, pevent: Ref<'_, IMFMediaEvent>) -> windows::core::Result<()> {
        let event = match &*pevent {
            Some(event) => event,
            None => return Ok(()),
        };
        let event_type = unsafe { event.GetExtendedType() }.unwrap_or(GUID::zeroed());
        if event_type == MF_CAPTURE_ENGINE_INITIALIZED {
            self.initialized.set();
        } else if event_type == MF_CAPTURE_ENGINE_PHOTO_TAKEN {
            self.photo_taken.set();
        } else if event_type == MF_CAPTURE_ENGINE_ERROR {
            // 失败只置位 failed，绝不置位 photo_taken —— 后者意味着拍照成功。
            // 同时置位 initialized，避免初始化阶段出错时 new() 白等 5 秒。
            self.failed.set();
            self.initialized.set();
        }
        Ok(())
    }
}

/// 高分辨率拍照器。与视频采集器（`DxCapture`）互斥使用同一摄像头。
pub struct DxPhotoCapture {
    engine: IMFCaptureEngine,
    photo_sink: IMFCapturePhotoSink,
    com_initialized: bool,
    /// 事件用于同步引擎初始化与拍照完成；回调对象需保活。
    photo_taken: Arc<WinEvent>,
    /// 引擎错误事件；capture_to_file 必须读它，否则失败会被误报为成功。
    failed: Arc<WinEvent>,
    #[allow(dead_code)]
    _callback: IMFCaptureEngineOnEventCallback,
}

impl DxPhotoCapture {
    /// 创建拍照管线。会短暂占用设备完成引擎初始化。
    pub fn new(device_info: &DeviceInfo) -> Result<Self> {
        MF_INIT.call_once(|| {
            let _ = unsafe {
                windows::Win32::Media::MediaFoundation::MFStartup(
                    windows::Win32::Media::MediaFoundation::MF_VERSION,
                    windows::Win32::Media::MediaFoundation::MFSTARTUP_LITE,
                )
            };
        });
        let com_initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();

        let initialized = Arc::new(WinEvent::new()?);
        let photo_taken = Arc::new(WinEvent::new()?);
        let failed = Arc::new(WinEvent::new()?);
        let callback: IMFCaptureEngineOnEventCallback = EngineCallback {
            initialized: Arc::clone(&initialized),
            photo_taken: Arc::clone(&photo_taken),
            failed: Arc::clone(&failed),
        }
        .into();

        let factory: windows::Win32::Media::MediaFoundation::IMFCaptureEngineClassFactory =
            unsafe {
                CoCreateInstance(
                    &CLSID_MFCaptureEngineClassFactory,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
            }
            .map_err(|error| photo_error("CoCreateInstance(factory)", error))?;
        let engine: IMFCaptureEngine = unsafe {
            factory.CreateInstance(&windows::Win32::Media::MediaFoundation::CLSID_MFCaptureEngine)
        }
        .map_err(|error| photo_error("CreateInstance(engine)", error))?;

        // 设备源：与 DxCapture::create_reader 相同的符号链接创建方式。
        let mut source_attrs = None;
        unsafe { windows::Win32::Media::MediaFoundation::MFCreateAttributes(&mut source_attrs, 2) }
            .map_err(|error| photo_error("MFCreateAttributes", error))?;
        let source_attrs =
            source_attrs.ok_or_else(|| CameraError::Dx("photo source attrs null".to_owned()))?;
        use windows::Win32::Media::MediaFoundation::{
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        };
        unsafe {
            source_attrs.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
        }
        .map_err(|error| photo_error("set source type", error))?;
        let path: Vec<u16> = device_info
            .unique_id()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            source_attrs.SetString(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                PCWSTR(path.as_ptr()),
            )
        }
        .map_err(|error| photo_error("set symbolic link", error))?;
        let source =
            unsafe { windows::Win32::Media::MediaFoundation::MFCreateDeviceSource(&source_attrs) }
                .map_err(|error| photo_error("MFCreateDeviceSource", error))?;

        let mut engine_attrs = None;
        unsafe { windows::Win32::Media::MediaFoundation::MFCreateAttributes(&mut engine_attrs, 1) }
            .map_err(|error| photo_error("MFCreateAttributes(engine)", error))?;
        let engine_attrs =
            engine_attrs.ok_or_else(|| CameraError::Dx("engine attrs null".to_owned()))?;
        unsafe { engine_attrs.SetUINT32(&MF_CAPTURE_ENGINE_USE_VIDEO_DEVICE_ONLY, 1) }
            .map_err(|error| photo_error("set engine attrs", error))?;

        unsafe {
            engine.Initialize(
                &callback,
                &engine_attrs,
                None::<&windows::core::IUnknown>,
                &source,
            )
        }
        .map_err(|error| photo_error("engine Initialize", error))?;
        if !initialized.wait(Duration::from_secs(5)) {
            return Err(CameraError::Dx("photo engine init timeout".to_owned()));
        }

        let sink = unsafe { engine.GetSink(MF_CAPTURE_ENGINE_SINK_TYPE_PHOTO) }
            .map_err(|error| photo_error("GetSink(photo)", error))?;
        let photo_sink: IMFCapturePhotoSink = sink
            .cast()
            .map_err(|error| photo_error("cast IMFCapturePhotoSink", error))?;

        Ok(Self {
            engine,
            photo_sink,
            com_initialized,
            photo_taken: Arc::clone(&photo_taken),
            failed: Arc::clone(&failed),
            _callback: callback,
        })
    }

    /// 抓一张高分辨率静帧并保存到文件（JPEG）。
    pub fn capture_to_file(&self, settings: &PhotoSettings, path: &Path) -> Result<()> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // 事件是手动复位的，连续拍照前必须清零，否则上一次的置位会让本次立刻返回。
        self.photo_taken.reset();
        self.failed.reset();

        unsafe { self.photo_sink.SetOutputFileName(PCWSTR(wide.as_ptr())) }
            .map_err(|error| photo_error("SetOutputFileName", error))?;
        unsafe { self.engine.TakePhoto() }.map_err(|error| photo_error("TakePhoto", error))?;

        // 用轮询同时观察两个事件（Win32 无法原地等待多个事件）。
        // 先查 failed：错误可能让 photo_taken 永不置位，等它就会白等 10 秒再报超时。
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if self.failed.is_set() {
                return Err(CameraError::Dx(
                    "photo capture failed (MF_CAPTURE_ENGINE_ERROR)".to_owned(),
                ));
            }
            if self.photo_taken.is_set() {
                // photo_taken 与 failed 可能在同一次 event 回调里先后置位；再查一次 failed。
                if self.failed.is_set() {
                    return Err(CameraError::Dx(
                        "photo capture failed (MF_CAPTURE_ENGINE_ERROR)".to_owned(),
                    ));
                }
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(CameraError::Dx("photo capture timeout".to_owned()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = settings; // 旋转/镜像等后处理由渲染层完成（见 PhotoSettings 文档）。
        Ok(())
    }
}

impl Drop for DxPhotoCapture {
    fn drop(&mut self) {
        if self.com_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn photo_settings_defaults() {
        let settings = super::PhotoSettings::default();
        assert!(settings.resolution.is_none());
        assert!(!settings.mirror);
        assert_eq!(settings.rotation_degrees, 0);
    }
}
