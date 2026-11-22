// 本文件由 pointer.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。
// 模块路径不变（src 里的声明用 #[path] 指过来），所以 use super::* 与私有可见性都照旧。

use super::*;

#[test]
fn down_move_up_lifecycle() {
    let mut t = PointerTable::new();
    assert_eq!(t.len(), 0);
    t.down(1, 10.0, 20.0);
    t.down(2, 30.0, 40.0);
    assert_eq!(t.len(), 2);
    assert_eq!(t.position_of(1), Some((10.0, 20.0)));
    assert!(t.move_pointer(2, 31.0, 41.0));
    assert_eq!(t.position_of(2), Some((31.0, 41.0)));
    // 未知 ID 的 Move 被忽略
    assert!(!t.move_pointer(9, 0.0, 0.0));
    assert!(t.up(1));
    assert_eq!(t.len(), 1);
    // 重复 Up：已不存在
    assert!(!t.up(1));
}

#[test]
fn duplicate_down_overwrites_in_place() {
    let mut t = PointerTable::new();
    t.down(1, 1.0, 1.0);
    t.down(2, 2.0, 2.0);
    // 缺 Up 的重复 Down：原位覆盖，不新增
    t.down(1, 5.0, 5.0);
    assert_eq!(t.len(), 2);
    assert_eq!(t.position_of(1), Some((5.0, 5.0)));
}

#[test]
fn nth_is_slot_ordered_and_deterministic() {
    let mut t = PointerTable::new();
    t.down(7, 0.0, 0.0);
    t.down(3, 1.0, 1.0);
    assert_eq!(t.nth(0).map(|(id, ..)| id), Some(7));
    assert_eq!(t.nth(1).map(|(id, ..)| id), Some(3));
    assert!(t.nth(2).is_none());
}

/// 第 9 个**新** ID 在满表时必须被忽略：既不能占用槽位，也不能把 count 加到 9
/// （count 是 u8，越界写更会破坏后续 len() 语义）。
#[test]
fn ninth_distinct_down_is_ignored_when_table_is_full() {
    let mut t = PointerTable::new();
    for id in 0..MAX_POINTERS as u64 {
        t.down(id, id as f32, 0.0);
    }
    assert_eq!(t.len(), MAX_POINTERS);

    t.down(99, 99.0, 99.0);
    assert_eq!(t.len(), MAX_POINTERS, "表满时新指针必须被忽略");
    assert_eq!(t.position_of(99), None, "第 9 个 down 不得占用槽位");
    for id in 0..MAX_POINTERS as u64 {
        assert_eq!(
            t.position_of(id),
            Some((id as f32, 0.0)),
            "已有槽位不得被挤掉"
        );
    }
    assert!(t.nth(MAX_POINTERS).is_none());
}

/// 即使表满，重复 ID 的 down 也必须走「先查同 ID」的原位覆盖路径，不能被当成
/// 新指针忽略掉（缺 Up 的重复 Down 是真实存在的外设行为）。
#[test]
fn duplicate_down_when_full_overwrites_in_place() {
    let mut t = PointerTable::new();
    for id in 0..MAX_POINTERS as u64 {
        t.down(id, id as f32, 0.0);
    }
    t.down(3, 300.0, 300.0);

    assert_eq!(t.len(), MAX_POINTERS, "覆盖不得改变活跃数量");
    assert_eq!(
        t.position_of(3),
        Some((300.0, 300.0)),
        "重复 ID 必须原位覆盖"
    );
    assert_eq!(t.nth(3), Some((3, 300.0, 300.0)), "槽位顺序不得变动");
    assert_eq!(t.position_of(0), Some((0.0, 0.0)));
}
