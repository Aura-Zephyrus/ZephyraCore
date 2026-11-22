//! 外部纹理构建器（§3.3，S4/S6/S7）。

use std::borrow::Cow;
use std::sync::Arc;

use zephyra_render_ext::prelude::{
    ColorSpace, CpuPlanes, ExternalTextureDesc, ExternalTextureFormat, ExternalTextureSource,
    FrameDesc, FrameSource, GpuFence, HdrMetadata, RawHandle,
};

/// 构建器层 CPU 描述（持有，便于延迟注册）。
#[derive(Clone, Debug)]
pub struct CpuTextureDescOwned {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub row_pitch: u32,
    pub format: ExternalTextureFormat,
}

/// 来源输入：构建器层（可含 `Auto`），注册时解析。
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum TextureSourceInput {
    #[default]
    Auto,
    NtHandle(RawHandle),
    VkImage {
        image: RawHandle,
        layout: u32,
        memory: Option<RawHandle>,
    },
    #[cfg(target_os = "linux")]
    Dmabuf {
        fd: i32,
        modifier: u64,
        planes: u32,
        offsets: [u32; 4],
        strides: [u32; 4],
    },
    CpuOwned(CpuTextureDescOwned),
}

/// 同步输入（S4）。
#[derive(Clone, Default)]
pub enum SyncInput {
    #[default]
    Auto,
    Fence(Arc<dyn GpuFence>),
    None,
}

impl core::fmt::Debug for SyncInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SyncInput::Auto => write!(f, "Auto"),
            SyncInput::Fence(_) => write!(f, "Fence(..)"),
            SyncInput::None => write!(f, "None"),
        }
    }
}

/// 外部纹理构建器：链式 API。
pub struct ExternalTextureBuilder {
    pub(crate) key: Cow<'static, str>,
    pub(crate) size: (u32, u32),
    pub(crate) format: ExternalTextureFormat,
    pub(crate) source: TextureSourceInput,
    pub(crate) sync: SyncInput,
    pub(crate) color_space: Option<ColorSpace>,
    pub(crate) hdr_metadata: Option<HdrMetadata>,
}

impl ExternalTextureBuilder {
    pub fn new(key: impl Into<Cow<'static, str>>, size: (u32, u32)) -> Self {
        Self {
            key: key.into(),
            size,
            format: ExternalTextureFormat::Rgba8UnormSrgb,
            source: TextureSourceInput::Auto,
            sync: SyncInput::Auto,
            color_space: None,
            hdr_metadata: None,
        }
    }
    pub fn format(mut self, fmt: ExternalTextureFormat) -> Self {
        self.format = fmt;
        self
    }
    pub fn source_auto(mut self, source: TextureSourceInput) -> Self {
        self.source = source;
        self
    }
    pub fn source_cpu(mut self, desc: CpuTextureDescOwned) -> Self {
        self.source = TextureSourceInput::CpuOwned(desc);
        self
    }
    pub fn sync_auto(mut self, sync: SyncInput) -> Self {
        self.sync = sync;
        self
    }
    pub fn color_space(mut self, cs: ColorSpace) -> Self {
        self.color_space = Some(cs);
        self
    }
    pub fn hdr_metadata(mut self, meta: HdrMetadata) -> Self {
        self.hdr_metadata = Some(meta);
        self
    }

    /// 供后端 `ExtContextImpl` 消费（导入/注册）。
    pub fn into_parts(self) -> (Cow<'static, str>, BuilderParts) {
        (
            self.key,
            BuilderParts {
                size: self.size,
                format: self.format,
                source: self.source,
                sync: self.sync,
                color_space: self.color_space,
                hdr_metadata: self.hdr_metadata,
            },
        )
    }
}

/// 构建器解析后的部件（供后端实现使用）。
#[derive(Clone, Debug)]
pub struct BuilderParts {
    pub size: (u32, u32),
    pub format: ExternalTextureFormat,
    pub source: TextureSourceInput,
    pub sync: SyncInput,
    pub color_space: Option<ColorSpace>,
    pub hdr_metadata: Option<HdrMetadata>,
}

