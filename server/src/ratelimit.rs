//! Per-IP sliding-window rate limiter (shared by both leaderboard POSTs).

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

pub struct RateLimiter {
    limit: usize,
    window: Duration,
    inner: Mutex<Inner>,
}

struct Inner {
    buckets: HashMap<String, VecDeque<Instant>>,
    last_prune: Instant,
}

impl RateLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        RateLimiter { limit, window, inner: Mutex::new(Inner { buckets: HashMap::new(), last_prune: Instant::now() }) }
    }

    /// Record a hit for `key`; `false` when it exceeds the limit (the
    /// rejected hit is not counted).
    pub fn check(&self, key: &str, now: Instant) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let window = self.window;
        let fresh = |t: &Instant| now.saturating_duration_since(*t) < window;
        if now.saturating_duration_since(g.last_prune) >= window || g.buckets.len() > 10_000 {
            g.buckets.retain(|_, q| q.back().is_some_and(fresh));
            g.last_prune = now;
        }
        let q = g.buckets.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| !fresh(t)) {
            q.pop_front();
        }
        if q.len() >= self.limit {
            return false;
        }
        q.push_back(now);
        true
    }

    pub fn bucket_count(&self) -> usize {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_key_and_slides() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(rl.check("a", t0));
        assert!(rl.check("a", t0));
        assert!(rl.check("a", t0 + Duration::from_secs(10)));
        assert!(!rl.check("a", t0 + Duration::from_secs(20)));
        assert!(rl.check("b", t0 + Duration::from_secs(20)));
        // The two t0 hits leave the window.
        assert!(rl.check("a", t0 + Duration::from_secs(61)));
        assert!(rl.check("a", t0 + Duration::from_secs(61)));
        assert!(!rl.check("a", t0 + Duration::from_secs(61)));
    }

    #[test]
    fn prunes_stale_buckets() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..50 {
            rl.check(&format!("ip{i}"), t0);
        }
        assert_eq!(rl.bucket_count(), 50);
        rl.check("late", t0 + Duration::from_secs(200));
        assert_eq!(rl.bucket_count(), 1);
    }
}
