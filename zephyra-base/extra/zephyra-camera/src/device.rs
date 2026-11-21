//! 与设备无关的数据模型：摄像头识别、分辨率/格式能力、选择策略与错误类型。
//!
//! 本模块始终编译且只依赖 `std`。启用 `capture` feature 时，`capture` 模块
//! 才与 nokhwa 交互，并把结果转换成这里的自有类型——因此公共 API 中完全不出现
//! nokhwa 的类型，更换采集后端不会影响调用方。
//!
//! 这里的"识别"指**设备识别**（枚举、名称/唯一 ID、热插拔变化），不是图像内容识别。

use std::fmt;
use std::path::Path;

/// 分辨率（宽 × 高，单位像素）。
#[derive(Copy, Clone, Debug, Default, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// 宽高比（宽 / 高）。高度为 0 时返回 `f32::INFINITY`。
    #[must_use]
    pub fn aspect_ratio(self) -> f32 {
        if self.height == 0 {
            f32::INFINITY
        } else {
            self.width as f32 / self.height as f32
        }
    }

    /// 是否近似 16:9（容差 0.01）。
    #[must_use]
    pub fn is_wide(self) -> bool {
        (self.aspect_ratio() - 16.0 / 9.0).abs() <= 0.01
    }

    /// 像素总数（`u64`，避免 4K 以上相乘溢出 `u32`）。
    #[must_use]
    pub const fn pixels(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// 宽高是否都为偶数（NV12/I420 等 4:2:0 格式的硬性要求）。
    #[must_use]
    pub const fn is_even(self) -> bool {
        self.width.is_multiple_of(2) && self.height.is_multiple_of(2)
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// 采集后端。与 nokhwa 的 `ApiBackend` 一一对应，但不暴露该类型。
#[derive(Copy, Clone, Debug, Default, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Backend {
    /// 由底层按平台自动选择。
    #[default]
    Auto,
    AvFoundation,
    Video4Linux,
    UsbVideoClass,
    MediaFoundation,
    OpenCv,
    GStreamer,
    Network,
    Browser,
}

impl Backend {
    /// 稳定的英文标签，适合写日志和配置文件。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::AvFoundation => "AVFoundation",
            Self::Video4Linux => "Video4Linux",
            Self::UsbVideoClass => "USB Video Class",
            Self::MediaFoundation => "Media Foundation",
            Self::OpenCv => "OpenCV",
            Self::GStreamer => "GStreamer",
            Self::Network => "Network",
            Self::Browser => "Browser",
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// 设备索引。绝大多数后端用数字；网络摄像头用字符串（URL）。
#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DeviceIndex {
    Index(u32),
    String(String),
}

impl Default for DeviceIndex {
    fn default() -> Self {
        Self::Index(0)
    }
}

impl DeviceIndex {
    /// 数字索引；字符串索引（即使内容是数字）一律返回 `None`，避免误用。
    #[must_use]
    pub const fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Index(i) => Some(*i),
            Self::String(_) => None,
        }
    }

    /// 日志/比较用的字符串形式。
    #[must_use]
    pub fn as_string(&self) -> String {
        match self {
            Self::Index(i) => i.to_string(),
            Self::String(s) => s.clone(),
        }
    }

    #[must_use]
    pub const fn is_numeric(&self) -> bool {
        matches!(self, Self::Index(_))
    }
}

impl From<u32> for DeviceIndex {
    fn from(index: u32) -> Self {
        Self::Index(index)
    }
}

impl From<String> for DeviceIndex {
    fn from(index: String) -> Self {
        Self::String(index)
    }
}

impl From<&str> for DeviceIndex {
    fn from(index: &str) -> Self {
        Self::String(index.to_owned())
    }
}

impl fmt::Display for DeviceIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(i) => write!(f, "{i}"),
            Self::String(s) => f.write_str(s),
        }
    }
}

/// 像素格式（FourCC）。对应 nokhwa 的 `FrameFormat`，但由本 crate 自有。
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PixelFormat {
    Mjpeg,
    Yuyv,
    Nv12,
    Gray,
    RawRgb,
    RawBgr,
}

