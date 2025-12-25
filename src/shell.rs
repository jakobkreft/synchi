use std::path::Path;

pub fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    let mut quoted = String::from("'");
    for ch in input.chars() {
        if ch == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

pub fn shell_quote_path(path: &Path) -> String {
    let as_str = path.to_string_lossy();
    shell_quote(as_str.as_ref())
}
