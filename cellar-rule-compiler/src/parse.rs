//! Extract a JSON object from an LLM response.
//!
//! Even with strict output instructions, models sometimes wrap JSON in
//! markdown code fences or precede it with a "Here is the rule:" sentence.
//! This module finds and returns the first balanced `{ ... }` substring.

/// Find the first balanced `{ ... }` substring. Returns `None` if no balanced
/// object is found.
///
/// Handles:
/// - Plain JSON (`{...}`)
/// - JSON in fenced blocks (```json {...} ```)
/// - JSON preceded by prose
/// - Embedded strings containing braces (skipped properly)
/// - Escape sequences inside strings (`\"` doesn't end the string)
pub fn extract_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut start: Option<usize> = None;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start_idx) = start {
                        return std::str::from_utf8(&bytes[start_idx..=i]).ok();
                    }
                } else if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_object() {
        let s = r#"{"a":1}"#;
        assert_eq!(extract_json_object(s), Some(r#"{"a":1}"#));
    }

    #[test]
    fn fenced_code_block() {
        let s = "Here is the rule:\n```json\n{\"a\":1,\"b\":2}\n```\n";
        assert_eq!(extract_json_object(s), Some(r#"{"a":1,"b":2}"#));
    }

    #[test]
    fn nested_objects() {
        let s = r#"{"outer":{"inner":1}}"#;
        assert_eq!(extract_json_object(s), Some(r#"{"outer":{"inner":1}}"#));
    }

    #[test]
    fn skips_braces_in_strings() {
        let s = r#"{"text":"a } b { c","n":1}"#;
        assert_eq!(extract_json_object(s), Some(s));
    }

    #[test]
    fn handles_escaped_quotes() {
        let s = r#"{"text":"say \"hi\" and {}","n":1}"#;
        assert_eq!(extract_json_object(s), Some(s));
    }

    #[test]
    fn returns_first_object_only() {
        let s = r#"{"a":1} {"b":2}"#;
        assert_eq!(extract_json_object(s), Some(r#"{"a":1}"#));
    }

    #[test]
    fn returns_none_on_no_brace() {
        assert_eq!(extract_json_object("hello world"), None);
    }

    #[test]
    fn returns_none_on_unbalanced() {
        assert_eq!(extract_json_object("{"), None);
    }

    #[test]
    fn handles_preceding_prose() {
        let s = "Sure, here you go:\n{\"id\":\"draft\"}";
        assert_eq!(extract_json_object(s), Some(r#"{"id":"draft"}"#));
    }
}
