//! 把版式算好的元素写成 .pptx：母版、版式、主题（来自配色表）、页面、原生表格、
//! 可编辑图表（内嵌数据工作簿）、图片和演讲者备注。

use std::collections::HashMap;
use std::path::Path;

use super::deck::{self, image_info, layout, ChartSpec, DeckSpec, Element, Para, Style, BULLET_INDENT, LEVEL_INDENT};
use super::zipw::write_zip;
use crate::markdown;
use crate::{EngineError, Result};

const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
const EMU: f64 = 12700.0;
const GRP: &str = r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#;
const CLR_MAP: &str = r#"bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink""#;

fn emu(pt: f64) -> i64 {
    (pt * EMU).round() as i64
}

fn rgb(c: &str) -> String {
    deck::hex(c).map(|h| h[1..].to_string()).unwrap_or_else(|| "000000".into())
}

fn esc(s: &str) -> String {
    deck::esc(s)
}

fn xfrm(x: f64, y: f64, w: f64, h: f64) -> String {
    format!(r#"<a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm>"#, emu(x), emu(y), emu(w), emu(h))
}

fn solid(c: &str) -> String {
    format!(r#"<a:solidFill><a:srgbClr val="{}"/></a:solidFill>"#, rgb(c))
}

fn theme(style: &Style, name: &str) -> String {
    let p = &style.palette;
    let colors = style.series_colors();
    let acc = |i: usize| rgb(colors.get(i).map(String::as_str).unwrap_or("#64748B"));
    let f = &style.fonts;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="{name}"><a:themeElements><a:clrScheme name="OfficePilot"><a:dk1><a:srgbClr val="{}"/></a:dk1><a:lt1><a:srgbClr val="{}"/></a:lt1><a:dk2><a:srgbClr val="{}"/></a:dk2><a:lt2><a:srgbClr val="{}"/></a:lt2><a:accent1><a:srgbClr val="{}"/></a:accent1><a:accent2><a:srgbClr val="{}"/></a:accent2><a:accent3><a:srgbClr val="{}"/></a:accent3><a:accent4><a:srgbClr val="{}"/></a:accent4><a:accent5><a:srgbClr val="{}"/></a:accent5><a:accent6><a:srgbClr val="{}"/></a:accent6><a:hlink><a:srgbClr val="{}"/></a:hlink><a:folHlink><a:srgbClr val="{}"/></a:folHlink></a:clrScheme><a:fontScheme name="OfficePilot"><a:majorFont><a:latin typeface="{h}"/><a:ea typeface="{h}"/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="{b}"/><a:ea typeface="{b}"/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="19050"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#,
        rgb(&p.text),
        rgb(&p.background),
        rgb(&p.text),
        rgb(&p.surface),
        acc(0),
        acc(1),
        acc(2),
        acc(3),
        acc(4),
        acc(5),
        rgb(&p.accent),
        rgb(&p.muted),
        h = esc(&f.heading),
        b = esc(&f.body),
    )
}

fn title_ph(style: &Style, pw: f64) -> String {
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr>{}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr anchor="b"/><a:lstStyle/><a:p><a:endParaRPr lang="zh-CN" sz="{}"/></a:p></p:txBody></p:sp>"#,
        xfrm(48.0, 32.0, pw - 96.0, 64.0),
        (style.title_size * 100.0) as i64
    )
}

fn master(style: &Style, pw: f64) -> String {
    let sz = |pt: f64| (pt * 100.0) as i64;
    let lvl = |n: usize, size: f64| {
        let mar = (n as f64 * LEVEL_INDENT + BULLET_INDENT) * EMU;
        format!(r#"<a:lvl{n1}pPr marL="{}" indent="{}"><a:buFont typeface="Arial"/><a:buChar char="•"/><a:defRPr sz="{}"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/></a:defRPr></a:lvl{n1}pPr>"#, mar as i64, -(BULLET_INDENT * EMU) as i64, sz(size), n1 = n + 1)
    };
    let body: String = (0..5).map(|i| lvl(i, if i == 0 { style.body_size } else { (style.body_size - 2.0).max(12.0) })).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldMaster {NS}><p:cSld><p:bg><p:bgPr>{}<a:effectLst/></p:bgPr></p:bg><p:spTree>{GRP}{}</p:spTree></p:cSld><p:clrMap {CLR_MAP}/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle><a:lvl1pPr algn="l"><a:lnSpc><a:spcPct val="90000"/></a:lnSpc><a:buNone/><a:defRPr sz="{}" b="1"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mj-lt"/><a:ea typeface="+mj-ea"/></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle>{body}</p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz="{}"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/></a:defRPr></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"#,
        solid(&style.palette.background),
        title_ph(style, pw),
        sz(style.title_size),
        sz(style.body_size)
    )
}

