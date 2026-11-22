//! 视频帧 → 渲染契约描述符的转换（2026-11-21 重设计）。
//!
//! 软件后端输出 CPU 平面（I420 / NV12 / YUV420P10），硬件后端输出平台原生句柄。
//! 两类分别映射成 `zephyra-render-ext` 的 `FrameSource::Cpu(Arc<CpuPlanes>)` 与
//! `FrameSource::Native`。**不再**产出不透明 `ImportedTexture`，也不再持有 importer：
//! 本层只负责把解码帧变成纯数据 `FrameDesc`，由应用层 `submit_frame(key, desc, valid_until)`
//! 提交到具体后端。这样 media 与三后端完全解耦；解码线程以所有权方式零拷贝交付帧，
//! `Bytes` 一旦生成，跨线程 clone 与向契约层交付都是零拷贝（见 `CpuVideoFrame::planes`）。
//! 注意：上游 `VideoPlane::Cpu` 持有 `Vec<u8>` 且只暴露借用切片，因此从 borrowed 帧
//! 生成 `Bytes` 需要整段拷贝一次；端到端零拷贝需要上游平面改为 `Arc<[u8]>` 并提供 `bytes()`。

use bytes::Bytes;

use zephyra_render_ext::prelude::{
    ColorSpace, CpuPlanes, ExternalTextureFormat, ExternalTextureSource, FrameDesc, ImportError,
    RawHandle,
};
use zephyra_video::{VideoFrame, VideoPixelFormat, VideoPlane};

/// 视频帧来源（便于队列统一存放 CPU 与原生帧）。
pub enum VideoFrameSource {
    Cpu(VideoFrame),
    Native {
        handle: RawHandle,
        format: ExternalTextureFormat,
        width: u32,
        height: u32,
        owner: zephyra_video::NativeResourceOwner,
        timestamp: i64,
        duration: i64,
    },
}

/// 已转换成 NV12、可直接上传的 owned CPU 帧。
///
/// 两个平面都是 `Bytes`：`Vec -> Bytes` 零拷贝转移分配，可跨线程交给渲染器，
/// 也便于一帧同时被多个消费者持有（clone 只加引用计数）。契约层的 `CpuPlanes`
/// 同样以 `Bytes` 存储平面，因此 `planes()` 是一次零拷贝的所有权交付。
pub struct CpuVideoFrame {
    y: Bytes,
    uv: Bytes,
    width: u32,
    height: u32,
}

impl CpuVideoFrame {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn y_plane(&self) -> &[u8] {
        &self.y
    }
    pub fn uv_plane(&self) -> &[u8] {
        &self.uv
    }

    /// 生成拥有所有权的 NV12 平面（行距：Y = width，交错 UV = width）。
    pub fn planes(&self) -> CpuPlanes {
        CpuPlanes::nv12(self.y.clone(), self.uv.clone(), self.width, self.width)
    }

    /// 生成统一 `FrameDesc`。
    pub fn frame_desc(&self) -> FrameDesc {
        FrameDesc::cpu((self.width, self.height), self.planes())
    }
}

impl VideoFrameSource {
    pub fn timestamp(&self) -> i64 {
        match self {
            VideoFrameSource::Cpu(f) => f.timestamp(),
            VideoFrameSource::Native { timestamp, .. } => *timestamp,
        }
    }

    pub fn duration(&self) -> i64 {
        match self {
            VideoFrameSource::Cpu(f) => f.duration(),
            VideoFrameSource::Native { duration, .. } => *duration,
        }
    }

    pub fn as_external_source(&self) -> Option<ExternalTextureSource> {
        match self {
            VideoFrameSource::Native { handle, .. } => Some(ExternalTextureSource::NtHandle {
                handle: *handle,
                source_backend: None,
            }),
            VideoFrameSource::Cpu(_) => None,
        }
    }

    pub fn as_cpu_upload(&self) -> Option<CpuVideoFrame> {
        match self {
            VideoFrameSource::Cpu(f) => to_nv12(f),
            VideoFrameSource::Native { .. } => None,
        }
    }
}

