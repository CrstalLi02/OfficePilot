//! 从一组 (部件名, 内容) 写出 zip 包

use std::io::Write;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::{EngineError, Result};

pub fn write_zip(parts: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in parts {
        w.start_file(name.as_str(), opts).map_err(|e| EngineError::Format(e.to_string()))?;
        w.write_all(data).map_err(|e| EngineError::Format(e.to_string()))?;
    }
    Ok(w.finish().map_err(|e| EngineError::Format(e.to_string()))?.into_inner())
}