impl PixelFormat {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mjpeg => "MJPEG",
            Self::Yuyv => "YUYV",
            Self::Nv12 => "NV12",
            Self::Gray => "GRAY",
            Self::RawRgb => "RGB24",
            Self::RawBgr => "BGR24",
        }
    }

    #[must_use]
    pub const fn is_nv12(self) -> bool {
        matches!(self, Self::Nv12)
    }

    /// 原生 NV12 直传路径可用的格式（无需 CPU 解码）。
    #[must_use]
    pub const fn is_planar_yuv(self) -> bool {
        matches!(self, Self::Nv12)
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// 一种设备媒体类型：分辨率 + 像素格式 + 帧率。
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FormatInfo {
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub fps: u32,
}

impl FormatInfo {
    #[must_use]
    pub const fn new(resolution: Resolution, format: PixelFormat, fps: u32) -> Self {
        Self {
            resolution,
            format,
            fps,
        }
    }

    #[must_use]
    pub const fn width(self) -> u32 {
        self.resolution.width
    }

    #[must_use]
    pub const fn height(self) -> u32 {
        self.resolution.height
    }

    #[must_use]
    pub const fn is_nv12(self) -> bool {
        self.format.is_nv12()
    }

    /// 人类可读标签，例如 `1280x720@30 NV12`。
    #[must_use]
    pub fn label(self) -> String {
        format!("{}@{} {}", self.resolution, self.fps, self.format)
    }
}

impl fmt::Display for FormatInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// 枚举到的摄像头信息。字段是自有类型，不依赖任何采集后端。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceInfo {
    index: DeviceIndex,
    name: String,
    description: String,
    unique_id: String,
    backend: Backend,
}

impl DeviceInfo {
    #[must_use]
    pub fn new(
        index: DeviceIndex,
        name: impl Into<String>,
        description: impl Into<String>,
        unique_id: impl Into<String>,
        backend: Backend,
    ) -> Self {
        Self {
            index,
            name: name.into(),
            description: description.into(),
            unique_id: unique_id.into(),
            backend,
        }
    }

    #[must_use]
    pub const fn index(&self) -> &DeviceIndex {
        &self.index
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 稳定的唯一标识（Windows 上是设备符号链接）。跨枚举 / 重启通常不变，
    /// 而数字索引可能因设备插拔顺序改变而改变。
    #[must_use]
    pub fn unique_id(&self) -> &str {
        &self.unique_id
    }

    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// UI 展示名：优先名称，其次描述，最后回退到 `Camera #index`。
    #[must_use]
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.clone()
        } else if !self.description.trim().is_empty() {
            self.description.clone()
        } else {
            format!("Camera #{}", self.index)
        }
    }

    /// 名称/描述是否包含查询串（忽略大小写）。
    #[must_use]
    pub fn matches_name(&self, query: &str) -> bool {
        let query = query.trim().to_ascii_lowercase();
        !query.is_empty()
            && (self.name.to_ascii_lowercase().contains(&query)
                || self.description.to_ascii_lowercase().contains(&query))
    }

    /// 唯一 ID 是否与查询串匹配：先按路径末段精确比较，再按大小写不敏感包含。
    /// 这样用户既能粘贴完整符号链接，也能输入末段的设备实例 ID。
    #[must_use]
    pub fn matches_id(&self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty() {
            return false;
        }
        let file_name = Path::new(&self.unique_id)
            .file_name()
            .map(|s| s.to_string_lossy());
        let last_segment = file_name.as_deref().unwrap_or(self.unique_id.as_str());
        query.eq_ignore_ascii_case(last_segment)
            || self
                .unique_id
                .to_ascii_lowercase()
                .contains(&query.to_ascii_lowercase())
    }

    /// 名称或唯一 ID 任一匹配。
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        self.matches_id(query) || self.matches_name(query)
    }
}

impl fmt::Display for DeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}] ({})",
            self.display_name(),
            self.index,
            self.backend
        )
    }
}

/// 设备声明的所有媒体类型（已按「分辨率从大到小 → 格式优先级 → 帧率从高到低」排序去重）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceCapabilities {
    formats: Vec<FormatInfo>,
}

