use anyhow::{Result, ensure};

/// Split option values using OpenSSH's `argv_split` rules, including comments.
/// See `misc.c`, with `terminate_on_comment` enabled.
pub(super) fn split(input: &str) -> Result<Vec<String>> {
    let input = input.split_once('\0').map_or(input, |(prefix, _)| prefix);
    let mut chars = input.chars().peekable();
    let mut words = Vec::new();

    loop {
        while matches!(chars.peek(), Some(' ' | '\t')) {
            chars.next();
        }
        if matches!(chars.peek(), None | Some('#')) {
            break;
        }

        let mut word = String::new();
        let mut quote = None;
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.peek().copied() {
                    Some(next @ ('\'' | '"' | '\\')) => {
                        chars.next();
                        word.push(next);
                    }
                    Some(' ') if quote.is_none() => {
                        chars.next();
                        word.push(' ');
                    }
                    _ => word.push('\\'),
                }
            } else if quote.is_none() && matches!(ch, ' ' | '\t') {
                break;
            } else if quote.is_none() && matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if quote == Some(ch) {
                quote = None;
            } else {
                word.push(ch);
            }
        }
        ensure!(quote.is_none(), "Unterminated quotation in -o option");
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_start_only_at_token_boundaries() -> Result<()> {
        for (input, expected) in [
            (" \t ", vec![]),
            ("# ignored 'unclosed", vec![]),
            ("target#literal", vec!["target#literal"]),
            ("target # ignored 'unclosed", vec!["target"]),
            ("''#literal", vec!["#literal"]),
            ("'#quoted' next", vec!["#quoted", "next"]),
        ] {
            assert_eq!(split(input)?, expected, "{input:?}");
        }
        Ok(())
    }

    #[test]
    fn only_openssh_escape_sequences_remove_backslashes() -> Result<()> {
        for (input, expected) in [
            (r"a\b", vec![r"a\b"]),
            (r"a\\b", vec![r"a\b"]),
            (r#"a\"b a\'b"#, vec!["a\"b", "a'b"]),
            (r"a\ b", vec!["a b"]),
            ("a\\\tb", vec!["a\\", "b"]),
            (r#""a\ b""#, vec![r"a\ b"]),
            (r#"'a\"b' "a\'b""#, vec!["a\"b", "a'b"]),
            ("trailing\\", vec!["trailing\\"]),
        ] {
            assert_eq!(split(input)?, expected, "{input:?}");
        }
        Ok(())
    }

    #[test]
    fn quoted_segments_join_and_only_horizontal_ascii_whitespace_separates() -> Result<()> {
        for (input, expected) in [
            (r#"a""b c'd'e"#, vec!["ab", "cde"]),
            (r#""" ''"#, vec!["", ""]),
            ("'a b'\t\"c d\"", vec!["a b", "c d"]),
            ("鍵\u{2003}path next", vec!["鍵\u{2003}path", "next"]),
            ("a\nb\rc", vec!["a\nb\rc"]),
            ("first\0ignored", vec!["first"]),
        ] {
            assert_eq!(split(input)?, expected, "{input:?}");
        }
        Ok(())
    }

    #[test]
    fn unmatched_quotes_are_errors() {
        for input in ["'unclosed", "\"unclosed", "\"escaped\\\""] {
            assert!(split(input).is_err(), "{input:?}");
        }
    }
}
