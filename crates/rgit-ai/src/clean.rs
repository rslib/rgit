use crate::Task;

/// A draft loses a code fence that wraps the whole message; fenced blocks in
/// the body stay. A review only loses blank lines at both ends.
pub(crate) fn clean(task: Task, raw: &str) -> String {
    let lines: Vec<&str> = match task {
        Task::Draft => raw.lines().map(str::trim_end).collect(),
        Task::Review => raw.lines().collect(),
    };
    let lines = trim_blank(&lines);
    let lines = match task {
        Task::Draft => match lines {
            [first, inner @ .., last] if is_fence(first) && is_fence(last) => trim_blank(inner),
            _ => lines,
        },
        Task::Review => lines,
    };
    lines.join("\n")
}

fn trim_blank<'a, 'b>(lines: &'b [&'a str]) -> &'b [&'a str] {
    let blank = |line: &&str| line.trim().is_empty();
    let start = lines.iter().position(|l| !blank(l)).unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !blank(l))
        .map_or(start, |i| i + 1);
    &lines[start..end]
}

fn is_fence(line: &str) -> bool {
    let line = line.trim();
    line.strip_prefix("```").is_some_and(|rest| {
        rest.chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    })
}