fn format_priority(format: PixelFormat) -> u8 {
    match format {
        PixelFormat::Nv12 => 0,
        PixelFormat::Mjpeg => 1,
        PixelFormat::Yuyv => 2,
        PixelFormat::RawRgb => 3,
        PixelFormat::RawBgr => 4,
        PixelFormat::Gray => 5,
    }
}

impl DeviceCapabilities {
    /// 构造能力集合并规整排序（输入顺序不影响结果）。
    #[must_use]
    pub fn new(mut formats: Vec<FormatInfo>) -> Self {
        formats.sort_by(|a, b| {
            b.resolution
                .width
                .cmp(&a.resolution.width)
                .then_with(|| b.resolution.height.cmp(&a.resolution.height))
                .then_with(|| format_priority(a.format).cmp(&format_priority(b.format)))
                .then_with(|| b.fps.cmp(&a.fps))
        });
        formats.dedup();
        Self { formats }
    }

    #[must_use]
    pub fn formats(&self) -> &[FormatInfo] {
        &self.formats
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.formats.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.formats.len()
    }

    /// 设备支持的分辨率，从大到小去重。
    #[must_use]
    pub fn resolutions(&self) -> Vec<Resolution> {
        let mut resolutions: Vec<Resolution> = self
            .formats
            .iter()
            .map(|format| format.resolution)
            .collect();
        resolutions.dedup();
        resolutions
    }

    /// 指定分辨率下可用的所有媒体类型。
    #[must_use]
    pub fn formats_for(&self, resolution: Resolution) -> Vec<FormatInfo> {
        self.formats
            .iter()
            .copied()
            .filter(|format| format.resolution == resolution)
            .collect()
    }

    #[must_use]
    pub fn max_resolution(&self) -> Option<Resolution> {
        self.formats.first().map(|format| format.resolution)
    }

    /// 所有原生 NV12 媒体类型（零拷贝直传路径可用）。
    #[must_use]
    pub fn nv12_formats(&self) -> Vec<FormatInfo> {
        self.formats
            .iter()
            .copied()
            .filter(|format| format.is_nv12())
            .collect()
    }

    /// 该分辨率是否支持 NV12。
    #[must_use]
    pub fn supports_nv12(&self, resolution: Resolution) -> bool {
        self.formats_for(resolution)
            .iter()
            .any(|format| format.is_nv12())
    }
}

/// 打开摄像头时的格式偏好。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FormatPreference {
    /// 目标分辨率。`None` 表示由设备决定（取能力表中最大的分辨率）。
    pub resolution: Option<Resolution>,
    /// 当 NV12 帧率不低于其他格式时，优先走 NV12 零拷贝直传。
    pub prefer_nv12: bool,
    /// 最低可接受帧率，0 表示不限制。
    pub min_fps: u32,
    /// 严格像素格式匹配（对应 Chromium 的「请求 NV12 时要求精确匹配」）。
    /// 为 `true` 且 `prefer_nv12` 时，非 NV12 候选一律拒绝——不会被静默换成
    /// MJPEG 再软件解码。设备不支持 NV12 时宁可打开失败，也不给低画质回退。
    pub strict_format: bool,
}

impl Default for FormatPreference {
    fn default() -> Self {
        Self {
            resolution: None,
            prefer_nv12: true,
            min_fps: 0,
            strict_format: false,
        }
    }
}

impl FormatPreference {
    #[must_use]
    pub const fn new(resolution: Resolution) -> Self {
        Self {
            resolution: Some(resolution),
            prefer_nv12: true,
            min_fps: 0,
            strict_format: false,
        }
    }

    /// 要求精确 NV12：设备不支持时打开失败，而不是悄悄降级到 MJPEG。
    #[must_use]
    pub const fn strict_nv12(mut self) -> Self {
        self.prefer_nv12 = true;
        self.strict_format = true;
        self
    }

    #[must_use]
    pub const fn with_min_fps(mut self, min_fps: u32) -> Self {
        self.min_fps = min_fps;
        self
    }

