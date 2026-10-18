//! Ограничение частоты: скользящее окно на IP.
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Окно ограничения (ТЗ §11: 5 попыток в минуту на IP).
pub const WINDOW: Duration = Duration::from_secs(60);
/// Допустимое число событий в окне.
pub const LIMIT: usize = 5;

/// Скользящее окно событий по IP. Пустые ключи вычищаются лениво, не реже раза за окно.
#[derive(Debug)]
pub struct RateLimiter {
    limit: usize,
    window: Duration,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    hits: HashMap<IpAddr, VecDeque<Instant>>,
    last_sweep: Instant,
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new(LIMIT, WINDOW)
    }
}

/// IPv6 сводится к /64: у одного клиента обычно вся подсеть.
fn key_of(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::from([s[0], s[1], s[2], s[3], 0, 0, 0, 0])
        }
        v4 => v4,
    }
}

impl RateLimiter {
    pub fn new(limit: usize, window: Duration) -> RateLimiter {
        RateLimiter {
            limit,
            window,
            inner: Mutex::new(Inner {
                hits: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    /// Регистрирует событие; при превышении — `Err(через сколько повторить)`, событие не считается.
    pub fn check(&self, key: IpAddr) -> Result<(), Duration> {
        self.at(key, Instant::now(), true)
    }

    /// Только проверка, без регистрации события.
    pub fn peek(&self, key: IpAddr) -> Result<(), Duration> {
        self.at(key, Instant::now(), false)
    }

    fn at(&self, key: IpAddr, now: Instant, record: bool) -> Result<(), Duration> {
        let window = self.window;
        let fresh = |q: &mut VecDeque<Instant>| {
            while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
                q.pop_front();
            }
        };
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if now.duration_since(g.last_sweep) >= window {
            g.hits.retain(|_, q| {
                fresh(q);
                !q.is_empty()
            });
            g.last_sweep = now;
        }
        let key = key_of(key);
        let q = g.hits.entry(key).or_default();
        fresh(q);
        if q.len() >= self.limit {
            let oldest = *q.front().expect("лимит > 0 ⇒ очередь не пуста");
            return Err(window - now.duration_since(oldest));
        }
        if record {
            q.push_back(now);
        } else if q.is_empty() {
            g.hits.remove(&key);
        }
        Ok(())
    }

    #[cfg(test)]
    fn keys(&self) -> usize {
        self.inner.lock().unwrap().hits.len()
    }
}

/// Retry-After в секундах, с округлением вверх, не меньше 1.
pub fn retry_after_secs(d: Duration) -> u64 {
    d.as_secs_f64().ceil().max(1.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn five_per_window_then_retry_after() {
        let rl = RateLimiter::default();
        let t0 = Instant::now();
        for i in 0..5 {
            assert!(
                rl.at(ip("1.1.1.1"), t0 + Duration::from_secs(i), true)
                    .is_ok()
            );
        }
        let e = rl
            .at(ip("1.1.1.1"), t0 + Duration::from_secs(10), true)
            .unwrap_err();
        assert_eq!(e, Duration::from_secs(50));
        assert_eq!(retry_after_secs(e), 50);
        // отказ не продлевает блокировку; окно скользит
        assert!(
            rl.at(ip("1.1.1.1"), t0 + Duration::from_secs(60), true)
                .is_ok()
        );
        assert!(
            rl.at(ip("1.1.1.1"), t0 + Duration::from_secs(60), true)
                .is_err()
        );
        assert!(rl.at(ip("2.2.2.2"), t0, true).is_ok());
    }

    #[test]
    fn peek_does_not_record() {
        let rl = RateLimiter::new(1, WINDOW);
        let t0 = Instant::now();
        assert!(rl.at(ip("1.1.1.1"), t0, false).is_ok());
        assert!(rl.at(ip("1.1.1.1"), t0, false).is_ok());
        assert_eq!(rl.keys(), 0);
        assert!(rl.at(ip("1.1.1.1"), t0, true).is_ok());
        assert!(rl.at(ip("1.1.1.1"), t0, false).is_err());
    }

    #[test]
    fn stale_keys_are_swept() {
        let rl = RateLimiter::default();
        let t0 = Instant::now();
        for i in 0..100u8 {
            rl.at(IpAddr::from([10, 0, 0, i]), t0, true).unwrap();
        }
        assert_eq!(rl.keys(), 100);
        rl.at(ip("9.9.9.9"), t0 + Duration::from_secs(61), true)
            .unwrap();
        assert_eq!(rl.keys(), 1);
    }

    #[test]
    fn ipv6_grouped_by_64_and_mapped_v4_canonical() {
        let rl = RateLimiter::new(1, WINDOW);
        let t0 = Instant::now();
        rl.at(ip("2001:db8:1:2::1"), t0, true).unwrap();
        assert!(rl.at(ip("2001:db8:1:2::ffff"), t0, true).is_err());
        assert!(rl.at(ip("2001:db8:1:3::1"), t0, true).is_ok());
        rl.at(ip("192.0.2.1"), t0, true).unwrap();
        assert!(rl.at(ip("::ffff:192.0.2.1"), t0, true).is_err());
    }

    #[test]
    fn retry_after_rounds_up() {
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(59_001)), 60);
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
    }
}
