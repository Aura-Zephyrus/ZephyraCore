//! 播放管线：demux → 解码线程 → 帧队列 → A/V 同步。
//!
//! 全部按 feature 选择编译：不开 `video` / `audio` 时本模块不编译。
//! demux 由 symphonia 完成（纯 Rust / MPL-2.0），裸码流交给 zephyra-video。

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::clock::PresentationClock;
use crate::error::{MediaError, Result};
#[cfg(feature = "audio")]
use crate::output::{bits_volume, volume_bits, AudioSink, OutputSampleFormat, VolumeHandle};
use crate::VideoFrameSource;

/// 轨道信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaTrackInfo {
    pub id: u32,
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub duration_us: Option<i64>,
    /// 容器里的编解码器配置（MP4 的 avcC），用于 AVCC → Annex-B 转换。
    pub extra_data: Option<Vec<u8>>,
}

/// 播放选项。
#[derive(Debug, Clone)]
pub struct PlayerOptions {
    pub video: bool,
    pub audio: bool,
    /// 预加载共享库并报告失败（dynamic feature）。
    pub check_libraries: bool,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            video: true,
            audio: true,
            check_libraries: true,
        }
    }
}

/// 解码线程向主线程上报的加载报告。
#[cfg(feature = "dynamic")]
pub type LibraryReport = zephyra_video::DynamicLoadReport;

pub(crate) enum Command {
    Seek,
    Reset,
}

/// 媒体播放器。
///
/// `open()` 只做探测与初始化；真正的解码在后台线程进行，主线程通过
/// `poll_video_frame()` / `poll_audio_frame()` 取帧。
pub struct MediaPlayer {
    video_rx: Option<Receiver<VideoFrameSource>>,
    audio_rx: Option<Receiver<crate::audio_frames::AudioFrame>>,
    /// 每个解码线程一个 sender：Stop 必须广播给全部线程，不能共享一个接收端。
    cmd_tx: Vec<Sender<Command>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    clock: Arc<Mutex<PresentationClock>>,
    /// 尚未到显示时间的帧；保留它而不是丢弃，否则视频会大量丢帧。
    pending_video: Mutex<Option<VideoFrameSource>>,
    /// 锁竞争失败计数（失败可见性：旧代码 `lock().ok()?` 静默吞掉中毒/竞争）。
    lock_failures: AtomicU32,
    tracks: Vec<MediaTrackInfo>,
    #[cfg(feature = "dynamic")]
    library_report: LibraryReport,
    // 音频输出与音量控制：全部用 atomic / 共享句柄，因此这些 API 都是 `&self`，
    // 宿主可以把播放器放在闭包里每帧调用，不必抢一个可变借用。
    #[cfg(feature = "audio")]
    audio_sink: Option<AudioSink>,
    #[cfg(feature = "audio")]
    volume_handle: Option<VolumeHandle>,
    #[cfg(feature = "audio")]
    output_format: OutputSampleFormat,
    #[cfg(feature = "audio")]
    volume: AtomicU32,
    #[cfg(feature = "audio")]
    muted: AtomicBool,
}

