use crate::agent::llm::Message;

/// 会话上下文：维护当前对话消息与系统提示词
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub max_history: usize,
}

impl SessionContext {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            messages: Vec::new(),
            max_history: 50,
        }
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
        self.trim_history();
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.system_prompt = prompt.into();
    }

    pub fn build_messages(&self) -> Vec<Message> {
        let mut result = vec![Message::system(self.system_prompt.clone())];
        result.extend(self.messages.iter().cloned());
        result
    }

    fn trim_history(&mut self) {
        if self.messages.len() > self.max_history {
            let excess = self.messages.len() - self.max_history;
            self.messages.drain(0..excess);
        }
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
}
