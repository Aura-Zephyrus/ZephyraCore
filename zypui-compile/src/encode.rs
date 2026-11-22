//! [`RawUi`] → 二进制。`zypui::decode` 的镜像 —— **改一处必改另一处**，
//! 两边各带 round-trip 测试钉住（zypui 测 decode 自洽，这里测 encode 产物
//! decode 得回同一棵树）。
//!
//! 格式表见 `zypui::decode` 的模块文档。

use zephyra_core::id::NodeTag;
use zypui::decode::MAGIC;
use zypui::expr::{Expr, CALLS};
use zypui::ir::{ClassTok, Node, RawUi};

/// 编码整份工件。
pub fn encode(raw: &RawUi) -> Vec<u8> {
    let mut w = Writer::default();
    w.bytes(MAGIC);
    w.u32(raw.components.len() as u32);
    for c in &raw.components {
        w.str(c.name);
        w.u32(c.params.len() as u32);
        for p in &c.params {
            w.str(p);
        }
        w.u32(c.body.len() as u32);
        for n in &c.body {
            node(&mut w, n);
        }
    }
    w.u32(raw.pages.len() as u32);
    for p in &raw.pages {
        w.str(p.name);
        w.u32(p.root.len() as u32);
        for n in &p.root {
            node(&mut w, n);
        }
    }
    w.out
}

fn node(w: &mut Writer, n: &Node) {
    match n {
        Node::El(el) => {
            w.u8(0);
            w.u8(match el.tag {
                NodeTag::Text => 0,
                NodeTag::Image => 1,
                NodeTag::Box => 2,
                NodeTag::Control => 3,
            });
            opt_expr(w, &el.key);
            w.u32(el.classes.len() as u32);
            for t in &el.classes {
                classtok(w, t);
            }
            w.u32(el.attrs.len() as u32);
            for (name, e) in &el.attrs {
                w.str(name);
                expr(w, e);
            }
            w.u32(el.children.len() as u32);
            for c in &el.children {
                node(w, c);
            }
        }
        Node::If(f) => {
            w.u8(1);
            expr(w, &f.cond);
            w.u32(f.then.len() as u32);
            for c in &f.then {
                node(w, c);
            }
            w.u32(f.els.len() as u32);
            for c in &f.els {
                node(w, c);
            }
        }
        Node::For(f) => {
            w.u8(2);
            w.str(f.var);
            opt_str(w, f.index);
            opt_expr(w, &f.track_key);
            expr(w, &f.list);
            w.u32(f.body.len() as u32);
            for c in &f.body {
                node(w, c);
            }
        }
        Node::Use(u) => {
            w.u8(3);
            w.str(u.comp);
            opt_expr(w, &u.key);
            w.u32(u.class_extra.len() as u32);
            for t in &u.class_extra {
                classtok(w, t);
            }
            w.u32(u.args.len() as u32);
            for (name, e) in &u.args {
                w.str(name);
                expr(w, e);
            }
            w.u32(u.children.len() as u32);
            for c in &u.children {
                node(w, c);
            }
        }
        Node::Slot => w.u8(4),
    }
}

fn classtok(w: &mut Writer, t: &ClassTok) {
    match t {
        ClassTok::Lit(s) => {
            w.u8(0);
            w.str(s);
        }
        ClassTok::If { cond, tok } => {
            w.u8(1);
            expr(w, cond);
            w.str(tok);
        }
        ClassTok::Dyn(e) => {
            w.u8(2);
            expr(w, e);
        }
    }
}

