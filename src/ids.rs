pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Accept UUIDs and historical schedule-derived IDs as a single safe filename.
pub fn is_safe_run_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    if !id.as_bytes()[0].is_ascii_alphanumeric()
        || id.ends_with('.')
        || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
    {
        return false;
    }
    let upper = id.split('.').next().unwrap_or_default().to_ascii_uppercase();
    !matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'))
}
