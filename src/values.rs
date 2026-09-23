//! Explicit outputs and data dependencies. No expressions or shell interpolation.

use crate::manifest::{Check, Manifest, ScopedCheck};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;

pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
pub type Store = HashMap<String, Value>;

#[derive(Debug, Clone)]
pub struct Capture {
    pub name: String,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum Source {
    Stdout,
    Json(Vec<String>),
}

pub fn identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

pub fn parse_captures(value: Option<&toml::Value>, check: &Check) -> Result<Vec<Capture>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let table = value
        .as_table()
        .context("'capture' must be a nonempty table")?;
    if table.is_empty() {
        bail!("'capture' must be a nonempty table");
    }
    table.iter().map(|(name, selector)| {
        if !identifier(name) { bail!("invalid capture name {name:?}: use letters, digits and underscores, starting with a letter or underscore"); }
        let selector = selector.as_str().context("capture selectors must be strings")?;
        let source = match check {
            Check::Run { .. } if selector == "stdout" => Source::Stdout,
            Check::Http { .. } if selector.starts_with("json.") => {
                let path: Vec<String> = selector[5..].split('.').map(String::from).collect();
                if path.iter().any(|part| !identifier(part)) {
                    bail!("capture {name:?}: use json.field or json.object.field (object keys only)");
                }
                Source::Json(path)
            }
            Check::Run { .. } => bail!("capture {name:?}: commands support only 'stdout'"),
            Check::Http { .. } => bail!("capture {name:?}: HTTP checks support 'json.field'"),
            _ => bail!("captures are supported only on completed commands and HTTP checks"),
        };
        Ok(Capture { name: name.clone(), source })
    }).collect()
}

pub fn key(scenario: &str, step: Option<usize>, name: &str) -> String {
    match step {
        Some(step) => format!("{scenario}.{step}.{name}"),
        None => format!("{scenario}.{name}"),
    }
}

#[derive(Debug)]
enum Part<'a> {
    Literal(&'a str),
    Reference(&'a str),
}

fn parts(text: &str) -> Result<Vec<Part<'_>>> {
    let mut result = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        if start > 0 && rest.as_bytes()[start - 1] == b'$' {
            result.push(Part::Literal(&rest[..start - 1]));
            let end = rest[start..]
                .find('}')
                .context("unclosed escaped reference")?
                + start
                + 1;
            result.push(Part::Literal(&rest[start..end]));
            rest = &rest[end..];
            continue;
        }
        result.push(Part::Literal(&rest[..start]));
        let end = rest[start + 2..]
            .find('}')
            .context("unclosed capture reference")?
            + start
            + 2;
        let name = &rest[start + 2..end];
        if name.is_empty() || name.contains(['{', '$', '\n', '\r']) {
            bail!("invalid capture reference");
        }
        result.push(Part::Reference(name));
        rest = &rest[end + 1..];
    }
    result.push(Part::Literal(rest));
    Ok(result)
}

fn references(text: &str) -> Result<Vec<String>> {
    Ok(parts(text)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Reference(name) => Some(name.to_string()),
            _ => None,
        })
        .collect())
}

fn inputs(scoped: &ScopedCheck) -> Vec<&str> {
    let mut fields: Vec<&str> = scoped.env.iter().map(|(_, value)| value.as_str()).collect();
    match &scoped.check {
        Check::Run {
            contains, absent, ..
        }
        | Check::Log {
            contains, absent, ..
        } => {
            fields.extend(contains.iter().chain(absent).map(String::as_str));
        }
        Check::Service {
            ready,
            contains,
            absent,
            allow,
            ..
        } => {
            fields.extend(ready.as_deref());
            fields.extend(
                contains
                    .iter()
                    .chain(absent)
                    .chain(allow)
                    .map(String::as_str),
            );
        }
        Check::Http {
            url,
            body,
            headers,
            contains,
            absent,
            ..
        } => {
            fields.push(url);
            fields.extend(body.as_deref());
            fields.extend(headers.iter().map(|(_, value)| value.as_str()));
            fields.extend(contains.iter().chain(absent).map(String::as_str));
        }
    }
    fields
}

