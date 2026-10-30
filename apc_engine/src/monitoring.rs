//! Sample-based runtime observations, not causal performance or product-quality claims.
//!
//! Each accepted row is one equally weighted engine observation. Limits are operating
//! limits, not independently configured product specifications; violations count rows,
//! never seconds. MV movement uses observed applied SP, not OP or proposed commands.
//! The caller must reset on acquisition gaps/faults and must not feed confirmation polls.

use anyhow::{ensure, Context, Result};
use std::mem::size_of;

const MAX_WINDOW_SAMPLES: usize = 100_000;
const MAX_VARIABLES_PER_KIND: usize = 1_024;
const MAX_ROW_CELLS: usize = 2_000_000;
const MAX_STORAGE_BYTES: usize = 64 * 1_024 * 1_024;

/// Population moments over retained samples, with error defined as PV minus target.
///
/// All four optional moments are unavailable until two samples have been retained.
/// A first-sample operating violation is nevertheless counted immediately.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct CvStatistics {
    pub variance: Option<f64>,
    pub tracking_error_variance: Option<f64>,
    pub bias: Option<f64>,
    pub rmse: Option<f64>,
    pub operating_violation_samples: usize,
}

/// Bounded, incremental statistics for complete validated per-scan input vectors.
///
/// A mode, target, or operating-limit change starts a new window with the current
/// sample. A rejected observation clears the window, so no statistic spans a fault.
/// Successful observations allocate nothing. CV updates touch only a balanced
/// moment tree's leaf-to-root path, O(CV * log(window)); MV updates are O(MV).
///
/// Pairwise centered add/remove moments avoid subtracting nearly equal second
/// moments when an outlier is evicted. No scan reconstructs the complete window.
pub struct RollingPerformance {
    window_samples: usize,
    cv_count: usize,
    mv_count: usize,
    leaf_count: usize,
    samples: usize,
    next_slot: usize,
    requested_mode: Option<f64>,
    targets: Vec<f64>,
    low: Vec<f64>,
    high: Vec<f64>,
    applied_sp_rows: Vec<f64>,
    moments: Vec<Moments>,
    statistics: Vec<CvStatistics>,
    mv_sums: Vec<CompensatedSum>,
    mv_variation: Vec<f64>,
}

impl RollingPerformance {
    /// Windows contain 2..=100000 rows, with 1..=1024 CVs and MVs respectively.
    /// Combined row storage is limited to 2 million cells and total backing
    /// storage to 64 MiB. Invalid dimensions and allocation failures return Err.
    pub fn new(window_samples: usize, cv_count: usize, mv_count: usize) -> Result<Self> {
        ensure!((2..=MAX_WINDOW_SAMPLES).contains(&window_samples), "Invalid observation window size");
        ensure!((1..=MAX_VARIABLES_PER_KIND).contains(&cv_count), "Invalid observation CV count");
        ensure!((1..=MAX_VARIABLES_PER_KIND).contains(&mv_count), "Invalid observation MV count");
        let row_width = cv_count.checked_add(mv_count).context("Observation dimension overflow")?;
        let row_cells = window_samples.checked_mul(row_width).context("Observation row storage overflow")?;
        ensure!(row_cells <= MAX_ROW_CELLS, "Observation row storage exceeds the cell bound");
        let mv_cells = window_samples.checked_mul(mv_count).context("Observation MV storage overflow")?;
        let leaf_count = window_samples.checked_next_power_of_two().context("Observation tree size overflow")?;
        let tree_cells = leaf_count.checked_mul(2).and_then(|nodes| nodes.checked_mul(cv_count))
            .context("Observation tree storage overflow")?;
        let storage_bytes = [
            (mv_cells, size_of::<f64>()),
            (tree_cells, size_of::<Moments>()),
            (cv_count * 3, size_of::<f64>()),
            (cv_count, size_of::<CvStatistics>()),
            (mv_count, size_of::<CompensatedSum>()),
            (mv_count, size_of::<f64>()),
        ].into_iter().try_fold(0usize, |bytes, (count, width)| {
            count.checked_mul(width).and_then(|size| bytes.checked_add(size))
                .context("Observation backing storage overflow")
        })?;
        ensure!(storage_bytes <= MAX_STORAGE_BYTES, "Observation backing storage exceeds the memory bound");
        Ok(Self {
            window_samples,
            cv_count,
            mv_count,
            leaf_count,
            samples: 0,
            next_slot: 0,
            requested_mode: None,
            targets: allocated(cv_count)?,
            low: allocated(cv_count)?,
            high: allocated(cv_count)?,
            applied_sp_rows: allocated(mv_cells)?,
            moments: allocated(tree_cells)?,
            statistics: allocated(cv_count)?,
            mv_sums: allocated(mv_count)?,
            mv_variation: allocated(mv_count)?,
        })
    }

