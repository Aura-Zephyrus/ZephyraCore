//! 表达式文本 → [`Expr`] AST。
//!
//! 语法（见 `zypui::expr` 的词汇表）：
//!
//! ```text
//! or    := and ("||" and)*
//! and   := cmp ("&&" cmp)*
//! cmp   := add (("=="|"!="|"<"|"<="|">"|">=") add)?
//! add   := mul (("+"|"-") mul)*
//! mul   := unary (("*"|"/"|"%") unary)*
//! unary := ("!"|"-") unary | postfix
//! postfix := primary ("." ident)*        —— 记录字段访问（item.id）
//! primary := num | str | true | false | ident | @ident | call | "(" or ")"
//! ```
//!
//! 属性值里的 `{…}` 段先由 `zypuic::build` 剥出来，这里只看纯表达式文本。
//! 标识符的**语义**校验（是不是已知参数/内置词）不在这层 —— 词法层只认形状。

use zypui::expr::{BinOp, Expr};

use crate::xml::Diag;

/// 解析表达式。`line`/`col` 是表达式在源文件里的起点（报错定位用）。
pub fn parse_expr(src: &str, file: &'static str, line: u32, col: u32) -> Result<Expr, Diag> {
    let mut p = ExprParser {
        s: src.trim(),
        file,
        line,
        col,
        i: 0,
    };
    let e = p.ternary()?;
    p.skip_ws();
    if p.i < p.s.len() {
        return Err(p.err_here(format!("表达式后有多余内容 `{}`", &p.s[p.i..])));
    }
    Ok(e)
}

struct ExprParser<'a> {
    s: &'a str,
    file: &'static str,
    line: u32,
    col: u32,
    i: usize,
}

impl<'a> ExprParser<'a> {
    /// 列号偏移：起点列 + 当前偏移（按字符数近似，报错定位够用）。
    fn col_at(&self) -> u32 {
        self.col + self.s[..self.i].chars().count() as u32
    }

    fn err_here(&self, msg: impl Into<String>) -> Diag {
        Diag::error(self.file, self.line, self.col_at(), msg)
    }

