//! Temperature alert evaluation logic: The swim-window (12-18 pm) average and the
//! notify/reset state machine.
//!
//! The thresholds and windows here were validated against a year of real sensor data.

use std::ops::Range;

use chrono::NaiveDate;

use crate::{api::HourlyTemperature, store::AlertStatus};

/// Local hours making up the swimming window: 12:00–17:59 (half-open `[12, 18)`).
pub const SWIM_WINDOW: Range<u32> = 12..18;
/// Fewest hourly buckets within the window for the day's average to be trustworthy.
pub const MIN_BUCKETS: usize = 4;
/// Consecutive warm days required before notifying the user.
pub const NOTIFY_DAYS: u32 = 2;
/// Consecutive clearly-below days required to reset a notified alert.
pub const RESET_DAYS: u32 = 3;
/// How far below the threshold (°C) a day must be to count toward resetting.
pub const RESET_MARGIN: f64 = 1.5;

/// The afternoon (swim-window) average for `date`, or `None` when too little data is
/// available.
///
/// Returns `None` if fewer than [`MIN_BUCKETS`] qualify - the scheduler treats that as
/// "skip this day" (freeze the streaks).
pub fn swim_window_average(hourly: &[HourlyTemperature], date: NaiveDate) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for bucket in hourly {
        if bucket.aggregation_date == date
            && SWIM_WINDOW.contains(&u32::from(bucket.aggregation_hour))
        {
            sum += bucket.average_temperature;
            count += 1;
        }
    }
    (count >= MIN_BUCKETS).then(|| sum / count as f64)
}

/// The mutable part of an alert's state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertState {
    /// Whether the alert is waiting to notify or has already notified.
    pub status: AlertStatus,
    /// Consecutive days the afternoon average was at or above the threshold.
    pub warm_streak: u32,
    /// Consecutive days the afternoon average was clearly below the threshold.
    pub cold_streak: u32,
}

impl Default for AlertState {
    fn default() -> Self {
        Self {
            status: AlertStatus::Watching,
            warm_streak: 0,
            cold_streak: 0,
        }
    }
}

/// What a single day's evaluation means for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do: streak building, dead-band, or skipped (no data).
    Quiet,
    /// Notify the user, the alert fired (`Watching` → `Notified`).
    Notify,
    /// The alert reset silently (`Notified` → `Watching`), no message.
    Reset,
}

/// The result of evaluating one day: the new state plus what it means for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// State to persist after this day.
    pub state: AlertState,
    /// User-facing consequence.
    pub outcome: Outcome,
}

/// How a single day's swim-window average classifies relative to the threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayClass {
    /// No swim-window average available (missing / insufficient data).
    NoData,
    /// At or above the threshold: counts toward notifying.
    Warm,
    /// Between the threshold and the reset margin: neither warm nor a recovery day.
    DeadBand,
    /// At or below `threshold - RESET_MARGIN`: counts toward resetting.
    Cold,
}

/// Round to one decimal place, matching the 0.1 °C precision the bot shows users.
fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Classify a day's swim-window average relative to the threshold (at display precision).
pub fn classify(swim_avg: Option<f64>, threshold: f64) -> DayClass {
    match swim_avg {
        None => DayClass::NoData,
        Some(avg) => {
            let avg = round1(avg);
            if avg >= threshold {
                DayClass::Warm
            } else if avg <= threshold - RESET_MARGIN {
                DayClass::Cold
            } else {
                DayClass::DeadBand
            }
        }
    }
}

