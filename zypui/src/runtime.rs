//! Per-element build cache runtime (P1 foundation).
//!
//! P1 目标：在 zypui `Use`/元素边界复用构建输出，core `diff/style/layout/paint` 不改。
//! 本文件先提供 `DepMask` 分析、缓存存储和 `UiRuntime` API；实际命中路径
//! 以 `instantiate` 的全量语义为 oracle，逐步接入。

use crate::expr::Expr;
use crate::instantiate::Host;
use crate::intern::Interner;
use crate::ir::{ClassTok, Node, RawUi};
use crate::val::Val;
use crate::ZypuiError;
use std::collections::HashMap;
use std::sync::Arc;
use zephyra_core::element::Element;

/// 稳定槽位 ID（由 IR 结构路径派生，If/For 分支稳定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SiteId(pub u64);

/// 缓存身份变体。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CacheVariant {
    Single,
    Key(&'static str),
    Index(usize),
}

/// 元素缓存身份：必须包含 `site`，避免重复 key/无 key Use 串身份。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheId {
    pub parent: u64,
    pub site: SiteId,
    pub variant: CacheVariant,
}

#[allow(dead_code)]
pub fn mix_site(parent: u64, index: u64, tag: u64) -> u64 {
    // 简单稳定混合（非密码学）；用于 IR 结构路径身份。
    let mut x = parent ^ index.rotate_left(17) ^ tag.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x
}

#[allow(dead_code)]
fn _placeholder() {}

/// 静态依赖掩码（加载期从 IR 计算，不改 `.zypui.bin` 格式）。
#[derive(Debug, Clone, Default)]
pub struct DepMask {
    pub bindings: Vec<&'static str>,
    pub uses_hover: bool,
    pub uses_press: bool,
    pub uses_focused: bool,
    pub uses_key: bool,
    pub uses_tr: bool,
    pub uses_render_source_attr: bool,
    pub dynamic_interaction: bool,
    pub uncacheable: bool,
}

impl DepMask {
    pub fn merge(&mut self, other: &DepMask) {
        for b in &other.bindings {
            if !self.bindings.contains(b) {
                self.bindings.push(*b);
            }
        }
        self.uses_hover |= other.uses_hover;
        self.uses_press |= other.uses_press;
        self.uses_focused |= other.uses_focused;
        self.uses_key |= other.uses_key;
        self.uses_tr |= other.uses_tr;
        self.uses_render_source_attr |= other.uses_render_source_attr;
        self.dynamic_interaction |= other.dynamic_interaction;
        self.uncacheable |= other.uncacheable;
    }

    pub fn can_cache(&self) -> bool {
        !self.uncacheable
    }
}

fn mask_expr(e: &Expr, out: &mut DepMask) {
    match e {
        Expr::LitS(_) | Expr::LitF(_) | Expr::LitB(_) => {}
        Expr::Ref(name) => match *name {
            "hover" => out.uses_hover = true,
            "press" => out.uses_press = true,
            "focused" => out.uses_focused = true,
            "key" => out.uses_key = true,
            _ => {}
        },
        Expr::Binding(name) => {
            if !out.bindings.contains(name) {
                out.bindings.push(*name);
            }
        }
        Expr::Call(name, args) => {
            if *name == "tr" {
                out.uses_tr = true;
            }
            if matches!(*name, "hover" | "press" | "focused") && !args.is_empty() {
                out.dynamic_interaction = true;
            }
            for a in args {
                mask_expr(a, out);
            }
        }
        Expr::Bin(_, l, r) => {
            mask_expr(l, out);
            mask_expr(r, out);
        }
        Expr::Not(x) | Expr::Neg(x) => mask_expr(x, out),
        Expr::Cond(c, a, b) => {
            mask_expr(c, out);
            mask_expr(a, out);
            mask_expr(b, out);
        }
        Expr::Field(base, _) => mask_expr(base, out),
    }
}

fn mask_class(tok: &ClassTok, out: &mut DepMask) {
    match tok {
        ClassTok::Lit(_) => {}
        ClassTok::If { cond, .. } => mask_expr(cond, out),
        ClassTok::Dyn(e) => mask_expr(e, out),
    }
}

