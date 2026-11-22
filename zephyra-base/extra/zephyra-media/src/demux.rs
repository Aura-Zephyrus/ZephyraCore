//! 容器解复用（symphonia，纯 Rust / MPL-2.0）。

use std::path::Path;

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

use crate::error::{MediaError, Result};
use crate::pipeline::MediaTrackInfo;

/// 解复用出的一个裸码流包。
#[derive(Debug, Clone)]
pub struct MediaPacket {
    pub track_id: u32,
    pub data: Vec<u8>,
    pub pts_us: i64,
    pub duration_us: i64,
    pub codec: String,
}

fn to_us(tb: TimeBase, ts: u64) -> i64 {
    let t = tb.calc_time(ts);
    (t.seconds as f64 * 1_000_000.0 + t.frac * 1_000_000.0) as i64
}

/// 在 MP4 字节流里找 `avcC` box，返回其配置内容。
fn find_avcc(data: &[u8]) -> Option<&[u8]> {
    let tag = b"avcC";
    data.windows(tag.len())
        .position(|w| w == tag)
        .map(|i| &data[i + tag.len()..])
}

pub fn probe(path: &Path) -> Result<(Vec<MediaTrackInfo>, Vec<MediaPacket>)> {
    let bytes =
        std::fs::read(path).map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
    let mss = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.clone())),
        Default::default(),
    );

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| MediaError::Demux(format!("probe failed: {e}")))?;
    let mut format = probed.format;

    let mut tracks = Vec::new();
    for track in format.tracks() {
        let p = &track.codec_params;
        let time_base = p.time_base;
        let duration_us = p.n_frames.and_then(|n| time_base.map(|tb| to_us(tb, n)));

        #[allow(unused_mut)]
        let (mut width, mut height) = (None, None);
        let sample_rate = p.sample_rate;
        let channels = p.channels.map(|c| c.count() as u16).or_else(|| {
            if sample_rate.is_some() {
                Some(2)
            } else {
                None
            }
        });

        let mut codec_name = format!("{:?}", p.codec);
        let avcc = p.extra_data.as_deref().or_else(|| find_avcc(&bytes));
        #[cfg(feature = "video")]
        if codec_name.contains("CodecType(0)") {
            // MP4 的视频轨在 symphonia 里报 CODEC_TYPE_NULL；从容器里的 avcC 判定 H.264。
            if let Some(cfg) =
                avcc.and_then(zephyra_video::h264::AvcDecoderConfigurationRecord::parse)
            {
                codec_name = "H264".into();
                if width.is_none() {
                    if let Some(sps) = cfg.sps_list.first() {
                        if let Ok(parsed) = zephyra_video::h264::parse_sps_payload(sps) {
                            let (w, h) = parsed.coded_size();
                            width = Some(w);
                            height = Some(h);
                        }
                    }
                }
            }
        }
        if codec_name.contains("4100") {
            codec_name = "AAC".into();
        }

        tracks.push(MediaTrackInfo {
            id: track.id,
            codec: codec_name,
            width,
            height,
            sample_rate,
            channels,
            duration_us,
            extra_data: avcc.map(|e| e.to_vec()),
        });
    }

    let mut packets = Vec::new();
    while let Ok(packet) = format.next_packet() {
        let track_id = packet.track_id();
        let track = format.tracks().iter().find(|t| t.id == track_id);
        let mut codec = track
            .map(|t| format!("{:?}", t.codec_params.codec))
            .unwrap_or_default();
        if codec.contains("4100") {
            codec = "AAC".into();
        }
        #[cfg(feature = "video")]
        if codec.contains("CodecType(0)") {
            if let Some(extra) = track.and_then(|t| t.codec_params.extra_data.as_deref()) {
                if zephyra_video::h264::AvcDecoderConfigurationRecord::parse(extra).is_some() {
                    codec = "H264".into();
                }
            }
        }
        let tb = track.and_then(|t| t.codec_params.time_base);
        let (pts_us, duration_us) = match tb {
            Some(tb) => (to_us(tb, packet.ts), to_us(tb, packet.dur)),
            None => (packet.ts as i64, packet.dur as i64),
        };
        packets.push(MediaPacket {
            track_id,
            data: packet.data.to_vec(),
            pts_us,
            duration_us,
            codec,
        });
    }

    Ok((tracks, packets))
}

#[cfg(feature = "audio")]
pub(crate) fn audio_codec_from(name: &str) -> Option<zephyra_video::AudioCodec> {
    use zephyra_video::AudioCodec;
    let n = name.to_ascii_lowercase();
    if n.contains("mp3") {
        Some(AudioCodec::Mp3)
    } else if n.contains("opus") {
        Some(AudioCodec::Opus)
    } else if n.contains("vorbis") {
        Some(AudioCodec::Vorbis)
    } else if n.contains("flac") {
        Some(AudioCodec::Flac)
    } else if n.contains("pcm") {
        Some(AudioCodec::Pcm)
    } else {
        None
    }
}

