//! Jersey numbers: CTC decoding of an OCR output, vote accumulation and plausibility.
//!
//! The OCR network itself lives in `tools::ocr` (feature `tools`). It hands over the class
//! probabilities per time step restricted to blank + digits (`DIGIT_CLASSES` = 11 columns:
//! column 0 = CTC blank, column 1 + d = digit d), and everything else - decoding, voting,
//! confirming a number - is plain Rust here.

use std::collections::HashMap;

/// Columns per time step: blank + ten digits.
pub const DIGIT_CLASSES: usize = 11;

#[derive(Clone, Debug, Default)]
pub struct JerseyVotes {
    score: HashMap<u8, f32>,
}

impl JerseyVotes {
    pub fn add(&mut self, number: u8, conf: f32) {
        *self.score.entry(number).or_insert(0.0) += conf.clamp(0.0, 1.0);
    }

    /// Evidence fades unless it is renewed, so a different person in the same slot can overturn it.
    pub fn decay(&mut self, factor: f32) {
        for v in self.score.values_mut() {
            *v *= factor;
        }
        self.score.retain(|_, v| *v > 0.02);
    }

    pub fn score_of(&self, number: u8) -> f32 {
        self.score.get(&number).copied().unwrap_or(0.0)
    }

    /// Best number if it has enough evidence and clearly beats the runner-up.
    /// `roster`: allowed numbers (empty = allow all).
    pub fn best(&self, roster: &[u8], min_score: f32, min_lead: f32) -> Option<u8> {
        let mut v: Vec<(u8, f32)> = self
            .score
            .iter()
            .filter(|(n, _)| roster.is_empty() || roster.contains(n))
            .map(|(n, s)| (*n, *s))
            .collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (n, s) = *v.first()?;
        let second = v.get(1).map(|x| x.1).unwrap_or(0.0);
        if s >= min_score && s - second >= min_lead {
            Some(n)
        } else {
            None
        }
    }

    /// Drop all evidence (used when the slot is taken over by someone else).
    pub fn clear(&mut self) {
        self.score.clear();
    }

    /// Scale down all evidence at once, e.g. when a lost slot is re-taken and we are not sure it is the same person.
    pub fn soften(&mut self, factor: f32) {
        self.decay(factor);
    }
}

/// Greedy CTC decoding. `probs` is `t x DIGIT_CLASSES` row-major (rows need not be normalised).
/// Returns the digit string and the mean probability of the emitted digits.
pub fn ctc_greedy(probs: &[f32], t: usize) -> (String, f32) {
    let (mut out, mut conf, mut prev) = (String::new(), Vec::new(), 0usize);
    for step in 0..t {
        let row = &probs[step * DIGIT_CLASSES..(step + 1) * DIGIT_CLASSES];
        let sum: f32 = row.iter().sum::<f32>().max(1e-9);
        let (mut best, mut bp) = (0usize, -1.0f32);
        for (i, p) in row.iter().enumerate() {
            if *p > bp {
                bp = *p;
                best = i;
            }
        }
        if best != prev && best != 0 {
            out.push((b'0' + (best as u8 - 1)) as char);
            conf.push(bp / sum);
        }
        prev = best;
    }
    let c = if conf.is_empty() {
        0.0
    } else {
        conf.iter().sum::<f32>() / conf.len() as f32
    };
    (out, c)
}