impl BuilderParts {
    /// 解析为统一的 [`FrameDesc`]（新 `submit_frame` 入口使用）。
    ///
    /// CPU 源转成拥有所有权的 `CpuPlanes`；句柄源直接映射。同步栅栏沿用
    /// `SyncInput::Fence`。`Auto` 无法解析，返回 `None`。
    pub fn into_frame_desc(self) -> Option<FrameDesc> {
        let (source, sync) = match self.source {
            TextureSourceInput::CpuOwned(c) => {
                let planes = CpuPlanes {
                    // Vec -> Bytes 零拷贝转移分配（字段类型推断出 Into<Bytes>）。
                    planes: [Some(c.pixels.into()), None, None],
                    format: c.format,
                    row_pitch: [c.row_pitch, 0, 0],
                };
                (FrameSource::Cpu(Arc::new(planes)), None)
            }
            TextureSourceInput::NtHandle(h) => (
                FrameSource::Native(ExternalTextureSource::NtHandle {
                    handle: h,
                    source_backend: None,
                }),
                self.sync_fence(),
            ),
            TextureSourceInput::VkImage {
                image,
                layout,
                memory,
            } => (
                FrameSource::Native(ExternalTextureSource::VkImage {
                    image,
                    layout,
                    memory,
                }),
                self.sync_fence(),
            ),
            #[cfg(target_os = "linux")]
            TextureSourceInput::Dmabuf {
                fd,
                modifier,
                planes,
                offsets,
                strides,
            } => (
                FrameSource::Native(ExternalTextureSource::Dmabuf {
                    fd,
                    modifier,
                    planes,
                    offsets,
                    strides,
                }),
                self.sync_fence(),
            ),
            TextureSourceInput::Auto => return None,
        };
        Some(FrameDesc {
            size: self.size,
            format: self.format,
            source,
            sync,
            color_space: self.color_space.unwrap_or(ColorSpace::Srgb),
        })
    }