#[cfg(feature = "video")]
pub(crate) fn video_codec_from(name: &str) -> Option<zephyra_video::VideoCodec> {
    use zephyra_video::VideoCodec;
    let n = name.to_ascii_lowercase();
    if n.contains("h264") || n.contains("avc") {
        Some(VideoCodec::H264)
    } else if n.contains("hevc") || n.contains("h265") {
        Some(VideoCodec::HEVC)
    } else if n.contains("av1") {
        Some(VideoCodec::AV1)
    } else if n.contains("vp9") {
        Some(VideoCodec::VP9)
    } else if n.contains("vp8") {
        Some(VideoCodec::VP8)
    } else if n.contains("theora") {
        Some(VideoCodec::Theora)
    } else if n.contains("mjpeg") {
        Some(VideoCodec::Mjpeg)
    } else if n.contains("vvc") || n.contains("h266") {
        Some(VideoCodec::Vvc)
    } else if n.contains("evc") {
        Some(VideoCodec::Evc)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_us_scales_seconds_and_frac_with_truncation() {
        // 1 tick = 1 us。
        assert_eq!(to_us(TimeBase::new(1, 1_000_000), 0), 0);
        assert_eq!(to_us(TimeBase::new(1, 1_000_000), 1), 1);
        assert_eq!(to_us(TimeBase::new(1, 1_000_000), 1_234_567), 1_234_567);
        // 1 tick = 1 ms，seconds 部分也要乘 1e6。
        assert_eq!(to_us(TimeBase::new(1, 1_000), 1), 1_000);
        assert_eq!(to_us(TimeBase::new(1, 1_000), 2_500), 2_500_000);
        // 1/3 秒 = 333_333.33... us，float → i64 是截断而非四舍五入。
        assert_eq!(to_us(TimeBase::new(1, 3), 1), 333_333);
        assert_eq!(to_us(TimeBase::new(1, 3), 3), 1_000_000);
    }

    #[test]
    fn find_avcc_pins_returned_offset_and_tail() {
        assert_eq!(find_avcc(b""), None);
        assert_eq!(find_avcc(b"no tag here"), None);
        // 当前实现返回 i + tag.len()，即包含随后 4 字节的 box size 字段。
        assert_eq!(
            find_avcc(b"avcC\x00\x00\x00\x05\x01\x02"),
            Some(&b"\x00\x00\x00\x05\x01\x02"[..])
        );
        // 前置字节不参与偏移计算。
        assert_eq!(
            find_avcc(b"ab_avcC\xde\xad\xbe\xef\x77"),
            Some(&b"\xde\xad\xbe\xef\x77"[..])
        );
        // 多处出现时取第一处。
        assert_eq!(
            find_avcc(b"avcC\x00\x00\x00\x00avcC\x01\x02\x03\x04\x09"),
            Some(&b"\x00\x00\x00\x00avcC\x01\x02\x03\x04\x09"[..])
        );
        // tag 恰好在末尾 → 返回空 slice。
        assert_eq!(find_avcc(b"zzavcC"), Some(&b""[..]));
    }

    #[cfg(feature = "audio")]
    #[test]
    fn audio_codec_from_is_case_insensitive_and_ordered() {
        use zephyra_video::AudioCodec;

        assert_eq!(audio_codec_from("MP3"), Some(AudioCodec::Mp3));
        assert_eq!(audio_codec_from("oPuS"), Some(AudioCodec::Opus));
        assert_eq!(audio_codec_from("Vorbis"), Some(AudioCodec::Vorbis));
        assert_eq!(audio_codec_from("FLAC"), Some(AudioCodec::Flac));
        assert_eq!(audio_codec_from("pCm"), Some(AudioCodec::Pcm));
        assert_eq!(audio_codec_from(""), None);
        assert_eq!(audio_codec_from("aac"), None);

        // 子串命中，且按 mp3 → opus → vorbis → flac → pcm 的优先级短路。
        assert_eq!(
            audio_codec_from("mp3opusvorbisflacpcm"),
            Some(AudioCodec::Mp3)
        );
        assert_eq!(
            audio_codec_from("opusvorbisflacpcm"),
            Some(AudioCodec::Opus)
        );
        assert_eq!(audio_codec_from("vorbisflacpcm"), Some(AudioCodec::Vorbis));
        assert_eq!(audio_codec_from("flacpcm"), Some(AudioCodec::Flac));
        assert_eq!(audio_codec_from("pcmxxx"), Some(AudioCodec::Pcm));
    }

    #[cfg(feature = "video")]
    #[test]
    fn video_codec_from_is_case_insensitive_and_ordered() {
        use zephyra_video::VideoCodec;

        assert_eq!(video_codec_from("H264"), Some(VideoCodec::H264));
        assert_eq!(video_codec_from("avc1"), Some(VideoCodec::H264));
        assert_eq!(video_codec_from("HEVC"), Some(VideoCodec::HEVC));
        assert_eq!(video_codec_from("h265"), Some(VideoCodec::HEVC));
        assert_eq!(video_codec_from("AV1"), Some(VideoCodec::AV1));
        assert_eq!(video_codec_from("VP9"), Some(VideoCodec::VP9));
        assert_eq!(video_codec_from("vp8"), Some(VideoCodec::VP8));
        assert_eq!(video_codec_from("Theora"), Some(VideoCodec::Theora));
        assert_eq!(video_codec_from("MJPEG"), Some(VideoCodec::Mjpeg));
        assert_eq!(video_codec_from("vvc"), Some(VideoCodec::Vvc));
        assert_eq!(video_codec_from("h266"), Some(VideoCodec::Vvc));
        assert_eq!(video_codec_from("evc"), Some(VideoCodec::Evc));
        assert_eq!(video_codec_from(""), None);
        assert_eq!(video_codec_from("aac"), None);

        // 先匹配到的别名优先。
        assert_eq!(video_codec_from("avc1hevc"), Some(VideoCodec::H264));
        assert_eq!(video_codec_from("H265AV1"), Some(VideoCodec::HEVC));
        assert_eq!(video_codec_from("AV1VP9VP8"), Some(VideoCodec::AV1));
        assert_eq!(video_codec_from("vp9vp8"), Some(VideoCodec::VP9));
    }
}