impl MediaPlayer {
    /// 打开媒体文件。
    pub fn open(path: impl AsRef<Path>, options: PlayerOptions) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(MediaError::Io(format!("{} does not exist", path.display())));
        }

        #[cfg(feature = "dynamic")]
        let library_report = if options.check_libraries {
            zephyra_video::load_all()
        } else {
            zephyra_video::current_report()
        };
        // 缺库不阻止打开：主程序可通过 library_report() 看到具体失败并自行决定；
        // 真正用到的解码器会在 initialize() 时返回对应错误。

        let (tracks, packets) = crate::demux::probe(path)?;
        if tracks.is_empty() {
            return Err(MediaError::NoStream("playable"));
        }

        let clock = Arc::new(Mutex::new(PresentationClock::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut cmd_tx: Vec<Sender<Command>> = Vec::new();
        let mut threads = Vec::new();
        #[allow(unused_mut)]
        let mut video_rx = None;
        #[allow(unused_mut)]
        let mut audio_rx = None;

        #[cfg(feature = "video")]
        if options.video {
            let (tx, rx) = mpsc::channel();
            let (ctx, crx) = mpsc::channel();
            cmd_tx.push(ctx);
            video_rx = Some(rx);
            let packets = packets.clone();
            let tracks = tracks.clone();
            let clock = Arc::clone(&clock);
            let stop = Arc::clone(&stop);
            threads.push(std::thread::spawn(move || {
                crate::decode_thread::video_loop(packets, tracks, tx, clock, stop, crx);
            }));
        }

        #[cfg(feature = "audio")]
        if options.audio {
            let (tx, rx) = mpsc::channel();
            let (ctx, crx) = mpsc::channel();
            cmd_tx.push(ctx);
            audio_rx = Some(rx);
            let packets = packets.clone();
            let tracks = tracks.clone();
            let stop = Arc::clone(&stop);
            threads.push(std::thread::spawn(move || {
                crate::decode_thread::audio_loop(packets, tracks, tx, stop, crx);
            }));
        }

        // 音频输出设备不需要在 open 时一定可用：拿不到就降级成「只解码不发声」，
        // 控制 API 仍然存在（写共享音量句柄时没有接收方，安静地无操作）。
        #[cfg(feature = "audio")]
        let mut audio_sink = None;
        #[cfg(feature = "audio")]
        let mut volume_handle = None;
        #[cfg(feature = "audio")]
        let mut output_format = OutputSampleFormat::I16;
        #[cfg(feature = "audio")]
        if options.audio {
            if let Some(track) = tracks
                .iter()
                .find(|t| t.sample_rate.is_some() && t.channels.is_some())
            {
                let sample_rate = track.sample_rate.unwrap();
                let channels = track.channels.unwrap();
                for fmt in [OutputSampleFormat::F32, OutputSampleFormat::I16] {
                    match AudioSink::open(sample_rate, channels, fmt) {
                        Ok(sink) => {
                            volume_handle = Some(sink.volume_handle());
                            audio_sink = Some(sink);
                            output_format = fmt;
                            break;
                        }
                        Err(e) => eprintln!("[zephyra-media] 音频设备不支持 {fmt:?}: {e}"),
                    }
                }
            }
        }

        Ok(Self {
            video_rx,
            audio_rx,
            cmd_tx,
            stop,
            threads,
            clock,
            pending_video: Mutex::new(None),
            lock_failures: AtomicU32::new(0),
            tracks,
            #[cfg(feature = "dynamic")]
            library_report,
            #[cfg(feature = "audio")]
            audio_sink,
            #[cfg(feature = "audio")]
            volume_handle,
            #[cfg(feature = "audio")]
            output_format,
            #[cfg(feature = "audio")]
            volume: AtomicU32::new(volume_bits(1.0)),
            #[cfg(feature = "audio")]
            muted: AtomicBool::new(false),
        })
    }

    pub fn tracks(&self) -> &[MediaTrackInfo] {
        &self.tracks
    }

    pub fn clock(&self) -> &Arc<Mutex<PresentationClock>> {
        &self.clock
    }

    /// 累计的锁竞争 / 中毒失败次数（诊断使用）。
    pub fn lock_failures(&self) -> u32 {
        self.lock_failures.load(Ordering::Relaxed)
    }

    #[cfg(feature = "dynamic")]
    pub fn library_report(&self) -> &LibraryReport {
        &self.library_report
    }

    pub fn poll_video_frame(&self) -> Option<VideoFrameSource> {
        self.video_rx.as_ref()?.try_recv().ok()
    }

    pub fn poll_audio_frame(&self) -> Option<crate::audio_frames::AudioFrame> {
        self.audio_rx.as_ref()?.try_recv().ok()
    }

    /// 按时钟取当前应显示的视频帧。
    ///
    /// 未到显示时间的帧会留在 `pending_video` 里，下次调用再判断；不再丢弃，
    /// 避免按 PTS 播放时几乎每帧都被误丢。
    pub fn present_video_frame(&self) -> Option<VideoFrameSource> {
        let mut pending = match self.pending_video.try_lock() {
            Ok(g) => g,
            Err(_) => {
                self.lock_failures.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        let frame = match pending.take() {
            Some(f) => f,
            None => self.poll_video_frame()?,
        };
        let delta = {
            let c = match self.clock.try_lock() {
                Ok(g) => g,
                Err(_) => {
                    self.lock_failures.fetch_add(1, Ordering::Relaxed);
                    // 帧已从 pending 取出，放回以免丢失。
                    *pending = Some(frame);
                    return None;
                }
            };
            c.delta_us(frame.timestamp())
        };
        if delta > 0 {
            *pending = Some(frame);
            return None;
        }
        Some(frame)
    }

    pub fn seek(&self, media_us: i64) {
        if let Ok(mut c) = self.clock.lock() {
            c.seek(media_us);
        }
        for tx in &self.cmd_tx {
            let _ = tx.send(Command::Seek);
        }
    }

    pub fn pause(&self) {
        if let Ok(mut c) = self.clock.lock() {
            c.pause();
        }
    }

    pub fn resume(&self) {
        if let Ok(mut c) = self.clock.lock() {
            c.resume();
        }
    }

    pub fn reset(&self) {
        for tx in &self.cmd_tx {
            let _ = tx.send(Command::Reset);
        }
    }

    /// 当前播放位置（微秒）。播放器控件的时间/进度条读它。
    pub fn position_us(&self) -> i64 {
        self.clock.lock().map(|c| c.now_us()).unwrap_or(0)
    }

    /// 容器里最长的轨道时长（微秒）；`0` = 未知。
    pub fn duration_us(&self) -> i64 {
        self.tracks
            .iter()
            .filter_map(|t| t.duration_us)
            .max()
            .unwrap_or(0)
    }

    pub fn set_rate(&self, rate: f64) {
        if let Ok(mut c) = self.clock.lock() {
            c.set_rate(rate);
        }
    }

    pub fn rate(&self) -> f64 {
        self.clock.lock().map(|c| c.rate()).unwrap_or(1.0)
    }

    /// 音量（0..1）。写共享句柄，设备回调立刻按新值缩放，不需要重建流。
    #[cfg(feature = "audio")]
    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume_bits(volume), Ordering::Relaxed);
        self.apply_volume();
    }

    #[cfg(feature = "audio")]
    pub fn volume(&self) -> f32 {
        bits_volume(self.volume.load(Ordering::Relaxed))
    }

    #[cfg(feature = "audio")]
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
        self.apply_volume();
    }

    #[cfg(feature = "audio")]
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    #[cfg(feature = "audio")]
    fn apply_volume(&self) {
        if let Some(handle) = &self.volume_handle {
            let effective = if self.muted() { 0.0 } else { self.volume() };
            handle.store(volume_bits(effective), Ordering::Relaxed);
        }
    }

    /// 把解码好的 PCM 帧送进音频设备。宿主每帧调一次。
    #[cfg(feature = "audio")]
    pub fn pump_audio(&self) {
        let Some(sink) = &self.audio_sink else {
            return;
        };
        while let Some(frame) = self.poll_audio_frame() {
            let bytes = crate::audio_output::interleave(&frame, self.output_format);
            if !bytes.is_empty() {
                sink.write(&bytes);
            }
        }
    }

    /// 是否真的有音频输出设备（没有也能播画面，只是静音）。
    #[cfg(feature = "audio")]
    pub fn has_audio_output(&self) -> bool {
        self.audio_sink.is_some()
    }
}

impl Drop for MediaPlayer {
    fn drop(&mut self) {
        // 原子标志广播给所有线程；不再往共享 channel 发一条会被某个线程独吞的 Stop。
        self.stop.store(true, Ordering::Release);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

pub(crate) fn is_stop(stop: &Arc<AtomicBool>) -> bool {
    stop.load(Ordering::Acquire)
}
