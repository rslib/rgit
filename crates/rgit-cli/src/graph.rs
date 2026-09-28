//! `log --graph`: a port of git's graph.c, so the ASCII graph matches git's
//! byte for byte (colors left out).

#[derive(Clone, Copy, PartialEq)]
enum State {
    Padding,
    Skip,
    PreCommit,
    Commit,
    PostMerge,
    Collapsing,
}

#[derive(Default)]
struct Line {
    buf: String,
    width: usize,
}

impl Line {
    fn push(&mut self, c: char) {
        self.buf.push(c);
        self.width += 1;
    }
    fn push_n(&mut self, c: char, n: usize) {
        for _ in 0..n {
            self.push(c);
        }
    }
}

pub struct Graph {
    commit: String,
    parents: Vec<String>,
    width: usize,
    expansion_row: usize,
    state: State,
    prev_state: State,
    commit_index: usize,
    prev_commit_index: usize,
    merge_layout: i32,
    edges_added: i32,
    prev_edges_added: i32,
    columns: Vec<String>,
    new_columns: Vec<String>,
    mapping: Vec<i32>,
    old_mapping: Vec<i32>,
    mapping_size: usize,
}

const MERGE_CHARS: [char; 3] = ['/', '|', '\\'];

impl Graph {
    pub fn new() -> Self {
        Graph {
            commit: String::new(),
            parents: Vec::new(),
            width: 0,
            expansion_row: 0,
            state: State::Padding,
            prev_state: State::Padding,
            commit_index: 0,
            prev_commit_index: 0,
            merge_layout: 0,
            edges_added: 0,
            prev_edges_added: 0,
            columns: Vec::new(),
            new_columns: Vec::new(),
            mapping: Vec::new(),
            old_mapping: Vec::new(),
            mapping_size: 0,
        }
    }

    /// Move to the next commit; `parents` are its parents that are shown.
    pub fn update(&mut self, commit: &str, parents: Vec<String>) {
        self.commit = commit.to_owned();
        self.parents = parents;
        self.prev_commit_index = self.commit_index;
        self.update_columns();
        self.expansion_row = 0;
        self.state = if self.state != State::Padding {
            State::Skip
        } else if self.needs_pre_commit_line() {
            State::PreCommit
        } else {
            State::Commit
        };
    }

    fn set_state(&mut self, s: State) {
        self.prev_state = self.state;
        self.state = s;
    }

    fn num_parents(&self) -> usize {
        self.parents.len()
    }

    fn find_new_column(&self, commit: &str) -> Option<usize> {
        self.new_columns.iter().position(|c| c == commit)
    }

    fn insert_into_new_columns(&mut self, commit: &str, idx: i32) {
        let i = match self.find_new_column(commit) {
            Some(i) => i,
            None => {
                self.new_columns.push(commit.to_owned());
                self.new_columns.len() - 1
            }
        };
        let mapping_idx;
        if self.num_parents() > 1 && idx > -1 && self.merge_layout == -1 {
            // The first parent of a merge picks the merge line's layout by
            // whether it sits in a column left of the merge.
            let dist = idx - i as i32;
            let shift = if dist > 1 { 2 * dist - 3 } else { 1 };
            self.merge_layout = if dist > 0 { 0 } else { 1 };
            self.edges_added = self.num_parents() as i32 + self.merge_layout - 2;
            mapping_idx = self.width as i32 + (self.merge_layout - 1) * shift;
            self.width += 2 * self.merge_layout as usize;
        } else if self.edges_added > 0
            && self.width >= 2
            && i as i32 == self.mapping[self.width - 2]
        {
            // A merge added columns but this parent is in the last one:
            // join the two edges at once.
            mapping_idx = self.width as i32 - 2;
            self.edges_added = -1;
        } else {
            mapping_idx = self.width as i32;
            self.width += 2;
        }
        self.mapping[mapping_idx as usize] = i as i32;
    }