/// Advance the alert state machine by one day.
///
/// Pure: takes the current state and returns the next state plus the [`Outcome`].
/// `swim_avg` is the day's [`swim_window_average`] (`None` freezes the streaks).
pub fn evaluate(mut state: AlertState, swim_avg: Option<f64>, threshold: f64) -> Transition {
    let outcome = match classify(swim_avg, threshold) {
        DayClass::NoData => Outcome::Quiet,
        DayClass::Warm => {
            state.warm_streak += 1;
            state.cold_streak = 0;
            if state.status == AlertStatus::Watching && state.warm_streak >= NOTIFY_DAYS {
                state.status = AlertStatus::Notified;
                state.warm_streak = 0;
                Outcome::Notify
            } else {
                Outcome::Quiet
            }
        }
        DayClass::Cold => {
            state.cold_streak += 1;
            state.warm_streak = 0;
            if state.status == AlertStatus::Notified && state.cold_streak >= RESET_DAYS {
                state.status = AlertStatus::Watching;
                state.cold_streak = 0;
                Outcome::Reset
            } else {
                Outcome::Quiet
            }
        }
        DayClass::DeadBand => {
            // Break the warm streak, leave the cold streak untouched.
            state.warm_streak = 0;
            Outcome::Quiet
        }
    };
    Transition { state, outcome }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    mod swim_window_average {
        use super::*;

        /// Build an hourly bucket; min/max are irrelevant to the swim-window average, so mirror avg.
        fn make_hourly(date: NaiveDate, hour: u8, avg: f64) -> HourlyTemperature {
            HourlyTemperature {
                aggregation_date: date,
                aggregation_hour: hour,
                minimum_temperature: avg,
                maximum_temperature: avg,
                average_temperature: avg,
            }
        }

        fn date(year: i32, month: u32, day: u32) -> NaiveDate {
            NaiveDate::from_ymd_opt(year, month, day).unwrap()
        }

        #[test]
        fn averages_only_window_hours() {
            // Hours are local; the window is [12, 18). Out-of-window hours carry a wildly
            // different value so their exclusion is observable.
            let d = date(2025, 7, 1);
            let mut hourly = vec![
                make_hourly(d, 11, 100.0), // before the window — excluded
                make_hourly(d, 18, 100.0), // after the window — excluded
            ];
            for hour in 12..18 {
                hourly.push(make_hourly(d, hour, 20.0)); // 12:00–17:00 — six buckets
            }
            assert_eq!(swim_window_average(&hourly, d), Some(20.0));
        }

        #[test]
        fn none_when_below_min_buckets() {
            let d = date(2025, 7, 1);
            // Only three in-window buckets (12, 13, 14); MIN_BUCKETS is 4.
            let hourly: Vec<_> = (12..15).map(|h| make_hourly(d, h, 20.0)).collect();
            assert_eq!(swim_window_average(&hourly, d), None);
        }

        #[test]
        fn excludes_other_dates() {
            let d = date(2025, 7, 1);
            let mut hourly: Vec<_> = (12..18).map(|h| make_hourly(d, h, 20.0)).collect();
            // In-window-hour buckets on another day must not count toward `d`.
            for hour in 12..18 {
                hourly.push(make_hourly(date(2025, 7, 2), hour, 100.0));
            }
            assert_eq!(swim_window_average(&hourly, d), Some(20.0));
        }
    }

    mod evaluate {
        use super::*;

        const THRESHOLD: f64 = 23.0;

        /// Fold `evaluate` over a sequence of daily swim-window averages.
        fn run(days: &[Option<f64>]) -> (AlertState, Vec<Outcome>) {
            let mut state = AlertState::default();
            let mut outcomes = Vec::new();
            for &avg in days {
                let transition = evaluate(state, avg, THRESHOLD);
                state = transition.state;
                outcomes.push(transition.outcome);
            }
            (state, outcomes)
        }

        fn count(outcomes: &[Outcome], target: Outcome) -> usize {
            outcomes.iter().filter(|&&o| o == target).count()
        }

        #[rstest]
        #[case(Some(23.0), 1, 0)] // warm
        #[case(Some(22.0), 0, 0)] // dead-band (between 21.5 and 23)
        #[case(Some(20.0), 0, 1)] // cold
        #[case(None, 0, 0)] // gap
        fn single_day_from_watching(
            #[case] avg: Option<f64>,
            #[case] warm: u32,
            #[case] cold: u32,
        ) {
            let transition = evaluate(AlertState::default(), avg, THRESHOLD);
            assert_eq!(transition.state.warm_streak, warm);
            assert_eq!(transition.state.cold_streak, cold);
            assert_eq!(transition.outcome, Outcome::Quiet);
        }

        #[test]
        fn two_warm_days_notify_once_on_day_two() {
            let (state, outcomes) = run(&[Some(23.0), Some(23.0)]);
            assert_eq!(outcomes, vec![Outcome::Quiet, Outcome::Notify]);
            assert_eq!(state.status, AlertStatus::Notified);
        }

        #[test]
        fn broken_streak_does_not_notify() {
            // Warm, a dead-band day, warm again: the middle day breaks the streak.
            let (_, outcomes) = run(&[Some(23.0), Some(22.0), Some(23.0)]);
            assert_eq!(count(&outcomes, Outcome::Notify), 0);
        }

        #[test]
        fn rounding_lets_a_borderline_day_qualify() {
            // 22.97 rounds to 23.0, so two such days notify.
            let (_, outcomes) = run(&[Some(22.97), Some(22.97)]);
            assert_eq!(count(&outcomes, Outcome::Notify), 1);
        }

        #[test]
        fn resets_after_three_cold_days() {
            let (state, outcomes) =
                run(&[Some(23.0), Some(23.0), Some(21.0), Some(21.0), Some(21.0)]);
            assert_eq!(count(&outcomes, Outcome::Notify), 1);
            assert_eq!(count(&outcomes, Outcome::Reset), 1);
            assert_eq!(state.status, AlertStatus::Watching);
        }

        #[test]
        fn two_cold_days_do_not_reset() {
            let (state, outcomes) = run(&[Some(23.0), Some(23.0), Some(21.0), Some(21.0)]);
            assert_eq!(count(&outcomes, Outcome::Reset), 0);
            assert_eq!(state.status, AlertStatus::Notified);
        }

        #[test]
        fn gap_day_freezes_the_streak() {
            // A None day between two warm days must not reset the warm streak.
            let (_, outcomes) = run(&[Some(23.0), None, Some(23.0)]);
            assert_eq!(
                outcomes,
                vec![Outcome::Quiet, Outcome::Quiet, Outcome::Notify]
            );
        }

        #[test]
        fn dead_band_keeps_cold_streak() {
            // After notifying: cold, dead-band, cold, cold. The dead-band day zeroes the warm streak
            // but leaves the cold streak, so the third cold day still resets.
            let (_, outcomes) = run(&[
                Some(23.0),
                Some(23.0),
                Some(21.0),
                Some(22.0),
                Some(21.0),
                Some(21.0),
            ]);
            assert_eq!(count(&outcomes, Outcome::Reset), 1);
        }
    }
}
