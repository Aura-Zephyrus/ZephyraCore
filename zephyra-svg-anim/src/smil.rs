//! SVG SMIL 动画解析：从 SVG 文本里抽出 `<animate>` / `<animateTransform>` /
//! `<animateMotion>`，挂到对应的 `<path>` 上。
//!
//! # 与图标解析器的关系
//!
//! `zephyra_vector::icon::parse_svg` 只关心静态图标（viewBox + path），本模块在它
//! 之上补 SMIL —— 两段解析共用一套「不做完整 XML、只做鲁棒属性提取」的约定，
//! 所以 `<title>` / 注释 / 命名空间声明都不会干扰动画属性。
//!
//! # 支持范围
//!
//! - `<animate attributeName="opacity|fill|d">`
//! - `<animateTransform type="translate|scale|rotate">`
//! - `<animateMotion path="...">`
//! - 时间属性：`begin` / `dur` / `repeatCount` / `fill` / `calcMode` / `keyTimes` / `keySplines`
//! - 值属性：`values` / `from` / `to` / `by`
//!
//! 不识别的 `<animate>`（例如 `attributeName` 未知）被忽略而不是报错 ——
//! 一个 SVG 里有无关动画不该让整个图标加载失败。

use zephyra_bezier::Bezier;
use zephyra_vector::icon::{parse_path_data, parse_svg, FillRule, PaintMode};

use crate::types::*;

/// `#rgb` / `#rrggbb` / `#rrggbbaa` → RGBA(0..1)。
fn parse_hex_rgba(s: &str) -> Option<[f32; 4]> {
    let hex = s.strip_prefix('#')?;
    let ch = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok();
    match hex.len() {
        3 => Some([
            ch(0, 1)? as f32 / 15.0,
            ch(1, 1)? as f32 / 15.0,
            ch(2, 1)? as f32 / 15.0,
            1.0,
        ]),
        6 => Some([
            ch(0, 2)? as f32 / 255.0,
            ch(2, 2)? as f32 / 255.0,
            ch(4, 2)? as f32 / 255.0,
            1.0,
        ]),
        8 => Some([
            ch(0, 2)? as f32 / 255.0,
            ch(2, 2)? as f32 / 255.0,
            ch(4, 2)? as f32 / 255.0,
            ch(6, 2)? as f32 / 255.0,
        ]),
        _ => None,
    }
}