pub struct Plan {
    pub references: Vec<Vec<Vec<String>>>,
    pub sensitive: Vec<bool>,
    producers: HashMap<String, (usize, usize)>,
}

impl Plan {
    pub fn build(manifest: &Manifest) -> Result<Self> {
        let mut producers = HashMap::new();
        for (si, scenario) in manifest.scenarios.iter().enumerate() {
            for (ci, check) in scenario.checks.iter().enumerate() {
                for capture in &check.captures {
                    let name = key(&scenario.name, check.step, &capture.name);
                    if name.contains(['$', '{', '}', '\n', '\r']) {
                        bail!("scenario {:?}: capture references cannot contain $, braces or newlines", scenario.name);
                    }
                    if producers.insert(name.clone(), (si, ci)).is_some() {
                        bail!("ambiguous capture reference {name:?}; rename one of the scenarios");
                    }
                }
            }
        }
        let mut all_refs = Vec::new();
        let mut sensitive = Vec::new();
        let mut dependencies = vec![Vec::new(); manifest.scenarios.len()];
        for (si, scenario) in manifest.scenarios.iter().enumerate() {
            let mut scenario_refs = Vec::new();
            let mut private = false;
            for (ci, scoped) in scenario.checks.iter().enumerate() {
                let refs = (|| -> Result<Vec<String>> {
                    let mut refs = Vec::new();
                    for field in inputs(scoped) { refs.extend(references(field)?); }
                    if let Check::Http { body: Some(body), .. } = &scoped.check {
                        if body.contains("${") {
                            let value: Value = serde_json::from_str(body).context("a templated HTTP body must be valid JSON, with references inside string values")?;
                            validate_json_keys(&value)?;
                        }
                    }
                    for reference in &refs {
                        let &(ps, pc) = producers.get(reference).with_context(|| format!("unknown capture reference {reference:?}"))?;
                        if ps == si {
                            if pc >= ci { bail!("capture {reference:?} must come from an earlier step in the same scenario"); }
                        } else if !dependencies[si].contains(&ps) { dependencies[si].push(ps); }
                    }
                    Ok(refs)
                })().with_context(|| format!("scenario {:?}, step {}", scenario.name, scoped.step.unwrap_or(1)))?;
                private |= !refs.is_empty() || !scoped.captures.is_empty();
                scenario_refs.push(refs);
            }
            all_refs.push(scenario_refs);
            sensitive.push(private);
        }
        topological(&dependencies, manifest)?; // Validate even excluded scenarios.
        Ok(Self {
            references: all_refs,
            sensitive,
            producers,
        })
    }

    pub fn execution(
        &self,
        manifest: &Manifest,
        selected: Option<&str>,
        host: &str,
    ) -> (Vec<usize>, Vec<bool>) {
        let mut included: Vec<bool> = manifest
            .scenarios
            .iter()
            .map(|s| selected.is_none_or(|name| name == s.name))
            .collect();
        let mut dependencies = vec![Vec::new(); included.len()];
        for (si, scenario) in manifest.scenarios.iter().enumerate() {
            for (ci, check) in scenario.checks.iter().enumerate() {
                if scenario.os.or(check.os).is_some_and(|os| !os.matches(host)) {
                    continue;
                }
                for reference in &self.references[si][ci] {
                    let (ps, _) = self.producers[reference];
                    if ps != si && !dependencies[si].contains(&ps) {
                        dependencies[si].push(ps);
                    }
                }
            }
        }
        let mut todo: Vec<usize> = (0..included.len()).filter(|&i| included[i]).collect();
        while let Some(i) = todo.pop() {
            for &producer in &dependencies[i] {
                if !included[producer] {
                    included[producer] = true;
                    todo.push(producer);
                }
            }
        }
        for (i, deps) in dependencies.iter_mut().enumerate() {
            if !included[i] {
                deps.clear();
            }
        }
        // A subgraph of the validated graph cannot introduce a cycle.
        (
            topological(&dependencies, manifest).expect("validated data dependencies"),
            included,
        )
    }
}