    /// 解析为契约层 `ExternalTextureDesc`（兼容视图；仅映射句柄源）。
    pub fn try_into_handle_desc(&self, key: Cow<'static, str>) -> Option<ExternalTextureDesc> {
        let (source, fence) = match &self.source {
            TextureSourceInput::NtHandle(h) => (
                ExternalTextureSource::NtHandle {
                    handle: *h,
                    source_backend: None,
                },
                self.sync_fence(),
            ),
            TextureSourceInput::VkImage {
                image,
                layout,
                memory,
            } => (
                ExternalTextureSource::VkImage {
                    image: *image,
                    layout: *layout,
                    memory: *memory,
                },
                self.sync_fence(),
            ),
            #[cfg(target_os = "linux")]
            TextureSourceInput::Dmabuf {
                fd,
                modifier,
                planes,
                offsets,
                strides,
            } => (
                ExternalTextureSource::Dmabuf {
                    fd: *fd,
                    modifier: *modifier,
                    planes: *planes,
                    offsets: *offsets,
                    strides: *strides,
                },
                self.sync_fence(),
            ),
            TextureSourceInput::Auto | TextureSourceInput::CpuOwned(_) => return None,
        };
        Some(ExternalTextureDesc {
            key,
            size: self.size,
            format: self.format,
            source,
            sync: fence,
        })
    }
    pub fn sync_fence(&self) -> Option<Arc<dyn GpuFence>> {
        match &self.sync {
            SyncInput::Fence(f) => Some(f.clone()),
            SyncInput::Auto | SyncInput::None => None,
        }
    }
    pub fn cpu_owned(&self) -> Option<&CpuTextureDescOwned> {
        match &self.source {
            TextureSourceInput::CpuOwned(c) => Some(c),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use zephyra_render_ext::prelude::{FenceHandle, FenceHandleKind, SyncError};

    /// Minimal no-GPU fence so `SyncInput::Fence` propagation can be pinned.
    struct TestFence(AtomicU64);

    impl GpuFence for TestFence {
        fn signaled_value(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
        fn wait(&self, _value: u64) -> Result<(), SyncError> {
            Ok(())
        }
        fn export_handle(&self) -> Result<FenceHandle, SyncError> {
            Ok(FenceHandle {
                raw: 42,
                kind: FenceHandleKind::Unknown,
            })
        }
        fn clone_handle(&self) -> Box<dyn GpuFence> {
            Box::new(TestFence(AtomicU64::new(self.0.load(Ordering::Relaxed))))
        }
    }

    fn fence(value: u64) -> Arc<dyn GpuFence> {
        Arc::new(TestFence(AtomicU64::new(value)))
    }

    fn parts(source: TextureSourceInput) -> BuilderParts {
        BuilderParts {
            size: (7, 9),
            format: ExternalTextureFormat::Bgra8Unorm,
            source,
            sync: SyncInput::Auto,
            color_space: None,
            hdr_metadata: None,
        }
    }

    #[test]
    fn into_frame_desc_cpu_owned_pins_planes_pitch_format_and_ignores_sync() {
        let owned = CpuTextureDescOwned {
            pixels: vec![10, 20, 30, 40, 50, 60],
            width: 3,
            height: 2,
            row_pitch: 12,
            format: ExternalTextureFormat::Nv12,
        };
        let mut p = parts(TextureSourceInput::CpuOwned(owned));
        // Builder format deliberately differs from the desc format, and a fence is
        // supplied -- the CPU path must use the desc format and drop the fence.
        p.format = ExternalTextureFormat::Rgba8Unorm;
        p.sync = SyncInput::Fence(fence(5));

        let d = p.into_frame_desc().expect("cpu frame desc");
        assert_eq!(d.size, (7, 9));
        assert_eq!(d.format, ExternalTextureFormat::Rgba8Unorm);
        assert!(d.sync.is_none(), "CPU source must not carry a fence");
        assert_eq!(d.color_space, ColorSpace::Srgb);

        let planes = match &d.source {
            FrameSource::Cpu(pl) => pl,
            _ => panic!("expected CPU source"),
        };
        assert_eq!(planes.format, ExternalTextureFormat::Nv12);
        assert_eq!(planes.row_pitch, [12, 0, 0]);
        assert_eq!(planes.plane(0), Some(&[10u8, 20, 30, 40, 50, 60][..]));
        assert!(planes.plane(1).is_none());
        assert!(planes.plane(2).is_none());
    }

    #[test]
    fn into_frame_desc_table_drives_all_source_variants() {
        let f: Arc<dyn GpuFence> = fence(11);
        // (name, source, requested color space, expect fence)
        let cases: Vec<(&str, TextureSourceInput, Option<ColorSpace>, bool)> = vec![
            (
                "nt",
                TextureSourceInput::NtHandle(RawHandle(0xABCD)),
                None,
                true,
            ),
            (
                "vk",
                TextureSourceInput::VkImage {
                    image: RawHandle(3),
                    layout: 4,
                    memory: Some(RawHandle(5)),
                },
                Some(ColorSpace::Bt709),
                true,
            ),
            (
                "auto",
                TextureSourceInput::Auto,
                Some(ColorSpace::Linear),
                false,
            ),
        ];

        for (name, source, cs, has_sync) in cases {
            let mut p = parts(source);
            p.color_space = cs;
            p.sync = SyncInput::Fence(f.clone());

            let maybe = p.into_frame_desc();
            if name == "auto" {
                assert!(maybe.is_none(), "Auto source must not resolve");
                continue;
            }
            let d = maybe.expect(name);
            assert_eq!(d.size, (7, 9), "{name}");
            assert_eq!(d.format, ExternalTextureFormat::Bgra8Unorm, "{name}");
            assert_eq!(d.color_space, cs.unwrap_or(ColorSpace::Srgb), "{name}");
            assert_eq!(d.sync.is_some(), has_sync, "{name}");
            if has_sync {
                assert_eq!(d.sync.as_ref().unwrap().signaled_value(), 11, "{name}");
            }

            match (&d.source, name) {
                (
                    FrameSource::Native(ExternalTextureSource::NtHandle {
                        handle,
                        source_backend,
                    }),
                    "nt",
                ) => {
                    assert_eq!(*handle, RawHandle(0xABCD));
                    assert_eq!(*source_backend, None);
                }
                (
                    FrameSource::Native(ExternalTextureSource::VkImage {
                        image,
                        layout,
                        memory,
                    }),
                    "vk",
                ) => {
                    assert_eq!(*image, RawHandle(3));
                    assert_eq!(*layout, 4);
                    assert_eq!(*memory, Some(RawHandle(5)));
                }
                _ => panic!("unexpected source for {name}"),
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn into_frame_desc_dmabuf_native() {
        let mut p = parts(TextureSourceInput::Dmabuf {
            fd: 12,
            modifier: 0x1234,
            planes: 2,
            offsets: [1, 2, 3, 4],
            strides: [5, 6, 7, 8],
        });
        p.sync = SyncInput::Fence(fence(15));
        let d = p.into_frame_desc().expect("dmabuf frame desc");
        match &d.source {
            FrameSource::Native(ExternalTextureSource::Dmabuf {
                fd,
                modifier,
                planes,
                offsets,
                strides,
            }) => {
                assert_eq!(*fd, 12);
                assert_eq!(*modifier, 0x1234);
                assert_eq!(*planes, 2);
                assert_eq!(*offsets, [1, 2, 3, 4]);
                assert_eq!(*strides, [5, 6, 7, 8]);
            }
            _ => panic!("expected Dmabuf source"),
        }
        assert_eq!(d.sync.as_ref().unwrap().signaled_value(), 15);
        assert_eq!(d.color_space, ColorSpace::Srgb);
    }

    #[test]
    fn builder_parts_sync_fence_only_returns_fence() {
        let mut p = parts(TextureSourceInput::Auto);
        assert!(p.sync_fence().is_none(), "Auto -> None");
        p.sync = SyncInput::None;
        assert!(p.sync_fence().is_none(), "None -> None");
        let f = fence(3);
        p.sync = SyncInput::Fence(f);
        let g = p.sync_fence().expect("Fence -> Some");
        assert_eq!(g.signaled_value(), 3);
        drop(p);
        assert_eq!(g.signaled_value(), 3, "cloned Arc must outlive parts");
    }

    #[test]
    fn builder_parts_cpu_owned_only_for_cpu_source() {
        let cpu = CpuTextureDescOwned {
            pixels: vec![1, 2, 3, 4],
            width: 2,
            height: 2,
            row_pitch: 4,
            format: ExternalTextureFormat::Rgba8Unorm,
        };
        assert_eq!(
            parts(TextureSourceInput::CpuOwned(cpu))
                .cpu_owned()
                .unwrap()
                .pixels,
            vec![1, 2, 3, 4]
        );
        assert!(parts(TextureSourceInput::NtHandle(RawHandle(1)))
            .cpu_owned()
            .is_none());
        assert!(parts(TextureSourceInput::VkImage {
            image: RawHandle(1),
            layout: 0,
            memory: None,
        })
        .cpu_owned()
        .is_none());
        assert!(parts(TextureSourceInput::Auto).cpu_owned().is_none());
    }

    /// Every field other than the one under test must stay at its default.
    fn assert_defaults(p: &BuilderParts, except: &str) {
        assert_eq!(p.size, (4, 5));
        if except != "format" {
            assert_eq!(p.format, ExternalTextureFormat::Rgba8UnormSrgb);
        }
        if except != "source" {
            assert!(matches!(p.source, TextureSourceInput::Auto));
        }
        if except != "sync" {
            assert!(matches!(p.sync, SyncInput::Auto));
        }
        if except != "color_space" {
            assert_eq!(p.color_space, None);
        }
        if except != "hdr_metadata" {
            assert_eq!(p.hdr_metadata, None);
        }
    }

    #[test]
    fn builder_setters_each_change_only_their_field() {
        // format
        let (k, p) = ExternalTextureBuilder::new("k", (4, 5))
            .format(ExternalTextureFormat::Rgba32Float)
            .into_parts();
        assert_eq!(k, "k");
        assert_eq!(p.format, ExternalTextureFormat::Rgba32Float);
        assert_defaults(&p, "format");

        // source_auto
        let (_, p) = ExternalTextureBuilder::new("k", (4, 5))
            .source_auto(TextureSourceInput::NtHandle(RawHandle(77)))
            .into_parts();
        assert!(matches!(&p.source, TextureSourceInput::NtHandle(h) if *h == RawHandle(77)));
        assert_defaults(&p, "source");

        // source_cpu
        let (_, p) = ExternalTextureBuilder::new("k", (4, 5))
            .source_cpu(CpuTextureDescOwned {
                pixels: vec![9, 9, 9, 9],
                width: 2,
                height: 2,
                row_pitch: 4,
                format: ExternalTextureFormat::Rgba8Unorm,
            })
            .into_parts();
        assert!(p.cpu_owned().is_some());
        assert_eq!(p.cpu_owned().unwrap().row_pitch, 4);
        assert_defaults(&p, "source");

        // sync_auto
        let (_, p) = ExternalTextureBuilder::new("k", (4, 5))
            .sync_auto(SyncInput::Fence(fence(6)))
            .into_parts();
        assert_eq!(p.sync_fence().unwrap().signaled_value(), 6);
        assert_defaults(&p, "sync");

        // color_space
        let (_, p) = ExternalTextureBuilder::new("k", (4, 5))
            .color_space(ColorSpace::Bt2020)
            .into_parts();
        assert_eq!(p.color_space, Some(ColorSpace::Bt2020));
        assert_defaults(&p, "color_space");

        // hdr_metadata
        let meta = HdrMetadata {
            max_luminance: 1000.0,
            min_luminance: 0.0001,
            max_content_light_level: 500.0,
            max_frame_average_light_level: 250.0,
        };
        let (_, p) = ExternalTextureBuilder::new("k", (4, 5))
            .hdr_metadata(meta)
            .into_parts();
        assert_eq!(p.hdr_metadata, Some(meta));
        assert_defaults(&p, "hdr_metadata");
    }

    #[test]
    fn try_into_handle_desc_preserves_key_size_format_and_sync() {
        let mut p = parts(TextureSourceInput::NtHandle(RawHandle(0x55)));
        p.size = (10, 20);
        p.format = ExternalTextureFormat::Rgba16Float;
        p.sync = SyncInput::Fence(fence(21));
        let d = p
            .try_into_handle_desc(Cow::Borrowed("mykey"))
            .expect("handle desc");
        assert_eq!(d.key, "mykey");
        assert_eq!(d.size, (10, 20));
        assert_eq!(d.format, ExternalTextureFormat::Rgba16Float);
        assert_eq!(d.sync.unwrap().signaled_value(), 21);
        match &d.source {
            ExternalTextureSource::NtHandle {
                handle,
                source_backend,
            } => {
                assert_eq!(*handle, RawHandle(0x55));
                assert_eq!(*source_backend, None);
            }
            _ => panic!("expected NtHandle"),
        }
    }

    #[test]
    fn try_into_handle_desc_rejects_auto_and_cpu() {
        assert!(parts(TextureSourceInput::Auto)
            .try_into_handle_desc(Cow::Borrowed("k"))
            .is_none());
        assert!(parts(TextureSourceInput::CpuOwned(CpuTextureDescOwned {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            row_pitch: 4,
            format: ExternalTextureFormat::Rgba8Unorm,
        }))
        .try_into_handle_desc(Cow::Borrowed("k"))
        .is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_into_handle_desc_maps_dmabuf() {
        let p = parts(TextureSourceInput::Dmabuf {
            fd: 9,
            modifier: 0xAB,
            planes: 1,
            offsets: [0, 0, 0, 0],
            strides: [64, 0, 0, 0],
        });
        let d = p
            .try_into_handle_desc(Cow::Borrowed("dm"))
            .expect("dmabuf desc");
        match &d.source {
            ExternalTextureSource::Dmabuf {
                fd,
                modifier,
                planes,
                offsets,
                strides,
            } => {
                assert_eq!(*fd, 9);
                assert_eq!(*modifier, 0xAB);
                assert_eq!(*planes, 1);
                assert_eq!(*offsets, [0, 0, 0, 0]);
                assert_eq!(*strides, [64, 0, 0, 0]);
            }
            _ => panic!("expected Dmabuf"),
        }
    }
}
