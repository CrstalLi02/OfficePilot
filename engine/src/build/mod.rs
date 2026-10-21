//! 新建文件：Word / Excel 的空白模板，以及从页面规格生成 PPT。
//!
//! 新文档都从模板开始，再用引擎的编辑操作填内容，所以新建和修改走同一套代码。

pub mod deck;
pub mod deck_render;
pub mod templates;
pub mod zipw;
