use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct Events {
    pending: Vec<u8>,
    oversized: bool,
    pub incomplete: bool,
    pub session_id: Option<String>,
    pub response: Option<String>,
    pub tokens: u64,
    pub usage_seen: bool,
    pub retryable: bool,
}

impl Events {
    pub fn feed(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if *byte == b'\n' {
                if !self.oversized {
                    self.line();
                }
                self.pending.clear();
                self.oversized = false;
            } else if !self.oversized {
                if self.pending.len() >= 16 * 1024 * 1024 {
                    self.pending.clear();
                    self.oversized = true;
                    self.incomplete = true;
                } else {
                    self.pending.push(*byte);
                }
            }
        }
    }
    pub fn finish(&mut self) {
        if !self.pending.is_empty() && !self.oversized {
            self.line();
        }
        self.pending.clear();
    }
    fn line(&mut self) {
        let Ok(event) = serde_json::from_slice::<Value>(&self.pending) else {
            self.incomplete = true;
            return;
        };
        match event["type"].as_str() {
            Some("thread.started") => {
                if let Some(id) = event["thread_id"]
                    .as_str()
                    .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                {
                    self.session_id = Some(id.to_owned());
                } else {
                    self.incomplete = true;
                }
            }
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                if let Some(text) = event["item"]["text"].as_str() {
                    if text.len() <= 1024 * 1024 {
                        self.response = Some(text.to_owned());
                    } else {
                        self.incomplete = true;
                    }
                }
            }
            Some("turn.completed") => {
                if let Some(tokens) = event["usage"]["input_tokens"].as_u64().and_then(|input| {
                    event["usage"]["output_tokens"]
                        .as_u64()
                        .and_then(|output| input.checked_add(output))
                }) {
                    if let Some(total) = self
                        .tokens
                        .checked_add(tokens)
                        .filter(|total| *total <= i64::MAX as u64)
                    {
                        self.tokens = total;
                        self.usage_seen = true;
                    } else {
                        self.incomplete = true;
                    }
                } else {
                    self.incomplete = true;
                }
            }
            Some("turn.failed" | "error") => {
                // Retry only explicit transient error codes.
                self.retryable = matches!(
                    event["error"]["code"]
                        .as_str()
                        .or_else(|| event["code"].as_str()),
                    Some("rate_limit_exceeded" | "server_error" | "service_unavailable")
                );
            }
            _ => {}
        }
    }
}

pub fn task_result(stdout: &str) -> (String, Vec<String>, bool) {
    let mut response = stdout.to_owned();
    let mut report = None;
    for line in stdout.lines() {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
            if event["type"] == "item.completed" && event["item"]["type"] == "agent_message" {
                if let Some(text) = event["item"]["text"].as_str() {
                    response = text.to_owned();
                }
            }
            if event.get("reactor_task_result").is_some() {
                report = Some(event);
            }
        }
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) {
        if value.get("reactor_task_result").is_some() {
            report = Some(value);
        }
    }
    let Some(report) = report else {
        return (response, Vec::new(), false);
    };
    let Some(items) = report["reactor_task_result"]["warnings"].as_array() else {
        return (
            response,
            vec!["invalid_task_warning_report".to_owned()],
            true,
        );
    };
    let mut warnings = Vec::new();
    let mut blocking = false;
    for warning in items.iter().take(100) {
        let level = warning["level"].as_str().unwrap_or("unknown");
        let message = warning["message"]
            .as_str()
            .unwrap_or("invalid_warning_message");
        blocking |= level != "info" || warning["message"].as_str().is_none();
        warnings.push(format!(
            "{level}: {}",
            message.chars().take(2000).collect::<String>()
        ));
    }
    if items.len() > 100 {
        blocking = true;
        warnings.push("warning_report_truncated".to_owned());
    }
    (response, warnings, blocking)
}