    pub fn reset(&mut self) {
        self.samples = 0;
        self.next_slot = 0;
        self.requested_mode = None;
        self.statistics.fill(CvStatistics::default());
        self.mv_sums.fill(CompensatedSum::default());
        self.mv_variation.fill(0.0);
        // Rows and tree storage need not be cleared: after reset, only the newly
        // populated prefix of leaves is eligible to contribute to an aggregate.
    }

    pub fn observe(
        &mut self,
        requested_mode: f64,
        cv: &[f64],
        targets: &[f64],
        cv_low: &[f64],
        cv_high: &[f64],
        applied_sp: &[f64],
    ) -> Result<()> {
        let result = self.observe_inner(requested_mode, cv, targets, cv_low, cv_high, applied_sp);
        if result.is_err() {
            self.reset();
        }
        result
    }

    pub fn samples(&self) -> usize {
        self.samples
    }

    pub fn window_samples(&self) -> usize {
        self.window_samples
    }

    pub fn cv_statistics(&self) -> impl ExactSizeIterator<Item = CvStatistics> + '_ {
        self.statistics.iter().copied()
    }

    pub fn mv_total_variation(&self) -> &[f64] {
        &self.mv_variation
    }

    fn observe_inner(
        &mut self,
        requested_mode: f64,
        cv: &[f64],
        targets: &[f64],
        cv_low: &[f64],
        cv_high: &[f64],
        applied_sp: &[f64],
    ) -> Result<()> {
        ensure!([cv.len(), targets.len(), cv_low.len(), cv_high.len()].into_iter()
            .all(|count| count == self.cv_count) && applied_sp.len() == self.mv_count,
            "Observation dimensions differ");
        ensure!(requested_mode == 0.0 || requested_mode == 1.0 || requested_mode == 2.0,
            "Invalid requested operating mode");
        ensure!(cv.iter().chain(targets).chain(cv_low).chain(cv_high).chain(applied_sp)
            .all(|value| value.is_finite()), "Nonfinite observation input");
        for index in 0..self.cv_count {
            ensure!(cv_low[index] < cv_high[index], "Invalid operating-limit order");
            finite(cv[index] - targets[index])?;
        }
        if self.requested_mode != Some(requested_mode) || targets != self.targets.as_slice()
            || cv_low != self.low.as_slice() || cv_high != self.high.as_slice()
        {
            self.reset();
            self.requested_mode = Some(requested_mode);
            self.targets.copy_from_slice(targets);
            self.low.copy_from_slice(cv_low);
            self.high.copy_from_slice(cv_high);
        }

        // All index arithmetic is bounded by the checked constructor dimensions.
        let slot = self.next_slot;
        let row_start = slot * self.mv_count;
        let next_slot = if slot + 1 == self.window_samples { 0 } else { slot + 1 };
        let previous_slot = if slot == 0 { self.window_samples - 1 } else { slot - 1 };
        let active_samples = (self.samples + 1).min(self.window_samples);
        for index in 0..self.mv_count {
            if self.samples == self.window_samples {
                // Drop only the edge from the evicted row to the new oldest row.
                let outgoing_edge = finite(
                    self.applied_sp_rows[next_slot * self.mv_count + index] - self.applied_sp_rows[row_start + index]
                )?.abs();
                self.mv_sums[index].add(-outgoing_edge)?;
            }
            if self.samples > 0 {
                let incoming_edge = finite(
                    applied_sp[index] - self.applied_sp_rows[previous_slot * self.mv_count + index]
                )?.abs();
                self.mv_sums[index].add(incoming_edge)?;
            }
            let variation = self.mv_sums[index].total()?;
            ensure!(variation >= 0.0, "Negative observed MV variation");
            self.mv_variation[index] = if variation == 0.0 { 0.0 } else { variation };
        }

        let leaf = self.leaf_count + slot;
        for index in 0..self.cv_count {
            self.moments[leaf * self.cv_count + index] = Moments {
                samples: 1,
                origin: cv[index],
                mean_offset: 0.0,
                m2: 0.0,
                violations: usize::from(cv[index] < cv_low[index] || cv[index] > cv_high[index]),
            };
        }
        let mut node = leaf / 2;
        let mut span = 2;
        while node > 0 {
            let right_start = (slot / span) * span + span / 2;
            for index in 0..self.cv_count {
                let left = self.moments[(node * 2) * self.cv_count + index];
                // Before the first full window, valid leaves form a prefix.
                // The partial-prefix subtree is on this update path; siblings
                // are either already current or completely outside the prefix.
                let right = if right_start < active_samples {
                    self.moments[(node * 2 + 1) * self.cv_count + index]
                } else {
                    Moments::default()
                };
                self.moments[node * self.cv_count + index] = left.merge(right)?;
            }
            node /= 2;
            span *= 2;
        }
        for index in 0..self.cv_count {
            self.statistics[index] = self.moments[self.cv_count + index].statistics(targets[index])?;
        }
        self.applied_sp_rows[row_start..row_start + self.mv_count].copy_from_slice(applied_sp);
        self.samples = active_samples;
        self.next_slot = next_slot;
        Ok(())
    }
}

