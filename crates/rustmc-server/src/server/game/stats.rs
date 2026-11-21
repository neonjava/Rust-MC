//! Tick timing: how long the game loop's ticks take, logged at intervals.
//!
//! Why: a server keeps 20 ticks per second only while each tick stays under
//! 50 ms; load tests and operators need that measured on the server itself,
//! not inferred from clients.
//! Done: mean and worst tick time per interval with the player and loaded
//! column counts. Not done: per-phase breakdown inside a tick.

use std::time::Duration;

use super::Game;
use crate::server::log;

/// Ticks between `tick_stats` events: 30 seconds at 20 TPS.
pub const STATS_INTERVAL_TICKS: u64 = 600;

/// Tick durations since the last report.
#[derive(Debug, Default)]
pub struct TickStats {
    ticks: u64,
    total: Duration,
    worst: Duration,
    /// Ticks longer than the 50 ms budget.
    over_budget: u64,
}

impl TickStats {
    pub fn record(&mut self, took: Duration, budget: Duration) {
        self.ticks += 1;
        self.total += took;
        self.worst = self.worst.max(took);
        if took > budget {
            self.over_budget += 1;
        }
    }

    /// Mean tick time in milliseconds, or 0 with no ticks.
    pub fn mean_ms(&self) -> f64 {
        if self.ticks == 0 {
            0.0
        } else {
            self.total.as_secs_f64() * 1_000.0 / self.ticks as f64
        }
    }
}

impl Game {
    /// Logs and resets the tick statistics once per interval.
    pub(super) fn report_tick_stats(&mut self) {
        if !self.tick.is_multiple_of(STATS_INTERVAL_TICKS) {
            return;
        }
        let stats = std::mem::take(&mut self.tick_stats);
        let (loaded, bytes) = self.store.stats();
        log::event(
            "tick_stats",
            &[
                ("ticks", &stats.ticks),
                ("mean_ms", &format!("{:.3}", stats.mean_ms())),
                (
                    "max_ms",
                    &format!("{:.3}", stats.worst.as_secs_f64() * 1_000.0),
                ),
                ("over_budget", &stats.over_budget),
                ("players", &self.players.len()),
                ("loaded_columns", &loaded),
                ("column_bytes", &bytes),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_worst_and_over_budget_ticks_are_tracked() {
        let budget = Duration::from_millis(50);
        let mut stats = TickStats::default();
        assert_eq!(stats.mean_ms(), 0.0);
        stats.record(Duration::from_millis(10), budget);
        stats.record(Duration::from_millis(70), budget);
        assert_eq!(stats.mean_ms(), 40.0);
        assert_eq!(stats.worst, Duration::from_millis(70));
        assert_eq!(stats.over_budget, 1);
    }
}
