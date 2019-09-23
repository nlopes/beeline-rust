use std::time::Instant;

pub(crate) trait Timing {
    fn finish(&self) -> u128;
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
    fn finish(&self) -> u128 {
        self.start.elapsed().as_millis()
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
            assert!(t.finish() > 3_599_000);
            assert!(t.finish() < 3_600_001);
        } else {
            panic!("Could not adjust start time");
        }
    }

    #[test]
    fn test_start() {
        let t = Timer::start();
        let elapsed = t.finish();
        assert!(elapsed < 1000);
    }
}