    #[must_use]
    pub const fn without_nv12(mut self) -> Self {
        self.prefer_nv12 = false;
        self
    }
}

/// 在指定分辨率的候选格式中挑最优者。
fn best_at_resolution(
    candidates: &[FormatInfo],
    resolution: Resolution,
    prefer_nv12: bool,
) -> Option<FormatInfo> {
    let at_res: Vec<FormatInfo> = candidates
        .iter()
        .copied()
        .filter(|format| format.resolution == resolution)
        .collect();
    if at_res.is_empty() {
        return None;
    }
    let choose_best = |pool: Vec<FormatInfo>| -> Option<FormatInfo> {
        pool.into_iter().max_by_key(|format| {
            (
                format.fps,
                std::cmp::Reverse(format_priority(format.format)),
            )
        })
    };
    if !prefer_nv12 {
        // 明确关闭 NV12 偏好时，只要还有非 NV12 格式就不选 NV12。
        let non_nv12: Vec<FormatInfo> = at_res
            .iter()
            .copied()
            .filter(|format| !format.is_nv12())
            .collect();
        return if non_nv12.is_empty() {
            choose_best(at_res)
        } else {
            choose_best(non_nv12)
        };
    }
    let best_overall = choose_best(at_res.clone())?;
    let best_nv12 = at_res
        .iter()
        .copied()
        .filter(|format| format.is_nv12())
        .max_by_key(|format| format.fps);
    // 与原设备的协商启发式一致：NV12 帧率不低于最优通用格式才选它。
    match best_nv12 {
        Some(nv12) if nv12.fps >= best_overall.fps => Some(nv12),
        _ => Some(best_overall),
    }
}

/// 按偏好从能力表中选一种格式（纯函数，无设备依赖，便于单测）。
///
/// 步骤：过滤最低帧率 → 若有指定分辨率且设备支持则锁定它，否则回退到像素数最大的
/// 分辨率 → 在该分辨率内按 [`FormatPreference::prefer_nv12`] 挑格式。
/// 分辨率是**偏好而非硬约束**；需要严格要求时用 [`select_exact_format`]。
#[must_use]
pub fn select_format(formats: &[FormatInfo], preference: &FormatPreference) -> Option<FormatInfo> {
    select_format_detailed(formats, preference).map(|report| report.chosen)
}

/// 候选被拒绝的原因（用于向 UI 解释画质选择）。
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RejectReason {
    /// 帧率低于用户设置的下限。
    BelowMinFps,
    /// 目标分辨率不支持，最终选了别的分辨率。
    ResolutionMismatch,
    /// 严格模式下像素格式不匹配（如请求 NV12 但设备只给 MJPEG）。
    FormatMismatch,
}

/// 带解释的格式选择报告。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectionReport {
    pub chosen: FormatInfo,
    /// 选择理由的固定标签：`exact-nv12` / `preferred-nv12` / `highest-fps` /
    /// `fallback-largest-resolution`。（用 `String` 而非 `&'static str`，
    /// 以便 serde 能反序列化出任意生命周期的值。）
    pub reason: String,
    /// 被拒绝的候选及原因（最多保留与选择相关的若干条）。
    pub rejected: Vec<(FormatInfo, RejectReason)>,
}

