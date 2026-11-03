//! Summary statistics: median, spread and a seeded bootstrap confidence interval of the median.

use crate::Rng;

#[derive(Clone, Debug)]
pub struct Summary {
    pub n: usize,
    pub median: f64,
    pub mean: f64,
    pub stdev: f64,
    pub min: f64,
    pub max: f64,
    /// 95% percentile-bootstrap CI of the median (2000 resamples, seeded). Only meaningful for n >= 5.
    pub ci95_lo: f64,
    pub ci95_hi: f64,
    pub ci_reliable: bool,
}

pub fn median_sorted(v: &[f64]) -> f64 {
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

pub fn summarize(xs: &[f64], seed: u64) -> Option<Summary> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n as f64;
    let var = if n > 1 {
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64
    } else {
        0.0
    };
    let (mut lo, mut hi) = (v[0], v[n - 1]);
    if n >= 3 {
        let mut r = Rng::new(seed);
        let mut meds = Vec::with_capacity(2000);
        let mut s = vec![0.0; n];
        for _ in 0..2000 {
            for x in s.iter_mut() {
                *x = v[(r.next_u64() as usize) % n];
            }
            s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            meds.push(median_sorted(&s));
        }
        meds.sort_by(|a, b| a.partial_cmp(b).unwrap());
        lo = meds[(meds.len() as f64 * 0.025) as usize];
        hi = meds[(meds.len() as f64 * 0.975) as usize - 1];
    }
    Some(Summary {
        n,
        median: median_sorted(&v),
        mean,
        stdev: var.sqrt(),
        min: v[0],
        max: v[n - 1],
        ci95_lo: lo,
        ci95_hi: hi,
        ci_reliable: n >= 5,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basics() {
        let s = summarize(&[1.0, 2.0, 3.0, 4.0, 100.0], 1).unwrap();
        assert_eq!(s.median, 3.0);
        assert!(s.ci95_lo <= 3.0 && s.ci95_hi >= 3.0 && s.ci_reliable);
        assert!(summarize(&[], 1).is_none());
        assert_eq!(summarize(&[2.0, 4.0], 1).unwrap().median, 3.0);
    }
}
