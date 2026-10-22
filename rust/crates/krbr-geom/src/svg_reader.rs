//! kicad-cli が出力する SVG（pcb export svg）を多角形に変換する最小限のリーダー。
//! 対応：<g> / <path> / <circle> / <polyline> / <polygon> / <rect> と、fill / stroke / stroke-width / fill-rule の継承
//! （属性と style="..."）。変換は translate と scale だけ。<text> は KiCad が補助情報として出すだけなので無視する

use std::collections::BTreeMap;
use std::f64::consts::PI;

use clipper2::FillRule;

use crate::shapes::{append, bezier_points, circle_points, disc, segments_for, stroke_polyline, to_path, unite, Paths, Vec2, ARC_TOL};

#[derive(Debug, Default)]
pub struct SvgResult {
    /// 基板座標（mm）
    pub paths: Paths,
    pub elements: usize,
}

#[derive(Debug, Clone, Copy)]
struct Style {
    /// SVG の既定は fill:black
    fill: bool,
    stroke: bool,
    stroke_width: f64,
    even_odd: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self { fill: true, stroke: false, stroke_width: 1.0, even_odd: false }
    }
}

#[derive(Debug, Clone, Copy)]
struct Transform {
    sx: f64,
    sy: f64,
    tx: f64,
    ty: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self { sx: 1.0, sy: 1.0, tx: 0.0, ty: 0.0 }
    }
}

impl Transform {
    fn apply(&self, p: Vec2) -> Vec2 {
        Vec2::new(p.x * self.sx + self.tx, p.y * self.sy + self.ty)
    }
    /// self * inner
    fn then(&self, inner: &Transform) -> Transform {
        Transform { sx: self.sx * inner.sx, sy: self.sy * inner.sy, tx: self.sx * inner.tx + self.tx, ty: self.sy * inner.ty + self.ty }
    }
    /// mm の許容誤差を、変換前の座標（SVG のユーザー単位。kicad-cli では 1 単位 = 1 µm）に直す。
    /// 縦横の倍率が違うときは小さい方に合わせる（どちらの向きでも mm で tol を超えない）
    fn tol_in_user_units(&self, tol_mm: f64) -> f64 {
        let s = self.sx.abs().max(self.sy.abs());
        if s > 0.0 { tol_mm / s } else { tol_mm }
    }
}

/// "0.12mm" などの単位は落とす
fn to_num(s: &str, def: f64) -> f64 {
    s.trim().trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().unwrap_or(def)
}

fn apply_style_prop(st: &mut Style, key: &str, val: &str) {
    match key {
        "fill" => st.fill = val != "none",
        "stroke" => st.stroke = val != "none",
        "stroke-width" => st.stroke_width = to_num(val, st.stroke_width),
        "fill-rule" => st.even_odd = val == "evenodd",
        _ => {}
    }
}

fn apply_style_string(st: &mut Style, css: &str) {
    for decl in css.split(';') {
        if let Some((k, v)) = decl.split_once(':') {
            apply_style_prop(st, k.trim(), v.trim());
        }
    }
}

fn split_numbers(s: &str) -> Vec<f64> {
    s.split([' ', ',']).filter(|t| !t.is_empty()).map(|t| to_num(t, 0.0)).collect()
}

fn parse_transform(s: &str) -> Transform {
    let mut t = Transform::default();
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')').map(|c| open + c) else { break };
        let name = rest[..open].trim();
        let args = split_numbers(&rest[open + 1..close]);
        let mut m = Transform::default();
        if name == "translate" && !args.is_empty() {
            m.tx = args[0];
            m.ty = args.get(1).copied().unwrap_or(0.0);
        } else if name == "scale" && !args.is_empty() {
            m.sx = args[0];
            m.sy = args.get(1).copied().unwrap_or(args[0]);
        }
        t = t.then(&m);
        rest = &rest[close + 1..];
    }
    t
}

#[derive(Debug, Default)]
struct Tag {
    name: String,
    attrs: BTreeMap<String, String>,
    closing: bool,
    self_closing: bool,
}