/// [`select_format`] 的可解释版本：除选中格式外，还报告为什么排除其他候选。
#[must_use]
pub fn select_format_detailed(
    formats: &[FormatInfo],
    preference: &FormatPreference,
) -> Option<SelectionReport> {
    let strict_nv12 = preference.strict_format && preference.prefer_nv12;
    let mut rejected = Vec::new();
    let mut candidates = Vec::new();
    for &format in formats {
        if format.fps < preference.min_fps {
            rejected.push((format, RejectReason::BelowMinFps));
            continue;
        }
        if strict_nv12 && !format.is_nv12() {
            rejected.push((format, RejectReason::FormatMismatch));
            continue;
        }
        // 分辨率不匹配只记为"非首选"，仍保留为候选：指定分辨率设备不支持时
        // 回退到像素数最大的分辨率（与 `select_format` 原有行为一致）。
        if preference
            .resolution
            .is_some_and(|wanted| format.resolution != wanted)
        {
            rejected.push((format, RejectReason::ResolutionMismatch));
        }
        candidates.push(format);
    }
    if candidates.is_empty() {
        return None;
    }
    // 精确分辨率若一个都没有，退回所有满足帧率/格式约束的候选。
    let resolution = preference
        .resolution
        .filter(|wanted| candidates.iter().any(|f| f.resolution == *wanted))
        .or_else(|| {
            candidates
                .iter()
                .map(|format| format.resolution)
                .max_by_key(|resolution| resolution.pixels())
        })?;
    let chosen = best_at_resolution(&candidates, resolution, preference.prefer_nv12)?;
    let reason = if strict_nv12 {
        "exact-nv12"
    } else if chosen.is_nv12() {
        "preferred-nv12"
    } else if preference.resolution == Some(resolution) {
        "highest-fps"
    } else {
        "fallback-largest-resolution"
    };
    Some(SelectionReport {
        chosen,
        reason: reason.to_owned(),
        rejected,
    })
}

/// 严格要求精确分辨率与最低帧率的版本，不满足就返回 `None`。
#[must_use]
pub fn select_exact_format(
    formats: &[FormatInfo],
    resolution: Resolution,
    min_fps: u32,
) -> Option<FormatInfo> {
    let candidates: Vec<FormatInfo> = formats
        .iter()
        .copied()
        .filter(|format| format.resolution == resolution && format.fps >= min_fps)
        .collect();
    best_at_resolution(&candidates, resolution, true)
}

// ---- 多分辨率 / 多帧率支持 -------------------------------------------------

/// 能力矩阵的一行：某个分辨率下支持的全部帧率与对应媒体类型。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ResolutionFps {
    pub resolution: Resolution,
    /// 该分辨率下可用的帧率（从高到低去重）。
    pub fps: Vec<u32>,
    /// 该分辨率下的所有媒体类型（与 [`DeviceCapabilities::formats`] 的排序一致）。
    pub formats: Vec<FormatInfo>,
}

impl ResolutionFps {
    #[must_use]
    pub const fn new(resolution: Resolution, fps: Vec<u32>, formats: Vec<FormatInfo>) -> Self {
        Self {
            resolution,
            fps,
            formats,
        }
    }

    /// 指定帧率下最优的媒体类型（优先 NV12）。
    #[must_use]
    pub fn best_format_for_fps(&self, fps: u32) -> Option<FormatInfo> {
        self.formats
            .iter()
            .copied()
            .filter(|format| format.fps == fps)
            .min_by_key(|format| format_priority(format.format))
    }
}

impl DeviceCapabilities {
    /// 全量分辨率 × 帧率矩阵，分辨率从大到小，帧率从高到低。
    #[must_use]
    pub fn matrix(&self) -> Vec<ResolutionFps> {
        self.resolutions()
            .into_iter()
            .map(|resolution| {
                let formats = self.formats_for(resolution);
                let mut fps: Vec<u32> = formats.iter().map(|format| format.fps).collect();
                fps.sort_unstable_by(|a, b| b.cmp(a));
                fps.dedup();
                ResolutionFps::new(resolution, fps, formats)
            })
            .collect()
    }

    /// 设备支持的所有帧率，从高到低去重（跨分辨率）。
    #[must_use]
    pub fn frame_rates(&self) -> Vec<u32> {
        let mut fps: Vec<u32> = self.formats.iter().map(|format| format.fps).collect();
        fps.sort_unstable_by(|a, b| b.cmp(a));
        fps.dedup();
        fps
    }

    /// 指定分辨率下支持的帧率，从高到低去重。
    #[must_use]
    pub fn fps_for(&self, resolution: Resolution) -> Vec<u32> {
        let mut fps: Vec<u32> = self
            .formats_for(resolution)
            .iter()
            .map(|format| format.fps)
            .collect();
        fps.sort_unstable_by(|a, b| b.cmp(a));
        fps.dedup();
        fps
    }

