//! Config loader with env interpolation and recursive imports.

use serde_yml::{Mapping, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("undefined variable: {0}")]
    UndefinedVar(String),
    #[error("io: {0}")]
    Io(String),
    #[error("import cycle detected: {0}")]
    Cycle(String),
    #[error("yaml parse: {0}")]
    Yaml(#[from] serde_yml::Error),
}

pub fn load_yaml_with_env(path: &Path) -> Result<String, LoaderError> {
    if let Some(parent) = path.parent() {
        let _ = dotenvy::from_filename(parent.join(".env"));
    }
    let raw = std::fs::read_to_string(path).map_err(|err| LoaderError::Io(err.to_string()))?;
    interpolate(&raw)
}

pub fn load_with_imports(path: &Path) -> Result<String, LoaderError> {
    let mut stack = Vec::new();
    let mut visited = HashSet::new();
    let mut merged = load_value_with_imports(path, &mut stack, &mut visited)?;

    if let Some(imports) = root_imports(path)? {
        if let Value::Mapping(mapping) = &mut merged {
            mapping.insert(
                Value::String("imports".into()),
                Value::Sequence(imports.into_iter().map(Value::String).collect()),
            );
        }
    }

    serde_yml::to_string(&merged).map_err(LoaderError::from)
}

pub fn interpolate(raw: &str) -> Result<String, LoaderError> {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            i += 2;
            let start = i;
            while i < chars.len() && chars[i] != '}' {
                i += 1;
            }
            let expr: String = chars[start..i].iter().collect();
            if i < chars.len() {
                i += 1;
            }
            let (name, default) = if let Some((name, default)) = expr.split_once(":-") {
                (name, Some(default))
            } else {
                (expr.as_str(), None)
            };
            match std::env::var(name) {
                Ok(value) => out.push_str(&value),
                Err(_) => {
                    if let Some(default) = default {
                        out.push_str(default);
                    } else {
                        return Err(LoaderError::UndefinedVar(name.to_string()));
                    }
                }
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn load_value_with_imports(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> Result<Value, LoaderError> {
    let path = normalize_path(path)?;
    if stack.contains(&path) {
        let mut cycle = stack
            .iter()
            .map(|entry| entry.display().to_string())
            .collect::<Vec<_>>();
        cycle.push(path.display().to_string());
        return Err(LoaderError::Cycle(cycle.join(" -> ")));
    }

    stack.push(path.clone());
    visited.insert(path.clone());

    let content = load_yaml_with_env(&path)?;
    let mut value: Value = serde_yml::from_str(&content)?;
    let imports = extract_imports(&mut value)?;
    let mut merged = Value::Mapping(Mapping::new());
    for import in imports {
        let import_path = resolve_import_path(&path, &import);
        let imported = load_value_with_imports(&import_path, stack, visited)?;
        merged = deep_merge(merged, imported);
    }
    stack.pop();
    Ok(deep_merge(merged, value))
}

fn extract_imports(value: &mut Value) -> Result<Vec<String>, LoaderError> {
    let Some(mapping) = value.as_mapping_mut() else {
        return Ok(Vec::new());
    };
    let Some(imports_value) = mapping.remove(Value::String("imports".into())) else {
        return Ok(Vec::new());
    };
    match imports_value {
        Value::Sequence(items) => Ok(items
            .into_iter()
            .map(|item| match item {
                Value::String(path) => Ok(path),
                other => Err(LoaderError::Io(format!(
                    "imports entries must be strings, got {other:?}"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?),
        other => Err(LoaderError::Io(format!(
            "imports must be a sequence, got {other:?}"
        ))),
    }
}

fn deep_merge(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Mapping(mut left), Value::Mapping(right)) => {
            for (key, value) in right {
                let merged = if let Some(existing) = left.remove(&key) {
                    deep_merge(existing, value)
                } else {
                    value
                };
                left.insert(key, merged);
            }
            Value::Mapping(left)
        }
        (Value::Sequence(mut left), Value::Sequence(right)) => {
            left.extend(right);
            Value::Sequence(left)
        }
        (_, right) => right,
    }
}

fn resolve_import_path(current: &Path, import: &str) -> PathBuf {
    let import_path = PathBuf::from(import);
    if import_path.is_absolute() {
        import_path
    } else {
        current
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(import_path)
    }
}

fn normalize_path(path: &Path) -> Result<PathBuf, LoaderError> {
    std::fs::canonicalize(path).map_err(|err| LoaderError::Io(err.to_string()))
}

fn root_imports(path: &Path) -> Result<Option<Vec<String>>, LoaderError> {
    let content = load_yaml_with_env(path)?;
    let mut value: Value = serde_yml::from_str(&content)?;
    let imports = extract_imports(&mut value)?;
    if imports.is_empty() {
        Ok(None)
    } else {
        Ok(Some(imports))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_var_errors() {
        std::env::remove_var("FLUX_MISSING_TEST");
        let err = interpolate("host: ${FLUX_MISSING_TEST}").unwrap_err();
        assert!(matches!(err, LoaderError::UndefinedVar(var) if var == "FLUX_MISSING_TEST"));
    }

    #[test]
    fn default_var_falls_back() {
        std::env::remove_var("FLUX_DEFAULT_TEST");
        let interpolated = interpolate("host: ${FLUX_DEFAULT_TEST:-fallback}").unwrap();
        assert_eq!(interpolated, "host: fallback");
    }

    #[test]
    fn interpolation_works_inside_yaml_quotes() {
        std::env::set_var("FLUX_QUOTED_TEST", "value");
        let interpolated = interpolate("host: \"${FLUX_QUOTED_TEST}\"").unwrap();
        let value: Value = serde_yml::from_str(&interpolated).unwrap();
        assert_eq!(value["host"], Value::String("value".into()));
        std::env::remove_var("FLUX_QUOTED_TEST");
    }

    #[test]
    fn imports_deep_merge_and_array_concat() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("base.yml");
        let override_file = dir.path().join("override.yml");
        let root = dir.path().join("root.yml");
        std::fs::write(
            &base,
            "flags: [\"-i\"]\nproxy:\n  enabled: false\n  local_port: 1000\n",
        )
        .unwrap();
        std::fs::write(&override_file, "flags: [\"-l\"]\nproxy:\n  enabled: true\n").unwrap();
        std::fs::write(
            &root,
            "imports: [base.yml, override.yml]\nproxy:\n  remote_port: 2000\n",
        )
        .unwrap();
        let merged = load_with_imports(&root).unwrap();
        let value: Value = serde_yml::from_str(&merged).unwrap();
        assert_eq!(
            value["flags"],
            Value::Sequence(vec![Value::String("-i".into()), Value::String("-l".into())])
        );
        assert_eq!(value["proxy"]["enabled"], Value::Bool(true));
        assert_eq!(value["proxy"]["local_port"], Value::Number(1000.into()));
        assert_eq!(value["proxy"]["remote_port"], Value::Number(2000.into()));
    }

    #[test]
    fn import_cycle_errors() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.yml");
        let b = dir.path().join("b.yml");
        std::fs::write(&a, "imports: [b.yml]\nhost: a\n").unwrap();
        std::fs::write(&b, "imports: [a.yml]\nhost: b\n").unwrap();
        let err = load_with_imports(&a).unwrap_err();
        assert!(matches!(err, LoaderError::Cycle(_)));
    }
}
