use std::time::Instant;

pub(crate) trait Timing {
    fn finish(&self) -> f64;
}

#[derive(Debug, Clone)]
pub(crate) struct Timer {
    start: Instant,
}

impl Default for Timer {
    fn default() -> Self {
        Timer::start()
    }
}

impl Timer {
    #[cfg(test)]
    const fn new(start: Instant) -> Self {
        Self { start }
    }

    pub(crate) fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Timing for Timer {
    fn finish(&self) -> f64 {
        self.start.elapsed().as_nanos() as f64 / 1_000_000f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_new() {
        let now = Instant::now();
        if let Some(start) = now.checked_sub(Duration::from_secs(3600)) {
            let t = Timer::new(start);
            assert!(t.finish() > 3_599_000f64);
            assert!(t.finish() < 3_600_001f64);
        } else {
            panic!("Could not adjust start time");
        }
    }

    #[test]
    fn test_start() {
        let t = Timer::start();
        let elapsed = t.finish();
        assert!(elapsed < 1000f64);
    }
}