fn expr(w: &mut Writer, e: &Expr) {
    match e {
        Expr::LitS(s) => {
            w.u8(0);
            w.str(s);
        }
        Expr::LitF(f) => {
            w.u8(1);
            w.bytes(&f.to_le_bytes());
        }
        Expr::LitB(b) => {
            w.u8(2);
            w.u8(*b as u8);
        }
        Expr::Ref(s) => {
            w.u8(3);
            w.str(s);
        }
        Expr::Binding(s) => {
            w.u8(4);
            w.str(s);
        }
        Expr::Call(name, args) => {
            debug_assert!(CALLS.contains(name), "zypuic 校验过的调用名");
            w.u8(5);
            w.str(name);
            w.u32(args.len() as u32);
            for a in args {
                expr(w, a);
            }
        }
        Expr::Bin(op, l, r) => {
            w.u8(6);
            w.u8(match op {
                zypui::expr::BinOp::Add => 0,
                zypui::expr::BinOp::Sub => 1,
                zypui::expr::BinOp::Mul => 2,
                zypui::expr::BinOp::Div => 3,
                zypui::expr::BinOp::Eq => 4,
                zypui::expr::BinOp::Ne => 5,
                zypui::expr::BinOp::Lt => 6,
                zypui::expr::BinOp::Le => 7,
                zypui::expr::BinOp::Gt => 8,
                zypui::expr::BinOp::Ge => 9,
                zypui::expr::BinOp::And => 10,
                zypui::expr::BinOp::Or => 11,
                zypui::expr::BinOp::Mod => 12,
            });
            expr(w, l);
            expr(w, r);
        }
        Expr::Not(a) => {
            w.u8(7);
            expr(w, a);
        }
        Expr::Neg(a) => {
            w.u8(8);
            expr(w, a);
        }
        Expr::Cond(c, a, b) => {
            w.u8(9);
            expr(w, c);
            expr(w, a);
            expr(w, b);
        }
        Expr::Field(base, name) => {
            w.u8(10);
            expr(w, base);
            w.str(name);
        }
    }
}

fn opt_expr(w: &mut Writer, e: &Option<Expr>) {
    match e {
        Some(e) => {
            w.u8(1);
            expr(w, e);
        }
        None => w.u8(0),
    }
}

fn opt_str(w: &mut Writer, s: Option<&'static str>) {
    match s {
        Some(s) => {
            w.u8(1);
            w.str(s);
        }
        None => w.u8(0),
    }
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn bytes(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }

    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }

    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.bytes(s.as_bytes());
    }
}

// ============================== 直接单测（逐字节钉住编码格式） ==============================

#[cfg(test)]
mod tests {
    use super::*;
    use zypui::expr::BinOp;
    use zypui::ir::{Component, ElementNode, ForNode, IfNode, Page, UseNode};

    fn bytes_of_expr(e: &Expr) -> Vec<u8> {
        let mut w = Writer::default();
        expr(&mut w, e);
        w.out
    }

    fn bytes_of_node(n: &Node) -> Vec<u8> {
        let mut w = Writer::default();
        node(&mut w, n);
        w.out
    }

    fn bytes_of_tok(t: &ClassTok) -> Vec<u8> {
        let mut w = Writer::default();
        classtok(&mut w, t);
        w.out
    }

    #[test]
    fn writer_primitives_pin_little_endian_layout() {
        let mut w = Writer::default();
        assert!(w.out.is_empty());
        w.bytes(&[0x01, 0x02]);
        w.u8(0xFF);
        w.u32(0x0102_0304);
        w.str("AB");
        assert_eq!(w.out, vec![1, 2, 0xFF, 4, 3, 2, 1, 2, 0, 0, 0, b'A', b'B']);
        // str 用字节长度（中文 3 字节），不是字符数。
        let mut w2 = Writer::default();
        w2.str("中");
        assert_eq!(w2.out, vec![3, 0, 0, 0, 0xE4, 0xB8, 0xAD]);
    }

    #[test]
    fn expr_literals_and_names_pin_exact_bytes() {
        assert_eq!(
            bytes_of_expr(&Expr::LitS("hi")),
            vec![0, 2, 0, 0, 0, b'h', b'i']
        );
        assert_eq!(bytes_of_expr(&Expr::LitF(1.0)), vec![1, 0, 0, 0x80, 0x3F]);
        assert_eq!(bytes_of_expr(&Expr::LitB(true)), vec![2, 1]);
        assert_eq!(bytes_of_expr(&Expr::LitB(false)), vec![2, 0]);
        assert_eq!(bytes_of_expr(&Expr::Ref("x")), vec![3, 1, 0, 0, 0, b'x']);
        assert_eq!(
            bytes_of_expr(&Expr::Binding("b")),
            vec![4, 1, 0, 0, 0, b'b']
        );
    }