    /// 支持指定帧率的所有分辨率，从大到小去重。
    #[must_use]
    pub fn resolutions_for_fps(&self, fps: u32) -> Vec<Resolution> {
        let mut resolutions: Vec<Resolution> = self
            .formats
            .iter()
            .filter(|format| format.fps == fps)
            .map(|format| format.resolution)
            .collect();
        resolutions.dedup();
        resolutions
    }

    /// 精确分辨率 + 帧率下最优的媒体类型（优先 NV12）。
    #[must_use]
    pub fn best_format_for(&self, resolution: Resolution, fps: u32) -> Option<FormatInfo> {
        self.formats_for(resolution)
            .into_iter()
            .filter(|format| format.fps == fps)
            .min_by_key(|format| format_priority(format.format))
    }

    /// 设备是否支持给定的分辨率 + 帧率（不限像素格式）。
    #[must_use]
    pub fn supports(&self, resolution: Resolution, fps: u32) -> bool {
        self.formats_for(resolution)
            .iter()
            .any(|format| format.fps == fps)
    }

    /// 设备支持的最低 / 最高帧率。
    #[must_use]
    pub fn fps_range(&self) -> Option<(u32, u32)> {
        let fps = self.frame_rates();
        fps.first().zip(fps.last()).map(|(&max, &min)| (min, max))
    }

    /// 离目标分辨率最近的可用分辨率（按像素数差，平局时取宽更大的）。
    #[must_use]
    pub fn nearest_resolution(&self, target: Resolution) -> Option<Resolution> {
        self.resolutions().into_iter().min_by_key(|resolution| {
            (
                resolution.pixels().abs_diff(target.pixels()),
                std::cmp::Reverse(resolution.width),
            )
        })
    }
}

/// 有序格式候选项：打开时按顺序尝试，第一个可用者胜出。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FormatCandidate {
    /// 目标分辨率。`None` 表示任意（取候选集中像素数最大的）。
    pub resolution: Option<Resolution>,
    /// 目标帧率。`None` 表示任意（取该分辨率下最高的）。
    pub fps: Option<u32>,
    /// 最低可接受帧率。
    pub min_fps: u32,
    /// 优先 NV12 零拷贝直传。
    pub prefer_nv12: bool,
    /// 严格像素格式匹配（NV12 不可用时直接跳过该候选项）。
    pub strict_format: bool,
}

impl FormatCandidate {
    #[must_use]
    pub const fn new(resolution: Resolution, fps: Option<u32>) -> Self {
        Self {
            resolution: Some(resolution),
            fps,
            min_fps: 0,
            prefer_nv12: true,
            strict_format: false,
        }
    }

    /// 任意分辨率、任意帧率。
    #[must_use]
    pub const fn any() -> Self {
        Self {
            resolution: None,
            fps: None,
            min_fps: 0,
            prefer_nv12: true,
            strict_format: false,
        }
    }

    #[must_use]
    pub const fn with_min_fps(mut self, min_fps: u32) -> Self {
        self.min_fps = min_fps;
        self
    }

    #[must_use]
    pub const fn strict_nv12(mut self) -> Self {
        self.prefer_nv12 = true;
        self.strict_format = true;
        self
    }

    #[must_use]
    pub const fn without_nv12(mut self) -> Self {
        self.prefer_nv12 = false;
        self
    }
}

impl From<FormatPreference> for FormatCandidate {
    fn from(preference: FormatPreference) -> Self {
        Self {
            resolution: preference.resolution,
            fps: None,
            min_fps: preference.min_fps,
            prefer_nv12: preference.prefer_nv12,
            strict_format: preference.strict_format,
        }
    }
}

/// 候选项选择的附带信息（哪个候选项胜出）。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CandidateSelection {
    pub chosen: FormatInfo,
    pub index: usize,
}

