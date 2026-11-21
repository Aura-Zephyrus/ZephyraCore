//! RGB 回退路径的 CPU 处理（RGB→RGBA + 最近邻降采样）。
//!
//! NV12 直传路径不经过此模块的逐帧处理：原始平面零拷贝切片后直接上传，
//! YUV→RGB 由渲染管线着色器完成（见 render/camera.wgsl）。
//!
//! # 跨 crate 所有权桥接
//!
//! 采集路径与主仓 `CpuPlanes` 契约现在**都用 `bytes::Bytes`**：`slice` 零拷贝切平面，
//! 桥接层直接把 [`Nv12Planes::y`] / [`Nv12Planes::uv`] 交给契约，Y/UV 各一次
//! `slice` 之后**零 memcpy**（见 [`Nv12Planes::into_bytes_planes`]）。
//! [`Nv12Planes::to_arc_planes`] / [`Nv12Planes::into_arc_planes`] 仅保留给只接受
//! `Arc<[u8]>` 的旧消费者：每平面**至多一次 memcpy**（slice 出来的平面必然共享
//! 底层分配，必须复制一份才能装进 `Arc`）。camera 不依赖主仓，只产出 std/bytes
//! 类型，由桥接 crate 映射到具体契约结构。
//!
//! # RGB→RGBA 的三种出口（稳态分配逐条递减）
//!
//! - [`CpuFrameProcessor::process`]：交出 `Vec` 所有权（调用方包进 `Arc`）。
//!   保留原语义；每次因所有权转移而重新分配。
//! - [`CpuFrameProcessor::process_into`]：写入调用方持有的 `Vec`，容量跨帧复用；
//!   适合调用方自己做缓冲池。
//! - [`CpuFrameProcessor::process_borrowed`]：写入 processor 内部缓冲并借出 `&[u8]`，
//!   预热后**零分配、零拷贝**；适合同步上传纹理的调用方（上传期间借用即可）。

use std::sync::Arc;

const DOWNSAMPLE_WIDTH: u32 = 854;
const DOWNSAMPLE_HEIGHT: u32 = 480;

/// NV12 帧的两个平面（零拷贝切片，共享同一底层分配）。
#[derive(Debug, Clone)]
pub struct Nv12Planes {
    /// Y 平面：w*h 字节，每像素 1 字节亮度
    pub y: bytes::Bytes,
    /// UV 平面：(w/2)*(h/2) 个交错 (U,V) 对 = w*h/2 字节
    pub uv: bytes::Bytes,
    pub width: u32,
    pub height: u32,
}

impl Nv12Planes {
    /// 把连续 NV12 缓冲切分为 Y/UV 两平面（Bytes::slice 零拷贝）。
    pub fn split(data: bytes::Bytes, w: u32, h: u32) -> Option<Nv12Planes> {
        // NV12 是 4:2:0：宽高必须都为偶数，否则 UV 平面大小无定义。
        if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            return None;
        }
        let y_size = (w as usize) * (h as usize);
        let uv_size = y_size / 2;
        if data.len() < y_size + uv_size {
            return None;
        }
        Some(Nv12Planes {
            y: data.slice(0..y_size),
            uv: data.slice(y_size..y_size + uv_size),
            width: w,
            height: h,
        })
    }

    /// 【仅拍照时调用】单次 NV12→RGBA 转换（BT.601 limited range，与着色器一致）。
    /// 每次拍照一次 O(w*h)，不在实时路径上。
    pub fn to_rgba(&self, dst: &mut Vec<u8>) {
        let w = self.width as usize;
        let h = self.height as usize;
        dst.clear();
        dst.resize(w * h * 4, 0);
        // BT.601 limited range: Y ∈ [16,235], UV ∈ [16,240]
        // 逐像素写入预分配的切片（4 字节一组），替代旧实现的 4 次 push：
        // 同样的算术，省掉每像素 4 次长度检查与容量增长分支。
        let uv_w = w / 2;
        for (i, px) in dst.chunks_exact_mut(4).enumerate() {
            let row = i / w;
            let col = i - row * w;
            let y_val = self.y[i] as i32;
            let uv_index = (row / 2) * uv_w + (col / 2);
            let u_val = self.uv[uv_index * 2] as i32 - 128;
            let v_val = self.uv[uv_index * 2 + 1] as i32 - 128;
            let yy = (y_val - 16) * 298; // ≈255/219*256
                                         // 整数近似（右移 8 位），与浮点公式偏差 ≤1
            px[0] = ((yy + 409 * v_val) >> 8).clamp(0, 255) as u8;
            px[1] = ((yy - 100 * u_val - 208 * v_val) >> 8).clamp(0, 255) as u8;
            px[2] = ((yy + 516 * u_val) >> 8).clamp(0, 255) as u8;
            px[3] = 255;
        }
    }

    /// 零拷贝出口：交出两个 `Bytes` 平面（只加引用计数，不复制任何字节）。
    ///
    /// 主仓 `CpuPlanes` 已以 `Bytes` 存储平面，桥接层用这个即可端到端零 memcpy：
    /// 采集缓冲 → [`Self::split`] 的两次 `slice` → 契约的 `plane(i)` 全程同一分配。
    pub fn into_bytes_planes(self) -> (bytes::Bytes, bytes::Bytes) {
        (self.y, self.uv)
    }

    /// [`Self::into_bytes_planes`] 的借用版：clone 只加引用计数，同样零 memcpy。
    pub fn bytes_planes(&self) -> (bytes::Bytes, bytes::Bytes) {
        (self.y.clone(), self.uv.clone())
    }

    /// **旧契约兼容出口**：把 `Bytes` 平面转成 `Arc<[u8]>`（**至多单次 memcpy/平面**）。
    ///
    /// 新代码走 [`Self::into_bytes_planes`] 零拷贝直达契约；只有面对仍要求
    /// `Arc<[u8]>` 的老消费者才用这里。每个平面先经 `From<Bytes> for Vec<u8>`：
    /// 底层独占时复用原分配，共享（`slice` 出来的平面必然共享）时拷贝一次；再用
    /// `Arc::from(Vec<u8>)` 把**同一块分配**直接转成 `Arc<[u8]>`（零拷贝、无新分配）。
    /// 所以最坏情况就是每平面一次 memcpy。
    pub fn to_arc_planes(&self) -> ArcNv12Planes {
        ArcNv12Planes {
            y: Arc::from(Vec::from(self.y.clone())),
            uv: Arc::from(Vec::from(self.uv.clone())),
            width: self.width,
            height: self.height,
        }
    }

    /// [`Self::to_arc_planes`] 的消费版：省掉两次 `Bytes` 引用计数递增，
    /// 拷贝次数相同（至多一次/平面）。拿到 `Nv12Planes` 所有权后用这个。
    pub fn into_arc_planes(self) -> ArcNv12Planes {
        ArcNv12Planes {
            y: Arc::from(Vec::from(self.y)),
            uv: Arc::from(Vec::from(self.uv)),
            width: self.width,
            height: self.height,
        }
    }
}

