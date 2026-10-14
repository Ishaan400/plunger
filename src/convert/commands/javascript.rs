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

/// Request headers that carry a single value (RFC 9110 §5.3): sending one twice is not valid
/// HTTP, and joining the values does not mean the same thing either.
const SINGLE_VALUE_HEADERS: &[&str] =
    &["authorization", "proxy-authorization", "content-type", "content-length", "host", "user-agent", "referer", "range", "if-modified-since", "if-unmodified-since"];

/// One entry per header name, in first-seen order and spelling, as fetch puts them on the wire:
/// it sends a repeated name once, Cookie values joined with `; ` (RFC 6265 §5.4) and any other
/// with `, ` (RFC 9110 §5.3), which a server reads the same as separate lines. Doing that here
/// shows the value that is really sent, on every runtime, instead of leaving it to `Headers`.
/// Also returns the single-value headers that were repeated, where the joined value differs.
fn merge_repeated(headers: &[(&str, &str)]) -> (Vec<(String, String)>, Vec<String>) {
    let mut merged: Vec<(String, String)> = Vec::new();
    let mut singletons: Vec<String> = Vec::new();
    for (name, value) in headers {
        match merged.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
            Some((first, joined)) => {
                joined.push_str(if name.eq_ignore_ascii_case("cookie") { "; " } else { ", " });
                joined.push_str(value);
                let lower = name.to_ascii_lowercase();
                if SINGLE_VALUE_HEADERS.contains(&lower.as_str()) && !singletons.iter().any(|s| s.eq_ignore_ascii_case(first)) {
                    singletons.push(first.clone());
                }
            }
            None => merged.push((name.to_string(), value.to_string())),
        }
    }
    (merged, singletons)
}

pub fn export(p: &Parts) -> String {
    let mut notes: Vec<String> = Vec::new();
    let mut before: Vec<String> = Vec::new();
    let mut options: Vec<String> = Vec::new();

    if p.method != "GET" {
        options.push(format!("method: {}", quote(&p.method)));
    }
    if p.has_body() && matches!(p.method.as_str(), "GET" | "HEAD") {
        notes.push("// fetch refuses a body on a GET or HEAD request.".into());
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
    let (headers, repeated_singletons) = merge_repeated(&headers);
    if !repeated_singletons.is_empty() {
        notes.push(format!(
            "// {} is repeated: fetch sends one line with the values joined, where curl sends one line each. A server reads that as one invalid value; keep the one you mean.",
            repeated_singletons.join(", ")
        ));
    }
    if !headers.is_empty() {
        let entries: Vec<String> = headers.iter().map(|(k, v)| format!("{}: {}", quote(k), quote(v))).collect();
        options.push(format!("headers: {{\n    {}\n  }}", entries.join(",\n    ")));
    }

    match &p.body {
        PartBody::None => {}
        PartBody::Text(text) if is_json => options.push(format!("body: {}", json_body(text))),
        PartBody::Text(text) | PartBody::Form(text) => options.push(format!("body: {}", quote(text))),
        PartBody::Multipart(fields) => {
            if fields.iter().any(|(_, is_file, _)| *is_file) {
                before.push("import { readFile } from \"node:fs/promises\";".into());
                before.push(String::new());
                notes.push("// Reading a file needs Node.js; in a browser, append a File from an <input type=\"file\"> instead.".into());
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
        notes.push("// Plunger skips certificate checks for this request; fetch has no option for that (Node.js: NODE_TLS_REJECT_UNAUTHORIZED=0 turns them off for the whole process).".into());
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
        out.push_str(&note);
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
    fn a_repeated_header_is_one_line_with_every_value() {
        // as fetch sends it, and as a server reads the separate lines curl sends
        let state = PersistedState {
            url: "https://h".into(),
            headers_text: "Accept: text/html\nCookie: session={{sid}}\naccept: application/json\nX-Forwarded-For: 10.0.0.1\ncookie: theme=dark\nX-Forwarded-For: 10.0.0.2".into(),
            ..Default::default()
        };
        let text = js(&state);
        assert!(
            text.contains("headers: {\n    \"Accept\": \"text/html, application/json\",\n    \"Cookie\": \"session={{sid}}; theme=dark\",\n    \"X-Forwarded-For\": \"10.0.0.1, 10.0.0.2\"\n  }"),
            "{text}"
        );
        assert!(!text.contains("is repeated"), "list headers join cleanly: {text}");
    }

    #[test]
    fn a_repeated_single_value_header_is_flagged() {
        let state = PersistedState { url: "https://h".into(), headers_text: "Authorization: Bearer one\nauthorization: Bearer two\nAccept: */*".into(), ..Default::default() };
        let text = js(&state);
        assert!(text.contains("\"Authorization\": \"Bearer one, Bearer two\""), "{text}");
        assert!(text.ends_with("\n// Authorization is repeated: fetch sends one line with the values joined, where curl sends one line each. A server reads that as one invalid value; keep the one you mean."), "{text}");
    }

    #[test]
    fn no_line_ends_in_whitespace() {
        let mut states = vec![
            json_post("{\n  \"a\": [1, {\"b\": \"c  \"}],\n  \"d\": {}\n}  "),
            PersistedState { method: "PUT".into(), url: "https://h ".into(), body_mode: BodyMode::Raw, raw_body: "x  \n  y \n".into(), ..Default::default() },
            PersistedState { follow_redirects: false, insecure_tls: true, headers_text: "A: 1\na: 2\nHost: h\nhost: i".into(), ..json_post("{}") },
        ];
        states.push(PersistedState {
            method: "POST".into(),
            body_mode: BodyMode::Multipart,
            multipart_fields: vec![FormField { key: "f".into(), kind: FieldKind::File, value: "a.txt ".into(), enabled: true }],
            ..Default::default()
        });
        for state in states {
            let text = js(&state);
            for line in text.lines() {
                assert_eq!(line, line.trim_end(), "{text}");
            }
        }
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
