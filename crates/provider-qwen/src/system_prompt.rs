use std::path::PathBuf;

/// Returns the path to the Qwen system prompt file if it exists,
/// otherwise returns None to use the built-in default.
pub fn qwen_system_prompt_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FERRUMGATE_QWEN_SYSTEM_PROMPT") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
