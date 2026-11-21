pub fn sanitize_filename_component(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_control() => out.push('_'),
            c => out.push(c),
        }
    }
    let cleaned = out
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    let cleaned = if cleaned.is_empty() {
        "unknown_file".to_string()
    } else {
        cleaned
    };
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    let cleaned = if reserved {
        format!("_{}", cleaned)
    } else {
        cleaned
    };
    cleaned.chars().take(200).collect()
}

pub fn content_disposition_attachment(filename: &str) -> String {
    let safe = sanitize_filename_component(filename);
    let ascii_fallback: String = safe
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '-' | ' ' | '(' | ')' | '[' | ']')
            {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii_fallback = ascii_fallback
        .trim_matches(|c: char| c == '.' || c == ' ')
        .to_string();
    let ascii_fallback = if ascii_fallback.is_empty() {
        "download".to_string()
    } else {
        ascii_fallback
    };
    let mut encoded = String::new();
    for b in safe.as_bytes() {
        match *b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'.'
            | b'_'
            | b'-'
            | b'~'
            | b'('
            | b')'
            | b' ' => {
                if *b == b' ' {
                    encoded.push_str("%20");
                } else {
                    encoded.push(*b as char);
                }
            }
            _ => encoded.push_str(&format!("%{:02X}", b)),
        }
    }
    format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii_fallback.replace('"', "_"),
        encoded
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_cyrillic_and_spaces() {
        let s = sanitize_filename_component("Отчёт 2026 (финал).pdf");
        assert_eq!(s, "Отчёт 2026 (финал).pdf");
    }

    #[test]
    fn strips_path_components_and_control() {
        let s = sanitize_filename_component("../a\\b\0c.txt");
        assert!(!s.contains('/'));
        assert!(!s.contains('\\'));
        assert!(!s.contains('\0'));
    }

    #[test]
    fn content_disposition_has_fallback_and_utf8() {
        let h = content_disposition_attachment("Отчёт (1).pdf");
        assert!(h.contains("filename=\""));
        assert!(h.contains("filename*=UTF-8''"));
    }
}
