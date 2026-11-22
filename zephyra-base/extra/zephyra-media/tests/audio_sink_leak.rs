//! 回归：AudioSink 反复 open/drop 不得持续泄漏内存。
//!
//! cpal 0.15.3 的 WASAPI 后端里 `supported_output_configs()` 每次调用泄漏一小块
//! COM 枚举资源（实测 ~2.6KB/次）。`AudioSink::open` 曾做这个预检，导致长稳运行
//! 中 pagefile 单调增长。现在去掉预检（build_output_stream 自身会校验配置），
//! 本测试钉住"反复开关不泄漏"。

#![cfg(feature = "audio")]

use zephyra_media::output::OutputSampleFormat;
use zephyra_media::AudioSink;

fn pagefile_kb() -> u64 {
    #[cfg(windows)]
    {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        unsafe {
            let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            if GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut c,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .is_ok()
            {
                return (c.PagefileUsage as u64) / 1024;
            }
        }
    }
    0
}

#[test]
#[ignore = "需要系统音频设备；手动跑：--ignored --nocapture"]
fn repeated_open_drop_does_not_leak() {
    let fmt = OutputSampleFormat::F32;
    // 预热：打开几次让设备/COM 初始化稳定。
    for _ in 0..20 {
        if let Ok(s) = AudioSink::open(48000, 2, fmt) {
            drop(s);
        }
    }
    let before = pagefile_kb();

    let mut opened = 0;
    for _ in 0..500 {
        if let Ok(s) = AudioSink::open(48000, 2, fmt) {
            opened += 1;
            drop(s);
        }
    }
    if opened == 0 {
        eprintln!("没有可用音频输出设备，跳过泄漏判定");
        return;
    }

    let after = pagefile_kb();
    let growth_kb = after.saturating_sub(before);
    eprintln!("500 次 open/drop 后 pagefile 增长 {growth_kb} KB（{before} → {after}）");
    // 500 次应远低于 1MB；旧实现（supported_output_configs 泄漏）会增长 ~1.3MB。
    assert!(
        growth_kb < 1024,
        "AudioSink 反复 open/drop 泄漏过多：pagefile 增长 {growth_kb} KB"
    );
}
