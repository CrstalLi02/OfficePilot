//! 开发用：读取或编辑一个 Office 文件。
//!   cargo run --example apply -- read  input.docx
//!   cargo run --example apply -- apply input.docx ops.json output.docx [--track]

use officepilot_engine::{docx, pptx, xlsx, Kind};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, input) = (args[0].as_str(), &args[1]);
    let bytes = std::fs::read(input).expect("读取文件失败");
    let kind = Kind::from_name(input).expect("不支持的扩展名");
    let result: Result<(), officepilot_engine::EngineError> = (|| {
        match (mode, kind) {
            ("read", Kind::Docx) => print!("{}", docx::format_blocks(&docx::Document::open(bytes)?.blocks()?, 200)),
            ("read", Kind::Xlsx) => {
                let wb = xlsx::Workbook::open(bytes)?;
                for s in wb.sheets.clone() {
                    print!("{}", xlsx::format_view(&wb.read(Some(&s.name), None, 40)?, 40));
                }
            }
            ("read", Kind::Pptx) => print!("{}", pptx::format_slides(&pptx::Deck::open(bytes)?.slides()?, 200)),
            ("apply", _) => {
                let ops = std::fs::read_to_string(&args[2]).expect("读取操作失败");
                let track = args.iter().any(|a| a == "--track");
                let (out, report) = match kind {
                    Kind::Docx => {
                        let mut d = docx::Document::open(bytes)?;
                        let r = d.apply(&serde_json::from_str::<Vec<_>>(&ops).expect("操作格式"), &docx::ApplyOptions { track_changes: track, ..Default::default() })?;
                        (d.save()?, r)
                    }
                    Kind::Xlsx => {
                        let mut w = xlsx::Workbook::open(bytes)?;
                        let r = w.apply(&serde_json::from_str::<Vec<_>>(&ops).expect("操作格式"))?;
                        (w.save()?, r)
                    }
                    Kind::Pptx => {
                        let mut d = pptx::Deck::open(bytes)?;
                        let r = d.apply(&serde_json::from_str::<Vec<_>>(&ops).expect("操作格式"))?;
                        (d.save()?, r)
                    }
                };
                std::fs::write(&args[3], out).expect("写入失败");
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
            }
            _ => panic!("未知模式"),
        }
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("错误：{e}");
        std::process::exit(1);
    }
}