impl Tag {
    fn num(&self, key: &str) -> f64 {
        self.attrs.get(key).map(|v| to_num(v, 0.0)).unwrap_or(0.0)
    }
}

/// 次のタグを読む。コメント・宣言・テキストは飛ばす
fn next_tag(s: &str, pos: &mut usize) -> Option<Tag> {
    let b = s.as_bytes();
    loop {
        *pos += s[*pos..].find('<')?;
        if s[*pos..].starts_with("<!--") {
            *pos += s[*pos..].find("-->")? + 3;
            continue;
        }
        if matches!(b.get(*pos + 1), Some(b'?') | Some(b'!')) {
            *pos += s[*pos..].find('>')? + 1;
            continue;
        }
        break;
    }
    let mut tag = Tag::default();
    let mut i = *pos + 1;
    if b.get(i) == Some(&b'/') {
        tag.closing = true;
        i += 1;
    }
    let mut j = i;
    while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
        j += 1;
    }
    tag.name = s[i..j].to_string();
    i = j;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        if b[i] == b'>' {
            *pos = i + 1;
            return Some(tag);
        }
        if b[i] == b'/' && b.get(i + 1) == Some(&b'>') {
            tag.self_closing = true;
            *pos = i + 2;
            return Some(tag);
        }
        let mut k = i;
        while k < b.len() && b[k] != b'=' && b[k] != b'>' && !b[k].is_ascii_whitespace() {
            k += 1;
        }
        let key = s[i..k].to_string();
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if b.get(k) == Some(&b'=') {
            k += 1;
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            let q = *b.get(k)?;
            if q == b'"' || q == b'\'' {
                let e = k + 1 + s[k + 1..].find(q as char)?;
                tag.attrs.insert(key, s[k + 1..e].to_string());
                i = e + 1;
            } else {
                let mut e = k;
                while e < b.len() && !b[e].is_ascii_whitespace() && b[e] != b'>' {
                    e += 1;
                }
                tag.attrs.insert(key, s[k..e].to_string());
                i = e;
            }
        } else {
            tag.attrs.insert(key, String::new());
            i = k;
        }
    }
    None
}

#[derive(Debug, Default, Clone)]
struct SubPath {
    pts: Vec<Vec2>,
    closed: bool,
}

/// 数の読み取り（SVG の数：符号、小数点、指数。"1.5.5" や "-1-2" のような詰めた書き方も区切る）
fn read_number(d: &[u8], i: &mut usize) -> Option<f64> {
    while *i < d.len() && (d[*i].is_ascii_whitespace() || d[*i] == b',') {
        *i += 1;
    }
    let start = *i;
    let mut j = *i;
    if j < d.len() && (d[j] == b'+' || d[j] == b'-') {
        j += 1;
    }
    let mut dot = false;
    let mut digits = false;
    while j < d.len() && (d[j].is_ascii_digit() || (d[j] == b'.' && !dot)) {
        dot |= d[j] == b'.';
        digits |= d[j].is_ascii_digit();
        j += 1;
    }
    if !digits {
        return None;
    }
    if j < d.len() && (d[j] == b'e' || d[j] == b'E') {
        let mut k = j + 1;
        if k < d.len() && (d[k] == b'+' || d[k] == b'-') {
            k += 1;
        }
        if k < d.len() && d[k].is_ascii_digit() {
            while k < d.len() && d[k].is_ascii_digit() {
                k += 1;
            }
            j = k;
        }
    }
    let v = std::str::from_utf8(&d[start..j]).ok()?.parse().ok()?;
    *i = j;
    Some(v)
}

fn read_flag(d: &[u8], i: &mut usize) -> Option<bool> {
    while *i < d.len() && (d[*i].is_ascii_whitespace() || d[*i] == b',') {
        *i += 1;
    }
    let v = match d.get(*i)? {
        b'0' => false,
        b'1' => true,
        _ => return None,
    };
    *i += 1;
    Some(v)
}