fn mask_node(
    node: &Node,
    comps: &HashMap<&'static str, usize>,
    raw: &RawUi,
    visiting: &mut Vec<&'static str>,
    out: &mut DepMask,
) {
    match node {
        Node::El(el) => {
            if let Some(k) = &el.key {
                mask_expr(k, out);
            }
            for c in &el.classes {
                mask_class(c, out);
            }
            for (name, e) in &el.attrs {
                if *name == "render-source" {
                    out.uses_render_source_attr = true;
                }
                mask_expr(e, out);
            }
            for ch in &el.children {
                mask_node(ch, comps, raw, visiting, out);
            }
        }
        Node::If(n) => {
            mask_expr(&n.cond, out);
            for c in &n.then {
                mask_node(c, comps, raw, visiting, out);
            }
            for c in &n.els {
                mask_node(c, comps, raw, visiting, out);
            }
        }
        Node::For(n) => {
            mask_expr(&n.list, out);
            if let Some(k) = &n.track_key {
                mask_expr(k, out);
            }
            for c in &n.body {
                mask_node(c, comps, raw, visiting, out);
            }
        }
        Node::Use(u) => {
            if let Some(k) = &u.key {
                mask_expr(k, out);
            }
            for c in &u.class_extra {
                mask_class(c, out);
            }
            for (_, e) in &u.args {
                mask_expr(e, out);
            }
            // Use 传 children 或组件含 slot：P1 不独立缓存该边界（slot 回填语义）。
            if !u.children.is_empty() {
                out.uncacheable = true;
                for c in &u.children {
                    mask_node(c, comps, raw, visiting, out);
                }
            }
            if let Some(&ci) = comps.get(u.comp) {
                if visiting.contains(&u.comp) {
                    out.uncacheable = true;
                } else {
                    let comp = &raw.components[ci];
                    if crate::instantiate::component_has_slot(comp) {
                        out.uncacheable = true;
                    }
                    visiting.push(u.comp);
                    for c in &comp.body {
                        mask_node(c, comps, raw, visiting, out);
                    }
                    visiting.pop();
                }
            } else {
                out.uncacheable = true;
            }
        }
        Node::Slot => out.uncacheable = true,
    }
}

pub fn mask_nodes(nodes: &[Node], comps: &HashMap<&'static str, usize>, raw: &RawUi) -> DepMask {
    let mut m = DepMask::default();
    let mut visiting = Vec::new();
    for n in nodes {
        mask_node(n, comps, raw, &mut visiting, &mut m);
    }
    m
}