fn topological(dependencies: &[Vec<usize>], manifest: &Manifest) -> Result<Vec<usize>> {
    fn visit(
        i: usize,
        deps: &[Vec<usize>],
        state: &mut [u8],
        order: &mut Vec<usize>,
        manifest: &Manifest,
    ) -> Result<()> {
        if state[i] == 2 {
            return Ok(());
        }
        if state[i] == 1 {
            bail!(
                "capture dependency cycle involving scenario {:?}",
                manifest.scenarios[i].name
            );
        }
        state[i] = 1;
        for &next in &deps[i] {
            visit(next, deps, state, order, manifest)?;
        }
        state[i] = 2;
        order.push(i);
        Ok(())
    }
    let mut state = vec![0; dependencies.len()];
    let mut order = Vec::new();
    for i in 0..dependencies.len() {
        visit(i, dependencies, &mut state, &mut order, manifest)?;
    }
    Ok(order)
}

fn validate_json_keys(value: &Value) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key.contains("${") {
                    bail!("capture references are allowed in JSON values, not object keys");
                }
                validate_json_keys(value)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                validate_json_keys(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn render(text: &str, store: &Store, url: bool) -> Result<String> {
    let mut out = String::new();
    for part in parts(text)? {
        match part {
            Part::Literal(text) => out.push_str(text),
            Part::Reference(name) => {
                let text = as_text(
                    store
                        .get(name)
                        .with_context(|| format!("capture {name:?} is unavailable"))?,
                );
                if url {
                    for byte in text.bytes() {
                        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                            out.push(byte as char);
                        } else {
                            out.push_str(&format!("%{byte:02X}"));
                        }
                    }
                } else {
                    out.push_str(&text);
                }
            }
        }
    }
    Ok(out)
}

