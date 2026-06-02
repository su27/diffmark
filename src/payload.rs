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
    target_lines: &[DiffLine],
    comment: &str,
    max_hunk_lines: usize,
) -> String {
    let Some(first_line) = target_lines.first() else {
        return String::new();
    };
    let file_path = first_line.file_path.as_deref().unwrap_or("(unknown file)");
    let mut output = vec![
        format!("File: {file_path}"),
        format!("My comment at {}", describe_target_lines(target_lines)),
        String::new(),
    ];

    if let Some(mut hunk_lines) = format_hunk_window(parsed, target_lines, comment, max_hunk_lines)
    {
        output.append(&mut hunk_lines);
    } else {
        output.push(target_start_marker(target_lines));
        output.extend(target_lines.iter().map(|line| line.raw.clone()));
        if target_lines.len() > 1 {
            output.push(">> target end".to_string());
        }
        output.extend(format_comment(comment));
    }

    output.push(String::new());
    output.join("\n")
}

fn format_hunk_window(
    parsed: &ParsedDiff,
    target_lines: &[DiffLine],
    comment: &str,
    max_hunk_lines: usize,
) -> Option<Vec<String>> {
    let first_line = target_lines.first()?;
    let last_line = target_lines.last()?;
    let hunk_index = first_line.hunk_index?;
    if !target_lines
        .iter()
        .all(|line| line.hunk_index == Some(hunk_index))
    {
        return None;
    }
    let target_start = first_line.hunk_line_index?;
    let target_end = last_line.hunk_line_index?;
    let (target_start, target_end) = normalized_range(target_start, target_end);
    let hunk = parsed.hunks.get(hunk_index)?;
    let hunk_lines = &hunk.line_indices;
    let max_hunk_lines = max_hunk_lines.max(1);
    let target_line_count = target_end - target_start + 1;
    let window_lines = max_hunk_lines.max(target_line_count);

    let (start, end) = if hunk_lines.len() <= window_lines {
        (0, hunk_lines.len())
    } else {
        let spare = window_lines.saturating_sub(target_line_count);
        let before = spare / 2;
        let mut start = target_start.saturating_sub(before);
        let mut end = (start + window_lines).min(hunk_lines.len());
        if end <= target_end {
            end = (target_end + 1).min(hunk_lines.len());
            start = end.saturating_sub(window_lines);
        }
        end = (start + window_lines).min(hunk_lines.len());
        (start, end)
    };

    let mut output = vec![hunk.header.clone()];
    if start > 0 {
        output.push("...".to_string());
    }

    for local_index in start..end {
        let line_index = *hunk_lines.get(local_index)?;
        let current = parsed.lines.get(line_index)?;
        if local_index == target_start {
            output.push(target_start_marker(target_lines));
        }
        if local_index >= target_start && local_index <= target_end {
            output.push(current.raw.clone());
            if local_index == target_end {
                if target_lines.len() > 1 {
                    output.push(">> target end".to_string());
                }
                output.extend(format_comment(comment));
            }
        } else {
            output.push(current.raw.clone());
        }
    }

    Some(output)
}

fn target_start_marker(target_lines: &[DiffLine]) -> String {
    if target_lines.len() > 1 {
        ">> target start".to_string()
    } else {
        ">> target:".to_string()
    }
}

pub fn describe_target_lines(lines: &[DiffLine]) -> String {
    match lines {
        [] => "line unknown".to_string(),
        [line] => format!("line {}", line_number(line)),
        lines => multi_line_label(lines),
    }
}

fn multi_line_label(lines: &[DiffLine]) -> String {
    let old_span = line_span(lines.iter().filter_map(|line| line.old_lineno));
    let new_span = line_span(lines.iter().filter_map(|line| line.new_lineno));
    let all_context = lines
        .iter()
        .all(|line| line.old_lineno.is_some() && line.new_lineno.is_some());

    match (old_span, new_span) {
        (Some(old), Some(new)) if old == new && all_context => format!("lines {old}"),
        (Some(old), Some(new)) => {
            format!(
                "old {} / new {} ({} diff lines)",
                qualified_line_span(&old),
                qualified_line_span(&new),
                lines.len()
            )
        }
        (Some(old), None) => format!("old {}", qualified_line_span(&old)),
        (None, Some(new)) => format!("new {}", qualified_line_span(&new)),
        (None, None) => format!("{} diff lines", lines.len()),
    }
}

fn line_span(values: impl Iterator<Item = usize>) -> Option<String> {
    let mut values = values;
    let first = values.next()?;
    let last = values.last().unwrap_or(first);
    Some(if first == last {
        first.to_string()
    } else {
        format!("{first}-{last}")
    })
}

fn qualified_line_span(span: &str) -> String {
    if span.contains('-') {
        format!("lines {span}")
    } else {
        format!("line {span}")
    }
}

fn line_number(line: &DiffLine) -> String {
    line.new_lineno
        .or(line.old_lineno)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn normalized_range(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
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

        let payload = build_review_payload(
            &parsed,
            std::slice::from_ref(target),
            "Please revisit this.\nSecond line.",
            11,
        );

        assert!(payload.contains("File: big.txt"));
        assert!(payload.contains("My comment at line 10"));
        assert!(payload.contains(">> target:\n+line 10"));
        assert!(payload.contains("+line 10"));
        assert!(payload.contains(">> comment:\n>> Please revisit this.\n>> Second line."));
        assert!(payload.contains("Second line."));
        assert!(!payload.contains("DIFFMARK_SELECTED_LINE"));
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

        let payload = build_review_payload(&parsed, std::slice::from_ref(target), "Trim this.", 11);

        assert!(payload.contains("+line 10"));
        assert!(payload.contains(">> target:\n+line 10"));
        assert!(payload.contains(">> comment: Trim this."));
        assert!(!payload.contains(" line 200"));
    }

    #[test]
    fn keeps_all_selected_lines_in_multi_line_payload() {
        let parsed = parse_unified_diff(&make_diff(200));
        let targets: Vec<_> = parsed
            .lines
            .iter()
            .filter(|line| {
                line.hunk_line_index
                    .is_some_and(|index| (8..=12).contains(&index))
            })
            .cloned()
            .collect();

        let payload = build_review_payload(&parsed, &targets, "Block comment.", 3);

        assert!(payload.contains("My comment at old lines 9-12 / new lines 9-13 (5 diff lines)"));
        assert!(payload.contains(
            ">> target start\n line 9\n+line 10\n line 11\n line 12\n line 13\n>> target end"
        ));
        assert!(payload.contains(">> comment: Block comment."));
        assert!(!payload.contains(" line 200"));
    }

    #[test]
    fn labels_replace_selection_with_old_and_new_lines() {
        let parsed = parse_unified_diff(concat!(
            "diff --git a/src/app.rs b/src/app.rs\n",
            "--- a/src/app.rs\n",
            "+++ b/src/app.rs\n",
            "@@ -1,3 +1,3 @@\n",
            " context\n",
            "-old\n",
            "+new\n",
            " tail\n",
        ));
        let targets: Vec<_> = parsed
            .lines
            .iter()
            .filter(|line| line.raw == "-old" || line.raw == "+new")
            .cloned()
            .collect();

        let payload = build_review_payload(&parsed, &targets, "Interesting.", 11);

        assert!(
            payload.contains("My comment at old line 2 / new line 2 (2 diff lines)"),
            "{payload}"
        );
        assert!(
            payload
                .contains(">> target start\n-old\n+new\n>> target end\n>> comment: Interesting."),
            "{payload}"
        );
    }
}