/// 按有序候选列表选择格式：逐项尝试，第一个能选出格式的候选项胜出。
///
/// 典型用法：`[1080p@30, 720p@60, 480p@30, any]`，设备不支持高分辨率时自动降级。
#[must_use]
pub fn select_from_candidates(
    formats: &[FormatInfo],
    candidates: &[FormatCandidate],
) -> Option<CandidateSelection> {
    for (index, candidate) in candidates.iter().enumerate() {
        // 候选列表中的分辨率是硬约束：不支持就跳到下一个候选（与单个 FormatPreference
        // 的"偏好"语义不同——后者不支持时会回退到最大分辨率）。
        if let Some(wanted) = candidate.resolution {
            if !formats.iter().any(|format| format.resolution == wanted) {
                continue;
            }
        }
        let filtered: Vec<FormatInfo> = formats
            .iter()
            .copied()
            .filter(|format| {
                candidate
                    .resolution
                    .is_none_or(|res| format.resolution == res)
            })
            .filter(|format| format.fps >= candidate.min_fps)
            .filter(|format| !candidate.strict_format || candidate.prefer_nv12 && format.is_nv12())
            .filter(|format| candidate.fps.is_none_or(|fps| format.fps == fps))
            .collect();
        let preference = FormatPreference {
            resolution: candidate.resolution,
            prefer_nv12: candidate.prefer_nv12,
            min_fps: candidate.min_fps,
            strict_format: candidate.strict_format,
        };
        if let Some(chosen) = select_format(&filtered, &preference) {
            if candidate.fps.is_some_and(|fps| chosen.fps != fps) {
                continue;
            }
            return Some(CandidateSelection { chosen, index });
        }
    }
    None
}

/// 按有序偏好列表选择格式：与 [`select_from_candidates`] 一样，指定分辨率是硬约束，
/// 不支持时尝试下一个偏好（单个 [`select_format`] 才会回退到最大分辨率）。
#[must_use]
pub fn select_from_preferences(
    formats: &[FormatInfo],
    preferences: &[FormatPreference],
) -> Option<CandidateSelection> {
    for (index, preference) in preferences.iter().enumerate() {
        if let Some(wanted) = preference.resolution {
            if !formats.iter().any(|format| format.resolution == wanted) {
                continue;
            }
        }
        let filtered: Vec<FormatInfo> = formats
            .iter()
            .copied()
            .filter(|format| {
                preference
                    .resolution
                    .is_none_or(|resolution| format.resolution == resolution)
            })
            .collect();
        if let Some(chosen) = select_format(&filtered, preference) {
            return Some(CandidateSelection { chosen, index });
        }
    }
    None
}

/// 自适应分辨率/帧率策略。
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AdaptivePolicy {
    /// 允许的最低帧率。
    pub min_fps: u32,
    /// 允许的最高帧率（0 = 不限）。
    pub max_fps: u32,
    /// 允许的最小像素数（0 = 不限）。
    pub min_pixels: u64,
    /// 允许的最大像素数（0 = 不限）。
    pub max_pixels: u64,
    /// 负载低于该值（如 0.5）时升档：更高分辨率/帧率。
    pub scale_up_below: f32,
    /// 负载高于该值（如 1.0）时降档：更低分辨率/帧率。
    pub scale_down_above: f32,
    /// 两次调整之间至少间隔的帧数（迟滞，避免来回抖动）。
    pub cooldown_frames: u64,
    /// 分辨率变化后的"探索期"帧数：期间缩短冷却，快速搜索新档位
    /// （Chromium `GetExplorationPeriodAfterSourceSizeChange()`）。
    pub exploration_frames: u64,
    /// 升档前需要持续低负载的帧数（Chromium `start_time_of_underutilization_` 证明期）。
    pub scale_up_proving_frames: u64,
    /// 优先 NV12。
    pub prefer_nv12: bool,
}

impl Default for AdaptivePolicy {
    fn default() -> Self {
        Self {
            min_fps: 15,
            max_fps: 0,
            min_pixels: 0,
            max_pixels: 0,
            scale_up_below: 0.5,
            scale_down_above: 1.0,
            cooldown_frames: 90,
            exploration_frames: 30,
            scale_up_proving_frames: 60,
            prefer_nv12: true,
        }
    }
}

