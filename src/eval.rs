//! In-memory evaluator for `.tlisp` architecture forms.
//!
//! Reads a `(deflava-architecture …)` form, runs the body with the
//! supplied input bindings, and produces a typed
//! [`lava_core::Architecture`] ready to render to terraform.json.
//!
//! Magma consumes this evaluator directly — no on-disk JSON dump
//! between authoring and plan/apply. The full pipeline:
//!
//! ```text
//! .tlisp source ──parse──► Sx
//!                    ──evaluate──► lava_core::Architecture
//!                              ──render_terraform_json──► serde_json::Value
//!                                          ──magma plan/apply──► cloud state
//! ```

use indexmap::IndexMap;
use lava_arch::Builder;
use lava_core::{Architecture, Resource, ResourceRef, Value};
use std::collections::BTreeMap;
use thiserror::Error;

use crate::sexpr::{parse, Atom, Sx};

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("parse: {0}")]
    Parse(#[from] crate::sexpr::ParseError),
    #[error("expected deflava-architecture form, got {0}")]
    NotArchForm(String),
    #[error("missing :{0} clause")]
    MissingClause(&'static str),
    #[error("missing input binding for {0}")]
    MissingInput(String),
    #[error("unknown reference form: {0}")]
    BadRef(String),
    #[error("interpolation: unknown var `{0}`")]
    UnknownVar(String),
    #[error("type mismatch: expected {0}")]
    Type(&'static str),
}

/// Input binding for a single deflava-architecture evaluation.
#[derive(Debug, Clone, Default)]
pub struct InputBindings {
    scalars: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
}

impl InputBindings {
    pub fn new() -> Self { Self::default() }
    pub fn set_str(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.scalars.insert(k.into(), v.into());
    }
    pub fn set_list(&mut self, k: impl Into<String>, v: Vec<String>) {
        self.lists.insert(k.into(), v);
    }
}

/// Parse + evaluate a deflava-architecture form. Supplies any missing
/// inputs from the form's declared defaults.
pub fn eval_architecture(src: &str, bindings: &InputBindings) -> Result<Architecture, EvalError> {
    let form = parse(src)?;
    let xs = form.as_list().ok_or_else(|| EvalError::NotArchForm("not a list".into()))?;
    if xs.first().and_then(Sx::as_sym) != Some("deflava-architecture") {
        return Err(EvalError::NotArchForm("head != deflava-architecture".into()));
    }
    let arch_name = xs.get(1).and_then(Sx::as_sym).unwrap_or("anonymous").to_string();

    // Walk :keyword clauses from position 2.
    let mut inputs_clause: Option<&Sx> = None;
    let mut resources_clause: Option<&Sx> = None;
    let mut i = 2;
    while i + 1 < xs.len() {
        let key = xs[i].as_kw();
        match key {
            Some("inputs")    => { inputs_clause    = Some(&xs[i + 1]); }
            Some("resources") => { resources_clause = Some(&xs[i + 1]); }
            // :result clause is for downstream consumers; not needed
            // to render terraform.json.
            Some("result") | Some(_) | None => {}
        }
        i += 2;
    }

    // Resolve inputs (defaults + user overrides).
    let mut env = Env::new();
    if let Some(inp) = inputs_clause {
        let pairs = inp.as_list().ok_or_else(|| EvalError::NotArchForm(":inputs not a list".into()))?;
        for pair in pairs {
            let plist = pair.as_list().ok_or_else(|| EvalError::NotArchForm("input not a list".into()))?;
            let key = plist.first().and_then(Sx::as_kw)
                .ok_or_else(|| EvalError::NotArchForm("input head not :kw".into()))?
                .to_string();
            // Default sits at plist[1].
            let default = plist.get(1);
            // User override wins.
            if let Some(s) = bindings.scalars.get(&key) {
                env.scalars.insert(key.clone(), s.clone());
            } else if let Some(l) = bindings.lists.get(&key) {
                env.lists.insert(key.clone(), l.clone());
            } else if let Some(d) = default {
                match d {
                    Sx::Atom(Atom::Str(s)) => { env.scalars.insert(key, s.clone()); }
                    Sx::Atom(Atom::Sym(s)) => { env.scalars.insert(key, s.clone()); }
                    Sx::List(items) => {
                        let mut out = Vec::new();
                        for it in items {
                            match it {
                                Sx::Atom(Atom::Str(s)) => out.push(s.clone()),
                                Sx::Atom(Atom::Sym(s)) => out.push(s.clone()),
                                _ => return Err(EvalError::Type("list of strings")),
                            }
                        }
                        env.lists.insert(key, out);
                    }
                    _ => return Err(EvalError::Type("string | list")),
                }
            } else {
                return Err(EvalError::MissingInput(key));
            }
        }
    }

    // Evaluate resources.
    let resources = resources_clause.ok_or(EvalError::MissingClause("resources"))?;
    let resource_list = resources.as_list().ok_or_else(|| EvalError::NotArchForm(":resources not a list".into()))?;

    let mut builder = Builder::new(arch_name);
    for r in resource_list {
        eval_resource(r, &env, &mut builder)?;
    }

    Ok(builder.finish())
}

#[derive(Default)]
struct Env {
    scalars: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
    // Loop-local single-element scalars (i, az).
    loop_scalars: BTreeMap<String, String>,
}

impl Env {
    fn new() -> Self { Self::default() }
    /// Resolve a `{var}` reference. Loop-local wins over global.
    fn get(&self, key: &str) -> Result<String, EvalError> {
        if let Some(s) = self.loop_scalars.get(key) { return Ok(s.clone()); }
        if let Some(s) = self.scalars.get(key) { return Ok(s.clone()); }
        Err(EvalError::UnknownVar(key.to_string()))
    }
}

fn eval_resource(r: &Sx, env: &Env, builder: &mut Builder) -> Result<(), EvalError> {
    let xs = r.as_list().ok_or_else(|| EvalError::NotArchForm("resource not a list".into()))?;
    let head = xs.first().and_then(Sx::as_sym)
        .ok_or_else(|| EvalError::NotArchForm("resource head not sym".into()))?;

    if head == "for-each" {
        // (for-each ((var ix) (enumerate :inputs.list)) <body-resource>)
        return eval_for_each(xs, env, builder);
    }

    // Generic resource: (aws-vpc "name" :attr v :attr v ...)
    let type_id = sym_to_type_id(head);
    let raw_name = xs.get(1).and_then(Sx::as_str)
        .ok_or_else(|| EvalError::NotArchForm("resource name not string".into()))?;
    let name = interp(raw_name, env)?;
    let mut attrs = IndexMap::new();
    let mut i = 2;
    while i + 1 < xs.len() {
        let key = xs[i].as_kw()
            .ok_or_else(|| EvalError::NotArchForm("attr key not :kw".into()))?
            .replace('-', "_");
        let val = eval_value(&xs[i + 1], env)?;
        attrs.insert(key, val);
        i += 2;
    }
    builder.add_resource(Resource {
        type_id,
        name,
        attributes: attrs,
        depends_on: vec![],
        provider: None,
        multiplicity: None,
    });
    Ok(())
}

fn eval_for_each(xs: &[Sx], env: &Env, builder: &mut Builder) -> Result<(), EvalError> {
    // xs = [ for-each, binding-form, body ]
    let binding_form = xs.get(1).and_then(Sx::as_list)
        .ok_or_else(|| EvalError::NotArchForm("for-each binding".into()))?;
    let vars = binding_form.first().and_then(Sx::as_list)
        .ok_or_else(|| EvalError::NotArchForm("for-each vars".into()))?;
    let iter_form = binding_form.get(1).and_then(Sx::as_list)
        .ok_or_else(|| EvalError::NotArchForm("for-each iter".into()))?;
    if iter_form.first().and_then(Sx::as_sym) != Some("enumerate") {
        return Err(EvalError::NotArchForm("for-each iter != enumerate".into()));
    }
    let source = iter_form.get(1).and_then(Sx::as_sym)
        .ok_or_else(|| EvalError::NotArchForm("enumerate source".into()))?;
    let items = env.lists.get(source)
        .ok_or_else(|| EvalError::UnknownVar(source.to_string()))?
        .clone();
    let body = xs.get(2).ok_or_else(|| EvalError::NotArchForm("for-each body".into()))?;

    let i_var = vars.first().and_then(Sx::as_sym).unwrap_or("i").to_string();
    let item_var = vars.get(1).and_then(Sx::as_sym).unwrap_or("item").to_string();

    for (i, item) in items.iter().enumerate() {
        let mut scoped = Env {
            scalars: env.scalars.clone(),
            lists: env.lists.clone(),
            loop_scalars: env.loop_scalars.clone(),
        };
        scoped.loop_scalars.insert(i_var.clone(), i.to_string());
        scoped.loop_scalars.insert(item_var.clone(), item.clone());
        eval_resource(body, &scoped, builder)?;
    }
    Ok(())
}

fn eval_value(s: &Sx, env: &Env) -> Result<Value, EvalError> {
    match s {
        Sx::Atom(Atom::Str(raw)) => Ok(Value::s(interp(raw, env)?)),
        Sx::Atom(Atom::Sym(s)) => {
            // Bare symbol — resolve as variable.
            Ok(Value::s(env.get(s)?))
        }
        Sx::Atom(Atom::Bool(b)) => Ok(Value::b(*b)),
        Sx::Atom(Atom::Kw(_)) => Err(EvalError::Type("value (not kw)")),
        Sx::List(items) => {
            // (ref aws-vpc "name" attr) → Value::Ref
            if let Some("ref") = items.first().and_then(Sx::as_sym) {
                let type_id = items.get(1).and_then(Sx::as_sym)
                    .ok_or_else(|| EvalError::BadRef("type".into()))?;
                let name_raw = items.get(2).and_then(Sx::as_str)
                    .ok_or_else(|| EvalError::BadRef("name".into()))?;
                let attr = items.get(3).and_then(Sx::as_sym)
                    .ok_or_else(|| EvalError::BadRef("attr".into()))?;
                let name = interp(name_raw, env)?;
                return Ok(Value::Ref(ResourceRef {
                    type_id: sym_to_type_id(type_id),
                    name,
                    attribute: attr.to_string(),
                }));
            }
            // Tags: nested list of (Key Value) pairs → JSON map.
            let mut map = serde_json::Map::new();
            for pair in items {
                let p = pair.as_list().ok_or(EvalError::Type("tag pair list"))?;
                let k_sym = p.first().and_then(Sx::as_sym)
                    .ok_or(EvalError::Type("tag key sym"))?;
                let v_raw = p.get(1).and_then(Sx::as_str)
                    .ok_or(EvalError::Type("tag val str"))?;
                map.insert(k_sym.to_string(), serde_json::Value::String(interp(v_raw, env)?));
            }
            Ok(Value::Json(serde_json::Value::Object(map)))
        }
    }
}

/// Substitute `{var}` and arithmetic-light `{var+N}` in template strings.
fn interp(template: &str, env: &Env) -> Result<String, EvalError> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // Find matching '}'.
            let end = bytes[i + 1..].iter().position(|c| *c == b'}')
                .map(|p| i + 1 + p);
            if let Some(j) = end {
                let expr = std::str::from_utf8(&bytes[i + 1..j])
                    .map_err(|_| EvalError::Type("utf8 in template"))?;
                out.push_str(&eval_template_expr(expr, env)?);
                i = j + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Ok(out)
}

/// Evaluate a template-string expression: `var` | `var+N` | `var-N`.
fn eval_template_expr(expr: &str, env: &Env) -> Result<String, EvalError> {
    if let Some(idx) = expr.find('+') {
        let var = &expr[..idx].trim();
        let off: i64 = expr[idx + 1..].trim().parse().map_err(|_| EvalError::Type("int"))?;
        let v = env.get(var)?.parse::<i64>().map_err(|_| EvalError::Type("int var"))?;
        return Ok((v + off).to_string());
    }
    if let Some(idx) = expr.find('-') {
        if idx > 0 {
            let var = &expr[..idx].trim();
            let off: i64 = expr[idx + 1..].trim().parse().map_err(|_| EvalError::Type("int"))?;
            let v = env.get(var)?.parse::<i64>().map_err(|_| EvalError::Type("int var"))?;
            return Ok((v - off).to_string());
        }
    }
    env.get(expr.trim())
}

/// Convert lisp-form symbol `aws-vpc` → terraform-form `aws_vpc`.
fn sym_to_type_id(s: &str) -> String {
    s.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_simple_var() {
        let mut env = Env::new();
        env.scalars.insert("name".into(), "main".into());
        assert_eq!(interp("{name}-vpc", &env).unwrap(), "main-vpc");
    }

    #[test]
    fn interpolates_arithmetic_var() {
        let mut env = Env::new();
        env.loop_scalars.insert("i".into(), "3".into());
        assert_eq!(interp("10.0.{i+10}.0/24", &env).unwrap(), "10.0.13.0/24");
    }

    #[test]
    fn sym_to_type_id_replaces_dashes() {
        assert_eq!(sym_to_type_id("aws-internet-gateway"), "aws_internet_gateway");
    }
}
