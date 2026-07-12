use serde_json::Value;

pub(crate) fn redact(value: &str) -> String {
    let mut result = value.to_owned();
    for marker in ["SECRET_TOKEN", "API_KEY", "AUTHORIZATION"] {
        result = result.replace(marker, "[REDACTED]");
    }
    result
}

pub(crate) fn redact_value(value: &mut Value) {
    match value {
        Value::String(text) => *text = redact(text),
        Value::Array(values) => values.iter_mut().for_each(redact_value),
        Value::Object(values) => values.values_mut().for_each(redact_value),
        _ => {}
    }
}