/// Center the mean on an observed PV rather than repeatedly adding small changes
/// to a large absolute PV. Pairwise second central moments remain nonnegative:
/// eviction replaces a leaf, never subtracts an almost-equal accumulated M2.
#[derive(Clone, Copy, Default)]
struct Moments {
    samples: usize,
    origin: f64,
    mean_offset: f64,
    m2: f64,
    violations: usize,
}

impl Moments {
    fn merge(self, other: Self) -> Result<Self> {
        if self.samples == 0 {
            return Ok(other);
        }
        if other.samples == 0 {
            return Ok(self);
        }
        let samples = self.samples.checked_add(other.samples).context("Observation sample count overflow")?;
        let violations = self.violations.checked_add(other.violations).context("Observation violation count overflow")?;
        let origin_delta = finite(other.origin - self.origin)?;
        let delta = finite(finite(origin_delta + other.mean_offset)? - self.mean_offset)?;
        let weighted_delta = finite(delta * (other.samples as f64 / samples as f64))?;
        let mean_offset = finite(self.mean_offset + weighted_delta)?;
        // Scale before squaring: delta^2 may overflow even when the weighted
        // contribution is representable (for example, two equally sized leaves).
        let cross_m2 = finite(finite(delta * weighted_delta)? * self.samples as f64)?;
        let m2 = finite(finite(self.m2 + other.m2)? + cross_m2)?;
        Ok(Self { samples, origin: self.origin, mean_offset, m2, violations })
    }

    fn statistics(self, target: f64) -> Result<CvStatistics> {
        if self.samples < 2 {
            return Ok(CvStatistics {
                operating_violation_samples: self.violations,
                ..CvStatistics::default()
            });
        }
        let variance = self.m2 / self.samples as f64;
        let bias = finite(finite(self.origin - target)? + self.mean_offset)?;
        let rmse = finite(variance.sqrt().hypot(bias))?;
        Ok(CvStatistics {
            variance: Some(variance),
            // A target change resets the window. Subtracting a constant target
            // cannot change population variance; using the PV moments avoids
            // losing small PV changes by first rounding a large tracking error.
            tracking_error_variance: Some(variance),
            bias: Some(bias),
            rmse: Some(rmse),
            operating_violation_samples: self.violations,
        })
    }
}

/// Neumaier summation preserves small retained MV edges when a much larger edge
/// is removed; a plain running sum can irreversibly lose those small movements.
#[derive(Clone, Copy, Default)]
struct CompensatedSum {
    sum: f64,
    correction: f64,
}

