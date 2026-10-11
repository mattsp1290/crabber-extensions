use super::*;
pub(super) fn validate(l: &Limits) -> Result<(), ExtensionError> {
    for (value, cap) in [
        (l.max_bindings, 256),
        (l.max_fields_per_binding, 256),
        (l.max_input_matches, 256),
        (l.max_config_bytes, 1024 * 1024),
        (l.max_input_bytes, 1024 * 1024),
        (l.max_field_bytes, 1024 * 1024),
        (l.max_concurrent, 64),
        (l.max_stdout_bytes, 1024 * 1024),
    ] {
        if value == 0 || value > cap {
            return Err(crate::config_error("limits"));
        }
    }
    if l.max_stdout_bytes > l.max_field_bytes {
        return Err(crate::config_error("limit-order"));
    }
    if [l.timeout, l.kill_wait]
        .iter()
        .any(|d| d.is_zero() || *d > Duration::from_secs(60))
    {
        return Err(crate::config_error("timeouts"));
    }
    if l.shutdown_grace.is_zero()
        || l.shutdown_grace > Duration::from_secs(120)
        || l.shutdown_grace < l.timeout + l.kill_wait
    {
        return Err(crate::config_error("shutdown-grace"));
    }
    Ok(())
}
