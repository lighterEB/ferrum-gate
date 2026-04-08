use uuid::Uuid;

pub(crate) fn new_openai_object_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_openai_object_id_has_correct_format() {
        let id = new_openai_object_id("chatcmpl");

        assert!(id.starts_with("chatcmpl_"));
        // UUID simple format is 32 hex chars
        let suffix = &id["chatcmpl_".len()..];
        assert_eq!(suffix.len(), 32, "UUID simple should be 32 hex chars");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn new_openai_object_id_uses_given_prefix() {
        let id = new_openai_object_id("resp");
        assert!(id.starts_with("resp_"));

        let id = new_openai_object_id("call");
        assert!(id.starts_with("call_"));
    }

    #[test]
    fn new_openai_object_id_generates_unique_ids() {
        let id1 = new_openai_object_id("test");
        let id2 = new_openai_object_id("test");
        assert_ne!(id1, id2);
    }
}
