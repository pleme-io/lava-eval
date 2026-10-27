use crate::sexpr::{Sx, parse_all};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafKey {
    pub key: String,
    pub each: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LeafParseError {
    #[error("parse: {0}")]
    Parse(String),
    #[error("interface {interface}: malformed :leaf clause: {reason}")]
    Malformed { interface: String, reason: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SliceError {
    #[error("variable {collection:?} is not a list of records")]
    NotRecords { collection: String },
    #[error("{collection:?}[{index}] has no string field {key:?}, so it belongs to no leaf")]
    Unkeyed {
        collection: String,
        index: usize,
        key: String,
    },
    #[error("{collection:?} declares leaf {leaf:?} twice")]
    Duplicate { collection: String, leaf: String },
    #[error("leaf {leaf:?} is not a row of {collection:?}")]
    UnknownLeaf { collection: String, leaf: String },
    #[error("{collection:?}[{index}] names leaf {leaf:?}, which is not a row of {primary:?}")]
    Orphan {
        collection: String,
        index: usize,
        leaf: String,
        primary: String,
    },
}

pub fn leaf_key_in_source(src: &str, interface: &str) -> Result<Option<LeafKey>, LeafParseError> {
    let forms = parse_all(src).map_err(|e| LeafParseError::Parse(e.to_string()))?;
    for form in forms {
        let Some(xs) = form.as_list() else { continue };
        if xs.first().and_then(Sx::as_sym) != Some("deflava-interface")
            || xs.get(1).and_then(Sx::as_sym) != Some(interface)
        {
            continue;
        }
        let mut i = 2;
        while i + 1 < xs.len() {
            if xs[i].as_kw() == Some("leaf") {
                return leaf_from_clause(interface, &xs[i + 1]).map(Some);
            }
            i += 2;
        }
        return Ok(None);
    }
    Ok(None)
}

fn leaf_from_clause(interface: &str, clause: &Sx) -> Result<LeafKey, LeafParseError> {
    let bad = |reason: &str| LeafParseError::Malformed {
        interface: interface.to_string(),
        reason: reason.to_string(),
    };
    let xs = clause.as_list().ok_or_else(|| bad("not a list"))?;
    let mut key = None;
    let mut each = None;
    let mut i = 0;
    while i < xs.len() {
        let name = xs[i].as_kw().ok_or_else(|| bad("expected :key or :each"))?;
        let val = xs.get(i + 1).ok_or_else(|| bad("a keyword has no value"))?;
        match name {
            "key" => {
                key = Some(
                    val.as_kw()
                        .ok_or_else(|| bad(":key is not a keyword"))?
                        .to_string(),
                )
            }
            "each" => {
                let items = val.as_list().ok_or_else(|| bad(":each is not a list"))?;
                let mut names = Vec::with_capacity(items.len());
                for it in items {
                    names.push(
                        it.as_kw()
                            .ok_or_else(|| bad(":each holds a non-keyword"))?
                            .to_string(),
                    );
                }
                if names.is_empty() {
                    return Err(bad(":each is empty"));
                }
                let distinct: BTreeSet<&String> = names.iter().collect();
                if distinct.len() != names.len() {
                    return Err(bad(":each names a collection twice"));
                }
                each = Some(names);
            }
            other => return Err(bad(&format!("unknown keyword :{other}"))),
        }
        i += 2;
    }
    Ok(LeafKey {
        key: key.ok_or_else(|| bad("missing :key"))?,
        each: each.ok_or_else(|| bad("missing :each"))?,
    })
}

impl LeafKey {
    #[must_use]
    pub fn primary(&self) -> &str {
        &self.each[0]
    }

    pub fn keys(&self, variables: &Map<String, Value>) -> Result<Vec<String>, SliceError> {
        let primary = self.primary();
        let mut seen = BTreeSet::new();
        for (index, row) in rows(variables, primary)?.iter().enumerate() {
            let leaf = self.key_of(primary, index, row)?;
            if !seen.insert(leaf.to_string()) {
                return Err(SliceError::Duplicate {
                    collection: primary.to_string(),
                    leaf: leaf.to_string(),
                });
            }
        }
        for collection in &self.each[1..] {
            for (index, row) in rows(variables, collection)?.iter().enumerate() {
                let leaf = self.key_of(collection, index, row)?;
                if !seen.contains(leaf) {
                    return Err(SliceError::Orphan {
                        collection: collection.clone(),
                        index,
                        leaf: leaf.to_string(),
                        primary: primary.to_string(),
                    });
                }
            }
        }
        Ok(seen.into_iter().collect())
    }

    pub fn slice(
        &self,
        variables: &Map<String, Value>,
        leaf: &str,
    ) -> Result<Map<String, Value>, SliceError> {
        let keys = self.keys(variables)?;
        if keys.binary_search_by(|k| k.as_str().cmp(leaf)).is_err() {
            return Err(SliceError::UnknownLeaf {
                collection: self.primary().to_string(),
                leaf: leaf.to_string(),
            });
        }
        let mut out = variables.clone();
        for collection in &self.each {
            if !variables.contains_key(collection) {
                continue;
            }
            let kept: Vec<Value> = rows(variables, collection)?
                .iter()
                .filter(|row| row.get(&self.key).and_then(Value::as_str) == Some(leaf))
                .cloned()
                .collect();
            out.insert(collection.clone(), Value::Array(kept));
        }
        Ok(out)
    }

    fn key_of<'a>(
        &self,
        collection: &str,
        index: usize,
        row: &'a Value,
    ) -> Result<&'a str, SliceError> {
        row.get(&self.key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SliceError::Unkeyed {
                collection: collection.to_string(),
                index,
                key: self.key.clone(),
            })
    }
}

fn rows<'a>(
    variables: &'a Map<String, Value>,
    collection: &str,
) -> Result<&'a [Value], SliceError> {
    match variables.get(collection) {
        None => Ok(&[]),
        Some(Value::Array(items)) if items.iter().all(Value::is_object) => Ok(items),
        Some(_) => Err(SliceError::NotRecords {
            collection: collection.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SRC: &str = r#"
(deflava-interface org
  :doc "x"
  :inputs ((:repos :type (:list-of :string)))
  :leaf (:key :slug :each (:repos :moves)))
(deflava-interface plain :inputs ((:a :type :string)))
"#;

    fn key() -> LeafKey {
        leaf_key_in_source(SRC, "org").unwrap().unwrap()
    }

    fn vars() -> Map<String, Value> {
        json!({
            "owner": "o",
            "labels": [{"name": "bug"}],
            "repos": [{"slug": "a", "name": "a"}, {"slug": "b", "name": "b"}],
            "moves": [{"slug": "b", "from": "x.old", "to": "x.b"}]
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn the_leaf_clause_reads_as_a_typed_key() {
        assert_eq!(
            key(),
            LeafKey {
                key: "slug".into(),
                each: vec!["repos".into(), "moves".into()]
            }
        );
    }

    #[test]
    fn no_leaf_clause_is_monolithic() {
        assert_eq!(leaf_key_in_source(SRC, "plain").unwrap(), None);
        assert_eq!(leaf_key_in_source(SRC, "absent").unwrap(), None);
    }

    #[test]
    fn a_malformed_clause_is_refused_by_name() {
        for clause in [
            "(:key :slug)",
            "(:each (:repos))",
            "(:key \"slug\" :each (:repos))",
            "(:key :slug :each ())",
            "(:key :slug :each (:repos :repos))",
            "(:key :slug :each (:repos) :shard 4)",
            ":slug",
        ] {
            let src = format!("(deflava-interface org :leaf {clause})");
            assert!(
                matches!(
                    leaf_key_in_source(&src, "org"),
                    Err(LeafParseError::Malformed { .. })
                ),
                "{clause}"
            );
        }
    }

    #[test]
    fn a_slice_keeps_its_own_rows_and_every_shared_input() {
        let k = key();
        assert_eq!(k.keys(&vars()).unwrap(), vec!["a", "b"]);
        let a = k.slice(&vars(), "a").unwrap();
        assert_eq!(a["repos"], json!([{"slug": "a", "name": "a"}]));
        assert_eq!(a["moves"], json!([]));
        assert_eq!(a["labels"], vars()["labels"]);
        assert_eq!(a["owner"], json!("o"));
        let b = k.slice(&vars(), "b").unwrap();
        assert_eq!(
            b["moves"],
            json!([{"slug": "b", "from": "x.old", "to": "x.b"}])
        );
    }

    #[test]
    fn an_absent_secondary_collection_stays_absent() {
        let mut v = vars();
        v.remove("moves");
        assert!(!key().slice(&v, "a").unwrap().contains_key("moves"));
    }

    #[test]
    fn bad_rows_are_refused_by_name() {
        let k = key();
        let mut v = vars();
        v.insert("moves".into(), json!([{"from": "x", "to": "y"}]));
        assert_eq!(
            k.keys(&v),
            Err(SliceError::Unkeyed {
                collection: "moves".into(),
                index: 0,
                key: "slug".into()
            })
        );
        let mut v = vars();
        v.insert("moves".into(), json!([{"slug": "zz"}]));
        assert!(matches!(k.keys(&v), Err(SliceError::Orphan { .. })));
        let mut v = vars();
        v.insert("repos".into(), json!([{"slug": "a"}, {"slug": "a"}]));
        assert!(matches!(k.keys(&v), Err(SliceError::Duplicate { .. })));
        let mut v = vars();
        v.insert("repos".into(), json!(["a"]));
        assert!(matches!(k.keys(&v), Err(SliceError::NotRecords { .. })));
        assert!(matches!(
            k.slice(&vars(), "zz"),
            Err(SliceError::UnknownLeaf { .. })
        ));
    }
}
