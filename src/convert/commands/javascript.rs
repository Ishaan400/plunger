//! JavaScript: a `fetch` call, as it runs in Node.js 18+ (top-level `await`, so an ES module or
//! the REPL). Export only.

use super::{PartBody, Parts};
use serde_json::Value;

/// A double-quoted JavaScript string. JSON's escaping is valid JavaScript.
fn quote(s: &str) -> String {
    Value::from(s).to_string()
}

/// The largest integer a JavaScript number holds exactly (`Number.MAX_SAFE_INTEGER`).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Whether `value`, written as a JavaScript literal, means the same thing: every integer fits in
/// a JavaScript number, and no object has a `__proto__` key (in a literal that sets the prototype
/// instead of adding a field, so `JSON.stringify` would drop it).
fn same_in_javascript(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(|f| f.fract() != 0.0 || f.abs() <= MAX_SAFE_INTEGER),
        Value::Array(items) => items.iter().all(same_in_javascript),
        Value::Object(fields) => fields.iter().all(|(k, v)| k != "__proto__" && same_in_javascript(v)),
        _ => true,
    }
}

/// A JSON body as `JSON.stringify(...)` of the same value, so it reads as an object. A body that
/// is not JSON (`{{variables}}` outside quotes, for one) or would change in JavaScript stays text.
fn json_body(text: &str) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(value) if same_in_javascript(&value) => {
            let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            format!("JSON.stringify({})", pretty.replace('\n', "\n  "))
        }
        _ => quote(text),
    }
}

/// The file name a path ends in, for the multipart part (`C:/data/a.txt` -> `a.txt`).
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(path)
}

