use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStatus {
    Distinguishable,
    Inconclusive,
    Exploratory,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WilsonInterval {
    pub lower_95: f64,
    pub upper_95: f64,
}

pub fn wilson_interval(k: u64, n: u64) -> Option<WilsonInterval> {
    if n == 0 || k > n {
        return None;
    }
    let n = n as f64;
    let p = k as f64 / n;
    let z = 1.96;
    let z2 = z * z;
    let d = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / d;
    let half = z * ((p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt()) / d;
    Some(WilsonInterval {
        lower_95: (center - half).max(0.0),
        upper_95: (center + half).min(1.0),
    })
}

pub fn compare(a: (u64, u64), b: (u64, u64), min_n: u64, exploratory: bool) -> ComparisonStatus {
    if exploratory {
        return ComparisonStatus::Exploratory;
    }
    if a.1 < min_n || b.1 < min_n {
        return ComparisonStatus::Inconclusive;
    }
    match (wilson_interval(a.0, a.1), wilson_interval(b.0, b.1)) {
        (Some(x), Some(y)) if x.upper_95 < y.lower_95 || y.upper_95 < x.lower_95 => {
            ComparisonStatus::Distinguishable
        }
        _ => ComparisonStatus::Inconclusive,
    }
}
