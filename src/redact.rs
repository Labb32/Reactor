/// Best-effort literal masking of known environment credentials, not a secret scanner.
pub struct Redactor { secrets: Vec<Vec<u8>>, pending: Vec<u8>, overlap: usize }

impl Redactor {
    pub fn new() -> Self {
        let mut secrets = std::env::vars_os().filter_map(|(key, value)| {
            let key = key.to_string_lossy().to_ascii_uppercase();
            let sensitive = ["TOKEN", "SECRET", "PASSWORD", "API_KEY", "PRIVATE_KEY", "ACCESS_KEY"]
                .iter().any(|part| key.contains(part));
            let value = value.into_string().ok()?;
            (sensitive && value.len() >= 8 && value.len() <= 16 * 1024).then(|| value.into_bytes())
        }).collect::<Vec<_>>();
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        let overlap = secrets.iter().map(Vec::len).max().unwrap_or(1).saturating_sub(1);
        Self { secrets, pending: Vec::new(), overlap }
    }

    pub fn push(&mut self, bytes: &[u8], final_chunk: bool) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let cut = if final_chunk { self.pending.len() } else { self.pending.len().saturating_sub(self.overlap) };
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < cut {
            if let Some(secret) = self.secrets.iter().find(|secret| self.pending[offset..].starts_with(secret)) {
                result.extend_from_slice(b"[REDACTED]"); offset += secret.len();
            } else { result.push(self.pending[offset]); offset += 1; }
        }
        self.pending.drain(..offset);
        result
    }
}

pub fn text(value: &str) -> String {
    String::from_utf8_lossy(&Redactor::new().push(value.as_bytes(), true)).into_owned()
}