impl CompensatedSum {
    fn add(&mut self, value: f64) -> Result<()> {
        let sum = finite(self.sum + value)?;
        let residue = if self.sum.abs() >= value.abs() {
            finite(finite(self.sum - sum)? + value)?
        } else {
            finite(finite(value - sum)? + self.sum)?
        };
        let correction = finite(self.correction + residue)?;
        finite(sum + correction)?;
        self.sum = sum;
        self.correction = correction;
        Ok(())
    }

    fn total(self) -> Result<f64> {
        finite(self.sum + self.correction)
    }
}

fn finite(value: f64) -> Result<f64> {
    ensure!(value.is_finite(), "Observation arithmetic overflow");
    Ok(value)
}

fn allocated<T: Clone + Default>(count: usize) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).context("Unable to allocate observation storage")?;
    values.resize(count, T::default());
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::RollingPerformance;

    fn close(actual: f64, expected: f64) {
        let tolerance = 1e-12 * expected.abs().max(1.0);
        assert!((actual - expected).abs() <= tolerance, "{actual} != {expected}");
    }

    #[test]
    fn population_moments_and_sampled_operating_violations_have_known_values() {
        let mut window = RollingPerformance::new(4, 2, 1).unwrap();
        assert_eq!(window.samples(), 0);
        assert_eq!(window.window_samples(), 4);
        assert!(window.cv_statistics().all(|stats| stats.variance.is_none()));
        for (index, pv) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            window.observe(2.0, &[pv, 10.0], &[2.0, 8.0], &[1.0, 9.0], &[3.0, 11.0], &[5.0]).unwrap();
            if index == 0 {
                let stats = window.cv_statistics().next().unwrap();
                assert_eq!(stats.variance, None);
                assert_eq!(stats.tracking_error_variance, None);
                assert_eq!(stats.bias, None);
                assert_eq!(stats.rmse, None);
            }
        }
        let stats: Vec<_> = window.cv_statistics().collect();
        close(stats[0].variance.unwrap(), 1.25);
        close(stats[0].tracking_error_variance.unwrap(), 1.25);
        close(stats[0].bias.unwrap(), 0.5);
        close(stats[0].rmse.unwrap(), 1.5_f64.sqrt());
        assert_eq!(stats[0].operating_violation_samples, 1);
        close(stats[1].variance.unwrap(), 0.0);
        close(stats[1].tracking_error_variance.unwrap(), 0.0);
        close(stats[1].bias.unwrap(), 2.0);
        close(stats[1].rmse.unwrap(), 2.0);
        assert_eq!(stats[1].operating_violation_samples, 0);
        assert_eq!(window.mv_total_variation(), &[0.0]);
    }

    #[test]
    fn eviction_retains_only_window_samples_and_internal_applied_sp_edges() {
        let mut window = RollingPerformance::new(3, 1, 2).unwrap();
        for (pv, sp) in [
            (100.0, [10.0, 20.0]),
            (1.0, [14.0, 20.0]),
            (2.0, [13.0, 23.0]),
            (3.0, [18.0, 25.0]),
        ] {
            window.observe(2.0, &[pv], &[2.0], &[0.0], &[4.0], &sp).unwrap();
        }
        assert_eq!(window.samples(), 3);
        let stats = window.cv_statistics().next().unwrap();
        close(stats.variance.unwrap(), 2.0 / 3.0);
        close(stats.bias.unwrap(), 0.0);
        close(stats.rmse.unwrap(), (2.0_f64 / 3.0).sqrt());
        assert_eq!(stats.operating_violation_samples, 0);
        assert_eq!(window.mv_total_variation(), &[6.0, 5.0]);

        window.observe(2.0, &[4.0], &[2.0], &[0.0], &[4.0], &[19.0, 25.0]).unwrap();
        assert_eq!(window.mv_total_variation(), &[6.0, 2.0]);
        for pv in [5.0, 6.0] {
            window.observe(2.0, &[pv], &[2.0], &[0.0], &[4.0], &[19.0, 25.0]).unwrap();
        }
        let stats = window.cv_statistics().next().unwrap();
        close(stats.variance.unwrap(), 2.0 / 3.0);
        close(stats.bias.unwrap(), 3.0);
        close(stats.rmse.unwrap(), (29.0_f64 / 3.0).sqrt());
        assert_eq!(stats.operating_violation_samples, 2);
        assert_eq!(window.mv_total_variation(), &[0.0, 0.0]);
    }

    #[test]
    fn two_sample_window_drops_the_old_edge_on_every_eviction() {
        let mut window = RollingPerformance::new(2, 1, 1).unwrap();
        for (pv, sp) in [(1.0, 0.0), (3.0, 10.0), (5.0, 8.0), (7.0, 9.0)] {
            window.observe(1.0, &[pv], &[4.0], &[0.0], &[10.0], &[sp]).unwrap();
        }
        assert_eq!(window.samples(), 2);
        let stats = window.cv_statistics().next().unwrap();
        close(stats.variance.unwrap(), 1.0);
        close(stats.bias.unwrap(), 2.0);
        close(stats.rmse.unwrap(), 5.0_f64.sqrt());
        assert_eq!(window.mv_total_variation(), &[1.0]);
    }

    #[test]
    fn mode_target_and_each_operating_limit_change_start_fresh_windows() {
        for (mode, target, low, high) in [
            (1.0, 10.0, 0.0, 20.0),
            (0.0, 10.0, 0.0, 20.0),
            (2.0, 11.0, 0.0, 20.0),
            (2.0, 10.0, 1.0, 20.0),
            (2.0, 10.0, 0.0, 21.0),
        ] {
            let mut window = RollingPerformance::new(5, 1, 1).unwrap();
            for pv in [8.0, 9.0, 10.0, 11.0, 12.0, 13.0] {
                window.observe(2.0, &[pv], &[10.0], &[0.0], &[20.0], &[pv]).unwrap();
            }
            window.observe(mode, &[12.0], &[target], &[low], &[high], &[100.0]).unwrap();
            assert_eq!(window.samples(), 1);
            assert_eq!(window.cv_statistics().next().unwrap().variance, None);
            assert_eq!(window.mv_total_variation(), &[0.0]);
            window.observe(mode, &[13.0], &[target], &[low], &[high], &[101.0]).unwrap();
            assert_eq!(window.samples(), 2);
            let stats = window.cv_statistics().next().unwrap();
            close(stats.variance.unwrap(), 0.25);
            close(stats.bias.unwrap(), 12.5 - target);
            assert_eq!(window.mv_total_variation(), &[1.0]);
        }
    }

    #[test]
    fn explicit_reset_discards_prior_moments_violations_and_movement() {
        let mut window = RollingPerformance::new(5, 1, 1).unwrap();
        for pv in [100.0, 200.0, 300.0, 400.0, 500.0] {
            window.observe(2.0, &[pv], &[0.0], &[-1.0], &[1.0], &[pv]).unwrap();
        }
        window.reset();
        assert_eq!(window.samples(), 0);
        assert_eq!(window.cv_statistics().next().unwrap(), super::CvStatistics::default());
        assert_eq!(window.mv_total_variation(), &[0.0]);
        // Grow through every tree depth after a reset with unchanged metadata.
        for expected_count in 1..=5 {
            window.observe(2.0, &[0.0], &[0.0], &[-1.0], &[1.0], &[0.0]).unwrap();
            assert_eq!(window.samples(), expected_count);
            let stats = window.cv_statistics().next().unwrap();
            assert_eq!(stats.operating_violation_samples, 0);
            if expected_count >= 2 {
                assert_eq!(stats.variance, Some(0.0));
                assert_eq!(stats.rmse, Some(0.0));
            }
        }
        assert_eq!(window.mv_total_variation(), &[0.0]);
    }

    #[test]
    fn held_constant_pv_has_zero_true_variance_even_with_nonzero_bias() {
        let mut window = RollingPerformance::new(7, 1, 1).unwrap();
        for _ in 0..100 {
            window.observe(2.0, &[3.25], &[1.0], &[0.0], &[4.0], &[12.5]).unwrap();
        }
        let stats = window.cv_statistics().next().unwrap();
        assert_eq!(stats.variance, Some(0.0));
        assert_eq!(stats.tracking_error_variance, Some(0.0));
        close(stats.bias.unwrap(), 2.25);
        close(stats.rmse.unwrap(), 2.25);
        assert_eq!(window.mv_total_variation(), &[0.0]);
    }

    #[test]
    fn small_variation_is_preserved_at_large_absolute_scale() {
        let mut window = RollingPerformance::new(4, 1, 1).unwrap();
        for offset in [1.0, 2.0, 3.0, 4.0] {
            window.observe(2.0, &[1e12 + offset], &[1e12 + 2.0], &[0.0], &[2e12], &[0.0]).unwrap();
        }
        let stats = window.cv_statistics().next().unwrap();
        close(stats.variance.unwrap(), 1.25);
        close(stats.tracking_error_variance.unwrap(), 1.25);
        close(stats.bias.unwrap(), 0.5);
        close(stats.rmse.unwrap(), 1.5_f64.sqrt());
    }

    #[test]
    fn evicting_a_large_outlier_does_not_erase_small_retained_variance() {
        let mut window = RollingPerformance::new(3, 1, 1).unwrap();
        for pv in [1e100, 1.0, 2.0, 3.0] {
            window.observe(1.0, &[pv], &[2.0], &[0.0], &[1e101], &[0.0]).unwrap();
        }
        let stats = window.cv_statistics().next().unwrap();
        close(stats.variance.unwrap(), 2.0 / 3.0);
        close(stats.bias.unwrap(), 0.0);
        for _ in 0..3 {
            window.observe(1.0, &[3.0], &[2.0], &[0.0], &[1e101], &[0.0]).unwrap();
        }
        assert_eq!(window.cv_statistics().next().unwrap().variance, Some(0.0));
    }

    #[test]
    fn a_large_constant_target_offset_does_not_erase_error_variance() {
        let mut window = RollingPerformance::new(3, 1, 1).unwrap();
        for pv in [1.0, 2.0, 3.0] {
            window.observe(1.0, &[pv], &[1e20], &[0.0], &[4.0], &[0.0]).unwrap();
        }
        let stats = window.cv_statistics().next().unwrap();
        close(stats.variance.unwrap(), 2.0 / 3.0);
        close(stats.tracking_error_variance.unwrap(), 2.0 / 3.0);
        close(stats.bias.unwrap(), -1e20);
        close(stats.rmse.unwrap(), 1e20);
    }

    #[test]
    fn representable_rmse_and_weighted_variance_avoid_unnecessary_square_overflow() {
        let mut window = RollingPerformance::new(2, 1, 1).unwrap();
        for _ in 0..2 {
            window.observe(1.0, &[0.0], &[1e200], &[-1.0], &[1.0], &[0.0]).unwrap();
        }
        close(window.cv_statistics().next().unwrap().rmse.unwrap(), 1e200);
        window.reset();
        for pv in [-7e153, 7e153] {
            window.observe(1.0, &[pv], &[0.0], &[-f64::MAX], &[f64::MAX], &[0.0]).unwrap();
        }
        let stats = window.cv_statistics().next().unwrap();
        close(stats.variance.unwrap(), 4.9e307);
        close(stats.rmse.unwrap(), 7e153);
    }

    #[test]
    fn small_mv_edges_survive_eviction_of_a_much_larger_edge() {
        let mut window = RollingPerformance::new(3, 1, 1).unwrap();
        for sp in [1e16, 0.0, 1.0, 2.0] {
            window.observe(1.0, &[0.0], &[0.0], &[-1.0], &[1.0], &[sp]).unwrap();
        }
        assert_eq!(window.mv_total_variation(), &[2.0]);
        for sp in [3.0, 4.0, 5.0] {
            window.observe(1.0, &[0.0], &[0.0], &[-1.0], &[1.0], &[sp]).unwrap();
        }
        assert_eq!(window.mv_total_variation(), &[2.0]);
    }

    #[test]
    fn invalid_dimensions_in_every_input_vector_reject_the_entire_observation() {
        for index in 0..5 {
            for bad in [&[][..], &[1.0, 2.0][..]] {
                let mut window = RollingPerformance::new(3, 1, 1).unwrap();
                window.observe(1.0, &[1.0], &[1.0], &[0.0], &[2.0], &[1.0]).unwrap();
                let mut values: [&[f64]; 5] = [&[1.0], &[1.0], &[0.0], &[2.0], &[1.0]];
                values[index] = bad;
                assert!(window.observe(1.0, values[0], values[1], values[2], values[3], values[4]).is_err());
                assert_eq!(window.samples(), 0);
                assert_eq!(window.mv_total_variation(), &[0.0]);
                assert!(window.cv_statistics().all(|stats| stats.variance.is_none()));
            }
        }
    }

    #[test]
    fn nonfinite_values_in_every_input_vector_reset_the_window() {
        for index in 0..5 {
            for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut window = RollingPerformance::new(3, 1, 1).unwrap();
                window.observe(1.0, &[1.0], &[1.0], &[0.0], &[2.0], &[1.0]).unwrap();
                let mut values = [1.0, 1.0, 0.0, 2.0, 1.0];
                values[index] = bad;
                assert!(window.observe(1.0, &values[0..1], &values[1..2], &values[2..3],
                    &values[3..4], &values[4..5]).is_err());
                assert_eq!(window.samples(), 0);
                window.observe(1.0, &[1.0], &[1.0], &[0.0], &[2.0], &[1.0]).unwrap();
                assert_eq!(window.samples(), 1);
                assert_eq!(window.mv_total_variation(), &[0.0]);
            }
        }
    }

    #[test]
    fn invalid_modes_and_non_strict_operating_limits_are_rejected() {
        let mut window = RollingPerformance::new(2, 1, 1).unwrap();
        for mode in [-1.0, 0.5, 2.5, 3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(window.observe(mode, &[1.0], &[1.0], &[0.0], &[2.0], &[1.0]).is_err());
        }
        for (low, high) in [(1.0, 1.0), (2.0, 1.0)] {
            assert!(window.observe(1.0, &[1.0], &[1.0], &[low], &[high], &[1.0]).is_err());
        }
        assert_eq!(window.samples(), 0);
    }

    #[test]
    fn arithmetic_overflow_is_an_error_not_a_nonfinite_published_statistic() {
        let mut window = RollingPerformance::new(3, 1, 1).unwrap();
        assert!(window.observe(1.0, &[f64::MAX], &[-f64::MAX], &[-f64::MAX],
            &[f64::MAX], &[0.0]).is_err());
        assert_eq!(window.samples(), 0);
        for magnitude in [1e200, f64::MAX] {
            window.observe(1.0, &[magnitude], &[0.0], &[-f64::MAX], &[f64::MAX], &[0.0]).unwrap();
            assert!(window.observe(1.0, &[-magnitude], &[0.0], &[-f64::MAX],
                &[f64::MAX], &[0.0]).is_err());
            assert_eq!(window.samples(), 0);
        }
        window.observe(1.0, &[0.0], &[0.0], &[-1.0], &[1.0], &[1e308]).unwrap();
        assert!(window.observe(1.0, &[0.0], &[0.0], &[-1.0], &[1.0], &[-1e308]).is_err());
        assert_eq!(window.samples(), 0);
        for sp in [0.0, 1e308] {
            window.observe(1.0, &[0.0], &[0.0], &[-1.0], &[1.0], &[sp]).unwrap();
        }
        assert!(window.observe(1.0, &[0.0], &[0.0], &[-1.0], &[1.0], &[0.0]).is_err());
        assert_eq!(window.samples(), 0);
        assert_eq!(window.mv_total_variation(), &[0.0]);
        assert_eq!(window.cv_statistics().next().unwrap(), super::CvStatistics::default());
    }

    #[test]
    fn constructor_enforces_sample_dimension_cell_and_memory_bounds() {
        for size in [0, 1, 100_001, usize::MAX] {
            assert!(RollingPerformance::new(size, 1, 1).is_err());
        }
        for count in [0, 1_025, usize::MAX] {
            assert!(RollingPerformance::new(2, count, 1).is_err());
            assert!(RollingPerformance::new(2, 1, count).is_err());
        }
        assert!(RollingPerformance::new(100_000, 21, 1).is_err());
        assert!(RollingPerformance::new(100_000, 7, 1).is_err());
        assert_eq!(RollingPerformance::new(100_000, 1, 1).unwrap().window_samples(), 100_000);
    }
}