/// [`Nv12Planes`] 的**旧契约兼容**版本：`Arc<[u8]>` 平面。
///
/// 字段语义与 [`Nv12Planes`] 一致（`y` 长 `w*h`，`uv` 长 `w*h/2`）。新代码请用
/// [`Nv12Planes::into_bytes_planes`]：主仓 `CpuPlanes` 已改用 `Bytes`，可直接零拷贝
/// 交付。本类型只依赖 std，给仍要求 `Arc<[u8]>` 的老消费者一条单向出口（每平面
/// 至多一次 memcpy），camera 侧不需要知道契约层的任何细节。
#[derive(Debug, Clone)]
pub struct ArcNv12Planes {
    /// Y 平面：`width * height` 字节亮度
    pub y: Arc<[u8]>,
    /// UV 平面：`width * height / 2` 字节交错 (U,V)
    pub uv: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
}

pub struct CpuFrameProcessor {
    /// 原地转换缓冲：由 [`Self::process`] 交出所有权，或由
    /// [`Self::process_borrowed`] 借出视图。
    output: Vec<u8>,
    last_w: u32,
    last_h: u32,
    index_size: (u32, u32, u32, u32),
    sx: Vec<u32>,
    sy: Vec<u32>,
}

impl CpuFrameProcessor {
    /// 无参构造等价于 `Default`。
    ///
    /// 刻意**不**只提供 `Default`：调用点写 `CpuFrameProcessor::new()` 更直白，
    /// 而 clippy 会要求两者等价——所以这里让 `Default` 委托给 `new()`，把
    /// 初始状态的**唯一定义**留在 `new` 里。反过来（`new` 调 `Default`）会让
    /// 默认值藏在 `#[derive(Default)]` 里，而那里的字段初值容易被误改。
    pub fn new() -> Self {
        Self {
            output: Vec::new(),
            last_w: 0,
            last_h: 0,
            index_size: (0, 0, 0, 0),
            sx: Vec::new(),
            sy: Vec::new(),
        }
    }

    fn downsample_size(src_w: u32, src_h: u32) -> (u32, u32) {
        let rgba_size = (src_w * src_h * 4) as usize;
        if rgba_size < 2 * 1024 * 1024 {
            return (src_w, src_h);
        }
        let aspect = src_w as f32 / src_h as f32;
        let dst_w = DOWNSAMPLE_WIDTH;
        let mut dst_h = DOWNSAMPLE_HEIGHT;
        if (aspect - 16.0 / 9.0).abs() > 0.01 {
            dst_h = (dst_w as f32 / aspect) as u32;
        }
        (dst_w.max(1), dst_h.max(1))
    }

