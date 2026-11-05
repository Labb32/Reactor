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
                if !self.oversized { self.line(); }
                self.pending.clear(); self.oversized = false;
            } else if !self.oversized {
                if self.pending.len() >= 16 * 1024 * 1024 {
                    self.pending.clear(); self.oversized = true; self.incomplete = true;
                } else { self.pending.push(*byte); }
            }
        }
    }
    pub fn finish(&mut self) {
        if !self.pending.is_empty() && !self.oversized { self.line(); }
        self.pending.clear();
    }
    fn line(&mut self) {
        let Ok(event) = serde_json::from_slice::<Value>(&self.pending) else {
            self.incomplete = true; return;
        };
        match event["type"].as_str() {
            Some("thread.started") => {
                if let Some(id) = event["thread_id"].as_str().filter(|id| uuid::Uuid::parse_str(id).is_ok()) {
                    self.session_id = Some(id.to_owned());
                } else { self.incomplete = true; }
            }
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                if let Some(text) = event["item"]["text"].as_str() {
                    if text.len() <= 1024 * 1024 { self.response = Some(text.to_owned()); }
                    else { self.incomplete = true; }
                }
            }
            Some("turn.completed") => {
                if let Some(tokens) = event["usage"]["input_tokens"].as_u64()
                    .and_then(|input| event["usage"]["output_tokens"].as_u64().and_then(|output| input.checked_add(output))) {
                    if let Some(total) = self.tokens.checked_add(tokens).filter(|total| *total <= i64::MAX as u64) {
                        self.tokens = total; self.usage_seen = true;
                    } else { self.incomplete = true; }
                } else { self.incomplete = true; }
            }
            Some("turn.failed" | "error") => {
                // Only explicit structured codes are retry hints; messages are not parsed.
                self.retryable = matches!(event["error"]["code"].as_str().or_else(|| event["code"].as_str()),
                    Some("rate_limit_exceeded" | "server_error" | "service_unavailable"));
            }
            _ => {}
        }
    }
}
