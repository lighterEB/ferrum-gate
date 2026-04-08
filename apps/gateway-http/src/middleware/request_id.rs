use uuid::Uuid;

pub(crate) fn new_openai_object_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}