/// SVG のパスデータ（M L H V C Q A Z とその相対版）。曲線は tol（変換前の座標の単位）以下のずれで折れ線にする
fn parse_path_data(d: &str, tol: f64) -> Vec<SubPath> {
    let d = d.as_bytes();
    let mut out: Vec<SubPath> = Vec::new();
    let mut i = 0usize;
    let mut cmd = 0u8;
    let (mut cur, mut start) = (Vec2::ZERO, Vec2::ZERO);
    fn cur_path(out: &mut Vec<SubPath>, cur: Vec2) -> &mut SubPath {
        if out.is_empty() {
            out.push(SubPath { pts: vec![cur], closed: false });
        }
        out.last_mut().unwrap()
    }
    loop {
        while i < d.len() && (d[i].is_ascii_whitespace() || d[i] == b',') {
            i += 1;
        }
        if i >= d.len() {
            break;
        }
        if d[i].is_ascii_alphabetic() && d[i] != b'e' && d[i] != b'E' {
            cmd = d[i];
            i += 1;
            if cmd == b'Z' || cmd == b'z' {
                if let Some(last) = out.last_mut() {
                    last.closed = true;
                }
                cur = start;
                continue;
            }
        }
        if cmd == 0 {
            break;
        }
        let rel = cmd.is_ascii_lowercase();
        let c = cmd.to_ascii_uppercase();
        let at = |cur: Vec2, x: f64, y: f64| if rel { cur + Vec2::new(x, y) } else { Vec2::new(x, y) };
        let mut num = || read_number(d, &mut i);
        match c {
            b'M' => {
                let (Some(x), Some(y)) = (num(), num()) else { break };
                cur = at(cur, x, y);
                start = cur;
                out.push(SubPath { pts: vec![cur], closed: false });
                cmd = if rel { b'l' } else { b'L' }; // 続く座標は lineto
            }
            b'L' => {
                let (Some(x), Some(y)) = (num(), num()) else { break };
                cur = at(cur, x, y);
                cur_path(&mut out, cur).pts.push(cur);
            }
            b'H' => {
                let Some(x) = num() else { break };
                cur.x = if rel { cur.x + x } else { x };
                cur_path(&mut out, cur).pts.push(cur);
            }
            b'V' => {
                let Some(y) = num() else { break };
                cur.y = if rel { cur.y + y } else { y };
                cur_path(&mut out, cur).pts.push(cur);
            }
            b'C' | b'Q' => {
                let n = if c == b'C' { 6 } else { 4 };
                let mut a = [0.0; 6];
                let mut ok = true;
                for v in a.iter_mut().take(n) {
                    match num() {
                        Some(x) => *v = x,
                        None => ok = false,
                    }
                }
                if !ok {
                    break;
                }
                let mut ctrl = vec![cur];
                for k in (0..n).step_by(2) {
                    ctrl.push(at(cur, a[k], a[k + 1]));
                }
                if c == b'Q' {
                    // 2 次を 3 次に
                    let (p0, q, p2) = (ctrl[0], ctrl[1], ctrl[2]);
                    ctrl = vec![p0, p0 + 2.0 / 3.0 * (q - p0), p2 + 2.0 / 3.0 * (q - p2), p2];
                }
                let pts = bezier_points(&ctrl, tol);
                cur_path(&mut out, cur).pts.extend_from_slice(&pts[1..]);
                cur = *ctrl.last().unwrap();
            }
            b'A' => {
                let (Some(rx0), Some(ry0), Some(rot)) = (num(), num(), num()) else { break };
                let (Some(large), Some(sweep)) = (read_flag(d, &mut i), read_flag(d, &mut i)) else { break };
                let (Some(x), Some(y)) = (read_number(d, &mut i), read_number(d, &mut i)) else { break };
                let p1 = at(cur, x, y);
                let (mut rx, mut ry) = (rx0.abs(), ry0.abs());
                let phi = rot.to_radians();
                let sp = cur_path(&mut out, cur);
                if rx == 0.0 || ry == 0.0 {
                    sp.pts.push(p1);
                } else {
                    // SVG 実装ノート F.6.5 の端点表現 -> 中心表現
                    let (cp, sn) = (phi.cos(), phi.sin());
                    let dp = (cur - p1) / 2.0;
                    let q = Vec2::new(cp * dp.x + sn * dp.y, -sn * dp.x + cp * dp.y);
                    let lam = (q.x * q.x) / (rx * rx) + (q.y * q.y) / (ry * ry);
                    if lam > 1.0 {
                        rx *= lam.sqrt();
                        ry *= lam.sqrt();
                    }
                    let num_ = rx * rx * ry * ry - rx * rx * q.y * q.y - ry * ry * q.x * q.x;
                    let den = rx * rx * q.y * q.y + ry * ry * q.x * q.x;
                    let mut coef = if den > 0.0 { (num_ / den).max(0.0).sqrt() } else { 0.0 };
                    if large == sweep {
                        coef = -coef;
                    }
                    let cq = Vec2::new(coef * rx * q.y / ry, -coef * ry * q.x / rx);
                    let c0 = Vec2::new(cp * cq.x - sn * cq.y, sn * cq.x + cp * cq.y) + (cur + p1) / 2.0;
                    let ang = |u: Vec2, v: Vec2| (u.x * v.y - u.y * v.x).atan2(u.dot(v));
                    let v1 = Vec2::new((q.x - cq.x) / rx, (q.y - cq.y) / ry);
                    let v2 = Vec2::new((-q.x - cq.x) / rx, (-q.y - cq.y) / ry);
                    let t1 = ang(Vec2::X, v1);
                    let mut dt = ang(v1, v2);
                    if !sweep && dt > 0.0 {
                        dt -= 2.0 * PI;
                    }
                    if sweep && dt < 0.0 {
                        dt += 2.0 * PI;
                    }
                    // 楕円の大きい方の半径の円とみなして分割数を決める（小さい方の半径の向きでは、ずれはそれより小さい）
                    let n = segments_for(rx.max(ry), dt, tol);
                    for k in 1..=n {
                        let t = t1 + dt * k as f64 / n as f64;
                        let e = Vec2::new(rx * t.cos(), ry * t.sin());
                        sp.pts.push(c0 + Vec2::new(cp * e.x - sn * e.y, sn * e.x + cp * e.y));
                    }
                }
                cur = p1;
            }
            _ => break, // 未対応のコマンド
        }
    }
    out
}