    #[test]
    fn expr_compound_forms_pin_exact_bytes() {
        assert_eq!(
            bytes_of_expr(&Expr::Call("tr", vec![Expr::LitS("k")])),
            vec![5, 2, 0, 0, 0, b't', b'r', 1, 0, 0, 0, 0, 1, 0, 0, 0, b'k']
        );
        assert_eq!(
            bytes_of_expr(&Expr::Call("str", vec![])),
            vec![5, 3, 0, 0, 0, b's', b't', b'r', 0, 0, 0, 0]
        );
        assert_eq!(
            bytes_of_expr(&Expr::Not(Box::new(Expr::LitB(true)))),
            vec![7, 2, 1]
        );
        assert_eq!(
            bytes_of_expr(&Expr::Neg(Box::new(Expr::LitF(1.0)))),
            vec![8, 1, 0, 0, 0x80, 0x3F]
        );
        assert_eq!(
            bytes_of_expr(&Expr::Cond(
                Box::new(Expr::LitB(true)),
                Box::new(Expr::LitS("a")),
                Box::new(Expr::LitS(""))
            )),
            vec![9, 2, 1, 0, 1, 0, 0, 0, b'a', 0, 0, 0, 0, 0]
        );
        assert_eq!(
            bytes_of_expr(&Expr::Field(Box::new(Expr::Ref("x")), "id")),
            vec![10, 3, 1, 0, 0, 0, b'x', 2, 0, 0, 0, b'i', b'd']
        );
    }

    #[test]
    fn expr_bin_op_tags_are_pinned() {
        let cases: &[(BinOp, u8)] = &[
            (BinOp::Add, 0),
            (BinOp::Sub, 1),
            (BinOp::Mul, 2),
            (BinOp::Div, 3),
            (BinOp::Eq, 4),
            (BinOp::Ne, 5),
            (BinOp::Lt, 6),
            (BinOp::Le, 7),
            (BinOp::Gt, 8),
            (BinOp::Ge, 9),
            (BinOp::And, 10),
            (BinOp::Or, 11),
            (BinOp::Mod, 12),
        ];
        for (op, tag) in cases {
            assert_eq!(
                bytes_of_expr(&Expr::Bin(
                    *op,
                    Box::new(Expr::LitF(1.0)),
                    Box::new(Expr::LitF(2.0))
                )),
                vec![6, *tag, 1, 0, 0, 0x80, 0x3F, 1, 0, 0, 0, 0x40],
                "BinOp {op:?} 的标签必须是 {tag}"
            );
        }
    }

    #[test]
    fn classtok_and_optional_fields_pin_exact_bytes() {
        assert_eq!(bytes_of_tok(&ClassTok::Lit("a")), vec![0, 1, 0, 0, 0, b'a']);
        assert_eq!(
            bytes_of_tok(&ClassTok::If {
                cond: Expr::LitB(true),
                tok: "t",
            }),
            vec![1, 2, 1, 1, 0, 0, 0, b't']
        );
        assert_eq!(
            bytes_of_tok(&ClassTok::Dyn(Expr::Ref("x"))),
            vec![2, 3, 1, 0, 0, 0, b'x']
        );

        let mut w = Writer::default();
        opt_expr(&mut w, &None);
        opt_expr(&mut w, &Some(Expr::LitB(false)));
        opt_str(&mut w, None);
        opt_str(&mut w, Some("x"));
        assert_eq!(w.out, vec![0, 1, 2, 0, 0, 1, 1, 0, 0, 0, b'x']);
    }

