use ohmydisk_core::AppError;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, AppError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            AppError::new(
                "invalid_arguments",
                format!("{key} must be a nonempty string"),
            )
        })
}
pub fn digest(value: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).unwrap_or_default())
    )
}
pub fn request(args: &Value) -> Result<(&str, String), AppError> {
    let key = text(args, "request_key")?;
    if key.len() > 256 {
        return Err(AppError::new(
            "invalid_arguments",
            "request_key exceeds 256 bytes",
        ));
    }
    let mut value = args.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("request_key");
    }
    Ok((key, digest(&value)))
}
/// Cursor signatures bind the operation, immutable version, and query arguments.
pub fn page<T: serde::Serialize>(
    items: &[T],
    args: &Value,
    binding: &str,
) -> Result<Value, AppError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 500) as usize;
    let mut filter = args.clone();
    if let Some(object) = filter.as_object_mut() {
        object.remove("cursor");
        object.remove("limit");
    }
    let signature = digest(&serde_json::json!([binding, filter]));
    let offset = match args.get("cursor").filter(|v| !v.is_null()) {
        None => 0,
        Some(Value::String(cursor)) => {
            let (sig, position) = cursor
                .split_once(':')
                .ok_or_else(|| AppError::new("invalid_cursor", "Malformed cursor"))?;
            if sig != signature {
                return Err(AppError::new(
                    "invalid_cursor",
                    "Cursor belongs to a different result or query",
                ));
            }
            position
                .parse::<usize>()
                .map_err(|_| AppError::new("invalid_cursor", "Malformed cursor offset"))?
        }
        _ => return Err(AppError::new("invalid_cursor", "Cursor must be a string")),
    };
    if offset > items.len() {
        return Err(AppError::new(
            "invalid_cursor",
            "Cursor is outside this result",
        ));
    }
    let end = offset.saturating_add(limit).min(items.len());
    let next = (end < items.len()).then(|| format!("{signature}:{end}"));
    Ok(serde_json::json!({"items":&items[offset..end],"next_cursor":next}))
}