/// 在给定负载下，从能力表中选一个合适的分辨率/帧率档位（纯函数）。
///
/// 把可用格式按 (像素数, 帧率) 升序排成"档位阶梯"：负载越高档位越低，
/// 负载越低档位越高；再在目标档位上挑最优像素格式。
#[must_use]
pub fn select_adaptive_format(
    formats: &[FormatInfo],
    policy: &AdaptivePolicy,
    utilization: f32,
) -> Option<FormatInfo> {
    let candidates: Vec<FormatInfo> = formats
        .iter()
        .copied()
        .filter(|format| format.fps >= policy.min_fps)
        .filter(|format| policy.max_fps == 0 || format.fps <= policy.max_fps)
        .filter(|format| policy.min_pixels == 0 || format.resolution.pixels() >= policy.min_pixels)
        .filter(|format| policy.max_pixels == 0 || format.resolution.pixels() <= policy.max_pixels)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // 档位阶梯：按 (像素数, 帧率) 升序，去重。
    let mut tiers: Vec<(u64, u32)> = candidates
        .iter()
        .map(|format| (format.resolution.pixels(), format.fps))
        .collect();
    tiers.sort_unstable();
    tiers.dedup();
    let last = tiers.len() - 1;
    let index = if utilization <= policy.scale_up_below {
        last
    } else if utilization >= policy.scale_down_above {
        0
    } else {
        let span = (policy.scale_down_above - policy.scale_up_below).max(f32::EPSILON);
        let progress = ((utilization - policy.scale_up_below) / span).clamp(0.0, 1.0);
        ((1.0 - progress) * last as f32).round() as usize
    };
    let (pixels, fps) = tiers[index.min(last)];
    let at_tier: Vec<FormatInfo> = candidates
        .into_iter()
        .filter(|format| format.resolution.pixels() == pixels && format.fps == fps)
        .collect();
    at_tier
        .iter()
        .copied()
        .filter(|format| !policy.prefer_nv12 || format.is_nv12())
        .min_by_key(|format| format_priority(format.format))
        .or_else(|| at_tier.first().copied())
}

/// 两次枚举之间的差异，用于热插拔检测。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceChanges {
    pub added: Vec<DeviceInfo>,
    pub removed: Vec<DeviceInfo>,
}

impl DeviceChanges {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.added.len() + self.removed.len()
    }
}

/// 本 crate 所有可恢复错误的统一类型。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CameraError {
    /// 查询后端失败（枚举能力时）。
    Backend(String),
    /// 按名称或 ID 找不到设备。
    DeviceNotFound(String),
    /// 分辨率非法（0 或奇数，后者 NV12 不支持）。
    InvalidResolution(Resolution),
    /// 打开设备失败。
    Open(String),
    /// 开启视频流失败。
    Stream(String),
    /// 读取设备能力表失败。
    CapabilityQuery(String),
    /// 尚未打开摄像头。
    NotOpen,
    /// 设备不支持请求的像素格式。
    UnsupportedFormat(PixelFormat),
    /// 帧解码失败。
    Decode(String),
    /// 摄像头参数控制失败（不支持 / 被驱动拒绝）。
    Control(String),
    /// 原生 DX11/DX12 零拷贝路径失败。
    Dx(String),
}

impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(message) => write!(f, "camera backend error: {message}"),
            Self::DeviceNotFound(query) => write!(f, "camera not found: {query}"),
            Self::InvalidResolution(resolution) => {
                write!(f, "invalid camera resolution: {resolution}")
            }
            Self::Open(message) => write!(f, "failed to open camera: {message}"),
            Self::Stream(message) => write!(f, "failed to open camera stream: {message}"),
            Self::CapabilityQuery(message) => {
                write!(f, "failed to query camera capabilities: {message}")
            }
            Self::NotOpen => f.write_str("camera is not open"),
            Self::UnsupportedFormat(format) => {
                write!(f, "unsupported camera pixel format: {format}")
            }
            Self::Decode(message) => write!(f, "failed to decode camera frame: {message}"),
            Self::Control(message) => write!(f, "camera control error: {message}"),
            Self::Dx(message) => write!(f, "DirectX camera error: {message}"),
        }
    }
}

impl std::error::Error for CameraError {}

/// 本 crate 的统一结果类型。
pub type Result<T> = std::result::Result<T, CameraError>;

#[cfg(test)]
#[path = "tests/device_tests.rs"]
mod tests;
