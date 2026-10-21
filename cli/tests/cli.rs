//! 运行真实的 officepilot 二进制，检查输出和生成的文件

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

fn op(dir: &Path, args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_officepilot")).current_dir(dir).arg("--json").args(args).output().expect("运行失败");
    let text = String::from_utf8_lossy(&out.stdout);
    let value: Value = serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("不是 JSON：{text}\n{}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code().unwrap_or(-1), value)
}

fn ok(dir: &Path, args: &[&str]) -> Value {
    let (code, v) = op(dir, args);
    assert_eq!(code, 0, "{args:?} → {v}");
    assert_eq!(v["ok"], true);
    v
}

#[test]
fn word_create_read_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::write(d.join("notes.md"), "# 周报\n\n本周完成 **引擎**。\n\n- 第一条\n- 第二条\n\n| 指标 | 值 |\n|---|---|\n| 用例 | 80 |\n").unwrap();
    ok(d, &["create", "--type", "docx", "--from", "notes.md", "--title", "项目周报", "--out", "w.docx"]);
    let read = ok(d, &["read", "w.docx"]);
    let blocks = read["blocks"].as_array().unwrap();
    assert_eq!(blocks[1]["kind"], "heading");
    assert_eq!(blocks[2]["text"], "本周完成 引擎。");
    let revision = read["revision"].as_str().unwrap().to_string();

    // 预览不写文件
    let before = std::fs::read(d.join("w.docx")).unwrap();
    let dry = ok(d, &["apply", "w.docx", "--ops", r#"[{"op":"replace_text","block":2,"find":"引擎","replace":"文档引擎"}]"#, "--dry-run"]);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(std::fs::read(d.join("w.docx")).unwrap(), before);

    ok(d, &["apply", "w.docx", "--ops", r#"[{"op":"replace_text","block":2,"find":"引擎","replace":"文档引擎"},{"op":"set_cell","block":5,"row":1,"col":1,"text":"96"}]"#, "--track", "--revision", &revision]);
    let read = ok(d, &["read", "w.docx"]);
    assert_eq!(read["blocks"][2]["text"], "本周完成 文档引擎。");
    assert_eq!(read["blocks"][2]["has_revisions"], true);

    // 过期的 revision 被拒绝；错误带原文
    let (code, v) = op(d, &["apply", "w.docx", "--ops", r#"[{"op":"delete","block":1}]"#, "--revision", &revision]);
    assert_eq!(code, 1);
    assert!(v["error"].as_str().unwrap().contains("已经变化"), "{v}");
    let (code, v) = op(d, &["apply", "w.docx", "--ops", r#"[{"op":"replace_text","block":1,"find":"不存在","replace":"x"}]"#]);
    assert_eq!(code, 1);
    assert!(v["error"].as_str().unwrap().contains("周报"), "{v}");
    // 操作格式错误给出指引
    let (code, v) = op(d, &["apply", "w.docx", "--ops", r#"[{"op":"replace_txt"}]"#]);
    assert_eq!(code, 1);
    assert!(v["hint"].as_str().unwrap().contains("guide docs"), "{v}");
}

#[test]
fn excel_create_formula_convert() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let spec = json!({"sheets":[{"name":"销售","rows":[["地区","金额"],["华东",130],["华北",120],["合计","=SUM(B2:B3)"]],"formats":[{"range":"B2:B4","number_format":"#,##0"}],"charts":[{"type":"column","categories":"销售!A2:A3","series":[{"name":"金额","values":"销售!B2:B3"}]}]}]});
    std::fs::write(d.join("data.json"), spec.to_string()).unwrap();
    ok(d, &["create", "--type", "xlsx", "--from", "data.json", "--out", "s.xlsx"]);
    let info = ok(d, &["info", "s.xlsx"]);
    assert_eq!(info["info"]["sheets"][0]["charts"], 1);
    ok(d, &["apply", "s.xlsx", "--ops", r#"[{"op":"set","sheet":"销售","cell":"B2","value":200}]"#]);
    let read = ok(d, &["read", "s.xlsx", "--range", "A4:B4"]);
    let cell = &read["view"]["rows"][0][1][1];
    assert_eq!(cell["value"], 320, "依赖的合计应重新计算：{read}");
    assert_eq!(cell["formula"], "=SUM(B2:B3)");

    ok(d, &["convert", "s.xlsx", "--to", "csv"]);
    let csv = std::fs::read_to_string(d.join("s.csv")).unwrap();
    assert!(csv.contains("合计,320"), "{csv}");
    std::fs::write(d.join("in.csv"), "名称,数量\n苹果,3\n\"梨,大\",05\n").unwrap();
    ok(d, &["convert", "in.csv", "--to", "xlsx"]);
    let read = ok(d, &["read", "in.xlsx"]);
    assert_eq!(read["view"]["rows"][2][1][1]["value"], "05", "以 0 开头的编号保持文字");
}

#[test]
fn convert_roundtrips() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::write(d.join("a.md"), "## 标题\n\n段落\n\n- 一\n- 二\n").unwrap();
    ok(d, &["convert", "a.md", "--to", "docx"]);
    ok(d, &["convert", "a.docx", "--to", "md", "--out", "b.md"]);
    let md = std::fs::read_to_string(d.join("b.md")).unwrap();
    assert_eq!(md, "## 标题\n\n段落\n\n- 一\n- 二\n");
    ok(d, &["convert", "a.md", "--to", "html"]);
    assert!(std::fs::read_to_string(d.join("a.html")).unwrap().contains("<ul>\n<li"));
    let (code, v) = op(d, &["convert", "a.docx", "--to", "pdf"]);
    assert_eq!(code, 1);
    assert!(v["hint"].is_string());
}

#[test]
fn deck_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::write(d.join("style.json"), r##"{"palette":{"accent":"#0F766E"}}"##).unwrap();
    std::fs::write(d.join("outline.json"), r#"{"title":"汇报","pages":[{"title":"封面"},{"title":"要点"},{"title":"数据"}]}"#).unwrap();
    ok(d, &["deck", "start", "deck", "--style", "style.json", "--outline", "outline.json"]);
    ok(d, &["deck", "page", "deck", "1", r#"{"layout":"cover","title":"封面","subtitle":"副标题","notes":"开场"}"#]);
    // 放不下的页被拒绝且不保存
    let long: Vec<String> = (0..20).map(|i| format!("这是第 {i} 条要点，内容写得比较长，用来撑满整个文本框看看会不会溢出")).collect();
    let (code, v) = op(d, &["deck", "page", "deck", "2", &json!({"layout":"bullets","title":"要点","bullets":long}).to_string()]);
    assert_eq!(code, 1);
    assert_eq!(v["saved"], false);
    assert!(v["findings"].as_array().unwrap().iter().any(|f| f["code"] == "overflow"));
    ok(d, &["deck", "page", "deck", "2", r#"{"layout":"bullets","title":"要点","bullets":["一","二"]}"#]);
    let status = ok(d, &["deck", "status", "deck"]);
    assert_eq!(status["missing"], json!([3]));
    let (code, _) = op(d, &["deck", "build", "deck", "--out", "talk.pptx"]);
    assert_eq!(code, 1, "缺页时不组装");
    ok(d, &["deck", "page", "deck", "3", r#"{"layout":"chart","title":"数据","chart":{"type":"column","categories":["A","B"],"series":[{"name":"x","values":[1,2]}]}}"#]);
    ok(d, &["deck", "build", "deck", "--out", "talk.pptx"]);
    let read = ok(d, &["read", "talk.pptx"]);
    assert_eq!(read["slides"].as_array().unwrap().len(), 3);
    assert_eq!(read["slides"][0]["notes"], "开场");
    assert_eq!(read["slides"][2]["shapes"].as_array().unwrap().iter().filter(|s| s["kind"] == "chart").count(), 1);
    // 重做一页并自动重新组装
    ok(d, &["deck", "replace", "deck", "2", r#"{"layout":"bullets","title":"要点","bullets":["新的一","新的二"]}"#]);
    let read = ok(d, &["read", "talk.pptx", "--from", "2", "--to", "2"]);
    assert!(read["slides"][0]["shapes"].as_array().unwrap().iter().any(|s| s["text"].as_str().unwrap_or("").contains("新的一")));
    // 生成的 PPT 也能用 apply 改
    ok(d, &["apply", "talk.pptx", "--ops", r#"[{"op":"set_text","slide":1,"shape":"title","text":"新封面"}]"#]);
    ok(d, &["slides", "audit", "talk.pptx"]);
}

/// 说明文档里的示例必须能被引擎解析（防止文档和代码不一致）
#[test]
fn guide_examples_parse() {
    for (topic, kind) in [("docs", "docx"), ("sheet", "xlsx"), ("slides", "pptx")] {
        let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("guides").join(format!("{topic}.md"))).unwrap();
        let block = text.split("```json").nth(1).unwrap().split("```").next().unwrap();
        let ops: Value = serde_json::from_str(block).unwrap_or_else(|e| panic!("{topic} 示例不是合法 JSON：{e}"));
        for o in ops.as_array().unwrap() {
            let r = match kind {
                "docx" => serde_json::from_value::<officepilot_engine::docx::DocOp>(o.clone()).map(|_| ()),
                "xlsx" => serde_json::from_value::<officepilot_engine::xlsx::SheetOp>(o.clone()).map(|_| ()),
                _ => serde_json::from_value::<officepilot_engine::pptx::SlideOp>(o.clone()).map(|_| ()),
            };
            r.unwrap_or_else(|e| panic!("{topic} 示例 {o} 解析失败：{e}"));
        }
    }
    let deck = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("guides/deck.md")).unwrap();
    let style = deck.split("## style.json\n```json").nth(1).unwrap().split("```").next().unwrap();
    serde_json::from_str::<officepilot_engine::build::deck::Style>(style).unwrap();
    let outline = deck.split("## outline.json\n```json").nth(1).unwrap().split("```").next().unwrap();
    serde_json::from_str::<officepilot_engine::build::deck::Outline>(outline).unwrap();
}

#[test]
fn guide_and_capabilities() {
    let tmp = tempfile::tempdir().unwrap();
    let v = ok(tmp.path(), &["guide", "excel"]);
    assert_eq!(v["topic"], "sheet");
    let (code, v) = op(tmp.path(), &["guide", "nope"]);
    assert_eq!(code, 1);
    assert!(v["hint"].as_str().unwrap().contains("deck"));
    let caps = ok(tmp.path(), &["capabilities"]);
    assert_eq!(caps["render"], false);
}
