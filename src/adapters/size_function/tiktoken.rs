use crate::domain::policy::{SizeFunction, SourceSpan};

/// Tiktoken-based size function
/// Uses tiktoken to count tokens in the source code span
pub struct TiktokenSizeFunction;

impl Default for TiktokenSizeFunction {
    fn default() -> Self {
        Self::new()
    }
}

impl TiktokenSizeFunction {
    pub fn new() -> Self {
        Self
    }
}

impl SizeFunction for TiktokenSizeFunction {
    fn compute(&self, source: &str, span: &SourceSpan, doc_texts: &[String]) -> u32 {
        // Extract the code snippet from the span
        let lines: Vec<&str> = source.lines().collect();

        if span.start_line as usize >= lines.len() {
            return 0;
        }

        let start_line_idx = span.start_line as usize;
        let end_line_idx = (span.end_line as usize).min(lines.len() - 1);

        let mut code_snippet = String::new();

        if start_line_idx == end_line_idx {
            // Single line
            let line = lines[start_line_idx];
            let start_col = span.start_column as usize;
            let end_col = span.end_column as usize;
            let start = column_to_byte_idx(line, start_col);
            let end = column_to_byte_idx(line, end_col);
            if start < end {
                code_snippet.push_str(&line[start..end]);
            }
        } else {
            // Multiple lines
            // First line
            let first_line = lines[start_line_idx];
            let start_col = span.start_column as usize;
            let start = column_to_byte_idx(first_line, start_col);
            if start < first_line.len() {
                code_snippet.push_str(&first_line[start..]);
            }
            code_snippet.push('\n');

            // Middle lines
            for line in lines.iter().take(end_line_idx).skip(start_line_idx + 1) {
                code_snippet.push_str(line);
                code_snippet.push('\n');
            }

            // Last line
            if end_line_idx < lines.len() {
                let last_line = lines[end_line_idx];
                let end_col = span.end_column as usize;
                let end = column_to_byte_idx(last_line, end_col);
                if end > 0 {
                    code_snippet.push_str(&last_line[..end]);
                }
            }
        }

        // --- Comment Stripping Logic ---

        // 1. Remove recognized doc_texts contents
        let mut pure_logic = code_snippet;
        for doc in doc_texts {
            pure_logic = pure_logic.replace(doc, "");
        }

        // 2. Strip common comment markers and empty comment lines
        // This covers ///, //, #, and block comment markers like /*, */, *
        let lines: Vec<String> = pure_logic
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                // If the line consists only of comment markers or is empty after markers removed
                if trimmed.starts_with("///")
                    || trimmed.starts_with("//")
                    || trimmed.starts_with('#')
                    || trimmed.starts_with("/*")
                    || trimmed.starts_with("*/")
                    || trimmed == "*"
                {
                    "" // Effectively remove the line
                } else {
                    line // Keep the line as is (minus the doc content removed earlier)
                }
            })
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let final_text = lines.join("\n");

        // Use a simple token counting approach (approximate)
        count_tokens_approx(&final_text)
    }

    fn id(&self) -> &'static str {
        "approx-tokenizer-v1"
    }
}

fn count_tokens_approx(text: &str) -> u32 {
    // Simple approximation: count words and punctuation
    text.split_whitespace()
        .map(|word| {
            // Rough approximation: 1 token per word, plus punctuation
            let punct_count = word.chars().filter(|c| !c.is_alphanumeric()).count();
            (1 + punct_count / 2).max(1)
        })
        .sum::<usize>() as u32
}

/// Convert an LSP-style column offset to a safe UTF-8 byte index.
///
/// Different indexers use different `Document.position_encoding` values (UTF-8 bytes,
/// UTF-16 code units, UTF-32 code points). We currently don't thread that encoding
/// through to this function, so we make a best-effort conversion that will never
/// panic on non-ASCII text:
/// - If the column lands on a UTF-8 char boundary, treat it as a byte offset.
/// - Otherwise, treat it as a Unicode scalar (Rust `char`) offset.
fn column_to_byte_idx(line: &str, column: usize) -> usize {
    let clamped = column.min(line.len());
    if line.is_char_boundary(clamped) {
        return clamped;
    }

    // Fallback: interpret `column` as a character offset.
    line.char_indices()
        .nth(column)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| line.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::policy::SourceSpan;

    #[test]
    fn test_single_line_span() {
        let f = TiktokenSizeFunction::new();
        let source = "def foo(): return 42";
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 18,
        };
        let n = f.compute(source, &span, &[]);
        assert!(n >= 1);
    }

    #[test]
    fn test_multi_line_span() {
        let f = TiktokenSizeFunction::new();
        let source = "line0\nline1\nline2";
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 2,
            end_column: 5,
        };
        let n = f.compute(source, &span, &[]);
        assert!(n >= 1);
    }

    #[test]
    fn test_boundary_handling() {
        let f = TiktokenSizeFunction::new();
        let source = "ab";
        let span = SourceSpan {
            start_line: 0,
            start_column: 1,
            end_line: 0,
            end_column: 2,
        };
        let n = f.compute(source, &span, &[]);
        assert!(n <= source.len() as u32); // sanity: not larger than char count
    }

    #[test]
    fn test_empty_span_returns_zero() {
        let f = TiktokenSizeFunction::new();
        let source = "x";
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let n = f.compute(source, &span, &[]);
        assert_eq!(n, 0);
    }

    #[test]
    fn test_out_of_range_line_returns_zero() {
        let f = TiktokenSizeFunction::new();
        let source = "one line";
        let span = SourceSpan {
            start_line: 10,
            start_column: 0,
            end_line: 10,
            end_column: 5,
        };
        assert_eq!(f.compute(source, &span, &[]), 0);
    }

    #[test]
    fn test_exclude_comments() {
        let f = TiktokenSizeFunction::new();
        // 10 lines of comments + 1 line of code
        let source = "/// doc\n/// doc\n/// doc\n/// doc\n/// doc\n/// doc\n/// doc\n/// doc\n/// doc\n/// doc\nfn main() {}";
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 10,
            end_column: 12,
        };

        let doc_texts = vec!["doc".to_string()];
        let size = f.compute(source, &span, &doc_texts);

        // "fn main() {}" should be very few tokens (around 3-5)
        println!("Size with comments stripped: {}", size);
        assert!(size < 10);
    }

    #[test]
    fn test_unicode_columns_do_not_panic() {
        let f = TiktokenSizeFunction::new();
        // Contains multi-byte UTF-8 chars like 'ü' and '€'
        let source = "                'üäö €'])";
        let span = SourceSpan {
            start_line: 0,
            start_column: 0,
            end_line: 0,
            // Intentionally use a column that is likely to land inside a multibyte sequence
            // if interpreted as a raw byte index.
            end_column: 25,
        };

        let _ = f.compute(source, &span, &[]);
    }
}