/// `0..=99` from a digit string of length 1-2 (leading zero dropped: "07" -> 7). Anything else -> None.
pub fn parse_number(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Log-likelihood of `label` (digits) under the CTC output.
pub fn ctc_loglik(probs: &[f32], t: usize, label: &[u8]) -> f64 {
    let mut ext: Vec<usize> = vec![0];
    for &d in label {
        ext.push(d as usize + 1);
        ext.push(0);
    }
    let s = ext.len();
    let lp = |step: usize, cls: usize| ((probs[step * DIGIT_CLASSES + cls] as f64) + 1e-12).ln();
    let lse = |a: f64, b: f64| {
        if a < b {
            b + (1.0 + (a - b).exp()).ln()
        } else {
            a + (1.0 + (b - a).exp()).ln()
        }
    };
    let neg = -1e30f64;
    let mut alpha = vec![neg; s];
    alpha[0] = lp(0, ext[0]);
    if s > 1 {
        alpha[1] = lp(0, ext[1]);
    }
    for step in 1..t {
        let mut next = vec![neg; s];
        for i in 0..s {
            let mut v = alpha[i];
            if i > 0 {
                v = lse(v, alpha[i - 1]);
            }
            if i > 1 && ext[i] != 0 && ext[i] != ext[i - 2] {
                v = lse(v, alpha[i - 2]);
            }
            next[i] = v + lp(step, ext[i]);
        }
        alpha = next;
    }
    if s > 1 {
        lse(alpha[s - 1], alpha[s - 2])
    } else {
        alpha[0]
    }
}

/// Best roster number and its posterior among the roster (constrained decoding: far more robust than free reading).
pub fn ctc_roster(probs: &[f32], t: usize, roster: &[u8]) -> Option<(u8, f32)> {
    if roster.is_empty() || t == 0 {
        return None;
    }
    let ll: Vec<f64> = roster
        .iter()
        .map(|n| {
            let digits: Vec<u8> = n.to_string().bytes().map(|b| b - b'0').collect();
            ctc_loglik(probs, t, &digits)
        })
        .collect();
    let mx = ll.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let sum: f64 = ll.iter().map(|l| (l - mx).exp()).sum();
    let (i, _) = ll
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
    Some((roster[i], ((ll[i] - mx).exp() / sum) as f32))
}

/// Goalkeeper numbers in a roster string: a number followed directly by `g`, `gk`, `tw`, `t`, `k`, `p` or `*`
/// (goalkeeper / Torwart / keeper / portero), e.g. "1g, 4, 5, 12tw" -> `[1, 12]`. Use [`parse_roster`] for all numbers.
pub fn parse_roster_keepers(s: &str) -> Vec<u8> {
    let b: Vec<char> = strip_names(s).chars().collect();
    let (mut i, mut out) = (0, Vec::new());
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n: Option<u8> = b[st..i].iter().collect::<String>().parse().ok();
            if let (Some(n), Some(c)) = (n, b.get(i))
                && matches!(c.to_ascii_lowercase(), 'g' | 't' | 'k' | 'p' | '*')
            {
                out.push(n);
            }
        } else {
            i += 1;
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Parse a roster string such as "4, 5,7;10 12" into numbers (a trailing keeper mark such as "1g" is ignored here).
pub fn parse_roster(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = strip_names(s)
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|x| x.parse().ok())
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// A roster string without the names: `7:Anna Müller, 12=Ben` -> `7, 12`. A name follows its number after `:` or `=`
/// and runs to the next `,` `;` or line break, so it may contain spaces (and no digits are read out of it).
fn strip_names(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_name = false;
    for c in s.chars() {
        match c {
            ',' | ';' | '\n' => {
                in_name = false;
                out.push(c);
            }
            ':' | '=' if !in_name => in_name = true,
            _ if in_name => {}
            _ => out.push(c),
        }
    }
    out
}

/// Names given in a roster string: `"7:Anna Müller, 1g=Ben, 4"` -> `[(7, "Anna Müller"), (1, "Ben")]`.
/// Entries without a name are left out; a number given twice keeps its first name.
pub fn parse_roster_names(s: &str) -> Vec<(u8, String)> {
    let mut out: Vec<(u8, String)> = Vec::new();
    for tok in s.split([',', ';', '\n']) {
        let Some((num, name)) = tok.split_once([':', '=']) else {
            continue;
        };
        let name = name.trim();
        let digits: String = num
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let (Ok(n), false) = (digits.parse::<u8>(), name.is_empty())
            && !out.iter().any(|(m, _)| *m == n)
        {
            out.push((n, name.to_string()));
        }
    }
    out
}
