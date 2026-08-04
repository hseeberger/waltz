use std::time::Duration;
use thiserror::Error;

/// The bounds of an exponential backoff, kept as one value so they cannot be transposed and so an
/// invalid pair is unrepresentable, whether it comes from code or from a deserialized config.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize),
    serde(try_from = "UncheckedBackoff")
)]
pub struct Backoff {
    min: Duration,
    max: Duration,
}

impl Backoff {
    /// The `min` of [Backoff::default].
    pub const DEFAULT_MIN: Duration = Duration::from_millis(250);

    /// The `max` of [Backoff::default].
    pub const DEFAULT_MAX: Duration = Duration::from_secs(4);

    /// The bounds of an exponential backoff starting at `min` and capped at `max`.
    ///
    /// # Errors
    /// Fails if `max` is below `min`.
    pub fn new(min: Duration, max: Duration) -> Result<Self, InvalidBackoff> {
        if max < min {
            return Err(InvalidBackoff { min, max });
        }

        Ok(Self { min, max })
    }

    /// The delay of the first step, doubled on every further one.
    pub fn min(self) -> Duration {
        self.min
    }

    /// The upper bound for the delay, never below [Backoff::min].
    pub fn max(self) -> Duration {
        self.max
    }

    /// Callers map their own attempt count onto a step: the restart path passes the restarts
    /// already made.
    pub(crate) fn duration(self, step: u32) -> Duration {
        let factor = 1u32.checked_shl(step).unwrap_or(u32::MAX);
        self.min.saturating_mul(factor).min(self.max)
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            min: Self::DEFAULT_MIN,
            max: Self::DEFAULT_MAX,
        }
    }
}

/// The bounds given to [Backoff::new] contradict each other.
#[derive(Debug, Error)]
#[error("max backoff {max:?} below min backoff {min:?}")]
pub struct InvalidBackoff {
    min: Duration,
    max: Duration,
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct UncheckedBackoff {
    #[serde(with = "humantime_serde")]
    min: Duration,

    #[serde(with = "humantime_serde")]
    max: Duration,
}

#[cfg(feature = "serde")]
impl TryFrom<UncheckedBackoff> for Backoff {
    type Error = InvalidBackoff;

    fn try_from(unchecked: UncheckedBackoff) -> Result<Self, Self::Error> {
        Self::new(unchecked.min, unchecked.max)
    }
}

#[cfg(test)]
mod tests {
    use crate::backoff::Backoff;
    use std::time::Duration;

    const MIN: Duration = Duration::from_millis(250);
    const MAX: Duration = Duration::from_secs(3);

    /// The first step is not delayed beyond the minimum, every further one doubles, and the cap
    /// holds; a step wide enough to overflow the shift must saturate into the cap, not wrap.
    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let backoff = Backoff::new(MIN, MAX).expect("the bounds are ordered");

        assert_eq!(backoff.duration(0), MIN);
        assert_eq!(backoff.duration(1), MIN * 2);
        assert_eq!(backoff.duration(2), MIN * 4);

        assert_eq!(backoff.duration(64), MAX);
        assert_eq!(backoff.duration(u32::MAX), MAX);
    }

    /// A zero minimum stays zero however often it is doubled, which is what makes the tests using
    /// a zero backoff run without delay.
    #[test]
    fn backoff_of_zero_stays_zero() {
        let backoff = Backoff::new(Duration::ZERO, MAX).expect("the bounds are ordered");

        assert_eq!(backoff.duration(5), Duration::ZERO);
    }

    /// A cap below the minimum is rejected rather than repaired, so no [Backoff] contradicts
    /// itself and a config file naming inverted bounds is reported instead of quietly rewritten.
    #[test]
    fn a_cap_below_the_minimum_is_rejected() {
        assert!(Backoff::new(MAX, MIN).is_err());
        assert!(Backoff::new(MIN, MIN).is_ok());
    }
}
