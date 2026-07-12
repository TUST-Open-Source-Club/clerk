use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

/// 工具参数定义
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParameterSchema {
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<ParameterSchema>>,
}

impl ParameterSchema {
    pub fn string(description: impl Into<String>) -> Self {
        Self {
            param_type: "string".to_string(),
            description: description.into(),
            enum_values: None,
            items: None,
        }
    }

    pub fn integer(description: impl Into<String>) -> Self {
        Self {
            param_type: "integer".to_string(),
            description: description.into(),
            enum_values: None,
            items: None,
        }
    }

    pub fn boolean(description: impl Into<String>) -> Self {
        Self {
            param_type: "boolean".to_string(),
            description: description.into(),
            enum_values: None,
            items: None,
        }
    }

    pub fn array(items: ParameterSchema, description: impl Into<String>) -> Self {
        Self {
            param_type: "array".to_string(),
            description: description.into(),
            enum_values: None,
            items: Some(Box::new(items)),
        }
    }
}

/// 工具 JSON Schema（OpenAI functions 格式）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolSchema {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    pub fn with_string(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        required: bool,
    ) -> Self {
        self.add_param(name, ParameterSchema::string(description), required)
    }

    pub fn with_integer(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        required: bool,
    ) -> Self {
        self.add_param(name, ParameterSchema::integer(description), required)
    }

    pub fn with_boolean(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        required: bool,
    ) -> Self {
        self.add_param(name, ParameterSchema::boolean(description), required)
    }

    pub fn with_array(
        self,
        name: impl Into<String>,
        items: ParameterSchema,
        description: impl Into<String>,
        required: bool,
    ) -> Self {
        self.add_param(name, ParameterSchema::array(items, description), required)
    }

    fn add_param(
        mut self,
        name: impl Into<String>,
        schema: ParameterSchema,
        required: bool,
    ) -> Self {
        let name = name.into();
        let properties = self
            .parameters
            .get_mut("properties")
            .and_then(|v| v.as_object_mut())
            .expect("parameters 必须是 object 类型");
        properties.insert(name.clone(), serde_json::to_value(schema).unwrap());

        if required {
            let required = self
                .parameters
                .get_mut("required")
                .and_then(|v| v.as_array_mut())
                .expect("parameters.required 必须是数组");
            required.push(Value::String(name));
        }
        self
    }

    /// 转换为 OpenAI ToolDefinition
    pub fn into_tool_definition(self) -> crate::agent::llm::ToolDefinition {
        crate::agent::llm::ToolDefinition {
            tool_type: "function".to_string(),
            function: crate::agent::llm::FunctionDefinition {
                name: self.name,
                description: self.description,
                parameters: self.parameters,
            },
        }
    }
}

/// 工具执行上下文
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    pub working_dir: std::path::PathBuf,
}

/// 工具执行结果
#[derive(Debug, Clone)]
pub enum ToolResult {
    Text(String),
    Error(String),
    Json(Value),
}

impl ToolResult {
    pub fn to_string_for_model(&self) -> String {
        match self {
            ToolResult::Text(s) => s.clone(),
            ToolResult::Error(e) => format!("错误: {}", e),
            ToolResult::Json(v) => v.to_string(),
        }
    }
}

impl std::fmt::Display for ToolResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string_for_model())
    }
}

/// 工具 trait
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    async fn execute(
        &self,
        args: HashMap<String, Value>,
        ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult>;
}

/// 从参数中获取字符串
pub fn get_string(args: &HashMap<String, Value>, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("缺少或类型错误参数: {}", key))
}

/// 从参数中获取布尔值
pub fn get_bool(args: &HashMap<String, Value>, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// 从参数中获取整数
pub fn get_i64(args: &HashMap<String, Value>, key: &str, default: i64) -> i64 {
    args.get(key).and_then(|v| v.as_i64()).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_schema_builder() {
        let schema = ToolSchema::new("read_file", "读取文件内容")
            .with_string("path", "文件路径", true)
            .with_boolean("limit", "是否限制长度", false);

        assert_eq!(schema.name, "read_file");
        let props = schema
            .parameters
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(props.contains_key("path"));
        let required = schema
            .parameters
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str().unwrap(), "path");
    }

    #[test]
    fn test_get_string_and_bool() {
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("/tmp/a".to_string()));
        args.insert("limit".to_string(), Value::Bool(true));

        assert_eq!(get_string(&args, "path").unwrap(), "/tmp/a");
        assert!(get_bool(&args, "limit", false));
        assert!(!get_bool(&args, "missing", false));
    }
}
