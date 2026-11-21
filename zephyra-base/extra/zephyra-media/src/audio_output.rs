//! 音频帧 → 设备输出字节：planar 转交错 + 采样格式归一。
//!
//! `zephyra-video` 的解码帧每声道一个平面（Chromium `AudioBuffer` 语义），
//! 而 `AudioSink` 的环形缓冲区收的是交错采样。转换在这里做一次，调用方只管
//! `sink.write(&bytes)`。
//!
//! 音量**不在这里**乘：`AudioSink` 的设备回调按共享 atomic 缩放，这样改音量
//! 不需要重编码已经写进环形缓冲区的 PCM。

use zephyra_video::AudioFrame;

use crate::output::OutputSampleFormat;

/// 把一帧 PCM 转成目标格式的交错字节。
///
/// 不支持的格式（如 U8、Unknown）返回空向量，调用方跳过即可 —— 解码器给出的
/// 常见格式是 PlanarS16 / PlanarF32 / PlanarS32。
pub fn interleave(frame: &AudioFrame, target: OutputSampleFormat) -> Vec<u8> {
    let channels = frame.channels() as usize;
    let frames = frame.frames();
    if channels == 0 || frames == 0 {
        return Vec::new();
    }
    let bps = frame.format().bytes_per_sample() as usize;
    if bps == 0 {
        return Vec::new();
    }
    let planes = frame.planes();
    let mut out = Vec::with_capacity(frames * channels * target.bytes_per_sample());
    for i in 0..frames {
        for c in 0..channels {
            let Some(plane) = planes.get(c) else {
                return Vec::new();
            };
            let start = i * bps;
            let Some(bytes) = plane.get(start..start + bps) else {
                return Vec::new();
            };
            match target {
                OutputSampleFormat::I16 => {
                    let s = sample_i16(bytes, bps);
                    out.extend_from_slice(&s.to_le_bytes());
                }
                OutputSampleFormat::F32 => {
                    let s = sample_f32(bytes, bps);
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
    }
    out
}

fn sample_i16(bytes: &[u8], bps: usize) -> i16 {
    match bps {
        2 => i16::from_le_bytes([bytes[0], bytes[1]]),
        4 => {
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            // 32-bit 整数 / 浮点都按满量程归一到 i16。
            let normalized = if raw >= 0 {
                (raw as f64 / i32::MAX as f64) * i16::MAX as f64
            } else {
                (raw as f64 / i32::MIN.abs() as f64) * i16::MIN as f64
            };
            normalized as i16
        }
        _ => 0,
    }
}

fn sample_f32(bytes: &[u8], bps: usize) -> f32 {
    match bps {
        2 => (i16::from_le_bytes([bytes[0], bytes[1]]) as f32) / i16::MAX as f32,
        4 => {
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            // F32 平面直接按位读；S32 平面按满量程归一。两种都落在 [-1, 1]。
            let as_float = f32::from_bits(raw as u32);
            if as_float.is_finite() && as_float.abs() <= 1.0 {
                as_float
            } else if raw >= 0 {
                raw as f32 / i32::MAX as f32
            } else {
                raw as f32 / i32::MIN.abs() as f32
            }
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zephyra_video::AudioSampleFormat;

    fn frame() -> AudioFrame {
        let left = vec![0x00u8, 0x40, 0x00, 0x80];
        let right = vec![0x00u8, 0xc0, 0x00, 0x80];
        AudioFrame::from_planar(
            AudioSampleFormat::PlanarS16,
            2,
            48000,
            vec![left, right],
            2,
            0,
            0,
        )
        .unwrap()
    }

    #[test]
    fn planar_s16_becomes_interleaved_i16() {
        let bytes = interleave(&frame(), OutputSampleFormat::I16);
        assert_eq!(bytes.len(), 2 * 2 * 2);
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 0x4000);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), -16384);
    }

    #[test]
    fn planar_s16_becomes_interleaved_f32() {
        let bytes = interleave(&frame(), OutputSampleFormat::F32);
        assert_eq!(bytes.len(), 2 * 2 * 4);
        let first = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!((first - 0.5).abs() < 0.01);
    }
}