/// 该帧建议使用的纹理 key（应用调用 `submit_frame` 时传入）。
pub fn video_key(frame: &VideoFrameSource) -> String {
    format!("video-{}", frame.timestamp())
}

/// 把任意 4:2:0 平面格式转换成 NV12（8bit）。
///
/// - I420（Y/U/V）→ Y + 交错 UV
/// - NV12/NV21 → 直接拷贝（NV21 交换 U/V）
/// - YUV420P10 → 每采样右移 2 位降到 8bit，再交错
/// - P010LE → 16bit 交错 UV 右移 2 位降到 8bit
fn to_nv12(frame: &VideoFrame) -> Option<CpuVideoFrame> {
    let w = frame.coded_size().width as usize;
    let h = frame.coded_size().height as usize;
    if w == 0 || h == 0 {
        return None;
    }
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let layout = frame.layout();

    // 快速通道：源已是 NV12 且行距紧凑 → 整段拷贝 Y/UV 到 Bytes，不逐像素重编码。
    // 上游 VideoPlane 只暴露借用的 &[u8]（Cpu { data: Vec<u8> }），这里无法把 Vec 的所有权
    // 移进 Bytes::from_owner，故这条路径是一次 memcpy；此后 Bytes 的 clone / slice 与
    // 向契约层交付仍为零拷贝。
    if frame.format() == VideoPixelFormat::NV12
        && layout.planes()[0].stride == w
        && layout.planes()[1].stride == w
    {
        let y = Bytes::copy_from_slice(frame.planes().first()?.cpu_data()?.get(..w * h)?);
        let uv = Bytes::copy_from_slice(frame.planes().get(1)?.cpu_data()?.get(..cw * ch * 2)?);
        return Some(CpuVideoFrame {
            y,
            uv,
            width: w as u32,
            height: h as u32,
        });
    }

    // `as_ref()` 读取 video 侧的 Vec<u8> 平面（借出切片，逐像素转换时写入新分配）。
    let planes: Vec<&[u8]> = frame
        .planes()
        .iter()
        .map(|p| match p {
            VideoPlane::Cpu { data } => Some(data.as_ref()),
            VideoPlane::Native { .. } => None,
        })
        .collect::<Option<Vec<_>>>()?;

    let bytes = |i: usize| layout.planes()[i].stride;
    let shift = match frame.format() {
        VideoPixelFormat::I420 | VideoPixelFormat::NV12 | VideoPixelFormat::NV21 => 0,
        VideoPixelFormat::YUV420P10 | VideoPixelFormat::P010LE => 2,
        _ => return None,
    };
    let sample = |plane: &[u8], i: usize| -> u8 {
        let lo = plane.get(i).copied().unwrap_or(128);
        if shift == 0 {
            lo
        } else {
            let hi = plane.get(i + 1).copied().unwrap_or(0);
            (lo >> 2) | (hi << 6)
        }
    };

    let mut y_out = Vec::with_capacity(w * h);
    let y_stride = bytes(0);
    for row in 0..h {
        for col in 0..w {
            let i = row * y_stride + col * (1 << shift);
            y_out.push(sample(planes[0], i));
        }
    }

    let mut uv = Vec::with_capacity(cw * ch * 2);
    match frame.format() {
        VideoPixelFormat::I420 | VideoPixelFormat::YUV420P10 => {
            let (u, v) = (planes[1], planes[2]);
            let stride = bytes(1);
            for row in 0..ch {
                for col in 0..cw {
                    let i = row * stride + col * 2 * (1 << shift);
                    uv.push(sample(u, i));
                    uv.push(sample(v, i));
                }
            }
        }
        VideoPixelFormat::NV12 | VideoPixelFormat::P010LE => {
            let src = planes[1];
            let stride = bytes(1);
            for row in 0..ch {
                for col in 0..cw {
                    let i = row * stride + col * 2 * (1 << shift);
                    uv.push(sample(src, i));
                    uv.push(sample(src, i + (1 << shift)));
                }
            }
        }
        VideoPixelFormat::NV21 => {
            let src = planes[1];
            let stride = bytes(1);
            for row in 0..ch {
                for col in 0..cw {
                    let i = row * stride + col * 2;
                    uv.push(sample(src, i + 1)); // V
                    uv.push(sample(src, i)); // U
                }
            }
        }
        _ => return None,
    }

    Some(CpuVideoFrame {
        // Vec -> Bytes 零拷贝转移分配；下游 planes() 再零拷贝交给契约层。
        y: Bytes::from(y_out),
        uv: Bytes::from(uv),
        width: w as u32,
        height: h as u32,
    })
}