fn emit(subs: &[SubPath], st: &Style, xf: &Transform, out: &mut Paths) {
    let rings: Vec<Vec<Vec2>> = subs.iter().map(|sp| sp.pts.iter().map(|&p| xf.apply(p)).collect()).collect();
    let w = st.stroke_width * xf.sx.abs();
    if st.fill {
        let fill: Vec<_> = rings.iter().filter(|r| r.len() >= 3).map(|r| to_path(r)).collect();
        if !fill.is_empty() {
            append(out, unite(&Paths::new(fill), if st.even_odd { FillRule::EvenOdd } else { FillRule::NonZero }));
        }
    }
    if st.stroke && w > 0.0 {
        for (r, sp) in rings.iter().zip(subs) {
            append(out, stroke_polyline(r, w, sp.closed));
        }
    }
}

/// kicad-cli の SVG を読む（--page-size-mode 0 で書き出したもの：ページ座標がそのまま基板座標）
pub fn read_kicad_svg(text: &str) -> SvgResult {
    let mut res = SvgResult { paths: Paths::new(Vec::new()), elements: 0 };
    let mut stack: Vec<(Style, Transform)> = vec![(Style::default(), Transform::default())];
    let mut pos = 0usize;
    while let Some(tag) = next_tag(text, &mut pos) {
        if tag.closing {
            if (tag.name == "g" || tag.name == "svg") && stack.len() > 1 {
                stack.pop();
            }
            continue;
        }
        let (mut st, mut xf) = *stack.last().unwrap();
        for k in ["fill", "stroke", "stroke-width", "fill-rule"] {
            if let Some(v) = tag.attrs.get(k) {
                apply_style_prop(&mut st, k, v);
            }
        }
        if let Some(css) = tag.attrs.get("style") {
            apply_style_string(&mut st, css);
        }
        if let Some(t) = tag.attrs.get("transform") {
            xf = xf.then(&parse_transform(t));
        }
        match tag.name.as_str() {
            "svg" => {
                // width="297mm" と viewBox の比から mm への換算を決める
                if let (Some(w), Some(vb)) = (tag.attrs.get("width"), tag.attrs.get("viewBox")) {
                    let v = split_numbers(vb);
                    let mm = to_num(w, 0.0);
                    if v.len() >= 4 && v[2] > 0.0 && w.contains("mm") {
                        let s = mm / v[2];
                        xf = xf.then(&Transform { sx: s, sy: s, tx: -v[0] * s, ty: -v[1] * s });
                    }
                }
                if !tag.self_closing {
                    stack.push((st, xf));
                }
            }
            "g" => {
                if !tag.self_closing {
                    stack.push((st, xf));
                }
            }
            "path" => {
                if let Some(d) = tag.attrs.get("d") {
                    emit(&parse_path_data(d, xf.tol_in_user_units(ARC_TOL)), &st, &xf, &mut res.paths);
                    res.elements += 1;
                }
            }
            "circle" => {
                let c = xf.apply(Vec2::new(tag.num("cx"), tag.num("cy")));
                let r = tag.num("r") * xf.sx.abs();
                let w = st.stroke_width * xf.sx.abs();
                if st.fill {
                    append(&mut res.paths, disc(c, r + if st.stroke { w / 2.0 } else { 0.0 }));
                } else if st.stroke && w > 0.0 {
                    append(&mut res.paths, stroke_polyline(&circle_points(c, r, ARC_TOL), w, true));
                }
                res.elements += 1;
            }
            "polyline" | "polygon" => {
                let d = format!("M {}", tag.attrs.get("points").map(String::as_str).unwrap_or(""));
                let mut subs = parse_path_data(&d, xf.tol_in_user_units(ARC_TOL));
                if tag.name == "polygon" {
                    if let Some(last) = subs.last_mut() {
                        last.closed = true;
                    }
                }
                emit(&subs, &st, &xf, &mut res.paths);
                res.elements += 1;
            }
            "rect" => {
                let (x, y, w, h) = (tag.num("x"), tag.num("y"), tag.num("width"), tag.num("height"));
                let sp = SubPath { pts: vec![Vec2::new(x, y), Vec2::new(x + w, y), Vec2::new(x + w, y + h), Vec2::new(x, y + h)], closed: true };
                emit(&[sp], &st, &xf, &mut res.paths);
                res.elements += 1;
            }
            _ => {}
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::{path_points, signed_area};

    fn area(p: &Paths) -> f64 {
        unite(p, FillRule::NonZero).iter().map(signed_area).sum::<f64>().abs()
    }

    fn bounds(p: &Paths) -> (Vec2, Vec2) {
        let mut lo = Vec2::splat(f64::MAX);
        let mut hi = Vec2::splat(f64::MIN);
        for path in p.iter() {
            for q in path_points(path) {
                lo = lo.min(q);
                hi = hi.max(q);
            }
        }
        (lo, hi)
    }

    /// KiCad 10 の書き出しと同じ形：mm の width と viewBox、g の style と transform
    fn wrap(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" standalone="no"?>
<!-- comment -->
<svg xmlns="http://www.w3.org/2000/svg" width="20mm" height="10mm" viewBox="10000 20000 20000 10000">
<g style="fill:#000000; fill-opacity:1.0;stroke:#000000; stroke-opacity:1.0; stroke-linecap:round; stroke-linejoin:round" transform="translate(0 0) scale(1 1)">
{body}
</g>
</svg>"#
        )
    }

    #[test]
    fn units_and_filled_rect() {
        // viewBox の 1 単位 = 1 µm。(12000, 21000)-(14000, 22000) の矩形は基板座標 (2, 1)-(4, 2) mm
        let r = read_kicad_svg(&wrap(r#"<rect x="12000" y="21000" width="2000" height="1000" style="stroke:none"/>"#));
        assert_eq!(r.elements, 1);
        assert!((area(&r.paths) - 2.0).abs() < 1e-6, "{}", area(&r.paths));
        let (lo, hi) = bounds(&r.paths);
        assert!((lo - Vec2::new(2.0, 1.0)).length() < 1e-6 && (hi - Vec2::new(4.0, 2.0)).length() < 1e-6, "{lo} {hi}");
    }

    #[test]
    fn stroked_path_relative_commands_and_width() {
        // 線（塗りなし）：幅 200 µm で 3 mm の水平線（相対の h）。面積は 3 x 0.2 + 端の丸 π 0.1²
        let r = read_kicad_svg(&wrap(r#"<path style="fill:none; stroke-width:200" d="M 12000 25000 h 3000 "/>"#));
        let want = 3.0 * 0.2 + PI * 0.01;
        assert!((area(&r.paths) - want).abs() < 2e-3, "{} vs {want}", area(&r.paths));
    }

    #[test]
    fn arc_makes_a_half_disc() {
        // 半径 1 mm の半円（A）を閉じて塗る：面積 π/2。折れ線は円弧より最大 ARC_TOL 内側なので、欠けは弧の長さ π × ARC_TOL まで
        let r = read_kicad_svg(&wrap(r#"<path style="stroke:none" d="M 14000 25000 A 1000 1000 0 0 1 16000 25000 Z"/>"#));
        let a = area(&r.paths);
        assert!(a <= PI / 2.0 && a > PI / 2.0 - PI * ARC_TOL, "{a}");
    }

    #[test]
    fn circles_polygons_and_transforms() {
        // circle（塗り）、polygon（閉じる）、入れ子の g の translate と scale
        let body = r#"<circle cx="12000" cy="22000" r="500" style="stroke:none"/>
<g transform="translate(1000 0) scale(2)"><polygon points="6000,10000 7000,10000 7000,11000" style="stroke:none"/></g>
<text x="0" y="0">ignored</text>"#;
        let r = read_kicad_svg(&wrap(body));
        assert_eq!(r.elements, 2);
        // 円 π 0.25 + 三角形（scale 2 で 2 mm 角の半分 = 2）。円は多角形で近似するので、その分だけ小さい
        let want = PI * 0.25 + 2.0;
        assert!((area(&r.paths) - want).abs() < 6e-3, "{} vs {want}", area(&r.paths));
    }

    #[test]
    fn curves_are_split_by_the_tolerance_in_mm() {
        // 半径 1 mm（1000 µm）の半円を A と C で描く。分割の細かさは mm の ARC_TOL で決まり、µm の単位に引きずられない
        let a = parse_path_data("M 0 0 A 1000 1000 0 0 1 2000 0", ARC_TOL * 1000.0);
        let want = segments_for(1.0, PI, ARC_TOL);
        assert_eq!(a[0].pts.len(), want + 1);
        let c = parse_path_data("M 0 0 C 0 -1000 2000 -1000 2000 0", ARC_TOL * 1000.0);
        assert!(c[0].pts.len() < 100, "{}", c[0].pts.len()); // µm のまま 0.002 で分けると上限の 512 分割になる
        let t = Transform { sx: 0.001, sy: 0.001, tx: 0.0, ty: 0.0 };
        assert!((t.tol_in_user_units(ARC_TOL) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn packed_numbers() {
        let mut i = 0;
        let d = b"1.5.5-2e1,3";
        let v: Vec<f64> = std::iter::from_fn(|| read_number(d, &mut i)).collect();
        assert_eq!(v, vec![1.5, 0.5, -20.0, 3.0]);
    }
}