/// 依赖快照：保留值做 `PartialEq` 比较，revision 仅作可选加速。
#[derive(Debug, Clone, PartialEq)]
pub struct DepSnapshot {
    pub host_scope: u64,
    pub state_rev: Option<u64>,
    pub bindings: Vec<(&'static str, Option<u64>, Option<Val>)>,
    pub hover: bool,
    pub press: bool,
    pub focused: bool,
    pub cur_key: &'static str,
    pub args: Vec<(&'static str, Val)>,
}

pub fn take_snapshot(
    mask: &DepMask,
    host: &dyn Host,
    cur_key: &'static str,
    args: Vec<(&'static str, Val)>,
) -> DepSnapshot {
    let host_scope = host.cache_scope();
    let state_rev = host.state_revision();
    let mut bindings = Vec::with_capacity(mask.bindings.len());
    for name in &mask.bindings {
        let rev = host.binding_revision(name);
        let val = host.binding(name).or(Some(Val::B(false)));
        bindings.push((*name, rev, val));
    }
    DepSnapshot {
        host_scope,
        state_rev,
        bindings,
        hover: if mask.uses_hover || mask.dynamic_interaction {
            host.hovers(cur_key)
        } else {
            false
        },
        press: if mask.uses_press || mask.dynamic_interaction {
            host.holds(cur_key)
        } else {
            false
        },
        focused: if mask.uses_focused || mask.dynamic_interaction {
            host.is_focused(cur_key)
        } else {
            false
        },
        cur_key,
        args,
    }
}

#[derive(Clone)]
pub struct CacheEntry {
    pub snapshot: DepSnapshot,
    pub output: Vec<Arc<Element>>,
    pub generation: u32,
}

#[derive(Default)]
pub struct ElementCacheStore {
    map: HashMap<CacheId, CacheEntry>,
    generation: u32,
    pub hits: u64,
    pub misses: u64,
    /// 由依赖/结构失效显式标脏的条目数（P1 统计）。
    pub invalidations: u64,
}

impl ElementCacheStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_frame(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&mut self, id: &CacheId, snapshot: &DepSnapshot) -> Option<&[Arc<Element>]> {
        if let Some(e) = self.map.get_mut(id) {
            if &e.snapshot == snapshot {
                e.generation = self.generation;
                self.hits += 1;
                return Some(&e.output);
            }
            self.invalidations += 1;
        }
        self.misses += 1;
        None
    }

    pub fn insert(&mut self, id: CacheId, snapshot: DepSnapshot, output: Vec<Arc<Element>>) {
        self.map.insert(
            id,
            CacheEntry {
                snapshot,
                output,
                generation: self.generation,
            },
        );
    }

    /// 显式失效：dep 变更可调用；P1 主要依赖 snapshot 比较，保留此口供后续 composition_dirty 传播。
    pub fn invalidate(&mut self, id: &CacheId) -> bool {
        let removed = self.map.remove(id).is_some();
        if removed {
            self.invalidations += 1;
        }
        removed
    }

    pub fn sweep(&mut self) {
        let g = self.generation;
        self.map
            .retain(|_, e| e.generation == g || e.generation.wrapping_add(1) == g);
    }
}

/// 组件名/页面名索引 + 静态 mask。
pub struct UiMasks {
    pub comp_idx: HashMap<&'static str, usize>,
    pub comp_masks: Vec<DepMask>,
    pub page_masks: HashMap<&'static str, DepMask>,
}

impl UiMasks {
    pub fn new(raw: &RawUi) -> Result<Self, ZypuiError> {
        let mut comp_idx = HashMap::with_capacity(raw.components.len());
        for (i, c) in raw.components.iter().enumerate() {
            if comp_idx.insert(c.name, i).is_some() {
                return crate::err(format!("组件 `{}` 重名", c.name));
            }
        }
        let comp_masks = raw
            .components
            .iter()
            .map(|c| mask_nodes(&c.body, &comp_idx, raw))
            .collect();
        let mut page_masks = HashMap::new();
        for p in &raw.pages {
            page_masks.insert(p.name, mask_nodes(&p.root, &comp_idx, raw));
        }
        Ok(Self {
            comp_idx,
            comp_masks,
            page_masks,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub invalidations: u64,
    pub entries: usize,
}

/// P1 运行时。保持 `Ui::build` 为 oracle；默认走 per-element build cache。
pub struct UiRuntime {
    pub ui: crate::Ui,
    pub masks: UiMasks,
    pub cache: ElementCacheStore,
    /// debug：命中路径与 oracle 全量构建结果不一致时 panic（昂贵，仅测试用）。
    oracle_check: bool,
}

impl UiRuntime {
    pub fn new(raw: RawUi) -> Result<Self, ZypuiError> {
        let masks = UiMasks::new(&raw)?;
        let ui = crate::Ui::new(raw)?;
        Ok(Self {
            ui,
            masks,
            cache: ElementCacheStore::new(),
            oracle_check: false,
        })
    }

    pub fn set_oracle_check(&mut self, enabled: bool) {
        self.oracle_check = enabled;
    }

    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            hits: self.cache.hits,
            misses: self.cache.misses,
            invalidations: self.cache.invalidations,
            entries: self.cache.len(),
        }
    }

    /// P1 per-element resolve：Use 边界缓存命中时复用子树，miss 回退全量展开。
    pub fn resolve_page(
        &mut self,
        page: &str,
        host: &dyn Host,
        it: &mut Interner,
    ) -> Result<Arc<Element>, ZypuiError> {
        self.cache.begin_frame();
        let mut els =
            self.ui
                .build_cached(page, host, it, Some(&self.masks), Some(&mut self.cache))?;
        self.cache.sweep();
        if els.len() != 1 {
            return crate::err(format!(
                "页 `{page}` 应恰好产出 1 个根元素，实际 {}",
                els.len()
            ));
        }
        let out = Arc::new(els.remove(0));
        if self.oracle_check {
            let mut it2 = Interner::new();
            let oracle = self.ui.build(page, host, &mut it2)?;
            debug_assert_eq!(oracle.len(), 1, "oracle page shape changed");
            debug_assert_eq!(
                element_fingerprint(&oracle[0]),
                element_fingerprint(out.as_ref()),
                "UiRuntime cache result diverged from oracle build for page `{page}` (hits={} misses={})",
                self.cache.hits,
                self.cache.misses
            );
        }
        Ok(out)
    }

    /// 兼容旧形状：返回 `Vec<Element>`。
    pub fn build(
        &mut self,
        page: &str,
        host: &dyn Host,
        it: &mut Interner,
    ) -> Result<Vec<Element>, ZypuiError> {
        self.ui.build(page, host, it)
    }
}

pub fn val_to_cache_key(v: Val, it: &mut Interner) -> &'static str {
    match v {
        Val::S(s) => s,
        Val::F(n) => it.intern(&format!("{n}")),
        Val::B(b) => {
            if b {
                "true"
            } else {
                "false"
            }
        }
        Val::Raw(_) | Val::Rec(_) | Val::L(_) => it.intern(&format!("{v:?}")),
    }
}

fn element_fingerprint(
    e: &Element,
) -> (
    zephyra_core::id::NodeTag,
    Option<zephyra_core::id::ElementKey>,
    Vec<&'static str>,
    usize,
) {
    // 轻量结构指纹：用于 debug oracle。props/text 的深比较留给更重的 oracle 模式。
    (
        e.tag(),
        e.key().cloned(),
        e.style_names().to_vec(),
        e.child_elements().count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::BinOp;
    use crate::ir::{ElementNode, ForNode, IfNode, Page, UseNode};
    use zephyra_core::id::{ElementKey, NodeTag};
    use zephyra_core::property::PropertyValue;

    /// 标志位顺序：hover, press, focused, key, tr, render-source, dynamic, uncacheable。
    fn flags(m: &DepMask) -> (bool, bool, bool, bool, bool, bool, bool, bool) {
        (
            m.uses_hover,
            m.uses_press,
            m.uses_focused,
            m.uses_key,
            m.uses_tr,
            m.uses_render_source_attr,
            m.dynamic_interaction,
            m.uncacheable,
        )
    }

    fn mask_of(e: &Expr) -> DepMask {
        let mut m = DepMask::default();
        mask_expr(e, &mut m);
        m
    }

    fn class_mask(t: &ClassTok) -> DepMask {
        let mut m = DepMask::default();
        mask_class(t, &mut m);
        m
    }

    fn node_mask(node: &Node) -> DepMask {
        let raw = RawUi {
            components: vec![],
            pages: vec![],
        };
        let comps = HashMap::new();
        let mut visiting = Vec::new();
        let mut out = DepMask::default();
        mask_node(node, &comps, &raw, &mut visiting, &mut out);
        out
    }

    fn use_mask(raw: &RawUi, node: &Node) -> DepMask {
        let mut comps = HashMap::new();
        for (i, c) in raw.components.iter().enumerate() {
            comps.insert(c.name, i);
        }
        let mut visiting = Vec::new();
        let mut out = DepMask::default();
        mask_node(node, &comps, raw, &mut visiting, &mut out);
        out
    }

    fn el(key: Option<Expr>, classes: Vec<ClassTok>, attrs: Vec<(&'static str, Expr)>) -> Node {
        Node::El(ElementNode {
            tag: NodeTag::Box,
            key,
            classes,
            attrs,
            children: vec![],
        })
    }

    fn use_node(comp: &'static str, children: Vec<Node>) -> Node {
        Node::Use(UseNode {
            comp,
            key: None,
            class_extra: vec![],
            args: vec![],
            children,
        })
    }

    struct EmptyHost;

    impl Host for EmptyHost {
        fn binding(&self, _name: &str) -> Option<Val> {
            None
        }
        fn tr(&self, key: &'static str) -> &'static str {
            key
        }
        fn hovers(&self, _key: &str) -> bool {
            false
        }
        fn holds(&self, _key: &str) -> bool {
            false
        }
    }

    #[test]
    fn mix_site_pins_exact_hashes() {
        assert_eq!(mix_site(0, 0, 0), 0);
        assert_eq!(mix_site(1, 2, 3), 11318103724633734439);
        assert_eq!(mix_site(0xDEAD_BEEF, 7, 21), 18098238933156416668);
        assert_eq!(mix_site(255, 255, 255), 9480753263772062864);
        assert_eq!(mix_site(100, 5, 42), 13157424463941904006);
    }

    #[test]
    fn mix_site_is_deterministic_and_sensitive_to_each_input() {
        let base = mix_site(10, 20, 30);
        assert_eq!(base, mix_site(10, 20, 30));
        assert_ne!(base, mix_site(11, 20, 30));
        assert_ne!(base, mix_site(10, 21, 30));
        assert_ne!(base, mix_site(10, 20, 31));
        let a = mix_site(0, 0, 1);
        let b = mix_site(0, 1, 0);
        let c = mix_site(1, 0, 0);
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn dep_mask_merge_is_union_with_binding_dedup() {
        let mut a = DepMask {
            bindings: vec!["x", "y"],
            uses_hover: true,
            uses_key: true,
            ..DepMask::default()
        };
        let b = DepMask {
            bindings: vec!["y", "z"],
            uses_press: true,
            uses_focused: true,
            uses_tr: true,
            uses_render_source_attr: true,
            dynamic_interaction: true,
            uncacheable: true,
            ..DepMask::default()
        };
        a.merge(&b);
        assert_eq!(a.bindings, vec!["x", "y", "z"]);
        assert_eq!(flags(&a), (true, true, true, true, true, true, true, true));
        let once = a.clone();
        a.merge(&b);
        assert_eq!(a.bindings, once.bindings, "merge 幂等");
        assert_eq!(flags(&a), flags(&once));
    }

    #[test]
    fn dep_mask_merge_empty_is_identity_and_copies_into_empty() {
        let full = DepMask {
            bindings: vec!["a", "b"],
            uses_hover: true,
            uses_press: true,
            uses_focused: true,
            uses_key: true,
            uses_tr: true,
            uses_render_source_attr: true,
            dynamic_interaction: true,
            uncacheable: true,
        };
        let mut into = DepMask::default();
        into.merge(&full);
        assert_eq!(into.bindings, vec!["a", "b"]);
        assert_eq!(
            flags(&into),
            (true, true, true, true, true, true, true, true)
        );

        let mut keep = full.clone();
        keep.merge(&DepMask::default());
        assert_eq!(keep.bindings, vec!["a", "b"]);
        assert_eq!(
            flags(&keep),
            (true, true, true, true, true, true, true, true)
        );
    }

    #[test]
    fn dep_mask_can_cache_only_depends_on_uncacheable() {
        assert!(DepMask::default().can_cache());
        let mut m = DepMask {
            bindings: vec!["x"],
            uses_hover: true,
            uses_press: true,
            uses_focused: true,
            uses_key: true,
            uses_tr: true,
            uses_render_source_attr: true,
            dynamic_interaction: true,
            uncacheable: false,
        };
        assert!(m.can_cache(), "其它依赖标志不剥夺缓存资格");
        m.uncacheable = true;
        assert!(!m.can_cache());
        m.uncacheable = false;
        assert!(m.can_cache());
        let other = DepMask {
            uncacheable: true,
            ..DepMask::default()
        };
        m.merge(&other);
        assert!(!m.can_cache(), "merge 传入 uncacheable 后不可缓存");
    }

    #[test]
    fn mask_expr_literals_and_refs_set_exact_flags() {
        let lit = mask_of(&Expr::LitS("x"));
        assert!(lit.bindings.is_empty());
        assert_eq!(
            flags(&lit),
            (false, false, false, false, false, false, false, false)
        );
        for e in [Expr::LitF(1.0), Expr::LitB(true)] {
            let m = mask_of(&e);
            assert!(m.bindings.is_empty());
            assert_eq!(
                flags(&m),
                (false, false, false, false, false, false, false, false)
            );
        }

        assert_eq!(
            flags(&mask_of(&Expr::Ref("hover"))),
            (true, false, false, false, false, false, false, false)
        );
        assert_eq!(
            flags(&mask_of(&Expr::Ref("press"))),
            (false, true, false, false, false, false, false, false)
        );
        assert_eq!(
            flags(&mask_of(&Expr::Ref("focused"))),
            (false, false, true, false, false, false, false, false)
        );
        assert_eq!(
            flags(&mask_of(&Expr::Ref("key"))),
            (false, false, false, true, false, false, false, false)
        );
        let other = mask_of(&Expr::Ref("other"));
        assert!(other.bindings.is_empty());
        assert_eq!(
            flags(&other),
            (false, false, false, false, false, false, false, false)
        );
    }

    #[test]
    fn mask_expr_bindings_and_calls_set_exact_flags() {
        let m = mask_of(&Expr::Binding("x"));
        assert_eq!(m.bindings, vec!["x"]);
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, false)
        );

        let m = mask_of(&Expr::Call("tr", vec![Expr::LitS("k")]));
        assert!(m.bindings.is_empty());
        assert_eq!(
            flags(&m),
            (false, false, false, false, true, false, false, false)
        );

        let m = mask_of(&Expr::Call("hover", vec![Expr::LitS("k")]));
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, true, false)
        );

        let m = mask_of(&Expr::Call("press", vec![]));
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, false),
            "空参调用不置 dynamic"
        );

        let m = mask_of(&Expr::Call("focused", vec![Expr::Binding("k")]));
        assert_eq!(m.bindings, vec!["k"]);
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, true, false)
        );

        let m = mask_of(&Expr::Call(
            "min",
            vec![Expr::Binding("a"), Expr::Ref("key")],
        ));
        assert_eq!(m.bindings, vec!["a"]);
        assert_eq!(
            flags(&m),
            (false, false, false, true, false, false, false, false)
        );
    }

    #[test]
    fn mask_expr_nested_operators_recurse_and_dedup() {
        let e = Expr::Bin(
            BinOp::Add,
            Box::new(Expr::Binding("a")),
            Box::new(Expr::Bin(
                BinOp::Mul,
                Box::new(Expr::Binding("a")),
                Box::new(Expr::Ref("hover")),
            )),
        );
        let m = mask_of(&e);
        assert_eq!(m.bindings, vec!["a"], "重复绑定去重");
        assert_eq!(
            flags(&m),
            (true, false, false, false, false, false, false, false)
        );

        let e = Expr::Cond(
            Box::new(Expr::Ref("focused")),
            Box::new(Expr::Not(Box::new(Expr::Binding("c")))),
            Box::new(Expr::Neg(Box::new(Expr::Ref("press")))),
        );
        let m = mask_of(&e);
        assert_eq!(m.bindings, vec!["c"]);
        assert_eq!(
            flags(&m),
            (false, true, true, false, false, false, false, false)
        );

        let m = mask_of(&Expr::Field(Box::new(Expr::Binding("rec")), "id"));
        assert_eq!(m.bindings, vec!["rec"]);
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, false)
        );
    }

    #[test]
    fn mask_class_masks_only_condition_expression() {
        let m = class_mask(&ClassTok::Lit("hero"));
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, false)
        );

        let m = class_mask(&ClassTok::If {
            cond: Expr::Ref("hover"),
            tok: "hovered",
        });
        assert_eq!(
            flags(&m),
            (true, false, false, false, false, false, false, false)
        );

        let m = class_mask(&ClassTok::Dyn(Expr::Binding("variant")));
        assert_eq!(m.bindings, vec!["variant"]);
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, false)
        );
    }

    #[test]
    fn mask_node_element_if_for_collect_all_deps() {
        let nested = Node::El(ElementNode {
            tag: NodeTag::Text,
            key: None,
            classes: vec![ClassTok::Dyn(Expr::Ref("press"))],
            attrs: vec![],
            children: vec![],
        });
        let m = node_mask(&Node::El(ElementNode {
            tag: NodeTag::Box,
            key: Some(Expr::Ref("key")),
            classes: vec![
                ClassTok::Lit("hero"),
                ClassTok::If {
                    cond: Expr::Ref("hover"),
                    tok: "h",
                },
            ],
            attrs: vec![
                ("render-source", Expr::LitS("v")),
                ("text", Expr::Binding("label")),
            ],
            children: vec![nested],
        }));
        assert_eq!(m.bindings, vec!["label"]);
        assert_eq!(
            flags(&m),
            (true, true, false, true, false, true, false, false)
        );

        let m = node_mask(&Node::If(IfNode {
            cond: Expr::Ref("focused"),
            then: vec![el(Some(Expr::Binding("a")), vec![], vec![])],
            els: vec![el(Some(Expr::Binding("b")), vec![], vec![])],
        }));
        assert_eq!(m.bindings, vec!["a", "b"]);
        assert_eq!(
            flags(&m),
            (false, false, true, false, false, false, false, false)
        );

        let m = node_mask(&Node::For(ForNode {
            var: "item",
            index: None,
            track_key: Some(Expr::Binding("id")),
            list: Expr::Binding("items"),
            body: vec![el(Some(Expr::Ref("press")), vec![], vec![])],
        }));
        assert_eq!(m.bindings, vec!["items", "id"], "list 先于 track_key");
        assert_eq!(
            flags(&m),
            (false, true, false, false, false, false, false, false)
        );
    }

    #[test]
    fn mask_node_use_uncacheable_cases() {
        let good_raw = RawUi {
            components: vec![crate::Component {
                name: "Btn",
                params: vec![],
                body: vec![el(
                    Some(Expr::Ref("hover")),
                    vec![],
                    vec![("text", Expr::Binding("label"))],
                )],
            }],
            pages: vec![],
        };
        let m = use_mask(
            &good_raw,
            &Node::Use(UseNode {
                comp: "Btn",
                key: Some(Expr::Ref("key")),
                class_extra: vec![ClassTok::Dyn(Expr::Binding("variant"))],
                args: vec![],
                children: vec![],
            }),
        );
        assert_eq!(
            m.bindings,
            vec!["variant", "label"],
            "class_extra 先于组件体"
        );
        assert_eq!(
            flags(&m),
            (true, false, false, true, false, false, false, false)
        );
        assert!(m.can_cache(), "无 slot、无 children、已知组件仍可缓存");

        let m = use_mask(
            &good_raw,
            &use_node(
                "Btn",
                vec![el(Some(Expr::Binding("child")), vec![], vec![])],
            ),
        );
        assert_eq!(m.bindings, vec!["child", "label"], "children 先于组件体");
        assert_eq!(
            flags(&m),
            (true, false, false, false, false, false, false, true)
        );

        let empty_raw = RawUi {
            components: vec![],
            pages: vec![],
        };
        let m = use_mask(&empty_raw, &use_node("Missing", vec![]));
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, true)
        );

        let slot_raw = RawUi {
            components: vec![crate::Component {
                name: "SlotC",
                params: vec![],
                body: vec![Node::Slot],
            }],
            pages: vec![],
        };
        let m = use_mask(&slot_raw, &use_node("SlotC", vec![]));
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, true)
        );

        let rec_raw = RawUi {
            components: vec![crate::Component {
                name: "R",
                params: vec![],
                body: vec![use_node("R", vec![])],
            }],
            pages: vec![],
        };
        let m = use_mask(&rec_raw, &use_node("R", vec![]));
        assert_eq!(
            flags(&m),
            (false, false, false, false, false, false, false, true)
        );
    }

    #[test]
    fn mask_nodes_aggregates_across_nodes() {
        let raw = RawUi {
            components: vec![],
            pages: vec![],
        };
        let comps = HashMap::new();
        let nodes = vec![
            el(Some(Expr::Ref("hover")), vec![], vec![]),
            Node::Slot,
            el(Some(Expr::Binding("t")), vec![], vec![]),
        ];
        let m = mask_nodes(&nodes, &comps, &raw);
        assert_eq!(m.bindings, vec!["t"]);
        assert_eq!(
            flags(&m),
            (true, false, false, false, false, false, false, true)
        );
        assert!(!m.can_cache());

        let empty = mask_nodes(&[], &comps, &raw);
        assert!(empty.bindings.is_empty());
        assert_eq!(
            flags(&empty),
            (false, false, false, false, false, false, false, false)
        );
    }

    #[test]
    fn val_to_cache_key_pins_exact_keys() {
        let mut it = Interner::new();
        assert_eq!(val_to_cache_key(Val::S("literal"), &mut it), "literal");
        assert_eq!(it.len(), 0, "S 直接返回静态串，不进去重表");

        assert_eq!(val_to_cache_key(Val::F(1.0), &mut it), "1");
        assert_eq!(val_to_cache_key(Val::F(2.5), &mut it), "2.5");
        assert_eq!(val_to_cache_key(Val::F(0.0), &mut it), "0");
        assert_eq!(it.len(), 3);

        assert_eq!(val_to_cache_key(Val::B(true), &mut it), "true");
        assert_eq!(val_to_cache_key(Val::B(false), &mut it), "false");
        assert_eq!(it.len(), 3, "B 返回静态串，不进去重表");

        assert_eq!(
            val_to_cache_key(Val::L(vec![Val::F(1.0), Val::S("a")]), &mut it),
            "L([F(1.0), S(\"a\")])"
        );
        assert_eq!(
            val_to_cache_key(Val::Rec(vec![("id", Val::S("a"))]), &mut it),
            "Rec([(\"id\", S(\"a\"))])"
        );
        assert_eq!(
            val_to_cache_key(Val::Raw(PropertyValue::StaticStr("x")), &mut it),
            "Raw(\"x\")"
        );
        assert_eq!(it.len(), 6, "F/L/Rec/Raw 各插入一条");
    }

    fn build_root(classes: Vec<ClassTok>, children: Vec<Node>) -> Element {
        let raw = RawUi {
            components: vec![],
            pages: vec![Page {
                name: "p",
                root: vec![Node::El(ElementNode {
                    tag: NodeTag::Box,
                    key: Some(Expr::LitS("root")),
                    classes,
                    attrs: vec![],
                    children,
                })],
            }],
        };
        let ui = crate::Ui::new(raw).unwrap();
        let mut it = Interner::new();
        let mut els = ui.build("p", &EmptyHost, &mut it).unwrap();
        els.remove(0)
    }

    #[test]
    fn element_fingerprint_pins_shape() {
        let e = build_root(
            vec![ClassTok::Lit("hero"), ClassTok::Lit("accent")],
            vec![Node::El(ElementNode {
                tag: NodeTag::Text,
                key: None,
                classes: vec![],
                attrs: vec![],
                children: vec![],
            })],
        );
        let fp = element_fingerprint(&e);
        assert_eq!(fp.0, NodeTag::Box);
        assert_eq!(fp.1, Some(ElementKey::Name("root")));
        assert_eq!(fp.2, vec!["hero", "accent"]);
        assert_eq!(fp.3, 1);

        let plain = build_root(vec![], vec![]);
        let fp = element_fingerprint(&plain);
        assert_eq!(fp.0, NodeTag::Box);
        assert_eq!(fp.1, Some(ElementKey::Name("root")));
        assert!(fp.2.is_empty());
        assert_eq!(fp.3, 0);
    }
}