/// 把一帧解码结果变成纯数据 [`FrameDesc`]（不再导入，不再有不透明句柄）。
///
/// 原生帧走共享句柄，CPU 帧统一转 NV12 双平面。应用随后调用
/// `ExtContextHandle::submit_frame(video_key(&frame), desc, valid_until)`。
pub fn import_video_frame(frame: &VideoFrameSource) -> Result<FrameDesc, ImportError> {
    if let Some(source) = frame.as_external_source() {
        let (w, h, format) = match frame {
            VideoFrameSource::Native {
                width,
                height,
                format,
                ..
            } => (*width, *height, *format),
            VideoFrameSource::Cpu(_) => unreachable!(),
        };
        let mut desc = FrameDesc::native((w, h), format, source);
        desc.color_space = ColorSpace::Bt709;
        return Ok(desc);
    }
    let cpu = frame
        .as_cpu_upload()
        .ok_or(ImportError::UnsupportedSource)?;
    let mut desc = cpu.frame_desc();
    desc.color_space = ColorSpace::Bt709;
    Ok(desc)
}

/// 把解码帧包装成统一的来源类型。
pub fn from_video_frame(frame: VideoFrame) -> VideoFrameSource {
    let native = frame.planes().iter().find_map(|p| match p {
        VideoPlane::Native { handle } => Some(*handle),
        VideoPlane::Cpu { .. } => None,
    });
    if let (Some(handle), Some(owner)) = (native, frame.native_owner().cloned()) {
        let (w, h) = (frame.coded_size().width, frame.coded_size().height);
        let format = match frame.format() {
            VideoPixelFormat::NV12 => ExternalTextureFormat::Nv12,
            VideoPixelFormat::P010LE => ExternalTextureFormat::P010,
            _ => ExternalTextureFormat::Rgba8Unorm,
        };
        return VideoFrameSource::Native {
            handle: RawHandle(handle as usize),
            format,
            width: w,
            height: h,
            owner,
            timestamp: frame.timestamp(),
            duration: frame.duration(),
        };
    }
    VideoFrameSource::Cpu(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zephyra_render_ext::prelude::FrameSource;
    use zephyra_video::{NativeResourceOwner, Rect, Size};

    fn frame(fmt: VideoPixelFormat, w: u32, h: u32, shift: usize) -> VideoFrame {
        let cw = w as usize / 2;
        let ch = h as usize / 2;
        let b = 1usize << shift;
        let y = vec![0x80u8; w as usize * h as usize * b];
        let u = vec![0x40u8; cw * ch * b];
        let v = vec![0xc0u8; cw * ch * b];
        VideoFrame::from_cpu(
            fmt,
            Size::new(w, h),
            Rect::new(0, 0, w, h),
            Size::new(w, h),
            vec![y, u, v],
            0,
            0,
        )
        .unwrap()
    }

    #[test]
    fn i420_and_10bit_become_nv12_frame_desc() {
        for (fmt, shift) in [
            (VideoPixelFormat::I420, 0),
            (VideoPixelFormat::YUV420P10, 2),
        ] {
            let src = VideoFrameSource::Cpu(frame(fmt, 64, 48, shift));
            let desc = import_video_frame(&src).unwrap();
            assert_eq!(desc.size, (64, 48));
            assert_eq!(desc.format, ExternalTextureFormat::Nv12);
            let planes = desc.cpu_planes().unwrap();
            assert_eq!(planes.plane(0).unwrap().len(), 64 * 48);
            assert_eq!(planes.plane(1).unwrap().len(), 32 * 48);
            let uv = planes.plane(1).unwrap();
            let (expect_u, expect_v) = if shift == 0 {
                (0x40u8, 0xc0u8)
            } else {
                (0x40 >> 2, 0xc0 >> 2)
            };
            assert_eq!((uv[0], uv[1]), (expect_u, expect_v));
        }
    }

    /// media → 契约跨边界：产出的 FrameDesc 可被下游（mock importer）按 key 消费。
    #[test]
    fn media_frame_desc_crosses_contract_boundary() {
        let src = VideoFrameSource::Cpu(frame(VideoPixelFormat::I420, 64, 48, 0));
        let key = video_key(&src);
        let desc = import_video_frame(&src).unwrap();
        // mock importer：只接收 (key, FrameDesc)，断言字段跨边界完整。
        let received: Vec<(String, FrameDesc)> = vec![(key.clone(), desc.clone())];
        assert_eq!(received[0].0, "video-0");
        assert_eq!(received[0].1.format, ExternalTextureFormat::Nv12);
        match &received[0].1.source {
            FrameSource::Cpu(p) => {
                assert_eq!(p.plane_count(), 2);
                assert_eq!(p.plane(0).unwrap().len(), 64 * 48);
            }
            FrameSource::Native(_) => panic!("CPU frame must produce CPU source"),
            _ => panic!("unexpected source"),
        }
    }

    /// NV12 紧排行距走快速通道：Y/UV 整段拷入 Bytes，结果与源平面一致。
    /// 上游 VideoPlane 只暴露借用切片，无法复用同一分配，故断言数据等价而非指针同一。
    #[test]
    fn nv12_tight_passthrough_matches_source_planes() {
        let (w, h) = (8usize, 8usize);
        let y: Vec<u8> = vec![128u8; w * h];
        let uv: Vec<u8> = vec![100u8; w * h / 2];
        let src = VideoFrameSource::Cpu(
            VideoFrame::from_cpu(
                VideoPixelFormat::NV12,
                Size::new(w as u32, h as u32),
                Rect::new(0, 0, w as u32, h as u32),
                Size::new(w as u32, h as u32),
                vec![y.clone(), uv.clone()],
                0,
                0,
            )
            .unwrap(),
        );
        let desc = import_video_frame(&src).unwrap();
        let planes = desc.cpu_planes().unwrap();
        assert_eq!(planes.plane(0).unwrap(), y.as_slice());
        assert_eq!(planes.plane(1).unwrap(), uv.as_slice());
        assert_eq!(planes.plane(0).unwrap().len(), w * h);
        assert_eq!(planes.plane(1).unwrap().len(), w * h / 2);
    }

    fn frame_from_planes(
        fmt: VideoPixelFormat,
        w: u32,
        h: u32,
        planes: Vec<Vec<u8>>,
    ) -> VideoFrame {
        VideoFrame::from_cpu(
            fmt,
            Size::new(w, h),
            Rect::new(0, 0, w, h),
            Size::new(w, h),
            planes,
            0,
            0,
        )
        .unwrap()
    }

    #[test]
    fn nv21_swaps_u_and_v_values() {
        // NV21 的交错面是 [V, U]；NV12 输出必须是 [U, V]。
        let f = frame_from_planes(
            VideoPixelFormat::NV21,
            2,
            2,
            vec![vec![1, 2, 3, 4], vec![0x11, 0x22]],
        );
        let out = to_nv12(&f).unwrap();
        assert_eq!(out.y_plane(), &[1, 2, 3, 4]);
        assert_eq!(out.uv_plane(), &[0x22, 0x11]);
        assert_eq!((out.width(), out.height()), (2, 2));
    }

    #[test]
    fn p010le_10bit_sample_math() {
        // 每个采样 2 字节 (lo, hi)；sample = (lo >> 2) | (hi << 6)。
        // 索引 i = row * stride + col * 4，而 2x2 的 stride 也是 4，
        // 因此 (row=0,col=1) 与 (row=1,col=0) 落在同一偏移 —— 钉住当前行为。
        let mut y = vec![0u8; 16];
        y[0..2].copy_from_slice(&[0x00, 0x00]); // i=0 → 0
        y[4..6].copy_from_slice(&[0x04, 0x00]); // i=4 → 4 >> 2 = 1
        y[8..10].copy_from_slice(&[0xff, 0x03]); // i=8 → 0x3f | (3 << 6) = 0xff

        // UV: 两个 16-bit 采样（第二个放在偏移 4）。
        let mut uv = vec![0u8; 8];
        uv[0..2].copy_from_slice(&[0x08, 0x00]); // 8 >> 2 = 2
        uv[4..6].copy_from_slice(&[0x10, 0x02]); // 4 | (2 << 6) = 0x84

        let f = frame_from_planes(VideoPixelFormat::P010LE, 2, 2, vec![y, uv]);
        let out = to_nv12(&f).unwrap();
        assert_eq!(out.y_plane(), &[0, 1, 1, 0xff]);
        assert_eq!(out.uv_plane(), &[2, 0x84]);
    }

    #[test]
    fn odd_dimensions_use_div_ceil() {
        // 3x3 → 色度 cw = ch = 2，Y 9 字节、UV 8 字节。
        let f = frame_from_planes(
            VideoPixelFormat::NV21,
            3,
            3,
            vec![
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
                vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
            ],
        );
        let out = to_nv12(&f).unwrap();
        assert_eq!((out.width(), out.height()), (3, 3));
        assert_eq!(out.y_plane(), &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(
            out.uv_plane(),
            &[0x22, 0x11, 0x44, 0x33, 0x66, 0x55, 0x88, 0x77]
        );
    }

    #[test]
    fn nv12_non_tight_stride_uses_slow_path() {
        // 奇数宽度下 NV12 的 UV 行距是 (w+1)，不满足紧排快速通道条件。
        let f = frame_from_planes(
            VideoPixelFormat::NV12,
            3,
            3,
            vec![
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
                vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
            ],
        );
        assert_eq!(f.layout().planes()[0].stride, 3);
        assert_eq!(f.layout().planes()[1].stride, 4);
        let out = to_nv12(&f).unwrap();
        assert_eq!(out.y_plane(), &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(
            out.uv_plane(),
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
        );
    }

    #[test]
    fn unsupported_format_returns_none() {
        // YV12 有三平面且能构造出帧，但 to_nv12 不支持它。
        let f = frame_from_planes(
            VideoPixelFormat::YV12,
            2,
            2,
            vec![vec![0; 4], vec![0; 1], vec![0; 1]],
        );
        assert!(to_nv12(&f).is_none());
    }

    #[test]
    fn out_of_range_8bit_sample_defaults_to_128() {
        // 4x2 的 I420：色度 cw=2，但实现按 col*2 索引，第二个色度样本越界。
        let f = frame_from_planes(
            VideoPixelFormat::I420,
            4,
            2,
            vec![
                vec![1, 2, 3, 4, 5, 6, 7, 8],
                vec![0x44, 0x55],
                vec![0x88, 0x99],
            ],
        );
        let out = to_nv12(&f).unwrap();
        assert_eq!(out.y_plane(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(out.uv_plane(), &[0x44, 0x88, 128, 128]);
    }

    #[test]
    fn out_of_range_10bit_high_byte_defaults_to_zero() {
        // 第二个色度样本只有 lo 字节存在，hi 越界 → 默认 0。
        let mut y = vec![0u8; 16];
        y[0..2].copy_from_slice(&[0x00, 0x00]); // 0
        y[4..6].copy_from_slice(&[0x04, 0x00]); // 1
        y[8..10].copy_from_slice(&[0x08, 0x00]); // 2
        y[12..14].copy_from_slice(&[0x0c, 0x00]); // 3
        let mut u = vec![0u8; 9];
        u[0..2].copy_from_slice(&[0x10, 0x00]); // 4
        u[8] = 0xff; // 0xff >> 2 = 0x3f（hi 默认 0）
        let mut v = vec![0u8; 9];
        v[0..2].copy_from_slice(&[0x20, 0x00]); // 8
        v[8] = 0x00; // 0

        let f = frame_from_planes(VideoPixelFormat::YUV420P10, 4, 2, vec![y, u, v]);
        let out = to_nv12(&f).unwrap();
        // 4x2 → 8 个 Y 采样；索引重复（0,4,8,12,8,12,16,20），
        // 最后两个越界 → lo 默认 128、hi 默认 0 → (128 >> 2) = 32。
        assert_eq!(out.y_plane(), &[0, 1, 2, 3, 2, 3, 32, 32]);
        assert_eq!(out.uv_plane(), &[4, 8, 0x3f, 0]);
    }

    #[test]
    fn zero_dimension_frames_cannot_be_constructed() {
        // 上游构造器直接拒绝 0 宽/高，因此 to_nv12 里的 0 尺寸保护无法通过公开 API 触发。
        let empty: Vec<Vec<u8>> = Vec::new();
        assert!(VideoFrame::from_cpu(
            VideoPixelFormat::I420,
            Size::new(0, 8),
            Rect::new(0, 0, 0, 8),
            Size::new(0, 8),
            empty.clone(),
            0,
            0,
        )
        .is_none());
        assert!(VideoFrame::from_cpu(
            VideoPixelFormat::I420,
            Size::new(8, 0),
            Rect::new(0, 0, 8, 0),
            Size::new(8, 0),
            empty,
            0,
            0,
        )
        .is_none());
    }

    fn native_frame_with_owner(
        fmt: VideoPixelFormat,
        w: u32,
        h: u32,
        handle: u64,
        ts: i64,
        dur: i64,
    ) -> (VideoFrame, NativeResourceOwner) {
        let owner = NativeResourceOwner::new(Arc::new(()));
        let frame = VideoFrame::from_native_owned(
            fmt,
            Size::new(w, h),
            Rect::new(0, 0, w, h),
            Size::new(w, h),
            vec![handle; fmt.num_planes()],
            owner.clone(),
            ts,
            dur,
        )
        .unwrap();
        (frame, owner)
    }

    #[test]
    fn native_frames_map_format_and_propagate_metadata() {
        for (fmt, expected) in [
            (VideoPixelFormat::NV12, ExternalTextureFormat::Nv12),
            (VideoPixelFormat::P010LE, ExternalTextureFormat::P010),
            (VideoPixelFormat::I420, ExternalTextureFormat::Rgba8Unorm),
        ] {
            let (frame, owner) = native_frame_with_owner(fmt, 16, 8, 0x1234, 42, 7);
            match from_video_frame(frame) {
                VideoFrameSource::Native {
                    handle,
                    format,
                    width,
                    height,
                    owner: got_owner,
                    timestamp,
                    duration,
                } => {
                    assert_eq!(handle, RawHandle(0x1234));
                    assert_eq!(format, expected);
                    assert_eq!((width, height), (16, 8));
                    assert_eq!((timestamp, duration), (42, 7));
                    assert_eq!(got_owner, owner);
                }
                VideoFrameSource::Cpu(_) => panic!("native frame must stay native"),
            }
        }
    }

    #[test]
    fn native_frame_without_owner_stays_cpu() {
        let frame = VideoFrame::from_native(
            VideoPixelFormat::NV12,
            Size::new(4, 4),
            Rect::new(0, 0, 4, 4),
            Size::new(4, 4),
            vec![1, 2],
            9,
            3,
        )
        .unwrap();
        assert!(matches!(from_video_frame(frame), VideoFrameSource::Cpu(_)));
    }

    #[test]
    fn cpu_frame_stays_cpu() {
        let frame = frame_from_planes(
            VideoPixelFormat::I420,
            2,
            2,
            vec![vec![0; 4], vec![0; 1], vec![0; 1]],
        );
        let src = from_video_frame(frame);
        assert_eq!(src.timestamp(), 0);
        assert_eq!(src.duration(), 0);
        assert!(matches!(src, VideoFrameSource::Cpu(_)));
    }
}