    #[test]
    fn node_el_tag_codes_and_empty_shape_are_pinned() {
        for (tag, code) in [
            (NodeTag::Text, 0u8),
            (NodeTag::Image, 1),
            (NodeTag::Box, 2),
            (NodeTag::Control, 3),
        ] {
            let n = Node::El(ElementNode {
                tag,
                key: None,
                classes: vec![],
                attrs: vec![],
                children: vec![],
            });
            assert_eq!(
                bytes_of_node(&n),
                vec![0, code, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                "NodeTag {tag:?} 的标签必须是 {code}"
            );
        }
    }

    #[test]
    fn node_el_with_key_classes_attrs_and_children() {
        let n = Node::El(ElementNode {
            tag: NodeTag::Box,
            key: Some(Expr::LitS("k")),
            classes: vec![ClassTok::Lit("a")],
            attrs: vec![("x", Expr::LitB(true))],
            children: vec![Node::Slot],
        });
        assert_eq!(
            bytes_of_node(&n),
            vec![
                0, 2, // El, Box
                1, 0, 1, 0, 0, 0, b'k', // opt key = LitS("k")
                1, 0, 0, 0, 0, 1, 0, 0, 0, b'a', // 一个 classtok = Lit("a")
                1, 0, 0, 0, 1, 0, 0, 0, b'x', 2, 1, // 一个属性 ("x", LitB(true))
                1, 0, 0, 0, 4, // 一个子节点 = Slot
            ]
        );
    }

    #[test]
    fn node_if_for_use_and_slot_pin_exact_bytes() {
        let n = Node::If(IfNode {
            cond: Expr::LitB(true),
            then: vec![Node::Slot],
            els: vec![],
        });
        assert_eq!(bytes_of_node(&n), vec![1, 2, 1, 1, 0, 0, 0, 4, 0, 0, 0, 0]);

        let n = Node::For(ForNode {
            var: "i",
            index: Some("ix"),
            track_key: Some(Expr::Ref("k")),
            list: Expr::Binding("items"),
            body: vec![Node::Slot],
        });
        assert_eq!(
            bytes_of_node(&n),
            vec![
                2, 1, 0, 0, 0, b'i', // var
                1, 2, 0, 0, 0, b'i', b'x', // opt index
                1, 3, 1, 0, 0, 0, b'k', // opt track_key = Ref("k")
                4, 5, 0, 0, 0, b'i', b't', b'e', b'm', b's', // list = Binding("items")
                1, 0, 0, 0, 4, // body: 一个 Slot
            ]
        );

        let n = Node::Use(UseNode {
            comp: "C",
            key: None,
            class_extra: vec![],
            args: vec![],
            children: vec![],
        });
        assert_eq!(
            bytes_of_node(&n),
            vec![3, 1, 0, 0, 0, b'C', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(bytes_of_node(&Node::Slot), vec![4]);
    }

    #[test]
    fn encode_empty_artifact_pins_magic_and_counts() {
        let raw = RawUi {
            components: vec![],
            pages: vec![],
        };
        assert_eq!(
            encode(&raw),
            vec![b'Z', b'Y', b'P', b'1', 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn encode_one_component_and_one_page_pins_full_layout() {
        let raw = RawUi {
            components: vec![Component {
                name: "C",
                params: vec!["a"],
                body: vec![],
            }],
            pages: vec![Page {
                name: "p",
                root: vec![Node::Slot],
            }],
        };
        assert_eq!(
            encode(&raw),
            vec![
                b'Z', b'Y', b'P', b'1', // MAGIC
                1, 0, 0, 0, // 一个组件
                1, 0, 0, 0, b'C', // 组件名
                1, 0, 0, 0, 1, 0, 0, 0, b'a', // 一个参数 "a"
                0, 0, 0, 0, // 空模板体
                1, 0, 0, 0, // 一个页面
                1, 0, 0, 0, b'p', // 页名
                1, 0, 0, 0, 4, // 一个根节点 = Slot
            ]
        );
    }
}