fn layout_part(style: &Style, pw: f64) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldLayout {NS} type="titleOnly" preserve="1"><p:cSld name="仅标题"><p:spTree>{GRP}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#,
        title_ph(style, pw)
    )
}

fn notes_master() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:notesMaster {NS}><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree>{GRP}<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr><p:nvPr><p:ph type="sldImg" idx="2"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="685800" y="1143000"/><a:ext cx="5486400" cy="3086100"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln w="12700"><a:solidFill><a:prstClr val="black"/></a:solidFill></a:ln></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="body" sz="quarter" idx="3"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="685800" y="4400550"/><a:ext cx="5486400" cy="3600450"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang="zh-CN"/></a:p></p:txBody></p:sp></p:spTree></p:cSld><p:clrMap {CLR_MAP}/><p:notesStyle><a:lvl1pPr marL="0" algn="l" rtl="0"><a:defRPr sz="1200" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/><a:cs typeface="+mn-cs"/></a:defRPr></a:lvl1pPr></p:notesStyle></p:notesMaster>"#
    )
}

fn notes_slide(text: &str) -> String {
    let paras: String = text.split('\n').map(|l| format!(r#"<a:p><a:r><a:rPr lang="zh-CN" dirty="0"/><a:t>{}</a:t></a:r></a:p>"#, esc(l))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:notes {NS}><p:cSld><p:spTree>{GRP}<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paras}</p:txBody></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>"#
    )
}

// ───────── 元素 ─────────

fn rpr(size: f64, color: &str, bold: bool, italic: bool, font: &str) -> String {
    format!(
        r#"<a:rPr lang="zh-CN" altLang="en-US" sz="{}"{}{} dirty="0">{}<a:latin typeface="{f}"/><a:ea typeface="{f}"/><a:cs typeface="{f}"/></a:rPr>"#,
        (size * 100.0).round() as i64,
        if bold { r#" b="1""# } else { "" },
        if italic { r#" i="1""# } else { "" },
        solid(color),
        f = deck::esc_attr(font)
    )
}

fn paragraph(p: &Para, defaults: (f64, &str, bool, bool, Option<&str>), font: &str) -> String {
    let (dsize, dcolor, dbold, ditalic, dalign) = defaults;
    let size = p.size.unwrap_or(dsize);
    let color = p.color.as_deref().unwrap_or(dcolor);
    let align = match p.align.as_deref().or(dalign) {
        Some("center") => r#" algn="ctr""#,
        Some("right") => r#" algn="r""#,
        _ => r#" algn="l""#,
    };
    let indent = if p.bullet {
        format!(r#" marL="{}" indent="{}""#, emu(BULLET_INDENT + p.level as f64 * LEVEL_INDENT), -emu(BULLET_INDENT))
    } else {
        format!(r#" marL="{}" indent="0""#, emu(p.level as f64 * LEVEL_INDENT))
    };
    let spc = match p.space_before {
        Some(v) => format!(r#"<a:spcBef><a:spcPts val="{}"/></a:spcBef>"#, (v * 100.0).round() as i64),
        None => String::new(),
    };
    let bullet = if p.bullet { format!(r#"<a:buClr><a:srgbClr val="{}"/></a:buClr><a:buFont typeface="Arial"/><a:buChar char="•"/>"#, rgb(color)) } else { "<a:buNone/>".to_string() };
    let runs: String = markdown::inline(&p.text)
        .into_iter()
        .filter(|s| !s.text.is_empty())
        .map(|s| format!("<a:r>{}<a:t>{}</a:t></a:r>", rpr(size, color, dbold || p.bold || s.bold, ditalic || p.italic || s.italic, font), esc(&s.text)))
        .collect();
    format!(r#"<a:p><a:pPr{indent}{align}>{spc}{bullet}</a:pPr>{runs}<a:endParaRPr lang="zh-CN" sz="{}" dirty="0"/></a:p>"#, (size * 100.0).round() as i64)
}

struct SlideCtx<'a> {
    style: &'a Style,
    base_dir: Option<&'a Path>,
    rels: Vec<(String, String, String)>,
    media: &'a mut HashMap<String, String>,
    parts: &'a mut Vec<(String, Vec<u8>)>,
    chart_no: &'a mut usize,
    next_id: u32,
}

impl SlideCtx<'_> {
    fn rel(&mut self, kind: &str, target: String) -> String {
        let id = format!("rId{}", self.rels.len() + 2);
        self.rels.push((id.clone(), kind.to_string(), target));
        id
    }

    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn element(&mut self, el: &Element) -> Result<String> {
        let style = self.style;
        let p = &style.palette;
        Ok(match el {
            Element::Text { x, y, w, h, fill, valign, font, padding, title, align, size, color, bold, italic, .. } => {
                let id = self.id();
                let font = font.clone().unwrap_or_else(|| style.fonts.body.clone());
                let pad = padding.unwrap_or(if fill.is_some() { 10.0 } else { 0.0 });
                let anchor = match valign.as_deref() {
                    Some("middle") => "ctr",
                    Some("bottom") => "b",
                    _ => "t",
                };
                let paras_spec = text_paras(el, style);
                let defaults = (size.unwrap_or(style.body_size), color.as_deref().unwrap_or(&p.text), *bold, *italic, align.as_deref());
                let paras: String = paras_spec.iter().map(|pp| paragraph(pp, defaults, &font)).collect();
                let fill_xml = fill.as_deref().map(solid).unwrap_or_else(|| "<a:noFill/>".into());
                let (name, nvpr, cnv) = if *title {
                    (format!("Title {id}"), r#"<p:nvPr><p:ph type="title"/></p:nvPr>"#.to_string(), r#"<p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>"#)
                } else {
                    (format!("TextBox {id}"), "<p:nvPr/>".to_string(), r#"<p:cNvSpPr txBox="1"/>"#)
                };
                format!(
                    r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{name}"/>{cnv}{nvpr}</p:nvSpPr><p:spPr>{}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>{fill_xml}</p:spPr><p:txBody><a:bodyPr wrap="square" lIns="{e}" tIns="{e}" rIns="{e}" bIns="{e}" anchor="{anchor}" rtlCol="0"><a:noAutofit/></a:bodyPr><a:lstStyle/>{paras}</p:txBody></p:sp>"#,
                    xfrm(*x, *y, *w, *h),
                    e = emu(pad)
                )
            }
            Element::Shape { x, y, w, h, shape, fill, line, line_width } => {
                let id = self.id();
                let (prst, av) = match shape.as_deref() {
                    Some("round") => ("roundRect", r#"<a:gd name="adj" fmla="val 8000"/>"#),
                    Some("ellipse") => ("ellipse", ""),
                    Some("line") => ("line", ""),
                    _ => ("rect", ""),
                };
                let fill_xml = match fill {
                    Some(c) if prst != "line" => solid(c),
                    _ => "<a:noFill/>".into(),
                };
                let ln = match line {
                    Some(c) => format!(r#"<a:ln w="{}">{}</a:ln>"#, emu(line_width.unwrap_or(1.0)), solid(c)),
                    None if prst == "line" => format!(r#"<a:ln w="{}">{}</a:ln>"#, emu(line_width.unwrap_or(1.0)), solid(fill.as_deref().unwrap_or(&p.muted))),
                    None => "<a:ln><a:noFill/></a:ln>".into(),
                };
                format!(r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Shape {id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}<a:prstGeom prst="{prst}"><a:avLst>{av}</a:avLst></a:prstGeom>{fill_xml}{ln}</p:spPr></p:sp>"#, xfrm(*x, *y, *w, *h))
            }
            Element::Image { x, y, w, h, path, fit } => {
                let full = match self.base_dir {
                    Some(d) if !Path::new(path).is_absolute() => d.join(path),
                    _ => Path::new(path).to_path_buf(),
                };
                let bytes = std::fs::read(&full).map_err(|_| EngineError::Invalid(format!("找不到图片 {}", full.display())))?;
                let (ext, iw, ih) = image_info(&bytes).ok_or_else(|| EngineError::Invalid(format!("{} 不是 PNG / JPEG / GIF", full.display())))?;
                let key = full.to_string_lossy().into_owned();
                let part = match self.media.get(&key) {
                    Some(p) => p.clone(),
                    None => {
                        let part = format!("ppt/media/image{}.{ext}", self.media.len() + 1);
                        self.parts.push((part.clone(), bytes));
                        self.media.insert(key, part.clone());
                        part
                    }
                };
                let rid = self.rel("http://schemas.openxmlformats.org/officeDocument/2006/relationships/image", format!("../media/{}", part.rsplit('/').next().unwrap()));
                let id = self.id();
                let (ia, ba) = (iw as f64 / ih.max(1) as f64, w / h);
                let (mut fx, mut fy, mut fw, mut fh, mut crop) = (*x, *y, *w, *h, String::new());
                if fit.as_deref() == Some("contain") {
                    if ia > ba {
                        fh = w / ia;
                        fy = y + (h - fh) / 2.0;
                    } else {
                        fw = h * ia;
                        fx = x + (w - fw) / 2.0;
                    }
                } else if (ia - ba).abs() > 0.01 {
                    if ia > ba {
                        let c = ((1.0 - ba / ia) / 2.0 * 100000.0).round() as i64;
                        crop = format!(r#"<a:srcRect l="{c}" r="{c}"/>"#);
                    } else {
                        let c = ((1.0 - ia / ba) / 2.0 * 100000.0).round() as i64;
                        crop = format!(r#"<a:srcRect t="{c}" b="{c}"/>"#);
                    }
                }
                let descr = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
                format!(
                    r#"<p:pic><p:nvPicPr><p:cNvPr id="{id}" name="Picture {id}" descr="{}"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{rid}"/>{crop}<a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>"#,
                    deck::esc_attr(descr),
                    xfrm(fx, fy, fw, fh)
                )
            }
            Element::Table { x, y, w, h, rows, header, size } => {
                let id = self.id();
                let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
                let cw = emu(w / cols as f64);
                let rh = emu(h / rows.len().max(1) as f64);
                let size = size.unwrap_or(style.body_size - 4.0);
                let font = style.fonts.body.clone();
                let grid: String = (0..cols).map(|_| format!(r#"<a:gridCol w="{cw}"/>"#)).collect();
                let border = |c: &str| format!(r#"<a:lnL w="6350">{s}</a:lnL><a:lnR w="6350">{s}</a:lnR><a:lnT w="6350">{s}</a:lnT><a:lnB w="6350">{s}</a:lnB>"#, s = solid(c));
                let mut trs = String::new();
                for (ri, row) in rows.iter().enumerate() {
                    let head = *header && ri == 0;
                    let fill = if head { solid(&p.accent) } else if ri % 2 == 0 { solid(&p.surface) } else { solid(&p.background) };
                    let color = if head { "#FFFFFF".to_string() } else { p.text.clone() };
                    let mut tcs = String::new();
                    for ci in 0..cols {
                        let v = row.get(ci).cloned().unwrap_or(serde_json::Value::Null);
                        let text = match &v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        let numeric = v.is_number() || (!text.is_empty() && text.trim_end_matches('%').replace(',', "").parse::<f64>().is_ok());
                        let algn = if numeric && !head { r#" algn="r""# } else { "" };
                        tcs.push_str(&format!(
                            r#"<a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:pPr{algn}/><a:r>{}<a:t>{}</a:t></a:r></a:p></a:txBody><a:tcPr marL="91440" marR="91440" marT="45720" marB="45720" anchor="ctr">{}{fill}</a:tcPr></a:tc>"#,
                            rpr(size, &color, head, false, &font),
                            esc(&text),
                            border("#E2E8F0")
                        ));
                    }
                    trs.push_str(&format!(r#"<a:tr h="{rh}">{tcs}</a:tr>"#));
                }
                format!(
                    r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="{id}" name="Table {id}"/><p:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></p:cNvGraphicFramePr><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="{}" bandRow="1"/><a:tblGrid>{grid}</a:tblGrid>{trs}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
                    emu(*x),
                    emu(*y),
                    emu(*w),
                    emu(*h),
                    if *header { 1 } else { 0 }
                )
            }
            Element::Chart { x, y, w, h, chart } => {
                *self.chart_no += 1;
                let n = *self.chart_no;
                let chart_part = format!("ppt/charts/chart{n}.xml");
                let book = format!("ppt/embeddings/Microsoft_Excel_Worksheet{n}.xlsx");
                self.parts.push((chart_part.clone(), chart_xml(chart, style).into_bytes()));
                self.parts.push((
                    format!("ppt/charts/_rels/chart{n}.xml.rels"),
                    format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/package" Target="../embeddings/Microsoft_Excel_Worksheet{n}.xlsx"/></Relationships>"#)
                    .into_bytes(),
                ));
                self.parts.push((book, chart_workbook(chart)?));
                let rid = self.rel("http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart", format!("../charts/chart{n}.xml"));
                let id = self.id();
                format!(
                    r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="{id}" name="Chart {id}"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="{rid}"/></a:graphicData></a:graphic></p:graphicFrame>"#,
                    emu(*x),
                    emu(*y),
                    emu(*w),
                    emu(*h)
                )
            }
        })
    }
}

fn text_paras(el: &Element, style: &Style) -> Vec<Para> {
    match el {
        Element::Text { text, paragraphs, .. } => {
            if !paragraphs.is_empty() {
                paragraphs.clone()
            } else {
                let _ = style;
                text.as_deref().unwrap_or("").split('\n').map(|t| Para { text: t.into(), ..Default::default() }).collect()
            }
        }
        _ => Vec::new(),
    }
}

// ───────── 图表 ─────────

fn chart_text(size: f64, color: &str, bold: bool) -> String {
    format!(r#"<c:txPr><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr sz="{}" b="{}">{}</a:defRPr></a:pPr><a:endParaRPr lang="zh-CN"/></a:p></c:txPr>"#, (size * 100.0) as i64, if bold { 1 } else { 0 }, solid(color))
}

fn str_ref(f: &str, vals: &[String]) -> String {
    let pts: String = vals.iter().enumerate().map(|(i, v)| format!(r#"<c:pt idx="{i}"><c:v>{}</c:v></c:pt>"#, esc(v))).collect();
    format!(r#"<c:strRef><c:f>{f}</c:f><c:strCache><c:ptCount val="{}"/>{pts}</c:strCache></c:strRef>"#, vals.len())
}

fn num_ref(f: &str, vals: &[f64], fmt: &str) -> String {
    let pts: String = vals.iter().enumerate().map(|(i, v)| format!(r#"<c:pt idx="{i}"><c:v>{}</c:v></c:pt>"#, crate::formula::fmt_num(*v))).collect();
    format!(r#"<c:numRef><c:f>{f}</c:f><c:numCache><c:formatCode>{}</c:formatCode><c:ptCount val="{}"/>{pts}</c:numCache></c:numRef>"#, esc(fmt), vals.len())
}

pub fn chart_xml(chart: &ChartSpec, style: &Style) -> String {
    let p = &style.palette;
    let colors = style.series_colors();
    let n = chart.categories.len();
    let cat_f = format!("Sheet1!$A$2:$A${}", n + 1);
    let fmt = chart.number_format.clone().unwrap_or_else(|| "General".into());
    let pie = matches!(chart.kind.as_str(), "pie" | "doughnut");
    let labels = if chart.labels || pie {
        format!(
            r#"<c:dLbls>{}<c:spPr><a:noFill/><a:ln><a:noFill/></a:ln></c:spPr>{}<c:showLegendKey val="0"/><c:showVal val="{}"/><c:showCatName val="0"/><c:showSerName val="0"/><c:showPercent val="{}"/><c:showBubbleSize val="0"/></c:dLbls>"#,
            if chart.number_format.is_some() { format!(r#"<c:numFmt formatCode="{}" sourceLinked="0"/>"#, esc(&fmt)) } else { String::new() },
            chart_text(11.0, if pie { "#FFFFFF" } else { &p.text }, pie),
            if chart.labels { 1 } else { 0 },
            if pie && !chart.labels { 1 } else { 0 }
        )
    } else {
        String::new()
    };
    let series: Vec<&deck::Series> = if pie { chart.series.iter().take(1).collect() } else { chart.series.iter().collect() };
    let mut sers = String::new();
    for (i, s) in series.iter().enumerate() {
        let col = crate::formula::index_to_col(i as u32 + 2);
        let color = colors.get(i % colors.len()).cloned().unwrap_or_else(|| p.accent.clone());
        let tx = format!(r#"<c:tx>{}</c:tx>"#, str_ref(&format!("Sheet1!${col}$1"), &[s.name.clone()]));
        let cat = format!("<c:cat>{}</c:cat>", str_ref(&cat_f, &chart.categories));
        let val = format!("<c:val>{}</c:val>", num_ref(&format!("Sheet1!${col}$2:${col}${}", n + 1), &s.values, &fmt));
        let body = match chart.kind.as_str() {
            "line" => format!(
                r#"{tx}<c:spPr><a:ln w="28575" cap="rnd"><a:solidFill><a:srgbClr val="{c}"/></a:solidFill><a:round/></a:ln></c:spPr><c:marker><c:symbol val="circle"/><c:size val="6"/><c:spPr><a:solidFill><a:srgbClr val="{c}"/></a:solidFill><a:ln><a:noFill/></a:ln></c:spPr></c:marker>{labels}{cat}{val}<c:smooth val="0"/>"#,
                c = rgb(&color)
            ),
            "pie" | "doughnut" => {
                let dpts: String = (0..n).map(|k| format!(r#"<c:dPt><c:idx val="{k}"/><c:bubble3D val="0"/><c:spPr>{}<a:ln w="12700">{}</a:ln></c:spPr></c:dPt>"#, solid(&colors[k % colors.len()]), solid(&p.background))).collect();
                format!("{tx}{dpts}{labels}{cat}{val}")
            }
            _ => format!(r#"{tx}<c:spPr>{}</c:spPr><c:invertIfNegative val="0"/>{labels}{cat}{val}"#, solid(&color)),
        };
        sers.push_str(&format!(r#"<c:ser><c:idx val="{i}"/><c:order val="{i}"/>{body}</c:ser>"#));
    }
    let ax = r#"<c:axId val="111111111"/><c:axId val="222222222"/>"#;
    let plot = match chart.kind.as_str() {
        "line" => format!(r#"<c:lineChart><c:grouping val="standard"/><c:varyColors val="0"/>{sers}<c:marker val="1"/>{ax}</c:lineChart>"#),
        "pie" => format!(r#"<c:pieChart><c:varyColors val="1"/>{sers}<c:firstSliceAng val="0"/></c:pieChart>"#),
        "doughnut" => format!(r#"<c:doughnutChart><c:varyColors val="1"/>{sers}<c:firstSliceAng val="0"/><c:holeSize val="55"/></c:doughnutChart>"#),
        kind => format!(r#"<c:barChart><c:barDir val="{}"/><c:grouping val="clustered"/><c:varyColors val="0"/>{sers}<c:gapWidth val="80"/>{ax}</c:barChart>"#, if kind == "bar" { "bar" } else { "col" }),
    };
    let axes = if pie {
        String::new()
    } else {
        let (cp, vp) = if chart.kind == "bar" { ("l", "b") } else { ("b", "l") };
        let line = r#"<c:spPr><a:ln w="9525"><a:solidFill><a:srgbClr val="CBD5E1"/></a:solidFill></a:ln></c:spPr>"#;
        format!(
            r#"<c:catAx><c:axId val="111111111"/><c:scaling><c:orientation val="{}"/></c:scaling><c:delete val="0"/><c:axPos val="{cp}"/><c:numFmt formatCode="General" sourceLinked="1"/><c:majorTickMark val="none"/><c:minorTickMark val="none"/><c:tickLblPos val="nextTo"/>{line}{}<c:crossAx val="222222222"/><c:crosses val="autoZero"/><c:auto val="1"/><c:lblAlgn val="ctr"/><c:lblOffset val="100"/><c:noMultiLvlLbl val="0"/></c:catAx><c:valAx><c:axId val="222222222"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="{vp}"/><c:majorGridlines><c:spPr><a:ln w="6350"><a:solidFill><a:srgbClr val="E2E8F0"/></a:solidFill></a:ln></c:spPr></c:majorGridlines><c:numFmt formatCode="{}" sourceLinked="0"/><c:majorTickMark val="none"/><c:minorTickMark val="none"/><c:tickLblPos val="nextTo"/><c:spPr><a:ln><a:noFill/></a:ln></c:spPr>{}<c:crossAx val="111111111"/><c:crosses val="autoZero"/><c:crossBetween val="between"/></c:valAx>"#,
            // 条形图分类从上往下排
            if chart.kind == "bar" { "maxMin" } else { "minMax" },
            chart_text(12.0, &p.muted, false),
            esc(&fmt),
            chart_text(11.0, &p.muted, false)
        )
    };
    let title = match &chart.title {
        Some(t) if !t.trim().is_empty() => format!(
            r#"<c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr sz="1400" b="1">{}</a:defRPr></a:pPr><a:r><a:rPr lang="zh-CN" sz="1400" b="1">{}</a:rPr><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title><c:autoTitleDeleted val="0"/>"#,
            solid(&p.text),
            solid(&p.text),
            esc(t)
        ),
        _ => r#"<c:autoTitleDeleted val="1"/>"#.into(),
    };
    let legend = if series.len() > 1 || pie { format!(r#"<c:legend><c:legendPos val="b"/><c:overlay val="0"/>{}</c:legend>"#, chart_text(12.0, &p.muted, false)) } else { String::new() };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><c:date1904 val="0"/><c:lang val="zh-CN"/><c:roundedCorners val="0"/><c:chart>{title}<c:plotArea><c:layout/>{plot}{axes}<c:spPr><a:noFill/><a:ln><a:noFill/></a:ln></c:spPr></c:plotArea>{legend}<c:plotVisOnly val="1"/><c:dispBlanksAs val="gap"/></c:chart><c:spPr><a:noFill/><a:ln><a:noFill/></a:ln></c:spPr><c:txPr><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr><a:latin typeface="{f}"/><a:ea typeface="{f}"/></a:defRPr></a:pPr><a:endParaRPr lang="zh-CN"/></a:p></c:txPr><c:externalData r:id="rId1"><c:autoUpdate val="0"/></c:externalData></c:chartSpace>"#,
        f = deck::esc_attr(&style.fonts.body)
    )
}

/// 图表的数据工作簿（在 PowerPoint 里“编辑数据”时打开）
fn chart_workbook(chart: &ChartSpec) -> Result<Vec<u8>> {
    let mut rows: Vec<Vec<serde_json::Value>> = vec![std::iter::once(serde_json::Value::String(String::new())).chain(chart.series.iter().map(|s| serde_json::Value::String(s.name.clone()))).collect()];
    for (i, c) in chart.categories.iter().enumerate() {
        let mut row = vec![serde_json::Value::String(c.clone())];
        row.extend(chart.series.iter().map(|s| s.values.get(i).map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null)));
        rows.push(row);
    }
    let mut wb = crate::xlsx::Workbook::open(super::templates::xlsx(&["Sheet1".into()])?)?;
    let ops: Vec<crate::xlsx::SheetOp> = serde_json::from_value(serde_json::json!([{ "op": "set_range", "start": "A1", "values": rows }])).map_err(|e| EngineError::Invalid(e.to_string()))?;
    wb.apply(&ops)?;
    wb.save()
}

// ───────── 整套 ─────────

pub fn render(spec: &DeckSpec, base_dir: Option<&Path>) -> Result<Vec<u8>> {
    let style = &spec.style;
    let (pw, ph) = style.page_size();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    let mut media: HashMap<String, String> = HashMap::new();
    let mut chart_no = 0usize;
    let mut overrides = String::new();
    let has_notes = spec.pages.iter().any(|p| p.notes.as_deref().map(|n| !n.trim().is_empty()).unwrap_or(false));
    let mut slide_ids = String::new();
    let mut pres_rels = String::new();

    for (i, page) in spec.pages.iter().enumerate() {
        let n = i + 1;
        let mut ctx = SlideCtx { style, base_dir, rels: Vec::new(), media: &mut media, parts: &mut parts, chart_no: &mut chart_no, next_id: 1 };
        let mut shapes = String::new();
        for el in layout(page, style) {
            shapes.push_str(&ctx.element(&el)?);
        }
        let mut rels = String::from(r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>"#);
        for (id, kind, target) in &ctx.rels {
            rels.push_str(&format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"/>"#));
        }
        if let Some(notes) = page.notes.as_deref().filter(|n| !n.trim().is_empty()) {
            let rid = format!("rId{}", ctx.rels.len() + 2);
            rels.push_str(&format!(r#"<Relationship Id="{rid}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide{n}.xml"/>"#));
            parts.push((format!("ppt/notesSlides/notesSlide{n}.xml"), notes_slide(notes).into_bytes()));
            parts.push((
                format!("ppt/notesSlides/_rels/notesSlide{n}.xml.rels"),
                format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster" Target="../notesMasters/notesMaster1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="../slides/slide{n}.xml"/></Relationships>"#)
                .into_bytes(),
            ));
            overrides.push_str(&format!(r#"<Override PartName="/ppt/notesSlides/notesSlide{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"/>"#));
        }
        let bg = page.background.as_deref().unwrap_or(&style.palette.background);
        parts.push((
            format!("ppt/slides/slide{n}.xml"),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld {NS}><p:cSld><p:bg><p:bgPr>{}<a:effectLst/></p:bgPr></p:bg><p:spTree>{GRP}{shapes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#, solid(bg)).into_bytes(),
        ));
        parts.push((
            format!("ppt/slides/_rels/slide{n}.xml.rels"),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#).into_bytes(),
        ));
        overrides.push_str(&format!(r#"<Override PartName="/ppt/slides/slide{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#));
        slide_ids.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + n, 10 + n));
        pres_rels.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{n}.xml"/>"#, 10 + n));
    }
    for k in 1..=chart_no {
        overrides.push_str(&format!(r#"<Override PartName="/ppt/charts/chart{k}.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>"#));
    }
    if has_notes {
        overrides.push_str(r#"<Override PartName="/ppt/notesMasters/notesMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml"/><Override PartName="/ppt/theme/theme2.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#);
        parts.push(("ppt/notesMasters/notesMaster1.xml".into(), notes_master().into_bytes()));
        parts.push((
            "ppt/notesMasters/_rels/notesMaster1.xml.rels".into(),
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme2.xml"/></Relationships>"#
                .to_vec(),
        ));
        parts.push(("ppt/theme/theme2.xml".into(), theme(style, "Notes").into_bytes()));
        pres_rels.push_str(r#"<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster" Target="notesMasters/notesMaster1.xml"/>"#);
    }
    let title = spec.title.clone().unwrap_or_default();
    let notes_list = if has_notes { r#"<p:notesMasterIdLst><p:notesMasterId r:id="rId9"/></p:notesMasterIdLst>"# } else { "" };
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let fixed: Vec<(String, String)> = vec![
        (
            "[Content_Types].xml".into(),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Default Extension="gif" ContentType="image/gif"/><Default Extension="xlsx" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/ppt/presProps.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presProps+xml"/><Override PartName="/ppt/viewProps.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml"/><Override PartName="/ppt/tableStyles.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>{overrides}</Types>"#),
        ),
        (
            "_rels/.rels".into(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#.into(),
        ),
        (
            "docProps/core.xml".into(),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>{}</dc:title><dc:creator>OfficePilot</dc:creator><dcterms:created xsi:type="dcterms:W3CDTF">{now}</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">{now}</dcterms:modified></cp:coreProperties>"#, esc(&title)),
        ),
        (
            "docProps/app.xml".into(),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>OfficePilot</Application><Slides>{}</Slides></Properties>"#, spec.pages.len()),
        ),
        (
            "ppt/presentation.xml".into(),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation {NS} saveSubsetFonts="1"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>{notes_list}<p:sldIdLst>{slide_ids}</p:sldIdLst><p:sldSz cx="{}" cy="{}"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#, emu(pw), emu(ph)),
        ),
        (
            "ppt/_rels/presentation.xml.rels".into(),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps" Target="presProps.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/viewProps" Target="viewProps.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableStyles" Target="tableStyles.xml"/>{pres_rels}</Relationships>"#),
        ),
        ("ppt/presProps.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentationPr {NS}/>"#)),
        ("ppt/viewProps.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:viewPr {NS}><p:normalViewPr><p:restoredLeft sz="15620"/><p:restoredTop sz="94660"/></p:normalViewPr><p:gridSpacing cx="76200" cy="76200"/></p:viewPr>"#)),
        ("ppt/tableStyles.xml".into(), r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" def="{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}"/>"#.into()),
        ("ppt/theme/theme1.xml".into(), theme(style, "OfficePilot")),
        ("ppt/slideMasters/slideMaster1.xml".into(), master(style, pw)),
        (
            "ppt/slideMasters/_rels/slideMaster1.xml.rels".into(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#.into(),
        ),
        ("ppt/slideLayouts/slideLayout1.xml".into(), layout_part(style, pw)),
        (
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels".into(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#.into(),
        ),
    ];
    let mut all: Vec<(String, Vec<u8>)> = fixed.into_iter().map(|(n, t)| (n, t.into_bytes())).collect();
    all.extend(parts);
    let _ = ph;
    write_zip(&all)
}