    fn record_size(&mut self, dst_w: u32, dst_h: u32) {
        self.last_w = dst_w;
        self.last_h = dst_h;
    }

    pub fn last_width(&self) -> u32 {
        self.last_w
    }
    pub fn last_height(&self) -> u32 {
        self.last_h
    }

    /// 更新降采样索引表并返回目标尺寸；尺寸未变时不重建。
    fn prepare(&mut self, src_w: u32, src_h: u32) -> (u32, u32) {
        let (dst_w, dst_h) = Self::downsample_size(src_w, src_h);
        let index_size = (src_w, src_h, dst_w, dst_h);
        self.record_size(dst_w, dst_h);
        if dst_w != src_w && dst_h != src_h && self.index_size != index_size {
            self.sy = (0..dst_h)
                .map(|dy| (dy as u64 * src_h as u64 / dst_h as u64) as u32)
                .collect();
            self.sx = (0..dst_w)
                .map(|dx| (dx as u64 * src_w as u64 / dst_w as u64) as u32)
                .collect();
            self.index_size = index_size;
        }
        (dst_w, dst_h)
    }

    /// 把 RGB 逐像素写成 RGBA 到 `dst`；长度不变时 `resize` 是 no-op，
    /// 既不重新分配也不清零（每个像素随后都会被完整覆盖）。
    fn write_rgba(
        dst: &mut Vec<u8>,
        rgb: &[u8],
        (src_w, src_h, dst_w, dst_h): (u32, u32, u32, u32),
        sx: &[u32],
        sy: &[u32],
    ) {
        let needed = (dst_w * dst_h * 4) as usize;
        if dst.len() != needed {
            dst.resize(needed, 0);
        }

        if dst_w == src_w && dst_h == src_h {
            for (i, px) in dst.chunks_exact_mut(4).enumerate() {
                let s = i * 3;
                px[0] = rgb[s];
                px[1] = rgb[s + 1];
                px[2] = rgb[s + 2];
                px[3] = 255;
            }
        } else {
            for (dy, row) in dst.chunks_exact_mut((dst_w * 4) as usize).enumerate() {
                let src_y = sy[dy] as usize;
                let base = src_y * src_w as usize;
                for (dx, px) in row.chunks_exact_mut(4).enumerate() {
                    let s = (base + sx[dx] as usize) * 3;
                    px[0] = rgb[s];
                    px[1] = rgb[s + 1];
                    px[2] = rgb[s + 2];
                    px[3] = 255;
                }
            }
        }
    }

    /// RGB → RGBA + 最近邻降采样（全 CPU，无 GPU 往返）。
    ///
    /// 保持原语义：交出 `Vec` 所有权（调用方包进 `Arc`）。所有权必须移交，
    /// 所以每次都会重新分配目标缓冲；稳态零分配场景请改用
    /// [`Self::process_into`] 或 [`Self::process_borrowed`]。
    /// 降采样索引表（`sx`/`sy`）在尺寸不变时复用。
    pub fn process(&mut self, rgb: &[u8], src_w: u32, src_h: u32) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.output);
        self.process_into(rgb, src_w, src_h, &mut out);
        out
    }

    /// 带缓冲区的 `process`：写入调用方持有的 `dst`，**复用其容量**。
    ///
    /// 预热到目标尺寸后，每帧不再分配（长度相同则 `resize` 为 no-op）。
    /// 适合调用方自己管理缓冲池：池里取 `Vec` → 本方法填充 → 用完归还。
    /// 返回后 `dst[..(last_width*last_height*4)]` 即本帧 RGBA。
    pub fn process_into(&mut self, rgb: &[u8], src_w: u32, src_h: u32, dst: &mut Vec<u8>) {
        let (dst_w, dst_h) = self.prepare(src_w, src_h);
        Self::write_rgba(dst, rgb, (src_w, src_h, dst_w, dst_h), &self.sx, &self.sy);
    }

    /// 借出版：写入 processor 内部缓冲并返回只读视图，预热后**零分配、零拷贝**。
    ///
    /// 返回的切片只在下一次调用本方法（或 [`Self::process`]、[`Self::process_into`]）
    /// 之前有效——那次调用会覆盖同一块缓冲。适合同步上传纹理：上传调用返回后
    /// 就不再需要这份数据，省掉 `Arc` 包装与每帧 1.6MB 的重新分配。
    pub fn process_borrowed(&mut self, rgb: &[u8], src_w: u32, src_h: u32) -> &[u8] {
        let (dst_w, dst_h) = self.prepare(src_w, src_h);
        Self::write_rgba(
            &mut self.output,
            rgb,
            (src_w, src_h, dst_w, dst_h),
            &self.sx,
            &self.sy,
        );
        &self.output
    }
}

impl Default for CpuFrameProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "tests/pixels_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/pixels_bridge_tests.rs"]
mod bridge_tests;