fn render_json(value: &mut Value, store: &Store) -> Result<()> {
    match value {
        Value::String(text) => {
            if let Some(name) = text.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
                if let Some(captured) = store.get(name) {
                    *value = captured.clone();
                    return Ok(());
                }
            }
            *text = render(text, store, false)?;
        }
        Value::Array(items) => {
            for item in items {
                render_json(item, store)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                render_json(item, store)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn resolve(scoped: &ScopedCheck, store: &Store) -> Result<(Check, Vec<(String, String)>)> {
    let env = scoped
        .env
        .iter()
        .map(|(name, value)| {
            let value = render(value, store, false)?;
            if value.contains('\0') {
                bail!("captured environment value contains NUL");
            }
            Ok((name.clone(), value))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut check = scoped.check.clone();
    let (contains, absent) = match &mut check {
        Check::Run {
            contains, absent, ..
        }
        | Check::Log {
            contains, absent, ..
        } => (contains, absent),
        Check::Service {
            ready,
            contains,
            absent,
            allow,
            ..
        } => {
            if let Some(url) = ready {
                *url = render(url, store, true)?;
            }
            for rule in allow {
                *rule = render(rule, store, false)?;
            }
            (contains, absent)
        }
        Check::Http {
            url,
            body,
            headers,
            contains,
            absent,
            ..
        } => {
            *url = render(url, store, true)?;
            for (_, value) in headers {
                *value = render(value, store, false)?;
                if value.contains(['\r', '\n', '\0']) {
                    bail!("captured HTTP header value contains a forbidden control character");
                }
            }
            if let Some(body) = body {
                if body.contains("${") {
                    let mut value: Value =
                        serde_json::from_str(body).context("invalid templated JSON body")?;
                    render_json(&mut value, store)?;
                    *body = serde_json::to_string(&value)?;
                }
            }
            (contains, absent)
        }
    };
    for rule in contains.iter_mut().chain(absent) {
        *rule = render(rule, store, false)?;
    }
    Ok((check, env))
}

pub fn extract(captures: &[Capture], output: &str) -> Result<Vec<(String, Value)>> {
    let json = if captures.iter().any(|c| matches!(c.source, Source::Json(_))) {
        Some(
            serde_json::from_str::<Value>(output)
                .map_err(|_| anyhow::anyhow!("capture requires a valid JSON response"))?,
        )
    } else {
        None
    };
    captures
        .iter()
        .map(|capture| {
            let value = match &capture.source {
                Source::Stdout => Value::String(output.trim_end_matches(['\r', '\n']).to_string()),
                Source::Json(path) => {
                    let mut value = json.as_ref().expect("JSON capture parsed");
                    for part in path {
                        value = value.get(part).with_context(|| {
                            format!(
                                "capture {:?}: missing JSON field {}",
                                capture.name,
                                path.join(".")
                            )
                        })?;
                    }
                    match value {
                        Value::String(_) | Value::Number(_) | Value::Bool(_) => value.clone(),
                        _ => bail!(
                            "capture {:?}: expected a non-null JSON scalar",
                            capture.name
                        ),
                    }
                }
            };
            if value.as_str().is_some_and(str::is_empty) {
                bail!("capture {:?}: value is empty", capture.name);
            }
            Ok((capture.name.clone(), value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Result<(Manifest, Plan)> {
        let manifest = crate::manifest::parse(text)?;
        let plan = Plan::build(&manifest)?;
        Ok((manifest, plan))
    }

    #[test]
    fn captures_and_environment_are_strict() {
        for text in [
            "[a]\nrun='true'\ncapture={}",
            "[a]\nrun='true'\ncapture={x=42}",
            "[a]\nrun='true'\ncapture={x='stderr'}",
            "[a]\nrun='true'\ncapture={x='json.token'}",
            "[a]\nget='http://x'\ncapture={x='stdout'}",
            "[a]\nget='http://x'\ncapture={x='json.items[0]'}",
            "[a]\nget='http://x'\ncapture={x='json..token'}",
            "[a]\nrun='true'\ncapture={'bad.name'='stdout'}",
            "[a]\nrun='true'\nbackground=true\ncapture={x='stdout'}",
            "[a]\nlog='app.log'\ncontains='ok'\ncapture={x='stdout'}",
            "[a]\nget='http://x'\nenv={X='value'}",
            "[a]\nrun='true'\nenv={X=42}",
            "[a]\nrun='true'\nenv={'bad-name'='value'}",
        ] {
            assert!(parsed(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn prerequisites_are_transitive_stable_and_run_once() {
        let (manifest, plan) = parsed(
            r#"
            [consumer]
            run = "true"
            env = { X = "${middle.x}", Y = "${producer.x}" }
            [unrelated]
            run = "true"
            [middle]
            run = "echo middle"
            env = { X = "${producer.x}" }
            capture = { x = "stdout" }
            [producer]
            run = "echo value"
            capture = { x = "stdout" }
        "#,
        )
        .unwrap();
        let (order, selected) = plan.execution(&manifest, Some("consumer"), "linux");
        assert_eq!(order, [3, 2, 0, 1]);
        assert_eq!(selected, [true, false, true, true]);
        assert_eq!(plan.sensitive, [true, false, true, true]);
    }

    #[test]
    fn excluded_consumers_do_not_schedule_producers() {
        let (manifest, plan) = parsed(
            r#"
            [consumer.1]
            run = "true"
            [consumer.2]
            os = "windows"
            run = "true"
            env = { X = "${producer.x}" }
            [producer]
            run = "echo value"
            capture = { x = "stdout" }
        "#,
        )
        .unwrap();
        assert_eq!(
            plan.execution(&manifest, Some("consumer"), "linux").1,
            [true, false]
        );
        assert_eq!(
            plan.execution(&manifest, Some("consumer"), "windows").1,
            [true, true]
        );
    }

    #[test]
    fn unknown_forward_and_cyclic_references_are_rejected_before_filtering() {
        for text in [
            "[a]\nos='windows'\nrun='true'\nenv={X='${missing.x}'}",
            "[a]\nrun='true'\ncapture={x='stdout'}\nenv={X='${a.x}'}",
            "[a.1]\nrun='true'\nenv={X='${a.2.x}'}\n[a.2]\nrun='echo x'\ncapture={x='stdout'}",
            "[a]\nrun='true'\ncapture={x='stdout'}\nenv={X='${b.x}'}\n[b]\nrun='true'\ncapture={x='stdout'}\nenv={X='${a.x}'}",
            "[a]\nrun='true'\nenv={X='${broken'}",
            "[a.1]\nrun='true'\ncapture={x='stdout'}\n['a.1']\nrun='true'\ncapture={x='stdout'}",
            "['a}']\nrun='true'\ncapture={x='stdout'}",
        ] { assert!(parsed(text).is_err(), "accepted {text}"); }
    }

    #[test]
    fn numbered_steps_and_quoted_names_have_exact_addresses() {
        let (manifest, plan) = parsed(
            r#"
            ["editor's login".2]
            run = "echo token"
            capture = { token = "stdout" }
            ["editor's login".10]
            run = "true"
            env = { TOKEN = "${editor's login.2.token}" }
        "#,
        )
        .unwrap();
        assert_eq!(plan.references[0][1], ["editor's login.2.token"]);
        assert_eq!(plan.execution(&manifest, None, "linux").0, [0]);
    }

    #[test]
    fn substitution_preserves_data_and_json_types_without_shell_evaluation() {
        let manifest = crate::manifest::parse(r#"
            [request]
            post = "http://localhost/item/${source.value}"
            headers = { Authorization = "Bearer ${source.value}" }
            body = '{"text":"${source.value}","number":"${source.number}","flag":"${source.flag}","literal":"$${untouched.value}"}'
            [command]
            run = "printf '%s' \"$INPUT\""
            env = { INPUT = "${source.value}" }
        "#).unwrap();
        let mut store = Store::new();
        let text = "a\"/雪 $(touch unwanted); ${no.recursion}";
        store.insert("source.value".into(), Value::String(text.into()));
        store.insert("source.number".into(), Value::from(42));
        store.insert("source.flag".into(), Value::from(false));
        let (check, _) = resolve(&manifest.scenarios[0].checks[0], &store).unwrap();
        let Check::Http {
            url, body, headers, ..
        } = check
        else {
            panic!("HTTP")
        };
        assert!(url.contains("a%22%2F%E9%9B%AA%20%24%28"), "{url}");
        assert_eq!(headers[0].1, format!("Bearer {text}"));
        let body: Value = serde_json::from_str(body.as_ref().unwrap()).unwrap();
        assert_eq!(body["text"], text);
        assert_eq!(body["number"], 42);
        assert_eq!(body["flag"], false);
        assert_eq!(body["literal"], "${untouched.value}");
        let (command, env) = resolve(&manifest.scenarios[1].checks[0], &store).unwrap();
        assert_eq!(command.label(), "printf '%s' \"$INPUT\"");
        assert_eq!(env[0].1, text);
        store.insert(
            "source.value".into(),
            Value::String("bad\r\nInjected: header".into()),
        );
        assert!(resolve(&manifest.scenarios[0].checks[0], &store).is_err());
        store.insert("source.value".into(), Value::String("bad\0value".into()));
        assert!(resolve(&manifest.scenarios[1].checks[0], &store).is_err());
    }

    #[test]
    fn templated_bodies_require_json_value_positions() {
        for body in ["${p.x}", "{\"${p.x}\":1}", "{\"x\":${p.x}}"] {
            let text = format!(
                "[p]\nrun='true'\ncapture={{x='stdout'}}\n[q]\npost='http://x'\nbody='{body}'"
            );
            assert!(parsed(&text).is_err(), "accepted {text}");
        }
        assert!(references("literal $${p.x}").unwrap().is_empty());
        assert_eq!(
            render("literal $${p.x}", &Store::new(), false).unwrap(),
            "literal ${p.x}"
        );
    }

    #[test]
    fn capture_is_a_required_scalar_and_stdout_preserves_inner_whitespace() {
        let captures = [Capture {
            name: "x".into(),
            source: Source::Json(vec!["data".into(), "id".into()]),
        }];
        assert_eq!(extract(&captures, r#"{"data":{"id":0}}"#).unwrap()[0].1, 0);
        assert_eq!(
            extract(&captures, r#"{"data":{"id":false}}"#).unwrap()[0].1,
            false
        );
        for output in [
            "invalid",
            "{}",
            r#"{"data":{"id":null}}"#,
            r#"{"data":{"id":[]}}"#,
            r#"{"data":{"id":""}}"#,
        ] {
            assert!(extract(&captures, output).is_err(), "accepted {output}");
        }
        let captures = [Capture {
            name: "x".into(),
            source: Source::Stdout,
        }];
        assert_eq!(extract(&captures, " a\nb \r\n").unwrap()[0].1, " a\nb ");
        assert!(extract(&captures, "\n").is_err());
    }
}