pub fn export(p: &Parts) -> String {
    let mut notes: Vec<&str> = Vec::new();
    let mut before: Vec<String> = Vec::new();
    let mut options: Vec<String> = Vec::new();

    if p.method != "GET" {
        options.push(format!("method: {}", quote(&p.method)));
    }
    if p.has_body() && matches!(p.method.as_str(), "GET" | "HEAD") {
        notes.push("// fetch refuses a body on a GET or HEAD request.");
    }

    let is_json = p.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.to_ascii_lowercase().contains("json"));
    let mut headers: Vec<(&str, &str)> = p
        .headers
        .iter()
        // fetch writes the multipart Content-Type itself, with the boundary it chose.
        .filter(|(k, _)| !(matches!(p.body, PartBody::Multipart(_)) && k.eq_ignore_ascii_case("content-type")))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    // curl and PowerShell send this one on their own; fetch would label a string body text/plain.
    if matches!(p.body, PartBody::Form(_)) && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        headers.push(("Content-Type", "application/x-www-form-urlencoded"));
    }
    if !headers.is_empty() {
        let lowered: Vec<String> = headers.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
        let repeated = lowered.iter().enumerate().any(|(i, k)| lowered[..i].contains(k));
        // An object literal keeps only the last of a repeated name; with a list of pairs every
        // value is sent (fetch joins them into one line, `X-A: 1, 2`).
        let entries: Vec<String> = headers
            .iter()
            .map(|(k, v)| if repeated { format!("[{}, {}]", quote(k), quote(v)) } else { format!("{}: {}", quote(k), quote(v)) })
            .collect();
        let (open, close) = if repeated { ('[', ']') } else { ('{', '}') };
        options.push(format!("headers: {open}\n    {}\n  {close}", entries.join(",\n    ")));
    }

    match &p.body {
        PartBody::None => {}
        PartBody::Text(text) if is_json => options.push(format!("body: {}", json_body(text))),
        PartBody::Text(text) | PartBody::Form(text) => options.push(format!("body: {}", quote(text))),
        PartBody::Multipart(fields) => {
            if fields.iter().any(|(_, is_file, _)| *is_file) {
                before.push("import { readFile } from \"node:fs/promises\";".into());
                before.push(String::new());
                notes.push("// Reading a file needs Node.js; in a browser, append a File from an <input type=\"file\"> instead.");
            }
            before.push("const form = new FormData();".into());
            for (name, is_file, value) in fields {
                if *is_file {
                    before.push(format!("form.append({}, new Blob([await readFile({})]), {});", quote(name), quote(value), quote(file_name(value))));
                } else {
                    before.push(format!("form.append({}, {});", quote(name), quote(value)));
                }
            }
            before.push(String::new());
            options.push("body: form".into());
        }
    }
    if !p.follow_redirects {
        options.push("redirect: \"manual\"".into());
    }
    if p.insecure_tls {
        notes.push("// Plunger skips certificate checks for this request; fetch has no option for that (Node.js: NODE_TLS_REJECT_UNAUTHORIZED=0 turns them off for the whole process).");
    }

    let mut out = before.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    if options.is_empty() {
        out.push_str(&format!("await fetch({});", quote(&p.url)));
    } else {
        out.push_str(&format!("await fetch({}, {{\n  {}\n}});", quote(&p.url), options.join(",\n  ")));
    }
    for note in notes {
        out.push('\n');
        out.push_str(note);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{export, Dialect};
    use crate::domain::model::{BodyMode, FieldKind, FormField, PersistedState};

    fn js(state: &PersistedState) -> String {
        export(state, Dialect::JavaScriptFetch)
    }

    fn json_post(body: &str) -> PersistedState {
        PersistedState { method: "POST".into(), url: "https://h/orders".into(), body_mode: BodyMode::Json, json_body: body.into(), ..Default::default() }
    }

    #[test]
    fn a_get_is_a_bare_fetch() {
        let state = PersistedState { method: "get".into(), url: " https://h/items?page=2 ".into(), ..Default::default() };
        assert_eq!(js(&state), "await fetch(\"https://h/items?page=2\");");
    }

    #[test]
    fn a_json_post_sends_an_object_through_json_stringify() {
        let state = PersistedState { headers_text: "Authorization: Bearer {{token}}".into(), ..json_post(r#"{"name":"widget","tags":["a","b"],"price":1.5}"#) };
        assert_eq!(
            js(&state),
            r#"await fetch("https://h/orders", {
  method: "POST",
  headers: {
    "Authorization": "Bearer {{token}}",
    "Content-Type": "application/json"
  },
  body: JSON.stringify({
    "name": "widget",
    "tags": [
      "a",
      "b"
    ],
    "price": 1.5
  })
});"#
        );
    }

    #[test]
    fn a_json_body_that_would_change_in_javascript_stays_text() {
        // a {{variable}} outside quotes is not JSON
        assert!(js(&json_post("{\"id\": {{id}}}")).contains(r#"body: "{\"id\": {{id}}}""#));
        // past Number.MAX_SAFE_INTEGER, JavaScript would round the id
        assert!(js(&json_post(r#"{"id":9007199254740993}"#)).contains(r#"body: "{\"id\":9007199254740993}""#));
        assert!(js(&json_post(r#"{"id":9007199254740991}"#)).contains("JSON.stringify"));
        // in a literal, __proto__ sets the prototype and JSON.stringify would drop it
        assert!(js(&json_post(r#"{"__proto__":{"admin":true}}"#)).contains(r#"body: "{\"__proto__\":{\"admin\":true}}""#));
    }

    #[test]
    fn quotes_backslashes_and_newlines_are_escaped() {
        let state = PersistedState {
            method: "PUT".into(),
            url: "https://h/it's".into(),
            headers_text: "X-Note: say \"hi\" \\ there\nContent-Type: text/plain".into(),
            body_mode: BodyMode::Raw,
            raw_body: "line one\nline \"two\"\t\\end".into(),
            ..Default::default()
        };
        let text = js(&state);
        assert!(text.starts_with("await fetch(\"https://h/it's\", {"), "{text}");
        assert!(text.contains(r#""X-Note": "say \"hi\" \\ there""#), "{text}");
        assert!(text.contains(r#"body: "line one\nline \"two\"\t\\end""#), "{text}");
        // a raw body is sent as written, even when it happens to be JSON
        let raw_json = PersistedState { body_mode: BodyMode::Raw, raw_body: "{\"a\": 1}".into(), ..state };
        assert!(js(&raw_json).contains(r#"body: "{\"a\": 1}""#));
    }

    #[test]
    fn a_form_body_says_it_is_url_encoded() {
        let state = PersistedState { method: "POST".into(), url: "https://h/form".into(), body_mode: BodyMode::UrlEncoded, urlencoded_body: "a=1\nb=two%20words".into(), ..Default::default() };
        let text = js(&state);
        assert!(text.contains(r#""Content-Type": "application/x-www-form-urlencoded""#), "{text}");
        assert!(text.contains(r#"body: "a=1&b=two%20words""#), "{text}");
        // a Content-Type the user set is kept, not doubled
        let own = PersistedState { headers_text: "content-type: application/x-www-form-urlencoded; charset=utf-8".into(), ..state };
        assert_eq!(js(&own).matches("ontent-").count(), 1);
    }

    #[test]
    fn a_multipart_body_is_a_form_data() {
        let state = PersistedState {
            method: "POST".into(),
            url: "https://h/upload".into(),
            headers_text: "Content-Type: multipart/form-data".into(),
            body_mode: BodyMode::Multipart,
            multipart_fields: vec![
                FormField { key: "title".into(), kind: FieldKind::Text, value: "my \"doc\"".into(), enabled: true },
                FormField { key: "file".into(), kind: FieldKind::File, value: "C:\\data\\a.txt".into(), enabled: true },
                FormField { key: "off".into(), kind: FieldKind::Text, value: "x".into(), enabled: false },
            ],
            ..Default::default()
        };
        let text = js(&state);
        assert!(text.starts_with("import { readFile } from \"node:fs/promises\";\n\nconst form = new FormData();\n"), "{text}");
        assert!(text.contains(r#"form.append("title", "my \"doc\"");"#), "{text}");
        assert!(text.contains(r#"form.append("file", new Blob([await readFile("C:\\data\\a.txt")]), "a.txt");"#), "{text}");
        assert!(!text.contains("\"off\"") && text.contains("body: form"), "{text}");
        // fetch sets the multipart Content-Type with its boundary
        assert!(!text.contains("multipart/form-data"), "{text}");
        // text fields alone need nothing from Node.js
        let text_only = PersistedState { multipart_fields: state.multipart_fields[..1].to_vec(), ..state };
        assert!(js(&text_only).starts_with("const form = new FormData();"));
    }

    #[test]
    fn repeated_headers_are_all_sent() {
        let state = PersistedState { url: "https://h".into(), headers_text: "Accept: text/html\naccept: application/json".into(), ..Default::default() };
        let text = js(&state);
        assert!(text.contains("headers: [\n    [\"Accept\", \"text/html\"],\n    [\"accept\", \"application/json\"]\n  ]"), "{text}");
    }

    #[test]
    fn redirects_tls_and_a_body_on_get_are_spelled_out() {
        let state = PersistedState { follow_redirects: false, insecure_tls: true, ..json_post("{}") };
        let text = js(&state);
        assert!(text.contains("redirect: \"manual\""), "{text}");
        assert!(text.contains("NODE_TLS_REJECT_UNAUTHORIZED"), "{text}");
        assert!(!text.contains("refuses a body"), "{text}");
        let get_with_body = PersistedState { method: "GET".into(), ..json_post("{}") };
        assert!(js(&get_with_body).contains("// fetch refuses a body on a GET or HEAD request."));
    }
}
