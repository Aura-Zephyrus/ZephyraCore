// 跨 crate 桥接与稳态缓冲复用的回归测试（由 pixels.rs 的 #[path] 挂入）。

use super::*;

#[test]
fn arc_planes_preserve_pixels_and_geometry() {
    let data: Vec<u8> = (0..24u8).collect();
    let planes = Nv12Planes::split(bytes::Bytes::from(data.clone()), 4, 4).unwrap();
    let arc = planes.to_arc_planes();
    assert_eq!((arc.width, arc.height), (4, 4));
    assert_eq!(&arc.y[..], &data[..16]);
    assert_eq!(&arc.uv[..], &data[16..24]);
    // Arc<[u8]> 的长度即平面语义长度，桥接层无需再传 size。
    assert_eq!((arc.y.len(), arc.uv.len()), (16, 8));
}

#[test]
fn bytes_planes_exit_is_zero_copy() {
    let data: Vec<u8> = (0..24u8).collect();
    let base = bytes::Bytes::from(data);
    let base_addr = base.as_ptr() as usize;
    let end = base_addr + 24;
    let planes = Nv12Planes::split(base, 4, 4).unwrap();
    let (y, uv) = planes.bytes_planes();
    // 两次 slice 都落在原分配的范围内，clone 只加引用计数 —— 全程零 memcpy。
    assert!((y.as_ptr() as usize) >= base_addr && (y.as_ptr() as usize) < end);
    assert!((uv.as_ptr() as usize) >= base_addr && (uv.as_ptr() as usize) < end);
    assert_eq!(y.len(), 16);
    assert_eq!(uv.len(), 8);
    let (oy, ou) = planes.into_bytes_planes();
    assert_eq!(oy.as_ptr(), y.as_ptr());
    assert_eq!(ou.as_ptr(), uv.as_ptr());
}

#[test]
fn into_arc_planes_matches_borrowed_conversion() {
    let data: Vec<u8> = (0..24u8).map(|i| i.wrapping_mul(7)).collect();
    let owned = Nv12Planes::split(bytes::Bytes::from(data.clone()), 4, 4)
        .unwrap()
        .into_arc_planes();
    let borrowed = Nv12Planes::split(bytes::Bytes::from(data), 4, 4)
        .unwrap()
        .to_arc_planes();
    assert_eq!(owned.y, borrowed.y);
    assert_eq!(owned.uv, borrowed.uv);
    assert_eq!(
        (owned.width, owned.height),
        (borrowed.width, borrowed.height)
    );
}

#[test]
fn process_into_reuses_buffer_capacity() {
    let rgb = vec![7u8; 64 * 64 * 3];
    let mut processor = CpuFrameProcessor::new();
    let mut buffer = Vec::new();

    processor.process_into(&rgb, 64, 64, &mut buffer);
    let capacity = buffer.capacity();
    let pointer = buffer.as_ptr();
    processor.process_into(&rgb, 64, 64, &mut buffer);

    // 同尺寸连续两帧：容量与首地址都不变 ⇒ 没有重新分配。
    assert_eq!(buffer.capacity(), capacity);
    assert_eq!(buffer.as_ptr(), pointer);
    assert_eq!(buffer, processor.process(&rgb, 64, 64));
}

#[test]
fn process_borrowed_reuses_internal_buffer_and_matches_process() {
    let small = vec![9u8; 64 * 64 * 3];
    let mut processor = CpuFrameProcessor::new();
    let first = processor.process_borrowed(&small, 64, 64).as_ptr();
    let snapshot = processor.process_borrowed(&small, 64, 64).to_vec();
    let second = processor.process_borrowed(&small, 64, 64).as_ptr();
    assert_eq!(first, second, "借出视图应复用同一块内部缓冲");
    assert_eq!(snapshot, processor.process(&small, 64, 64));

    // 降采样路径同样逐位一致。
    let large: Vec<u8> = (0..1280 * 720 * 3).map(|i| (i % 251) as u8).collect();
    let borrowed = processor.process_borrowed(&large, 1280, 720).to_vec();
    let expected = CpuFrameProcessor::new().process(&large, 1280, 720);
    assert_eq!(borrowed, expected);
    assert_eq!(
        (processor.last_width(), processor.last_height()),
        (854, 480)
    );
}