/// 解析带 SMIL 动画的 SVG 文本。
pub fn parse_animated_svg(text: &str) -> Result<AnimatedSvg, SvgAnimError> {
    let icon = parse_svg(text).map_err(|e| match e {
        zephyra_vector::icon::SvgError::NoSvgRoot => SvgAnimError::NoSvgRoot,
        zephyra_vector::icon::SvgError::NoViewBox => SvgAnimError::NoViewBox,
        zephyra_vector::icon::SvgError::BadViewBox => SvgAnimError::BadViewBox,
        zephyra_vector::icon::SvgError::NoPath => SvgAnimError::NoPath,
    })?;

    // 把 SVG 切成「一个 <path> 一段」，每段里找它自己的动画子元素。
    // <animate*> 必须出现在它要作用的那条 <path> 之后、下一条 <path> 之前。
    let mut paths: Vec<AnimatedPath> = Vec::new();
    let mut rest = text;
    // 跳过 <svg ...>，从第一个 <path 开始逐段处理。
    if let Some((_, after)) = next_tag(rest, "<svg") {
        rest = after;
    } else {
        return Err(SvgAnimError::NoSvgRoot);
    }

    while let Some((path_attrs, after_path)) = next_tag(rest, "<path") {
        rest = after_path;
        let paint_mode = if path_attrs.contains("fill=\"none\"") {
            PaintMode::None
        } else {
            // 解析 path 上的具体 fill 颜色（如 <set> 的基值来源）。
            attr(path_attrs, "fill")
                .and_then(parse_hex_rgba)
                .map(PaintMode::Solid)
                .unwrap_or(PaintMode::CurrentColor)
        };
        let fill_rule = if path_attrs.contains("fill-rule=\"evenodd\"")
            || path_attrs.contains("fill-rule='evenodd'")
        {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        let base = attr(path_attrs, "d")
            .and_then(|d| parse_path_data(d).ok())
            .unwrap_or_default();

        // 收集紧跟这条 <path> 的动画元素（直到下一条 <path>）。
        let mut animations = Vec::new();
        let mut anim_rest = rest;
        loop {
            let mut found: Option<(&str, &str, &str)> = None; // (tag, attrs, after)
            for tag in [
                "<animateMotion",
                "<animateTransform",
                "<animateColor",
                "<set",
                "<animate",
            ] {
                if let Some((attrs, after)) = next_tag(anim_rest, tag) {
                    // <animate> 是 <animateTransform> / <animateMotion> 的前缀：
                    // 若更长的标签在同一位置命中，就跳过短的那个。
                    if tag == "<animate" {
                        let shadowed = ["<animateMotion", "<animateTransform"]
                            .iter()
                            .any(|long| next_tag(anim_rest, long).map(|(_, a)| a) == Some(after));
                        if shadowed {
                            continue;
                        }
                    }
                    // 越靠前的匹配，after 越长 —— 选它。
                    if found.is_none() || after.len() > found.unwrap().2.len() {
                        found = Some((tag, attrs, after));
                    }
                }
            }
            // 先处理动画元素本身，<mpath> 是下一轮要处理的子元素。
            match found {
                Some((tag, attrs, after)) => {
                    if let Some(anim) = parse_animation(tag, attrs, &base) {
                        animations.push(anim);
                    }
                    anim_rest = after;
                    // <mpath> 是 <animateMotion> 的子元素：紧跟其后，记到刚加的
                    // motion 动画上，然后跳过它。
                    if tag == "<animateMotion" {
                        if let Some((mattrs, after_m)) = next_tag(anim_rest, "<mpath") {
                            let href = attr(mattrs, "href")
                                .or_else(|| attr(mattrs, "xlink:href"))
                                .map(|s| s.to_string());
                            if let Some(h) = href {
                                for a in animations.iter_mut().rev() {
                                    if let Animation::Motion(m) = a {
                                        m.mpath_href = Some(h.clone());
                                        break;
                                    }
                                }
                            }
                            anim_rest = after_m;
                        }
                    }
                }
                None => break,
            }
        }

        paths.push(AnimatedPath {
            base,
            paint_mode,
            fill_rule,
            animations,
        });
    }

    // <mpath xlink:href="#id"> 子元素：当前解析器只在 <animateMotion> 属性上
    // 记了 mpath_href。这里扫文档里带 id 的 <path>，把引用解析成实际路径段。
    // （<mpath> 作为子元素时，其属性会被 next_tag 跳过，所以从全文里搜 id。）
    let mut id_paths: std::collections::HashMap<String, Vec<Segment>> =
        std::collections::HashMap::new();
    let mut scan = text;
    while let Some((attrs, after)) = next_tag(scan, "<path") {
        scan = after;
        if let Some(id) = attr(attrs, "id") {
            if let Some(d) = attr(attrs, "d") {
                if let Ok(segs) = parse_path_data(d) {
                    id_paths.insert(id.to_string(), segs);
                }
            }
        }
    }
    for path in &mut paths {
        for anim in &mut path.animations {
            if let Animation::Motion(m) = anim {
                if let Some(href) = &m.mpath_href {
                    let id = href.trim_start_matches('#');
                    if let Some(segs) = id_paths.get(id) {
                        m.path = segs.clone();
                    }
                }
            }
        }
    }

    if paths.is_empty() {
        return Err(SvgAnimError::NoPath);
    }
    Ok(AnimatedSvg {
        view_box: icon.view_box,
        paths,
    })
}

fn parse_animation(tag: &str, attrs: &str, base: &[Segment]) -> Option<Animation> {
    // <set> 用 dur 但不一定有 values，单独解析时间。
    if tag == "<set" {
        let timing = parse_timing(attrs)?;
        let attribute_name = attr(attrs, "attributeName")?.to_string();
        let to = attr(attrs, "to")?.to_string();
        return Some(Animation::Set(SetAnim {
            timing,
            attribute_name,
            to,
        }));
    }
    let timing = parse_timing(attrs)?;
    match tag {
        // 废弃但仍常见的 <animateColor>：与 fill 动画同义。
        "<animateColor" => {
            let values = parse_color_values(attrs)?;
            Some(Animation::Fill(FillAnim { timing, values }))
        }
        "<animateMotion" => {
            let path = attr(attrs, "path")
                .and_then(|d| parse_path_data(d).ok())
                .unwrap_or_default();
            // 注意：path 可能为空 —— SMIL 允许用 <mpath> 子元素引用路径，
            // 那时真正的 path 在后面的 mpath 解析阶段填入。
            let key_points = attr(attrs, "keyPoints")
                .map(parse_f32_list)
                .unwrap_or_default();
            Some(Animation::Motion(MotionAnim {
                timing,
                path,
                rotate_mode: match attr(attrs, "rotate") {
                    Some("auto") => MotionRotate::Auto,
                    Some("auto-reverse") => MotionRotate::AutoReverse,
                    Some(v) => v
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .map(MotionRotate::FixedAngle)
                        .unwrap_or(MotionRotate::None),
                    None => MotionRotate::None,
                },
                mpath_href: attr(attrs, "mpath")
                    .or_else(|| attr(attrs, "href"))
                    .map(|s| s.to_string()),
                origin: attr(attrs, "origin")
                    .map(parse_f32_list)
                    .filter(|v| v.len() >= 2)
                    .map(|v| (v[0], v[1]))
                    .unwrap_or((0.0, 0.0)),
                key_points,
            }))
        }
        "<animateTransform" => {
            let kind = match attr(attrs, "type")? {
                "translate" => TransformKind::Translate,
                "scale" => TransformKind::Scale,
                "rotate" => TransformKind::Rotate,
                "skewX" => TransformKind::SkewX,
                "skewY" => TransformKind::SkewY,
                _ => return None,
            };
            let values = parse_f32x3_values(attrs, &[0.0, 0.0, 0.0])?;
            Some(Animation::Transform(TransformAnim {
                timing,
                kind,
                values,
            }))
        }
        "<animate" => {
            let name = attr(attrs, "attributeName")?;
            match name {
                "opacity" => {
                    let values = parse_f32_values(attrs, 1.0)?;
                    Some(Animation::Opacity(OpacityAnim { timing, values }))
                }
                "fill" => {
                    let values = parse_color_values(attrs)?;
                    Some(Animation::Fill(FillAnim { timing, values }))
                }
                "d" => {
                    let values = parse_path_values(attrs, base)?;
                    Some(Animation::Path(PathAnim { timing, values }))
                }
                "stroke" => {
                    // stroke 可以是颜色，也可能是 `none`；只处理颜色。
                    let values = parse_color_values(attrs)?;
                    Some(Animation::StrokeColor(StrokeColorAnim { timing, values }))
                }
                "fill-opacity" | "stroke-opacity" | "stroke-width" => {
                    let values = parse_f32_values(attrs, 1.0)?;
                    Some(Animation::Style(StyleAnim {
                        timing,
                        attribute_name: name.to_string(),
                        values,
                    }))
                }
                "x" | "y" | "width" | "height" | "r" | "cx" | "cy" => {
                    let values = parse_f32_values(attrs, 0.0)?;
                    Some(Animation::Geometry(GeometryAnim {
                        timing,
                        attribute_name: name.to_string(),
                        values,
                    }))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// 解析 SMIL 的公共时间属性。缺 `dur` 视为非法（0 时长动画没有意义）。
fn parse_timing(attrs: &str) -> Option<Timing> {
    let dur_raw = attr(attrs, "dur")?;
    let dur_ms = if dur_raw.trim() == "indefinite" {
        None
    } else {
        let d = parse_time(dur_raw)?;
        if d > 0.0 {
            Some(d)
        } else {
            return None;
        }
    };
    let begin = parse_begin(attr(attrs, "begin").unwrap_or("0ms"));
    let rc = match attr(attrs, "repeatCount") {
        Some("indefinite") => Some(None), // indefinite
        Some(v) => v.parse::<u32>().ok().map(Some),
        None => None,
    };
    let rd = match attr(attrs, "repeatDur") {
        Some("indefinite") => Some(None),
        Some(v) => parse_time(v).filter(|d| *d > 0.0).map(Some),
        None => None,
    };
    let repeat = match (rc, rd) {
        (Some(None), _) | (_, Some(None)) => Repeat::Indefinite,
        (Some(Some(c)), Some(Some(d))) => Repeat::Bounded {
            count: c,
            total_ms: d,
        },
        (Some(Some(c)), None) => Repeat::Count(c),
        (None, Some(Some(d))) => Repeat::Duration(d),
        (None, None) => Repeat::Count(1),
    };
    let fill = if attr(attrs, "fill") == Some("freeze") {
        FillMode::Freeze
    } else {
        FillMode::Remove
    };
    let calc_mode = match attr(attrs, "calcMode") {
        Some("discrete") => CalcMode::Discrete,
        Some("paced") => CalcMode::Paced,
        Some("spline") => CalcMode::Spline,
        _ => CalcMode::Linear,
    };
    let key_times = attr(attrs, "keyTimes")
        .map(parse_f32_list)
        .unwrap_or_default();
    let key_splines = attr(attrs, "keySplines")
        .map(parse_splines)
        .unwrap_or_default();
    let end_ms = attr(attrs, "end").and_then(parse_time);
    let additive_sum = attr(attrs, "additive") == Some("sum");
    let accumulate_sum = attr(attrs, "accumulate") == Some("sum");
    let restart = match attr(attrs, "restart") {
        Some("whenNotActive") => Restart::WhenNotActive,
        Some("never") => Restart::Never,
        _ => Restart::Always,
    };
    let min_ms = attr(attrs, "min").and_then(parse_time).unwrap_or(0.0);
    let max_ms = attr(attrs, "max").and_then(parse_time);
    let access_key = attr(attrs, "accessKey").and_then(|s| s.trim().chars().next());
    // 扩展字段：CSS animation-direction / animation-fill-mode 的语义，
    // 让构建期导入器能把 CSS 动画无损翻译成 SMIL（见 docs/developer/syntax.md）。
    let direction = match attr(attrs, "direction").map(str::trim) {
        Some("reverse") => Direction::Reverse,
        Some("alternate") => Direction::Alternate,
        Some("alternate-reverse") => Direction::AlternateReverse,
        _ => Direction::Normal,
    };
    let backwards = matches!(
        attr(attrs, "fill-mode").map(str::trim),
        Some("backwards") | Some("both")
    );
    Some(Timing {
        begin,
        dur_ms,
        repeat,
        fill,
        backwards,
        direction,
        calc_mode,
        key_times,
        key_splines,
        end_ms,
        additive_sum,
        accumulate_sum,
        restart,
        min_ms,
        max_ms,
        access_key,
    })
}

/// `begin` 属性：偏移 / indefinite / 事件 / syncbase。
fn parse_begin(s: &str) -> Begin {
    let s = s.trim();
    if s == "indefinite" {
        return Begin::Indefinite;
    }
    // wallclock(1999-12-31T23:59:59Z) / wallclock(12:30:00) —— 简化为时钟字符串，
    // 转成从 UTC 午夜起的毫秒；宿主用 set_wallclock_ms 注入当前时间。
    if let Some(inner) = s
        .strip_prefix("wallclock(")
        .and_then(|x| x.strip_suffix(')'))
    {
        let t = inner.trim();
        let ms = if t.contains(':') {
            let parts: Vec<f32> = t.split(':').filter_map(|p| p.parse().ok()).collect();
            match parts.len() {
                2 => parts[0] * 3_600_000.0 + parts[1] * 60_000.0,
                3 => parts[0] * 3_600_000.0 + parts[1] * 60_000.0 + parts[2] * 1000.0,
                _ => 0.0,
            }
        } else {
            parse_time(t).unwrap_or(0.0)
        };
        return Begin::Wallclock { time_ms: ms };
    }
    // accessKey 通常单独给；begin 里也可能写成 "accessKey(k)"（非标准但兼容）。
    if let Some(inner) = s
        .strip_prefix("accessKey(")
        .and_then(|x| x.strip_suffix(')'))
    {
        if let Some(c) = inner.trim().chars().next() {
            return Begin::AccessKey(c);
        }
    }
    // syncbase: "other.end+500ms" / "other.begin-1s"
    if let Some(at) = s.find('.') {
        let id = &s[..at];
        let rest = &s[at + 1..];
        let (event, offset) = if let Some(split) = rest.find(['+', '-']) {
            let ev = &rest[..split];
            let sign = if rest.as_bytes()[split] == b'-' {
                -1.0
            } else {
                1.0
            };
            let off = parse_time(&rest[split + 1..]).unwrap_or(0.0) * sign;
            (ev, off)
        } else {
            (rest, 0.0)
        };
        if !id.is_empty() && !event.is_empty() && event.chars().all(|c| c.is_ascii_alphabetic()) {
            return Begin::Syncbase {
                id: id.to_string(),
                event: event.to_string(),
                offset_ms: offset,
            };
        }
    }
    // 事件: "click" / "btn.click"
    if s.chars().all(|c| c.is_ascii_alphabetic()) {
        return Begin::Event {
            target: None,
            event: s.to_string(),
        };
    }
    if let Some(dot) = s.rfind('.') {
        let (target, event) = (&s[..dot], &s[dot + 1..]);
        if event.chars().all(|c| c.is_ascii_alphabetic()) {
            return Begin::Event {
                target: Some(target.to_string()),
                event: event.to_string(),
            };
        }
    }
    // 偏移（可为负）。
    let negative = s.starts_with('-');
    let raw = s.trim_start_matches(['-', '+']);
    let v = parse_time(raw).unwrap_or(0.0);
    Begin::Offset(if negative { -v } else { v })
}

/// SMIL 时钟：`7878ms` / `1.5s` / `0.25s`。
fn parse_time(s: &str) -> Option<f32> {
    let s = s.trim();
    // SMIL clock-value: hh:mm:ss.frac / mm:ss.frac
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let nums: Option<Vec<f32>> = parts.iter().map(|p| p.parse::<f32>().ok()).collect();
        let nums = nums?;
        let ms = match nums.len() {
            2 => nums[0] * 60_000.0 + nums[1] * 1000.0,
            3 => nums[0] * 3_600_000.0 + nums[1] * 60_000.0 + nums[2] * 1000.0,
            _ => return None,
        };
        return (ms.is_finite() && ms >= 0.0).then_some(ms);
    }
    for (suffix, scale) in [
        ("ms", 1.0),
        ("min", 60_000.0),
        ("h", 3_600_000.0),
        ("s", 1000.0),
    ] {
        if let Some(v) = s.strip_suffix(suffix) {
            return v
                .trim()
                .parse::<f32>()
                .ok()
                .map(|v| v * scale)
                .filter(|v| v.is_finite() && *v >= 0.0);
        }
    }
    // 裸数字按秒。
    s.parse::<f32>()
        .ok()
        .map(|v| v * 1000.0)
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn parse_f32_list(s: &str) -> Vec<f32> {
    s.split(|c: char| c == ';' || c == ',' || c.is_ascii_whitespace())
        .filter_map(|t| t.trim().parse::<f32>().ok())
        .collect()
}

fn parse_splines(s: &str) -> Vec<Bezier> {
    let nums = parse_f32_list(s);
    nums.chunks_exact(4)
        .filter_map(|c| {
            if c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
                Some(Bezier::new(c[0], c[1], c[2], c[3]))
            } else {
                None
            }
        })
        .collect()
}

fn parse_f32_values(attrs: &str, base: f32) -> Option<Values<f32>> {
    if let Some(v) = attr(attrs, "values") {
        let list = parse_f32_list(v);
        if list.is_empty() {
            return None;
        }
        return Some(Values::List(list));
    }
    if let (Some(from), Some(to)) = (attr(attrs, "from"), attr(attrs, "to")) {
        let a = from.trim().parse().ok()?;
        let b = to.trim().parse().ok()?;
        return Some(Values::FromTo(a, b));
    }
    if let Some(by) = attr(attrs, "by") {
        return Some(Values::By(by.trim().parse().ok()?));
    }
    // 只有 to：从基值到目标。
    if let Some(to) = attr(attrs, "to") {
        return Some(Values::FromTo(base, to.trim().parse().ok()?));
    }
    None
}

fn parse_f32x3_values(attrs: &str, base: &[f32; 3]) -> Option<Values<[f32; 3]>> {
    // translate / scale 通常只有 2 个分量，rotate 是 3 个（angle cx cy）。
    let parse_triple = |s: &str| -> Option<[f32; 3]> {
        let v = parse_f32_list(s);
        match v.len() {
            1 => Some([v[0], 0.0, 0.0]), // skewX/skewY 只有一个角度
            2 => Some([v[0], v[1], 0.0]),
            3 => Some([v[0], v[1], v[2]]),
            _ => None,
        }
    };
    if let Some(v) = attr(attrs, "values") {
        let triples = v
            .split(';')
            .filter_map(|s| parse_triple(s.trim()))
            .collect::<Vec<_>>();
        if !triples.is_empty() {
            return Some(Values::List(triples));
        }
    }
    if let (Some(from), Some(to)) = (attr(attrs, "from"), attr(attrs, "to")) {
        return Some(Values::FromTo(parse_triple(from)?, parse_triple(to)?));
    }
    if let Some(by) = attr(attrs, "by") {
        return Some(Values::By(parse_triple(by)?));
    }
    if let Some(to) = attr(attrs, "to") {
        return Some(Values::FromTo(*base, parse_triple(to)?));
    }
    let _ = base;
    None
}

fn parse_color_values(attrs: &str) -> Option<Values<[f32; 4]>> {
    let parse_color = |s: &str| -> Option<[f32; 4]> {
        let s = s.trim();
        let named = match s {
            "red" => Some([1.0, 0.0, 0.0, 1.0]),
            "green" => Some([0.0, 0.5, 0.0, 1.0]),
            "blue" => Some([0.0, 0.0, 1.0, 1.0]),
            "white" => Some([1.0, 1.0, 1.0, 1.0]),
            "black" => Some([0.0, 0.0, 0.0, 1.0]),
            "transparent" => Some([0.0, 0.0, 0.0, 0.0]),
            _ => None,
        };
        if named.is_some() {
            return named;
        }
        if let Some(inner) = s
            .strip_prefix("rgba(")
            .and_then(|x| x.strip_suffix(')'))
            .or_else(|| s.strip_prefix("rgb(").and_then(|x| x.strip_suffix(')')))
        {
            let v: Vec<f32> = inner
                .split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect();
            if v.len() >= 3 {
                return Some([
                    (v[0] / 255.0).min(1.0),
                    (v[1] / 255.0).min(1.0),
                    (v[2] / 255.0).min(1.0),
                    if v.len() > 3 { v[3].min(1.0) } else { 1.0 },
                ]);
            }
        }
        let hex = s.strip_prefix('#')?;
        let ch = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok();
        match hex.len() {
            3 => Some([
                ch(0, 1)? as f32 / 15.0,
                ch(1, 1)? as f32 / 15.0,
                ch(2, 1)? as f32 / 15.0,
                1.0,
            ]),
            6 => Some([
                ch(0, 2)? as f32 / 255.0,
                ch(2, 2)? as f32 / 255.0,
                ch(4, 2)? as f32 / 255.0,
                1.0,
            ]),
            8 => Some([
                ch(0, 2)? as f32 / 255.0,
                ch(2, 2)? as f32 / 255.0,
                ch(4, 2)? as f32 / 255.0,
                ch(6, 2)? as f32 / 255.0,
            ]),
            _ => None,
        }
    };
    if let Some(v) = attr(attrs, "values") {
        // 注意：颜色值里没有分号冲突，`;` 是 SMIL 的分隔符。
        let list = v.split(';').filter_map(parse_color).collect::<Vec<_>>();
        if !list.is_empty() {
            return Some(Values::List(list));
        }
    }
    if let (Some(from), Some(to)) = (attr(attrs, "from"), attr(attrs, "to")) {
        return Some(Values::FromTo(parse_color(from)?, parse_color(to)?));
    }
    if let Some(to) = attr(attrs, "to") {
        return Some(Values::FromTo([1.0, 1.0, 1.0, 1.0], parse_color(to)?));
    }
    None
}

fn parse_path_values(attrs: &str, base: &[Segment]) -> Option<Values<Vec<Segment>>> {
    if let Some(v) = attr(attrs, "values") {
        // 路径的 `d` 里可能含分号吗？SVG path data 用空格/逗号，不用分号 —— 安全。
        let list = v
            .split(';')
            .filter_map(|s| parse_path_data(s.trim()).ok())
            .collect::<Vec<_>>();
        if !list.is_empty() {
            return Some(Values::List(list));
        }
    }
    if let (Some(from), Some(to)) = (attr(attrs, "from"), attr(attrs, "to")) {
        return Some(Values::FromTo(
            parse_path_data(from).ok()?,
            parse_path_data(to).ok()?,
        ));
    }
    if let Some(to) = attr(attrs, "to") {
        return Some(Values::FromTo(base.to_vec(), parse_path_data(to).ok()?));
    }
    None
}

/// 在文本中找下一个 `<tag`，返回 (属性串, `>` 之后的剩余文本)。
/// 引号内的 `>` 不算闭合，与 vector 的 svg.rs 同款约定。
fn next_tag<'a>(text: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(tag) {
        let start = from + rel;
        let after = start + tag.len();
        match bytes.get(after) {
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'>') | Some(b'/') => {}
            _ => {
                from = after;
                continue;
            }
        }
        let mut i = after;
        let mut quote = 0u8;
        while i < bytes.len() {
            let b = bytes[i];
            if quote != 0 {
                if b == quote {
                    quote = 0;
                }
            } else if b == b'"' || b == b'\'' {
                quote = b;
            } else if b == b'>' {
                return Some((&text[after..i], &text[i + 1..]));
            }
            i += 1;
        }
        return None;
    }
    None
}

/// 读属性值（双引号 / 单引号均可）。
fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let attr_name = &attrs[start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                i += 1;
                let v_start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                if attr_name == name {
                    return Some(&attrs[v_start..i]);
                }
            }
            Some(_) => {
                let v_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if attr_name == name {
                    return Some(&attrs[v_start..i]);
                }
            }
            None => break,
        }
    }
    None
}

