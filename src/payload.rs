use crate::diff_parser::{DiffLine, ParsedDiff};

pub fn describe_target(line: &DiffLine) -> String {
    match (line.old_lineno, line.new_lineno) {
        (Some(old), Some(new)) => format!("context line old:{old} new:{new}"),
        (None, Some(new)) => format!("added line new:{new}"),
        (Some(old), None) => format!("deleted line old:{old}"),
        (None, None) => "diff line".to_string(),
    }
}

pub fn build_review_payload(
    parsed: &ParsedDiff,
    line: &DiffLine,
    comment: &str,
    max_hunk_lines: usize,
) -> String {
    let file_path = line.file_path.as_deref().unwrap_or("(unknown file)");
    let mut output = vec![
        format!("File: {file_path}"),
        format!("My comment at line {}", comment_line_number(line)),
        String::new(),
    ];

    if let Some(mut hunk_lines) = format_hunk_window(parsed, line, comment, max_hunk_lines) {
        output.append(&mut hunk_lines);
    } else {
        output.push(line.raw.clone());
        output.extend(format_comment(comment));
    }

    output.push(String::new());
    output.join("\n")
}

fn format_hunk_window(
    parsed: &ParsedDiff,
    line: &DiffLine,
    comment: &str,
    max_hunk_lines: usize,
) -> Option<Vec<String>> {
    let hunk_index = line.hunk_index?;
    let target_index = line.hunk_line_index?;
    let hunk = parsed.hunks.get(hunk_index)?;
    let hunk_lines = &hunk.line_indices;
    let max_hunk_lines = max_hunk_lines.max(1);

    let (start, end) = if hunk_lines.len() <= max_hunk_lines {
        (0, hunk_lines.len())
    } else {
        let before = max_hunk_lines.saturating_sub(1) / 2;
        let mut start = target_index.saturating_sub(before);
        let mut end = (start + max_hunk_lines).min(hunk_lines.len());
        start = end.saturating_sub(max_hunk_lines);
        end = (start + max_hunk_lines).min(hunk_lines.len());
        (start, end)
    };

    let mut output = vec![hunk.header.clone()];
    if start > 0 {
        output.push("...".to_string());
    }

    for local_index in start..end {
        let line_index = *hunk_lines.get(local_index)?;
        let current = parsed.lines.get(line_index)?;
        if local_index == target_index {
            output.push(current.raw.clone());
            output.extend(format_comment(comment));
        } else {
            output.push(current.raw.clone());
        }
    }

    Some(output)
}

fn comment_line_number(line: &DiffLine) -> String {
    line.new_lineno
        .or(line.old_lineno)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn format_comment(comment: &str) -> Vec<String> {
    let lines: Vec<&str> = comment.trim_end().lines().collect();
    if lines.len() == 1 {
        return vec![format!(">> comment: {}", lines[0])];
    }

    let mut output = vec![">> comment:".to_string()];
    output.extend(lines.into_iter().map(|line| format!(">> {line}")));
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_parser::{LineKind, parse_unified_diff};

    fn make_diff(line_count: usize) -> String {
        let mut lines = vec![
            "diff --git a/big.txt b/big.txt".to_string(),
            "--- a/big.txt".to_string(),
            "+++ b/big.txt".to_string(),
            format!("@@ -1,{line_count} +1,{line_count} @@"),
        ];
        for index in 1..=line_count {
            let prefix = if index == 10 { '+' } else { ' ' };
            lines.push(format!("{prefix}line {index}"));
        }
        lines.join("\n") + "\n"
    }

    #[test]
    fn marks_selected_line_and_comment_insertion() {
        let parsed = parse_unified_diff(&make_diff(20));
        let target = parsed
            .lines
            .iter()
            .find(|line| line.kind == LineKind::Add && line.raw == "+line 10")
            .unwrap();

        let payload =
            build_review_payload(&parsed, target, "Please revisit this.\nSecond line.", 11);

        assert!(payload.contains("File: big.txt"));
        assert!(payload.contains("My comment at line 10"));
        assert!(payload.contains("+line 10"));
        assert!(payload.contains(">> comment:\n>> Please revisit this.\n>> Second line."));
        assert!(payload.contains("Second line."));
        assert!(!payload.contains("VDIFF_SELECTED_LINE"));
        assert!(!payload.contains("```"));
    }

    #[test]
    fn trims_long_hunks_around_target() {
        let parsed = parse_unified_diff(&make_diff(200));
        let target = parsed
            .lines
            .iter()
            .find(|line| line.kind == LineKind::Add && line.raw == "+line 10")
            .unwrap();

        let payload = build_review_payload(&parsed, target, "Trim this.", 11);

        assert!(payload.contains("+line 10"));
        assert!(payload.contains(">> comment: Trim this."));
        assert!(!payload.contains(" line 200"));
    }
}
