use std::collections::HashMap;
use tokio::sync::RwLock;

use super::tool::{DynTool, OfficeTool};

pub struct ToolRegistry {
    tools: RwLock<HashMap<String, DynTool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(&self, tool: DynTool) {
        let name = tool.name().to_string();
        self.tools.write().await.insert(name, tool);
    }

    pub async fn get(&self, name: &str) -> Option<DynTool> {
        self.tools.read().await.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<DynTool> {
        let tools = self.tools.read().await;
        let mut list: Vec<DynTool> = tools.values().cloned().collect();
        list.sort_by(|a, b| a.name().cmp(b.name()));
        list
    }
}

use once_cell::sync::Lazy;
pub static REGISTRY: Lazy<ToolRegistry> = Lazy::new(ToolRegistry::new);
