use super::{Limits, invalid};
use crabber::ExtensionError;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    task: String,
    profile: String,
}

pub(super) fn normalize(value: Value, limits: &Limits) -> Result<(String, String), ExtensionError> {
    let input: Input = serde_json::from_value(value).map_err(|_| invalid("shape"))?;
    if input.task.len() > limits.max_task_bytes
        || input.task.contains('\0')
        || input.task.trim().is_empty()
    {
        return Err(invalid("task"));
    }
    let mut bytes = input.profile.bytes();
    if input.profile.len() > limits.max_profile_bytes
        || !bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
    {
        return Err(invalid("profile"));
    }
    Ok((input.task, input.profile))
}
