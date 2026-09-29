use std::cmp::Reverse;

const MATCH: i32 = 16;
const START_BONUS: i32 = 10;
const BOUNDARY_BONUS: i32 = 8;
const CAMEL_BONUS: i32 = 6;
const CONSECUTIVE_BONUS: i32 = 8;
const GAP_START_PENALTY: i32 = 5;
const GAP_EXTENSION_PENALTY: i32 = 1;
const SEPARATORS: [char; 7] = ['/', '-', '_', '.', ' ', ':', '@'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    pub positions: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    pub positions: Vec<usize>,
}

pub fn rank<'a>(query: &str, candidates: impl IntoIterator<Item = &'a str>) -> Vec<Ranked> {
    let terms: Vec<&str> = query.split_whitespace().collect();
    let candidates = candidates.into_iter().enumerate();
    if terms.is_empty() {
        return candidates
            .map(|(index, _)| Ranked {
                index,
                positions: Vec::new(),
            })
            .collect();
    }
    let mut matched: Vec<(Match, usize, usize)> = candidates
        .filter_map(|(index, candidate)| {
            let found = match_terms(&terms, candidate)?;
            Some((found, candidate.chars().count(), index))
        })
        .collect();
    matched.sort_by_key(|(found, length, index)| (Reverse(found.score), *length, *index));
    matched
        .into_iter()
        .map(|(found, _, index)| Ranked {
            index,
            positions: found.positions,
        })
        .collect()
}

fn match_terms(terms: &[&str], candidate: &str) -> Option<Match> {
    let text: Vec<char> = candidate.chars().collect();
    let mut total = Match {
        score: 0,
        positions: Vec::new(),
    };
    for term in terms {
        let found = match_term(term, &text)?;
        total.score += found.score;
        total.positions.extend(found.positions);
    }
    total.positions.sort_unstable();
    total.positions.dedup();
    Some(total)
}

fn match_term(term: &str, text: &[char]) -> Option<Match> {
    let case_sensitive = term.chars().any(char::is_uppercase);
    let term: Vec<char> = term.chars().collect();
    if term.is_empty() || term.len() > text.len() {
        return None;
    }
    let equal = |wanted: char, found: char| {
        if case_sensitive {
            wanted == found
        } else {
            found.to_lowercase().eq(wanted.to_lowercase())
        }
    };
    let bonuses: Vec<i32> = (0..text.len()).map(|at| bonus(text, at)).collect();

    let mut scores: Vec<Vec<Option<i32>>> = vec![vec![None; text.len()]; term.len()];
    let mut previous: Vec<Vec<Option<usize>>> = vec![vec![None; text.len()]; term.len()];
    for (at, &found) in text.iter().enumerate() {
        if equal(term[0], found) {
            scores[0][at] = Some(MATCH + bonuses[at]);
        }
    }
    for row in 1..term.len() {
        let mut gapped: Option<(i32, usize)> = None;
        for at in 1..text.len() {
            if at >= 2 {
                let extended = gapped.map(|(score, from)| (score - GAP_EXTENSION_PENALTY, from));
                let opened =
                    scores[row - 1][at - 2].map(|score| (score - GAP_START_PENALTY, at - 2));
                gapped = best(extended, opened);
            }
            if !equal(term[row], text[at]) {
                continue;
            }
            let adjacent = scores[row - 1][at - 1].map(|score| (score + CONSECUTIVE_BONUS, at - 1));
            if let Some((score, from)) = best(adjacent, gapped) {
                scores[row][at] = Some(score + MATCH + bonuses[at]);
                previous[row][at] = Some(from);
            }
        }
    }

    let last = term.len() - 1;
    let (score, end) = scores[last]
        .iter()
        .enumerate()
        .filter_map(|(at, score)| score.map(|score| (score, at)))
        .max_by_key(|&(score, at)| (score, Reverse(at)))?;
    let mut positions = vec![end];
    let mut at = end;
    for row in (1..term.len()).rev() {
        at = previous[row][at]?;
        positions.push(at);
    }
    positions.reverse();
    Some(Match { score, positions })
}

fn best(a: Option<(i32, usize)>, b: Option<(i32, usize)>) -> Option<(i32, usize)> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.0 > a.0 { b } else { a }),
        (a, b) => a.or(b),
    }
}

fn bonus(text: &[char], at: usize) -> i32 {
    let Some(before) = at.checked_sub(1).map(|before| text[before]) else {
        return START_BONUS;
    };
    if SEPARATORS.contains(&before) {
        BOUNDARY_BONUS
    } else if before.is_lowercase() && text[at].is_uppercase() {
        CAMEL_BONUS
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positions(query: &str, candidate: &str) -> Option<Vec<usize>> {
        let terms: Vec<&str> = query.split_whitespace().collect();
        match_terms(&terms, candidate).map(|found| found.positions)
    }

    fn ranked<'a>(query: &str, candidates: &[&'a str]) -> Vec<&'a str> {
        rank(query, candidates.iter().copied())
            .into_iter()
            .map(|ranked| candidates[ranked.index])
            .collect()
    }

    #[test]
    fn the_query_matches_in_order_anywhere_in_the_candidate() {
        assert_eq!(positions("amx", "amux"), Some(vec![0, 1, 3]));
        assert_eq!(positions("ux", "amux"), Some(vec![2, 3]));
        assert_eq!(positions("xa", "amux"), None);
        assert_eq!(positions("amuxx", "amux"), None);
        assert_eq!(positions("", "amux"), Some(vec![]));
    }

    #[test]
    fn lowercase_ignores_case_and_uppercase_does_not() {
        assert_eq!(positions("am", "AMux"), Some(vec![0, 1]));
        assert_eq!(positions("AM", "AMux"), Some(vec![0, 1]));
        assert_eq!(positions("Am", "amux"), None);
        assert_eq!(positions("é", "CAFÉ"), Some(vec![3]));
    }

    #[test]
    fn each_word_of_the_query_has_to_match() {
        assert_eq!(
            positions("api work", "work/api"),
            Some(vec![0, 1, 2, 3, 5, 6, 7])
        );
        assert_eq!(positions("api home", "work/api"), None);
    }

    #[test]
    fn the_best_alignment_wins_over_the_first_one() {
        assert_eq!(positions("ab", "a-xab"), Some(vec![3, 4]));
        assert_eq!(positions("fb", "foo-bar"), Some(vec![0, 4]));
    }

    #[test]
    fn boundaries_runs_and_short_names_rank_first() {
        assert_eq!(ranked("fb", &["fabric", "foo-bar"]), ["foo-bar", "fabric"]);
        assert_eq!(
            ranked("amux", &["a-m-u-x", "tamux", "amux-old", "amux"]),
            ["amux", "amux-old", "tamux", "a-m-u-x"]
        );
        assert_eq!(ranked("dots", &["dotfiles", "dots"]), ["dots", "dotfiles"]);
        assert_eq!(ranked("wa", &["workApi", "wax"]), ["wax", "workApi"]);
    }

    #[test]
    fn an_empty_query_keeps_every_candidate_in_order() {
        assert_eq!(ranked("  ", &["b", "a", "c"]), ["b", "a", "c"]);
        assert_eq!(ranked("zz", &["b", "a"]), Vec::<&str>::new());
    }
}