// ========================================================================
// 内联单元测试：SMIL 文本解析的纯函数分支（格式 / 缺属性 / 非法值）
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn base_path() -> Vec<Segment> {
        vec![Segment::MoveTo(0.0, 0.0), Segment::LineTo(10.0, 10.0)]
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn attr_reads_double_single_and_unquoted_values() {
        let a = r##" fill="#ff0000" fill-opacity='0.5' stroke=none x = 10 "##;
        assert_eq!(attr(a, "fill"), Some("#ff0000"));
        assert_eq!(attr(a, "fill-opacity"), Some("0.5"));
        assert_eq!(attr(a, "stroke"), Some("none"));
        assert_eq!(attr(a, "x"), Some("10"));
        assert_eq!(attr(a, "dur"), None);
    }

    #[test]
    fn attr_requires_exact_name() {
        let a = "fill-opacity='0.5'";
        assert_eq!(attr(a, "fill"), None);
        assert_eq!(attr(a, "fill-opacity"), Some("0.5"));
    }

    #[test]
    fn next_tag_respects_boundaries_and_quotes() {
        let text = r#"<svg viewBox="0 0 1 1"><path d="M0 0L1 1" fill="a > b"/><animate attributeName="opacity" dur="1s"/></svg>"#;
        let (attrs, after) = next_tag(text, "<path").unwrap();
        assert!(attrs.contains("d=\"M0 0L1 1\""));
        assert!(after.starts_with("<animate"));
        assert!(next_tag(text, "<animate")
            .unwrap()
            .0
            .contains("attributeName"));
        assert!(next_tag(text, "<animateTransform").is_none());
        assert!(next_tag(text, "<circle").is_none());
        let (attrs, after) = next_tag("<path d='M0 0'/>", "<path").unwrap();
        assert_eq!(attrs, " d='M0 0'/");
        assert_eq!(after, "");
    }

    #[test]
    fn parse_time_handles_units_and_clock_values() {
        assert_eq!(parse_time("7878ms"), Some(7878.0));
        assert_eq!(parse_time("1.5s"), Some(1500.0));
        assert!(close(parse_time("0.001min").unwrap(), 60.0));
        assert_eq!(parse_time("1h"), Some(3_600_000.0));
        assert_eq!(parse_time("2"), Some(2000.0));
        assert_eq!(parse_time("1:30"), Some(90_000.0));
        assert_eq!(parse_time("1:02:03"), Some(3_723_000.0));
        assert_eq!(parse_time(" 500ms "), Some(500.0));
    }

    #[test]
    fn parse_time_rejects_invalid_values() {
        assert_eq!(parse_time("abc"), None);
        assert_eq!(parse_time("-5ms"), None);
        assert_eq!(parse_time("NaNms"), None);
        assert_eq!(parse_time("1:2:3:4"), None);
        assert_eq!(parse_time("indefinite"), None);
    }

    #[test]
    fn parse_begin_covers_offset_event_syncbase_wallclock_and_key() {
        assert_eq!(parse_begin("indefinite"), Begin::Indefinite);
        assert_eq!(parse_begin("0ms"), Begin::Offset(0.0));
        assert_eq!(parse_begin("-500ms"), Begin::Offset(-500.0));
        assert_eq!(parse_begin("+500ms"), Begin::Offset(500.0));
        assert_eq!(
            parse_begin("click"),
            Begin::Event {
                target: None,
                event: "click".to_string()
            }
        );
        assert_eq!(
            parse_begin("other.end+500ms"),
            Begin::Syncbase {
                id: "other".to_string(),
                event: "end".to_string(),
                offset_ms: 500.0
            }
        );
        assert_eq!(
            parse_begin("other.begin-1s"),
            Begin::Syncbase {
                id: "other".to_string(),
                event: "begin".to_string(),
                offset_ms: -1000.0
            }
        );
        assert_eq!(
            parse_begin("wallclock(12:30:00)"),
            Begin::Wallclock {
                time_ms: 12.0 * 3_600_000.0 + 30.0 * 60_000.0
            }
        );
        assert_eq!(
            parse_begin("wallclock(500ms)"),
            Begin::Wallclock { time_ms: 500.0 }
        );
        assert_eq!(parse_begin("accessKey(k)"), Begin::AccessKey('k'));
        // 空串的 chars().all(..) 恒真 ⇒ 落成空事件名。
        assert_eq!(
            parse_begin(""),
            Begin::Event {
                target: None,
                event: String::new()
            }
        );
    }

    #[test]
    fn parse_begin_malformed_syncbase_falls_back_to_offset() {
        assert_eq!(parse_begin("other.123"), Begin::Offset(0.0));
        // 裸数字按秒解释。
        assert_eq!(parse_begin("123"), Begin::Offset(123_000.0));
    }

    #[test]
    fn parse_f32_list_and_splines_filter_bad_tokens() {
        assert_eq!(parse_f32_list("0;0.5;1"), vec![0.0, 0.5, 1.0]);
        assert_eq!(parse_f32_list("1, 2 3"), vec![1.0, 2.0, 3.0]);
        assert_eq!(parse_f32_list("1;abc;2"), vec![1.0, 2.0]);
        let s = parse_splines("0 0 1 1;0.25 0.1 0.25 1");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].control_points(), (0.0, 0.0, 1.0, 1.0));
        assert!(parse_splines("0 0 1").is_empty());
        assert!(parse_splines("2 0 1 1").is_empty());
    }

    #[test]
    fn parse_f32_values_covers_values_from_to_by_and_to_only() {
        assert_eq!(
            parse_f32_values("values=\"0;0.5;1\"", 1.0),
            Some(Values::List(vec![0.0, 0.5, 1.0]))
        );
        assert_eq!(
            parse_f32_values("from=\"0\" to=\"1\"", 1.0),
            Some(Values::FromTo(0.0, 1.0))
        );
        assert_eq!(parse_f32_values("by=\"0.5\"", 1.0), Some(Values::By(0.5)));
        assert_eq!(
            parse_f32_values("to=\"1\"", 1.0),
            Some(Values::FromTo(1.0, 1.0))
        );
        assert_eq!(parse_f32_values("values=\"\"", 1.0), None);
        assert_eq!(parse_f32_values("from=\"abc\" to=\"1\"", 1.0), None);
        assert_eq!(parse_f32_values("", 1.0), None);
    }

    #[test]
    fn parse_f32x3_values_expands_component_counts() {
        assert_eq!(
            parse_f32x3_values("values=\"45\"", &[0.0; 3]),
            Some(Values::List(vec![[45.0, 0.0, 0.0]]))
        );
        assert_eq!(
            parse_f32x3_values("values=\"10 20\"", &[0.0; 3]),
            Some(Values::List(vec![[10.0, 20.0, 0.0]]))
        );
        assert_eq!(
            parse_f32x3_values("values=\"1 2 3\"", &[0.0; 3]),
            Some(Values::List(vec![[1.0, 2.0, 3.0]]))
        );
        assert_eq!(
            parse_f32x3_values("from=\"0 0\" to=\"10 20\"", &[0.0; 3]),
            Some(Values::FromTo([0.0, 0.0, 0.0], [10.0, 20.0, 0.0]))
        );
        assert_eq!(
            parse_f32x3_values("by=\"5 6\"", &[0.0; 3]),
            Some(Values::By([5.0, 6.0, 0.0]))
        );
        assert_eq!(
            parse_f32x3_values("to=\"1 2 3\"", &[9.0, 9.0, 9.0]),
            Some(Values::FromTo([9.0, 9.0, 9.0], [1.0, 2.0, 3.0]))
        );
        assert_eq!(parse_f32x3_values("", &[0.0; 3]), None);
    }

    #[test]
    fn parse_color_values_named_hex_rgb_and_rgba() {
        assert_eq!(
            parse_color_values("values=\"red;transparent\""),
            Some(Values::List(vec![
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 0.0]
            ]))
        );
        let hex = parse_color_values("values=\"#fff;#000000;#ff000080\"").unwrap();
        match hex {
            Values::List(v) => {
                assert_eq!(v[0], [1.0, 1.0, 1.0, 1.0]);
                assert_eq!(v[1], [0.0, 0.0, 0.0, 1.0]);
                assert!(close(v[2][0], 1.0) && close(v[2][3], 128.0 / 255.0));
            }
            _ => panic!("values 应产出列表"),
        }
        let rgb = parse_color_values("values=\"rgb(255,0,0);rgba(0,0,255,0.5)\"").unwrap();
        match rgb {
            Values::List(v) => {
                assert_eq!(v[0], [1.0, 0.0, 0.0, 1.0]);
                assert_eq!(v[1], [0.0, 0.0, 1.0, 0.5]);
            }
            _ => panic!("values 应产出列表"),
        }
    }

    #[test]
    fn parse_color_values_from_to_and_invalid() {
        assert_eq!(
            parse_color_values("from=\"red\" to=\"blue\""),
            Some(Values::FromTo([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]))
        );
        assert_eq!(
            parse_color_values("to=\"red\""),
            Some(Values::FromTo([1.0, 1.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]))
        );
        assert_eq!(parse_color_values("values=\"not-a-color\""), None);
        assert_eq!(parse_color_values("to=\"not-a-color\""), None);
        assert_eq!(parse_color_values(""), None);
    }

    #[test]
    fn parse_path_values_covers_values_from_to_and_to_only() {
        let base = base_path();
        let to = vec![Segment::MoveTo(2.0, 2.0), Segment::LineTo(3.0, 3.0)];
        assert_eq!(
            parse_path_values("values=\"M0 0L10 10;M2 2L3 3\"", &base),
            Some(Values::List(vec![base.clone(), to.clone()]))
        );
        assert_eq!(
            parse_path_values("from=\"M0 0L10 10\" to=\"M2 2L3 3\"", &base),
            Some(Values::FromTo(base.clone(), to.clone()))
        );
        assert_eq!(
            parse_path_values("to=\"M2 2L3 3\"", &base),
            Some(Values::FromTo(base.clone(), to))
        );
        assert_eq!(parse_path_values("values=\"???\"", &base), None);
        assert_eq!(parse_path_values("", &base), None);
    }

    #[test]
    fn parse_timing_reads_full_attribute_set() {
        let attrs = r#" begin="500ms" dur="1000ms" repeatCount="2" fill="freeze"
            calcMode="spline" keyTimes="0;1" keySplines="0 0 1 1" end="2000ms"
            additive="sum" accumulate="sum" restart="never" min="100ms" max="3000ms"
            accessKey="k" direction="alternate" fill-mode="both""#;
        let t = parse_timing(attrs).unwrap();
        assert_eq!(t.begin, Begin::Offset(500.0));
        assert_eq!(t.dur_ms, Some(1000.0));
        assert_eq!(t.repeat, Repeat::Count(2));
        assert_eq!(t.fill, FillMode::Freeze);
        assert!(t.backwards);
        assert_eq!(t.direction, Direction::Alternate);
        assert_eq!(t.calc_mode, CalcMode::Spline);
        assert_eq!(t.key_times, vec![0.0, 1.0]);
        assert_eq!(t.key_splines.len(), 1);
        assert_eq!(t.end_ms, Some(2000.0));
        assert!(t.additive_sum && t.accumulate_sum);
        assert_eq!(t.restart, Restart::Never);
        assert_eq!(t.min_ms, 100.0);
        assert_eq!(t.max_ms, Some(3000.0));
        assert_eq!(t.access_key, Some('k'));
    }

    #[test]
    fn parse_timing_rejects_missing_or_zero_duration() {
        assert_eq!(parse_timing("begin=\"0ms\""), None);
        assert_eq!(parse_timing("dur=\"0ms\""), None);
        assert_eq!(parse_timing("dur=\"-1s\""), None);
        assert_eq!(parse_timing("dur=\"indefinite\"").unwrap().dur_ms, None);
    }

    #[test]
    fn parse_timing_repeat_combinations() {
        assert_eq!(
            parse_timing("dur=\"1s\" repeatCount=\"indefinite\"")
                .unwrap()
                .repeat,
            Repeat::Indefinite
        );
        assert_eq!(
            parse_timing("dur=\"1s\" repeatDur=\"indefinite\"")
                .unwrap()
                .repeat,
            Repeat::Indefinite
        );
        assert_eq!(
            parse_timing("dur=\"1s\" repeatCount=\"3\"").unwrap().repeat,
            Repeat::Count(3)
        );
        assert_eq!(
            parse_timing("dur=\"1s\" repeatDur=\"2500ms\"")
                .unwrap()
                .repeat,
            Repeat::Duration(2500.0)
        );
        assert_eq!(
            parse_timing("dur=\"1s\" repeatCount=\"5\" repeatDur=\"2500ms\"")
                .unwrap()
                .repeat,
            Repeat::Bounded {
                count: 5,
                total_ms: 2500.0
            }
        );
    }

    #[test]
    fn parse_timing_defaults_for_missing_optional_attributes() {
        let t = parse_timing("dur=\"1s\"").unwrap();
        assert_eq!(t.begin, Begin::Offset(0.0));
        assert_eq!(t.repeat, Repeat::Count(1));
        assert_eq!(t.fill, FillMode::Remove);
        assert!(!t.backwards);
        assert_eq!(t.direction, Direction::Normal);
        assert_eq!(t.calc_mode, CalcMode::Linear);
        assert!(t.key_times.is_empty() && t.key_splines.is_empty());
        assert_eq!(t.end_ms, None);
        assert!(!t.additive_sum && !t.accumulate_sum);
        assert_eq!(t.restart, Restart::Always);
        assert_eq!(t.min_ms, 0.0);
        assert_eq!(t.max_ms, None);
        assert_eq!(t.access_key, None);
        assert_eq!(
            parse_timing("dur=\"1s\" calcMode=\"discrete\"")
                .unwrap()
                .calc_mode,
            CalcMode::Discrete
        );
        assert_eq!(
            parse_timing("dur=\"1s\" calcMode=\"paced\"")
                .unwrap()
                .calc_mode,
            CalcMode::Paced
        );
        assert_eq!(
            parse_timing("dur=\"1s\" restart=\"whenNotActive\"")
                .unwrap()
                .restart,
            Restart::WhenNotActive
        );
        assert!(
            parse_timing("dur=\"1s\" fill-mode=\"backwards\"")
                .unwrap()
                .backwards
        );
    }

    #[test]
    fn parse_animation_builds_every_variant() {
        let base = base_path();
        let set = parse_animation(
            "<set",
            " attributeName=\"opacity\" to=\"0.5\" dur=\"1s\"",
            &base,
        );
        assert!(matches!(set, Some(Animation::Set(_))));
        assert!(matches!(
            parse_animation(
                "<animateColor",
                " from=\"red\" to=\"blue\" dur=\"1s\"",
                &base
            ),
            Some(Animation::Fill(_))
        ));
        let motion = parse_animation(
            "<animateMotion",
            r##" path="M0 0L10 0" dur="1s" rotate="auto-reverse" origin="5 7" keyPoints="0;1" mpath="#route""##,
            &base,
        );
        match motion {
            Some(Animation::Motion(m)) => {
                assert_eq!(m.rotate_mode, MotionRotate::AutoReverse);
                assert_eq!(m.origin, (5.0, 7.0));
                assert_eq!(m.key_points, vec![0.0, 1.0]);
                assert_eq!(m.mpath_href.as_deref(), Some("#route"));
                assert_eq!(m.path.len(), 2);
            }
            _ => panic!("animateMotion 应产出 Motion"),
        }
        let transform = parse_animation(
            "<animateTransform",
            " type=\"translate\" from=\"0 0\" to=\"20 0\" dur=\"1s\"",
            &base,
        );
        match transform {
            Some(Animation::Transform(t)) => {
                assert_eq!(t.kind, TransformKind::Translate);
                assert_eq!(t.values, Values::FromTo([0.0, 0.0, 0.0], [20.0, 0.0, 0.0]));
            }
            _ => panic!("animateTransform 应产出 Transform"),
        }
        assert!(matches!(
            parse_animation(
                "<animate",
                " attributeName=\"opacity\" values=\"0;1\" dur=\"1s\"",
                &base
            ),
            Some(Animation::Opacity(_))
        ));
        assert!(matches!(
            parse_animation(
                "<animate",
                " attributeName=\"fill\" values=\"red;blue\" dur=\"1s\"",
                &base
            ),
            Some(Animation::Fill(_))
        ));
        assert!(matches!(
            parse_animation(
                "<animate",
                " attributeName=\"d\" from=\"M0 0L1 1\" to=\"M2 2L3 3\" dur=\"1s\"",
                &base
            ),
            Some(Animation::Path(_))
        ));
        assert!(matches!(
            parse_animation(
                "<animate",
                " attributeName=\"stroke\" values=\"red;blue\" dur=\"1s\"",
                &base
            ),
            Some(Animation::StrokeColor(_))
        ));
        assert!(matches!(
            parse_animation(
                "<animate",
                " attributeName=\"fill-opacity\" values=\"0;1\" dur=\"1s\"",
                &base
            ),
            Some(Animation::Style(_))
        ));
        assert!(matches!(
            parse_animation(
                "<animate",
                " attributeName=\"width\" values=\"0;1\" dur=\"1s\"",
                &base
            ),
            Some(Animation::Geometry(_))
        ));
    }

    #[test]
    fn parse_animation_rejects_unknown_and_incomplete_elements() {
        let base = base_path();
        assert_eq!(
            parse_animation("<set", " attributeName=\"opacity\" dur=\"1s\"", &base),
            None
        );
        assert_eq!(
            parse_animation(
                "<animateTransform",
                " type=\"wobble\" from=\"0\" to=\"1\" dur=\"1s\"",
                &base
            ),
            None
        );
        assert_eq!(
            parse_animation(
                "<animate",
                " attributeName=\"mystery\" values=\"0;1\" dur=\"1s\"",
                &base
            ),
            None
        );
        assert_eq!(
            parse_animation("<animate", " values=\"0;1\" dur=\"1s\"", &base),
            None
        );
        assert_eq!(
            parse_animation("<animate", " attributeName=\"opacity\" dur=\"1s\"", &base),
            None
        );
        assert_eq!(parse_animation("<unknown", " dur=\"1s\"", &base), None);
    }

    #[test]
    fn parse_hex_rgba_accepts_three_six_eight_digits() {
        assert_eq!(parse_hex_rgba("#fff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse_hex_rgba("#000000"), Some([0.0, 0.0, 0.0, 1.0]));
        let c = parse_hex_rgba("#ff000080").unwrap();
        assert!(close(c[0], 1.0) && close(c[3], 128.0 / 255.0));
        assert_eq!(parse_hex_rgba("fff"), None);
        assert_eq!(parse_hex_rgba("#12345"), None);
    }
}
