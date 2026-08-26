//! A small fzf-style subsequence matcher: a query matches when its characters
//! appear in order in the label, scored so that matches at word boundaries and
//! runs of adjacent matches rank first. Case-insensitive.

use super::Item;

/// One label that matched, with its score and the matched char positions.
pub struct Match {
    pub index: usize,
    pub score: i32,
    pub positions: Vec<usize>,
}

const MATCH: i32 = 16;
const BOUNDARY: i32 = 30;
const CONSECUTIVE: i32 = 20;
const MAX_GAP_PENALTY: i32 = 10;

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Score `text` against `query`; `None` if `query` is not a subsequence. An
/// empty query matches everything with score 0 and no highlights.
fn score(query: &[char], text: &str) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let mut positions = Vec::with_capacity(query.len());
    let mut total = 0;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for (i, &c) in chars.iter().enumerate() {
        if qi >= query.len() || lower(c) != query[qi] {
            continue;
        }
        let boundary = i == 0
            || !chars[i - 1].is_alphanumeric()
            || (c.is_uppercase() && chars[i - 1].is_lowercase());
        let mut s = MATCH;
        if boundary {
            s += BOUNDARY;
        }
        match prev {
            Some(p) if p + 1 == i => s += CONSECUTIVE,
            Some(p) => s -= ((i - p - 1) as i32).min(MAX_GAP_PENALTY),
            None => s -= (i as i32).min(MAX_GAP_PENALTY),
        }
        total += s;
        positions.push(i);
        prev = Some(i);
        qi += 1;
    }
    (qi == query.len()).then_some((total, positions))
}

/// Filter `items` by `query`, returning the matches sorted best-first (ties
/// keep original order). An empty query returns all items in order.
pub fn filter<T>(query: &str, items: &[Item<T>]) -> Vec<Match> {
    let needle: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(lower).collect();
    let mut matches: Vec<Match> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            score(&needle, &item.label).map(|(score, positions)| Match {
                index,
                score,
                positions,
            })
        })
        .collect();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(labels: &[&str]) -> Vec<Item<usize>> {
        labels
            .iter()
            .enumerate()
            .map(|(i, l)| Item::new(i, *l))
            .collect()
    }

    #[test]
    fn subsequence_matches_out_of_order_gaps() {
        let its = items(&["feature", "refactor", "fixups"]);
        let got: Vec<usize> = filter("ftr", &its).into_iter().map(|m| m.index).collect();
        // "ftr" is a subsequence of feaTuRe and RefacTor, not of fixups.
        assert!(got.contains(&0) && got.contains(&1));
        assert!(!got.contains(&2));
    }

    #[test]
    fn empty_query_keeps_all_in_order() {
        let its = items(&["b", "a", "c"]);
        let got: Vec<usize> = filter("", &its).into_iter().map(|m| m.index).collect();
        assert_eq!(got, vec![0, 1, 2]);
    }

    #[test]
    fn word_boundary_outranks_mid_word() {
        let its = items(&["unrelated cli text", "cli"]);
        let ranked: Vec<usize> = filter("cli", &its).into_iter().map(|m| m.index).collect();
        // The standalone "cli" (whole-word, boundary start) should rank first.
        assert_eq!(ranked.first(), Some(&1));
    }

    #[test]
    fn case_insensitive_with_positions() {
        let its = items(&["Cargo.toml"]);
        let m = &filter("cto", &its)[0];
        assert_eq!(m.positions, vec![0, 6, 7]);
    }
}
