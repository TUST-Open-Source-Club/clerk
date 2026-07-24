use crate::agent::llm::Message;
use crate::config::ContextConfig;

/// 会话上下文：维护当前对话消息与系统提示词
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub max_history: usize,
}

impl SessionContext {
    /// 创建会话上下文，默认保留最近 50 条历史消息。
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            messages: Vec::new(),
            max_history: 50,
        }
    }

    /// 追加一条消息并按 max_history 裁剪历史。
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
        self.trim_history();
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.system_prompt = prompt.into();
    }

    /// 组装发送给 LLM 的消息：系统提示词 + 历史消息。
    pub fn build_messages(&self) -> Vec<Message> {
        let mut result = vec![Message::system(self.system_prompt.clone())];
        result.extend(self.messages.iter().cloned());
        result
    }

    /// 历史超过 max_history 时从头部裁掉最旧的消息。
    fn trim_history(&mut self) {
        if self.messages.len() > self.max_history {
            let excess = self.messages.len() - self.max_history;
            self.messages.drain(0..excess);
        }
    }

    /// 当历史消息达到阈值的 80% 时，将旧消息压缩为一条摘要。
    /// 返回 true 表示发生了压缩。
    pub async fn maybe_compress(
        &mut self,
        client: &dyn crate::agent::llm::LlmClient,
        config: &ContextConfig,
    ) -> anyhow::Result<bool> {
        let threshold = (config.max_messages as f32 * 0.8) as usize;
        if self.messages.len() < threshold {
            return Ok(false);
        }
        if config.compression_summary_keep >= self.messages.len() {
            return Ok(false);
        }

        tracing::info!(
            "上下文压缩触发: {} 条消息，阈值 {}（{}%）",
            self.messages.len(),
            threshold,
            80
        );

        let split = self.messages.len() - config.compression_summary_keep;
        let old_messages = self.messages.drain(0..split).collect::<Vec<_>>();
        let old_text = old_messages
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join(
                "
",
            );

        let summary_prompt = format!(
            "请将以下对话历史压缩为简洁摘要，保留关键事实、决定和任务状态：

{}",
            old_text
        );

        let response = client
            .chat(vec![Message::user(summary_prompt)], vec![])
            .await?;

        let summary = match response {
            crate::agent::llm::LlmResponse::Text(text) => text,
            crate::agent::llm::LlmResponse::ToolCalls(_) => {
                return Ok(false);
            }
        };

        self.messages
            .insert(0, Message::system(format!("[历史对话摘要] {}", summary)));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_trim() {
        let mut ctx = SessionContext::new("test");
        ctx.max_history = 2;
        ctx.add_message(Message::user("a"));
        ctx.add_message(Message::user("b"));
        ctx.add_message(Message::user("c"));
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[0].content, "b");
    }

    #[test]
    fn test_build_messages_includes_system() {
        let mut ctx = SessionContext::new("sys");
        ctx.add_message(Message::user("hi"));
        let messages = ctx.build_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "sys");
    }

    #[tokio::test]
    async fn test_maybe_compress_triggers() {
        struct FakeLlm;

        #[async_trait::async_trait]
        impl crate::agent::llm::LlmClient for FakeLlm {
            async fn chat(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<crate::agent::llm::ToolDefinition>,
            ) -> anyhow::Result<crate::agent::llm::LlmResponse> {
                Ok(crate::agent::llm::LlmResponse::Text("摘要".to_string()))
            }
        }

        let mut ctx = SessionContext::new("sys");
        for i in 0..8 {
            ctx.add_message(Message::user(format!("msg {}", i)));
        }
        let config = ContextConfig {
            max_messages: 10,
            compression_summary_keep: 2,
        };
        // 8 条消息 = 80% 的 10，应触发压缩
        let compressed = ctx.maybe_compress(&FakeLlm, &config).await.unwrap();
        assert!(compressed);
        assert_eq!(ctx.messages.len(), 3);
        assert!(ctx.messages[0].content.contains("摘要"));
    }
}