    fn update_columns(&mut self) {
        std::mem::swap(&mut self.columns, &mut self.new_columns);
        self.new_columns.clear();
        let max_new = self.columns.len() + self.num_parents();
        if self.mapping.len() < 2 * max_new {
            self.mapping.resize(2 * max_new, -1);
            self.old_mapping.resize(2 * max_new, -1);
        }
        self.mapping_size = 2 * max_new;
        for m in &mut self.mapping[..self.mapping_size] {
            *m = -1;
        }
        self.width = 0;
        self.prev_edges_added = self.edges_added;
        self.edges_added = 0;

        let mut seen_this = false;
        let num_columns = self.columns.len();
        for i in 0..=num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit.clone()
            } else {
                self.columns[i].clone()
            };
            if col_commit == self.commit {
                seen_this = true;
                self.commit_index = i;
                self.merge_layout = -1;
                for p in self.parents.clone() {
                    self.insert_into_new_columns(&p, i as i32);
                }
                if self.num_parents() == 0 {
                    self.width += 2;
                }
            } else {
                self.insert_into_new_columns(&col_commit, -1);
            }
        }
        while self.mapping_size > 1 && self.mapping[self.mapping_size - 1] < 0 {
            self.mapping_size -= 1;
        }
    }

    fn num_dashed_parents(&self) -> i32 {
        self.num_parents() as i32 + self.merge_layout - 3
    }

    fn needs_pre_commit_line(&self) -> bool {
        self.num_parents() >= 3
            && self.commit_index + 1 < self.columns.len()
            && (self.expansion_row as i32) < self.num_dashed_parents() * 2
    }

    fn is_mapping_correct(&self) -> bool {
        (0..self.mapping_size).all(|i| {
            let t = self.mapping[i];
            t < 0 || t as usize == i / 2
        })
    }

    fn pad(&self, line: &mut Line) {
        if line.width < self.width {
            line.push_n(' ', self.width - line.width);
        }
    }

    fn padding(&mut self, line: &mut Line) {
        for _ in 0..self.new_columns.len() {
            line.push('|');
            line.push(' ');
        }
    }

    fn skip(&mut self, line: &mut Line) {
        line.push_n('.', 3);
        let next = if self.needs_pre_commit_line() {
            State::PreCommit
        } else {
            State::Commit
        };
        self.set_state(next);
    }

    fn pre_commit(&mut self, line: &mut Line) {
        let mut seen_this = false;
        for i in 0..self.columns.len() {
            if self.columns[i] == self.commit {
                seen_this = true;
                line.push('|');
                line.push_n(' ', self.expansion_row);
            } else if seen_this && self.expansion_row == 0 {
                if self.prev_state == State::PostMerge && self.prev_commit_index < i {
                    line.push('\\');
                } else {
                    line.push('|');
                }
            } else if seen_this {
                line.push('\\');
            } else {
                line.push('|');
            }
            line.push(' ');
        }
        self.expansion_row += 1;
        if !self.needs_pre_commit_line() {
            self.set_state(State::Commit);
        }
    }

    fn octopus(&self, line: &mut Line) {
        let dashed = self.num_dashed_parents();
        for i in 0..dashed {
            line.push('-');
            line.push(if i == dashed - 1 { '.' } else { '-' });
        }
    }

    fn commit_line(&mut self, line: &mut Line) {
        let mut seen_this = false;
        let num_columns = self.columns.len();
        for i in 0..=num_columns {
            let is_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                true
            } else {
                self.columns[i] == self.commit
            };
            if is_commit {
                seen_this = true;
                line.push('*');
                if self.num_parents() > 2 {
                    self.octopus(line);
                }
            } else if seen_this && self.edges_added > 1 {
                line.push('\\');
            } else if seen_this && self.edges_added == 1 {
                if self.prev_state == State::PostMerge
                    && self.prev_edges_added > 0
                    && self.prev_commit_index < i
                {
                    line.push('\\');
                } else {
                    line.push('|');
                }
            } else if self.prev_state == State::Collapsing
                && self.old_mapping.get(2 * i + 1) == Some(&(i as i32))
                && self.mapping.get(2 * i).is_some_and(|&m| m < i as i32)
            {
                line.push('/');
            } else {
                line.push('|');
            }
            line.push(' ');
        }
        let next = if self.num_parents() > 1 {
            State::PostMerge
        } else if self.is_mapping_correct() {
            State::Padding
        } else {
            State::Collapsing
        };
        self.set_state(next);
    }

    fn post_merge(&mut self, line: &mut Line) {
        let mut seen_this = false;
        let first_parent = self.parents.first().cloned();
        let mut parent_col = false;
        let num_columns = self.columns.len();
        for i in 0..=num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit.clone()
            } else {
                self.columns[i].clone()
            };
            if col_commit == self.commit {
                seen_this = true;
                let mut idx = self.merge_layout as usize;
                for j in 0..self.num_parents() {
                    line.push(MERGE_CHARS[idx]);
                    if idx == 2 {
                        if self.edges_added > 0 || j + 1 < self.num_parents() {
                            line.push(' ');
                        }
                    } else {
                        idx += 1;
                    }
                }
                if self.edges_added == 0 {
                    line.push(' ');
                }
            } else if seen_this {
                line.push(if self.edges_added > 0 { '\\' } else { '|' });
                line.push(' ');
            } else {
                line.push('|');
                if self.merge_layout != 0 || i + 1 != self.commit_index {
                    line.push(if parent_col { '_' } else { ' ' });
                }
            }
            if Some(&col_commit) == first_parent.as_ref() {
                parent_col = true;
            }
        }
        let next = if self.is_mapping_correct() {
            State::Padding
        } else {
            State::Collapsing
        };
        self.set_state(next);
    }

    fn collapsing(&mut self, line: &mut Line) {
        let mut used_horizontal = false;
        let mut horizontal_edge: i32 = -1;
        let mut horizontal_edge_target: i32 = -1;
        std::mem::swap(&mut self.mapping, &mut self.old_mapping);
        for m in &mut self.mapping[..self.mapping_size] {
            *m = -1;
        }
        for i in 0..self.mapping_size {
            let target = self.old_mapping[i];
            if target < 0 {
                continue;
            }
            let t2 = (target * 2) as usize;
            if t2 == i {
                self.mapping[i] = target;
            } else if self.mapping[i - 1] < 0 {
                self.mapping[i - 1] = target;
                if horizontal_edge == -1 {
                    horizontal_edge = i as i32;
                    horizontal_edge_target = target;
                    let mut j = t2 + 3;
                    while j + 2 < i {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            } else if self.mapping[i - 1] == target {
                // Joins the branch line to its left: same parent.
            } else {
                self.mapping[i - 2] = target;
                if horizontal_edge == -1 {
                    horizontal_edge_target = target;
                    horizontal_edge = i as i32 - 1;
                    let mut j = t2 + 3;
                    while j + 2 < i {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            }
        }
        self.old_mapping[..self.mapping_size].copy_from_slice(&self.mapping[..self.mapping_size]);
        if self.mapping[self.mapping_size - 1] < 0 {
            self.mapping_size -= 1;
        }
        for i in 0..self.mapping_size {
            let target = self.mapping[i];
            if target < 0 {
                line.push(' ');
            } else if (target * 2) as usize == i {
                line.push('|');
            } else if target == horizontal_edge_target && i as i32 != horizontal_edge - 1 {
                if i != (target * 2) as usize + 3 {
                    self.mapping[i] = -1;
                }
                used_horizontal = true;
                line.push('_');
            } else {
                if used_horizontal && (i as i32) < horizontal_edge {
                    self.mapping[i] = -1;
                }
                line.push('/');
            }
        }
        if self.is_mapping_correct() {
            self.set_state(State::Padding);
        }
    }

    /// The next line of graph, and whether it was the commit's own line.
    fn next_line(&mut self) -> (String, bool) {
        let mut line = Line::default();
        let mut shown = false;
        match self.state {
            State::Padding => self.padding(&mut line),
            State::Skip => self.skip(&mut line),
            State::PreCommit => self.pre_commit(&mut line),
            State::Commit => {
                self.commit_line(&mut line);
                shown = true;
            }
            State::PostMerge => self.post_merge(&mut line),
            State::Collapsing => self.collapsing(&mut line),
        }
        self.pad(&mut line);
        (line.buf, shown)
    }

    fn is_finished(&self) -> bool {
        self.state == State::Padding
    }

    /// The graph for a line of text that is not the commit's (git's
    /// graph_padding_line).
    pub fn padding_line(&mut self) -> String {
        if self.state != State::Commit {
            return self.next_line().0;
        }
        let mut line = Line::default();
        for c in &self.columns {
            line.push('|');
            if *c == self.commit && self.num_parents() > 2 {
                line.push_n(' ', (self.num_parents() - 2) * 2);
            } else {
                line.push(' ');
            }
        }
        self.pad(&mut line);
        self.prev_state = State::Padding;
        line.buf
    }

    /// Graph lines down to and including the commit's line, which is left
    /// open for the commit's text.
    pub fn show_commit(&mut self) -> String {
        let mut out = String::new();
        if self.is_finished() {
            return self.padding_line();
        }
        loop {
            let (line, shown) = self.next_line();
            out.push_str(&line);
            if shown || self.is_finished() {
                break;
            }
            out.push('\n');
        }
        out
    }

    /// The prefix for the next line of text (git's graph_show_oneline).
    pub fn oneline(&mut self) -> String {
        self.next_line().0
    }

    /// The commit's text with the graph before each line after the first,
    /// then whatever graph lines the commit still needs.
    pub fn commit_msg(&mut self, msg: &str) -> String {
        let mut out = String::new();
        let mut rest = msg;
        while !rest.is_empty() {
            let (line, next) = match rest.find('\n') {
                Some(i) => rest.split_at(i + 1),
                None => (rest, ""),
            };
            out.push_str(line);
            if !next.is_empty() {
                out.push_str(&self.oneline());
            }
            rest = next;
        }
        if !self.is_finished() {
            let terminated = msg.ends_with('\n');
            if !terminated {
                out.push('\n');
            }
            loop {
                out.push_str(&self.next_line().0);
                if self.is_finished() {
                    break;
                }
                out.push('\n');
            }
            if terminated {
                out.push('\n');
            }
        }
        out
    }
}
