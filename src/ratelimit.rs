use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) struct RateLimiter {
    max_requests: u32,
    window: Duration,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

#[derive(Clone, Copy)]
struct Bucket {
    window_start: Instant,
    count: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Allow,
    Deny { retry_after_secs: u64 },
}

impl RateLimiter {
    pub(crate) fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn check(&self, ip: IpAddr) -> Decision {
        self.check_at(ip, Instant::now())
    }

    fn check_at(&self, ip: IpAddr, now: Instant) -> Decision {
        if self.max_requests == 0 {
            return Decision::Allow;
        }

        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = buckets.entry(ip).or_insert(Bucket {
            window_start: now,
            count: 0,
        });

        if now.saturating_duration_since(bucket.window_start) >= self.window {
            bucket.window_start = now;
            bucket.count = 0;
        }

        if bucket.count >= self.max_requests {
            let elapsed = now.saturating_duration_since(bucket.window_start);
            let retry_after = self.window.saturating_sub(elapsed).as_secs().max(1);
            return Decision::Deny {
                retry_after_secs: retry_after,
            };
        }

        bucket.count += 1;
        Decision::Allow
    }

    pub(crate) fn prune_stale(&self) {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let window = self.window;
        buckets.retain(|_, b| now.saturating_duration_since(b.window_start) < window * 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, n))
    }

    #[test]
    fn allows_up_to_the_limit_then_denies() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let now = Instant::now();
        let addr = ip(1);

        assert_eq!(limiter.check_at(addr, now), Decision::Allow);
        assert_eq!(limiter.check_at(addr, now), Decision::Allow);
        assert_eq!(limiter.check_at(addr, now), Decision::Allow);

        match limiter.check_at(addr, now) {
            Decision::Deny { retry_after_secs } => assert!(retry_after_secs >= 1),
            Decision::Allow => panic!("expected the 4th request in the window to be denied"),
        }
    }

    #[test]
    fn resets_after_the_window_elapses() {
        let limiter = RateLimiter::new(1, Duration::from_secs(10));
        let now = Instant::now();
        let addr = ip(2);

        assert_eq!(limiter.check_at(addr, now), Decision::Allow);
        assert!(matches!(limiter.check_at(addr, now), Decision::Deny { .. }));

        let later = now + Duration::from_secs(11);
        assert_eq!(limiter.check_at(addr, later), Decision::Allow);
    }

    #[test]
    fn tracks_each_ip_independently() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();

        assert_eq!(limiter.check_at(ip(1), now), Decision::Allow);
        // a different IP has its own, untouched budget
        assert_eq!(limiter.check_at(ip(2), now), Decision::Allow);
        assert!(matches!(
            limiter.check_at(ip(1), now),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn disabled_limiter_always_allows() {
        let limiter = RateLimiter::new(0, Duration::from_secs(60));
        let now = Instant::now();
        let addr = ip(3);
        for _ in 0..1000 {
            assert_eq!(limiter.check_at(addr, now), Decision::Allow);
        }
    }
}