    fn skip_ws(&mut self) {
        while self.i < self.s.len() && (self.s.as_bytes()[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn eat(&mut self, tok: &str) -> bool {
        self.skip_ws();
        if self.s[self.i..].starts_with(tok) {
            // `|` 不能吃掉 `||` 的一半；`=` 不能吃掉 `==` 的头。
            let nxt = self.s.as_bytes().get(self.i + tok.len()).copied();
            let clash = matches!(
                (tok, nxt),
                ("|", Some(b'|')) | ("=", Some(b'=')) | ("<", Some(b'=')) | (">", Some(b'='))
            );
            if !clash {
                self.i += tok.len();
                return true;
            }
        }
        false
    }

    fn ident(&mut self) -> Option<String> {
        self.skip_ws();
        let b = self.s.as_bytes();
        let start = self.i;
        if self.i < b.len() && (b[self.i].is_ascii_alphabetic() || b[self.i] == b'_') {
            self.i += 1;
            while self.i < b.len() && (b[self.i].is_ascii_alphanumeric() || b[self.i] == b'_') {
                self.i += 1;
            }
            Some(self.s[start..self.i].to_string())
        } else {
            None
        }
    }

    /// 条件式 `cond ? a : b`（右结合，优先级低于 `||`）。
    ///
    /// **单臂合法**：`hover ? 'hovered'` ≡ `hover ? 'hovered' : ''` ——
    /// 条件分类名是最高频场景，强制写全空串分支是纯噪音。降级成空串后，
    /// 类名的 If 形降级（`build::class_tokens`）与真值语义（空串为假）都自然成立。
    fn ternary(&mut self) -> Result<Expr, Diag> {
        let c = self.or_expr()?;
        self.skip_ws();
        if self.eat("?") {
            let a = self.ternary()?;
            self.skip_ws();
            if self.eat(":") {
                let b = self.ternary()?;
                return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
            }
            // 单臂：else 分支 = 空串。
            return Ok(Expr::Cond(
                Box::new(c),
                Box::new(a),
                Box::new(Expr::LitS("")),
            ));
        }
        Ok(c)
    }

    fn or_expr(&mut self) -> Result<Expr, Diag> {
        let mut l = self.and_expr()?;
        while self.eat("||") {
            let r = self.and_expr()?;
            l = Expr::Bin(BinOp::Or, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> Result<Expr, Diag> {
        let mut l = self.cmp_expr()?;
        while self.eat("&&") {
            let r = self.cmp_expr()?;
            l = Expr::Bin(BinOp::And, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn cmp_expr(&mut self) -> Result<Expr, Diag> {
        let l = self.add_expr()?;
        let op = if self.eat("==") {
            BinOp::Eq
        } else if self.eat("!=") {
            BinOp::Ne
        } else if self.eat("<=") {
            BinOp::Le
        } else if self.eat(">=") {
            BinOp::Ge
        } else if self.eat("<") {
            BinOp::Lt
        } else if self.eat(">") {
            BinOp::Gt
        } else {
            return Ok(l);
        };
        let r = self.add_expr()?;
        Ok(Expr::Bin(op, Box::new(l), Box::new(r)))
    }

    fn add_expr(&mut self) -> Result<Expr, Diag> {
        let mut l = self.mul_expr()?;
        loop {
            if self.eat("+") {
                let r = self.mul_expr()?;
                l = Expr::Bin(BinOp::Add, Box::new(l), Box::new(r));
            } else if self.eat("-") {
                let r = self.mul_expr()?;
                l = Expr::Bin(BinOp::Sub, Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    fn mul_expr(&mut self) -> Result<Expr, Diag> {
        let mut l = self.unary()?;
        loop {
            if self.eat("*") {
                let r = self.unary()?;
                l = Expr::Bin(BinOp::Mul, Box::new(l), Box::new(r));
            } else if self.eat("/") {
                let r = self.unary()?;
                l = Expr::Bin(BinOp::Div, Box::new(l), Box::new(r));
            } else if self.eat("%") {
                let r = self.unary()?;
                l = Expr::Bin(BinOp::Mod, Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    fn unary(&mut self) -> Result<Expr, Diag> {
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    /// `primary ("." ident)*`：记录字段访问。字段名不做语义校验（记录形状
    /// 在宿主手里，运行期取不到降级为假）。
    fn postfix(&mut self) -> Result<Expr, Diag> {
        let mut e = self.primary()?;
        loop {
            self.skip_ws();
            if self.peek() == Some(b'.')
                && self
                    .s
                    .as_bytes()
                    .get(self.i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            {
                self.i += 1;
                let name = self
                    .ident()
                    .ok_or_else(|| self.err_here("`.` 后面要跟字段名"))?;
                e = Expr::Field(Box::new(e), leak(&name));
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, Diag> {
        self.skip_ws();
        let Some(c) = self.peek() else {
            return Err(self.err_here("表达式提前结束"));
        };
        match c {
            b'(' => {
                self.i += 1;
                let e = self.ternary()?;
                self.skip_ws();
                if !self.eat(")") {
                    return Err(self.err_here("缺闭合括号 `)`"));
                }
                Ok(e)
            }
            b'\'' | b'"' => self.string_lit(c),
            b'0'..=b'9' => {
                let start = self.i;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
                if self.peek() == Some(b'.') {
                    self.i += 1;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.i += 1;
                    }
                }
                let text = &self.s[start..self.i];
                let v: f32 = text
                    .parse()
                    .map_err(|_| self.err_here(format!("数字字面量 `{text}` 解析不了")))?;
                Ok(Expr::LitF(v))
            }
            b'@' => {
                self.i += 1;
                let name = self
                    .ident()
                    .ok_or_else(|| self.err_here("`@` 后面要跟绑定名"))?;
                Ok(Expr::Binding(leak(&name)))
            }
            _ => {
                let Some(name) = self.ident() else {
                    return Err(self.err_here(format!("意外字符 `{}`", char_at(self.s, self.i))));
                };
                // 函数调用。
                self.skip_ws();
                if self.peek() == Some(b'(') {
                    self.i += 1;
                    let mut args = Vec::new();
                    self.skip_ws();
                    if self.peek() != Some(b')') {
                        loop {
                            args.push(self.ternary()?);
                            self.skip_ws();
                            if self.eat(",") {
                                continue;
                            }
                            break;
                        }
                    }
                    if !self.eat(")") {
                        return Err(self.err_here("调用缺闭合括号 `)`"));
                    }
                    return Ok(Expr::Call(leak(&name), args));
                }
                match name.as_str() {
                    "true" => Ok(Expr::LitB(true)),
                    "false" => Ok(Expr::LitB(false)),
                    _ => Ok(Expr::Ref(leak(&name))),
                }
            }
        }
    }

    /// 字符串字面量：`'…'` 或 `"…"`，支持 `\\` `\'` `\"` `\n` `\t`。
    fn string_lit(&mut self, quote: u8) -> Result<Expr, Diag> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err_here("字符串没有闭合引号"));
            };
            self.i += 1;
            match c {
                q if q == quote => return Ok(Expr::LitS(leak(&out))),
                b'\\' => {
                    let Some(esc) = self.peek() else {
                        return Err(self.err_here("转义符后面没有字符"));
                    };
                    self.i += 1;
                    match esc {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'\\' => out.push('\\'),
                        b'\'' => out.push('\''),
                        b'"' => out.push('"'),
                        other => {
                            return Err(self.err_here(format!("不认识的转义 `\\{}`", other as char)))
                        }
                    }
                }
                _ => {
                    // 按 UTF-8 整字符拷贝（多字节字符的后续字节原样接上）。
                    let b = self.s.as_bytes();
                    let start = self.i - 1;
                    let mut end = self.i;
                    while end < b.len() && (b[end] & 0xC0) == 0x80 {
                        end += 1;
                    }
                    out.push_str(&self.s[start..end]);
                    self.i = end;
                }
            }
        }
    }
}

/// 解析产物里的标识符直接泄漏成 `&'static`（加载期一次，IR 全程 `&'static`）。
fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn char_at(s: &str, i: usize) -> String {
    s[i..]
        .chars()
        .next()
        .map(|c| c.to_string())
        .unwrap_or_default()
}

// ============================== 直接单测（解析器各层） ==============================

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 `parse_expr` 同样的起点（第 1 行第 1 列），但直接拿到私有 parser 调各层。
    fn parser(s: &str) -> ExprParser<'_> {
        ExprParser {
            s,
            file: "t.zypui",
            line: 1,
            col: 1,
            i: 0,
        }
    }

    fn e1(v: f32) -> Box<Expr> {
        Box::new(Expr::LitF(v))
    }

    #[test]
    fn parse_expr_pins_literals_references_and_bindings() {
        assert_eq!(parse_expr("42", "t.zypui", 1, 1), Ok(Expr::LitF(42.0)));
        assert_eq!(parse_expr("3.5", "t.zypui", 1, 1), Ok(Expr::LitF(3.5)));
        assert_eq!(parse_expr("true", "t.zypui", 1, 1), Ok(Expr::LitB(true)));
        assert_eq!(parse_expr("false", "t.zypui", 1, 1), Ok(Expr::LitB(false)));
        assert_eq!(parse_expr("name", "t.zypui", 1, 1), Ok(Expr::Ref("name")));
        assert_eq!(parse_expr("@n", "t.zypui", 1, 1), Ok(Expr::Binding("n")));
        // 首尾空白被 trim 掉，结构不受影响。
        assert_eq!(parse_expr("  42  ", "t.zypui", 1, 1), Ok(Expr::LitF(42.0)));
    }

    #[test]
    fn parse_expr_pins_precedence_and_unary() {
        // 1 + 2 * 3
        assert_eq!(
            parse_expr("1 + 2 * 3", "t.zypui", 1, 1),
            Ok(Expr::Bin(
                BinOp::Add,
                e1(1.0),
                Box::new(Expr::Bin(BinOp::Mul, e1(2.0), e1(3.0)))
            ))
        );
        // a && b || c
        assert_eq!(
            parse_expr("a && b || c", "t.zypui", 1, 1),
            Ok(Expr::Bin(
                BinOp::Or,
                Box::new(Expr::Bin(
                    BinOp::And,
                    Box::new(Expr::Ref("a")),
                    Box::new(Expr::Ref("b"))
                )),
                Box::new(Expr::Ref("c"))
            ))
        );
        assert_eq!(
            parse_expr("!flag", "t.zypui", 1, 1),
            Ok(Expr::Not(Box::new(Expr::Ref("flag"))))
        );
        assert_eq!(parse_expr("-1", "t.zypui", 1, 1), Ok(Expr::Neg(e1(1.0))));
        assert_eq!(
            parse_expr("!!x", "t.zypui", 1, 1),
            Ok(Expr::Not(Box::new(Expr::Not(Box::new(Expr::Ref("x"))))))
        );
    }

    #[test]
    fn parse_expr_pins_all_comparison_ops() {
        for (src, op) in [
            ("1 == 2", BinOp::Eq),
            ("1 != 2", BinOp::Ne),
            ("1 < 2", BinOp::Lt),
            ("1 <= 2", BinOp::Le),
            ("1 > 2", BinOp::Gt),
            ("1 >= 2", BinOp::Ge),
            ("1 + 2", BinOp::Add),
            ("1 - 2", BinOp::Sub),
            ("1 * 2", BinOp::Mul),
            ("1 / 2", BinOp::Div),
            ("1 % 2", BinOp::Mod),
        ] {
            assert_eq!(
                parse_expr(src, "t.zypui", 1, 1),
                Ok(Expr::Bin(op, e1(1.0), e1(2.0))),
                "表达式 {src:?}"
            );
        }
    }

    #[test]
    fn parse_expr_pins_calls_fields_parentheses_and_ternary() {
        assert_eq!(
            parse_expr("tr('k')", "t.zypui", 1, 1),
            Ok(Expr::Call("tr", vec![Expr::LitS("k")]))
        );
        assert_eq!(
            parse_expr("item.id.name", "t.zypui", 1, 1),
            Ok(Expr::Field(
                Box::new(Expr::Field(Box::new(Expr::Ref("item")), "id")),
                "name"
            ))
        );
        assert_eq!(
            parse_expr("(1 + 2)", "t.zypui", 1, 1),
            Ok(Expr::Bin(BinOp::Add, e1(1.0), e1(2.0)))
        );
        assert_eq!(
            parse_expr("a ? b : c", "t.zypui", 1, 1),
            Ok(Expr::Cond(
                Box::new(Expr::Ref("a")),
                Box::new(Expr::Ref("b")),
                Box::new(Expr::Ref("c"))
            ))
        );
        // 单臂：else 降级为空串。
        assert_eq!(
            parse_expr("hover ? 'hovered'", "t.zypui", 1, 1),
            Ok(Expr::Cond(
                Box::new(Expr::Ref("hover")),
                Box::new(Expr::LitS("hovered")),
                Box::new(Expr::LitS(""))
            ))
        );
    }

    #[test]
    fn parse_expr_errors_pin_exact_diagnostics() {
        assert_eq!(
            parse_expr("", "t.zypui", 1, 1),
            Err(Diag::error("t.zypui", 1, 1, "表达式提前结束"))
        );
        assert_eq!(
            parse_expr("@", "t.zypui", 1, 1),
            Err(Diag::error("t.zypui", 1, 2, "`@` 后面要跟绑定名"))
        );
        assert_eq!(
            parse_expr("1 +", "t.zypui", 1, 1),
            Err(Diag::error("t.zypui", 1, 4, "表达式提前结束"))
        );
        assert_eq!(
            parse_expr("a b", "t.zypui", 1, 1),
            Err(Diag::error("t.zypui", 1, 3, "表达式后有多余内容 `b`"))
        );
        assert_eq!(
            parse_expr("#", "t.zypui", 5, 10),
            Err(Diag::error("t.zypui", 5, 10, "意外字符 `#`"))
        );
        assert_eq!(
            parse_expr("(1", "t.zypui", 1, 1),
            Err(Diag::error("t.zypui", 1, 3, "缺闭合括号 `)`"))
        );
        // 调用者给的起点列必须进入诊断。
        assert_eq!(
            parse_expr("@", "t.zypui", 9, 7),
            Err(Diag::error("t.zypui", 9, 8, "`@` 后面要跟绑定名"))
        );
    }

    #[test]
    fn parser_primary_constructs_each_form() {
        assert_eq!(parser("42").primary(), Ok(Expr::LitF(42.0)));
        assert_eq!(parser("3.5").primary(), Ok(Expr::LitF(3.5)));
        assert_eq!(parser("true").primary(), Ok(Expr::LitB(true)));
        assert_eq!(parser("false").primary(), Ok(Expr::LitB(false)));
        assert_eq!(parser("foo").primary(), Ok(Expr::Ref("foo")));
        assert_eq!(parser("@foo").primary(), Ok(Expr::Binding("foo")));
        assert_eq!(
            parser("tr('k')").primary(),
            Ok(Expr::Call("tr", vec![Expr::LitS("k")]))
        );
        assert_eq!(
            parser("f(1,2)").primary(),
            Ok(Expr::Call("f", vec![Expr::LitF(1.0), Expr::LitF(2.0)]))
        );
        assert_eq!(parser("f()").primary(), Ok(Expr::Call("f", vec![])));
        assert_eq!(
            parser("(1 + 2)").primary(),
            Ok(Expr::Bin(BinOp::Add, e1(1.0), e1(2.0)))
        );
    }

    #[test]
    fn parser_primary_errors_are_pinned() {
        assert_eq!(
            parser("").primary(),
            Err(Diag::error("t.zypui", 1, 1, "表达式提前结束"))
        );
        assert_eq!(
            parser("#").primary(),
            Err(Diag::error("t.zypui", 1, 1, "意外字符 `#`"))
        );
        assert_eq!(
            parser(".5").primary(),
            Err(Diag::error("t.zypui", 1, 1, "意外字符 `.`"))
        );
        assert_eq!(
            parser("@").primary(),
            Err(Diag::error("t.zypui", 1, 2, "`@` 后面要跟绑定名"))
        );
        assert_eq!(
            parser("(1").primary(),
            Err(Diag::error("t.zypui", 1, 3, "缺闭合括号 `)`"))
        );
        assert_eq!(
            parser("f(1,)").primary(),
            Err(Diag::error("t.zypui", 1, 5, "意外字符 `)`"))
        );
    }

    #[test]
    fn parser_postfix_records_field_chains() {
        assert_eq!(parser("x").postfix(), Ok(Expr::Ref("x")));
        assert_eq!(
            parser("item.id").postfix(),
            Ok(Expr::Field(Box::new(Expr::Ref("item")), "id"))
        );
        assert_eq!(
            parser("item.id.name").postfix(),
            Ok(Expr::Field(
                Box::new(Expr::Field(Box::new(Expr::Ref("item")), "id")),
                "name"
            ))
        );
        // 点号前允许空白；点号后必须紧跟字段名。
        assert_eq!(
            parser("item .id").postfix(),
            Ok(Expr::Field(Box::new(Expr::Ref("item")), "id"))
        );
        // 悬空的点不被消费，留给 parse_expr 的「多余内容」报错。
        let mut p = parser("item.");
        assert_eq!(p.postfix(), Ok(Expr::Ref("item")));
        assert_eq!(p.i, 4);
        assert_eq!(
            parse_expr("item.", "t.zypui", 1, 1),
            Err(Diag::error("t.zypui", 1, 5, "表达式后有多余内容 `.`"))
        );
    }

    #[test]
    fn parser_ternary_full_single_arm_and_right_assoc() {
        assert_eq!(
            parser("a ? b : c").ternary(),
            Ok(Expr::Cond(
                Box::new(Expr::Ref("a")),
                Box::new(Expr::Ref("b")),
                Box::new(Expr::Ref("c"))
            ))
        );
        assert_eq!(
            parser("hover ? 'hovered'").ternary(),
            Ok(Expr::Cond(
                Box::new(Expr::Ref("hover")),
                Box::new(Expr::LitS("hovered")),
                Box::new(Expr::LitS(""))
            ))
        );
        // 右结合：a ? b : (c ? d : e)
        assert_eq!(
            parser("a ? b : c ? d : e").ternary(),
            Ok(Expr::Cond(
                Box::new(Expr::Ref("a")),
                Box::new(Expr::Ref("b")),
                Box::new(Expr::Cond(
                    Box::new(Expr::Ref("c")),
                    Box::new(Expr::Ref("d")),
                    Box::new(Expr::Ref("e"))
                ))
            ))
        );
        // 真臂也是三元：a ? (b ? c : d) : e
        assert_eq!(
            parser("a ? b ? c : d : e").ternary(),
            Ok(Expr::Cond(
                Box::new(Expr::Ref("a")),
                Box::new(Expr::Cond(
                    Box::new(Expr::Ref("b")),
                    Box::new(Expr::Ref("c")),
                    Box::new(Expr::Ref("d"))
                )),
                Box::new(Expr::Ref("e"))
            ))
        );
    }

    #[test]
    fn parser_string_lit_decodes_every_escape() {
        assert_eq!(
            parser(r#"'a\nb'"#).string_lit(b'\''),
            Ok(Expr::LitS("a\nb"))
        );
        assert_eq!(
            parser(r#"'a\tb'"#).string_lit(b'\''),
            Ok(Expr::LitS("a\tb"))
        );
        assert_eq!(
            parser(r#"'a\\b'"#).string_lit(b'\''),
            Ok(Expr::LitS("a\\b"))
        );
        assert_eq!(parser(r#"'a\'b'"#).string_lit(b'\''), Ok(Expr::LitS("a'b")));
        assert_eq!(parser(r#""a\"b""#).string_lit(b'"'), Ok(Expr::LitS("a\"b")));
        assert_eq!(parser("''").string_lit(b'\''), Ok(Expr::LitS("")));
        // 多字节 UTF-8 按整字符收。
        assert_eq!(parser("'世界'").string_lit(b'\''), Ok(Expr::LitS("世界")));
    }

    #[test]
    fn parser_string_lit_errors_are_pinned() {
        assert_eq!(
            parser("'abc").string_lit(b'\''),
            Err(Diag::error("t.zypui", 1, 5, "字符串没有闭合引号"))
        );
        assert_eq!(
            parser(r#"'a\q'"#).string_lit(b'\''),
            Err(Diag::error("t.zypui", 1, 5, "不认识的转义 `\\q`"))
        );
        assert_eq!(
            parser(r#"'a\"#).string_lit(b'\''),
            Err(Diag::error("t.zypui", 1, 4, "转义符后面没有字符"))
        );
    }

    #[test]
    fn leak_and_char_at_edge_cases() {
        let s: &'static str = leak("abc");
        assert_eq!(s, "abc");
        assert_eq!(char_at("abc", 0), "a");
        assert_eq!(char_at("abc", 2), "c");
        assert_eq!(char_at("世界", 0), "世");
        assert_eq!(char_at("世界", 3), "界");
        assert_eq!(char_at("abc", 3), "");
        assert_eq!(char_at("", 0), "");
    }
}
