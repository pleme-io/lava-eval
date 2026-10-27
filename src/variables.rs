use crate::eval::{InputBindings, Scalar};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VariableError {
    #[error("variable {0:?} is null; lava has no null binding")]
    Null(String),
    #[error("variable {0:?} is an object; structure belongs in the architecture")]
    Object(String),
    #[error(
        "variable {name:?}[{index}] is not an object, but {name:?}[0] is; a record list is uniform"
    )]
    MixedRecords { name: String, index: usize },
    #[error("variable {name:?}[{index}].{field} is structured; a record field is a scalar")]
    StructuredField {
        name: String,
        index: usize,
        field: String,
    },
    #[error("variable {name:?}[{index}] is structured; lava binds lists of scalars")]
    StructuredItem { name: String, index: usize },
}

impl InputBindings {
    pub fn from_variables(variables: &Map<String, Value>) -> Result<Self, VariableError> {
        let mut b = InputBindings::new();
        let mut keys: Vec<&String> = variables.keys().collect();
        keys.sort();
        for k in keys {
            match &variables[k] {
                Value::String(v) => b.set_str(k.clone(), v.clone()),
                Value::Number(n) => b.set_scalar(k.clone(), Scalar::Number(n.clone())),
                Value::Bool(v) => b.set_scalar(k.clone(), *v),
                Value::Array(items) if items.first().is_some_and(Value::is_object) => {
                    b.set_scalar_records(k.clone(), records(k, items)?);
                }
                Value::Array(items) => b.set_list(k.clone(), list(k, items)?),
                Value::Null => return Err(VariableError::Null(k.clone())),
                Value::Object(_) => return Err(VariableError::Object(k.clone())),
            }
        }
        Ok(b)
    }
}

fn records(name: &str, items: &[Value]) -> Result<Vec<BTreeMap<String, Scalar>>, VariableError> {
    let mut rows = Vec::with_capacity(items.len());
    for (index, it) in items.iter().enumerate() {
        let Value::Object(map) = it else {
            return Err(VariableError::MixedRecords {
                name: name.to_string(),
                index,
            });
        };
        let mut row = BTreeMap::new();
        for (field, v) in map {
            let scalar = match v {
                Value::String(x) => Scalar::Text(x.clone()),
                Value::Number(x) => Scalar::Number(x.clone()),
                Value::Bool(x) => Scalar::Bool(*x),
                Value::Null => Scalar::Text(String::new()),
                _ => {
                    return Err(VariableError::StructuredField {
                        name: name.to_string(),
                        index,
                        field: field.clone(),
                    });
                }
            };
            row.insert(field.clone(), scalar);
        }
        rows.push(row);
    }
    Ok(rows)
}

fn list(name: &str, items: &[Value]) -> Result<Vec<String>, VariableError> {
    items
        .iter()
        .enumerate()
        .map(|(index, it)| match it {
            Value::String(v) => Ok(v.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(v) => Ok(v.to_string()),
            _ => Err(VariableError::StructuredItem {
                name: name.to_string(),
                index,
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn scalars_lists_and_records_bind_by_shape() {
        let b = InputBindings::from_variables(&vars(json!({
            "owner": "o",
            "n": 3,
            "on": true,
            "azs": ["a", "b"],
            "repos": [{"slug": "a", "archived": false, "gone": null}]
        })))
        .unwrap();
        assert_eq!(b.records["repos"].len(), 1);
        assert_eq!(b.records["repos"][0]["gone"], Scalar::Text(String::new()));
    }

    #[test]
    fn structure_is_refused_with_the_key_named() {
        let cases = [
            (json!({"x": null}), VariableError::Null("x".into())),
            (json!({"x": {"a": 1}}), VariableError::Object("x".into())),
            (
                json!({"x": [{"a": 1}, "b"]}),
                VariableError::MixedRecords {
                    name: "x".into(),
                    index: 1,
                },
            ),
            (
                json!({"x": [{"a": [1]}]}),
                VariableError::StructuredField {
                    name: "x".into(),
                    index: 0,
                    field: "a".into(),
                },
            ),
            (
                json!({"x": ["a", ["b"]]}),
                VariableError::StructuredItem {
                    name: "x".into(),
                    index: 1,
                },
            ),
        ];
        for (v, want) in cases {
            assert_eq!(InputBindings::from_variables(&vars(v)).unwrap_err(), want);
        }
    }
}
