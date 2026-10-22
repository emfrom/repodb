use crate::Result;

// Maximum retained preparation memory across a refresh. Keep this finite: refresh
// accounting is a safety limit, not merely an overflow check.
pub(crate) const MAX_REFRESH_ACCOUNTING_BYTES: usize = 512 * 1024 * 1024;
const MAX_CONFIGURED_REFRESH_BYTES: usize = 4 * 1024 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_REFRESH_RECORDS: usize = 10_000_000;
const MAX_CONFIGURED_REFRESH_RECORDS: usize = 100_000_000;
const REFRESH_BUDGET_ENV: &str = "REPODB_MAX_REFRESH_BYTES";
const REFRESH_RECORDS_ENV: &str = "REPODB_MAX_REFRESH_RECORDS";

pub(crate) struct Budget {
    limit: usize,
    used: usize,
    description: &'static str,
    record_limit: usize,
    records: usize,
}

impl Budget {
    pub(crate) fn new(limit: usize, description: &'static str) -> Self {
        Self {
            limit,
            used: 0,
            description,
            record_limit: usize::MAX,
            records: 0,
        }
    }

    pub(crate) fn refresh(description: &'static str) -> Result<Self> {
        let limit = match std::env::var(REFRESH_BUDGET_ENV) {
            Ok(value) => {
                let limit: usize = value
                    .parse()
                    .map_err(|_| format!("{REFRESH_BUDGET_ENV} must be a byte count"))?;
                if !(1..=MAX_CONFIGURED_REFRESH_BYTES).contains(&limit) {
                    return Err(format!(
                        "{REFRESH_BUDGET_ENV} must be between 1 and {MAX_CONFIGURED_REFRESH_BYTES}"
                    )
                    .into());
                }
                limit
            }
            Err(std::env::VarError::NotPresent) => MAX_REFRESH_ACCOUNTING_BYTES,
            Err(error) => return Err(error.into()),
        };
        let record_limit = match std::env::var(REFRESH_RECORDS_ENV) {
            Ok(value) => {
                let limit: usize = value
                    .parse()
                    .map_err(|_| format!("{REFRESH_RECORDS_ENV} must be a record count"))?;
                if !(1..=MAX_CONFIGURED_REFRESH_RECORDS).contains(&limit) {
                    return Err(format!(
                        "{REFRESH_RECORDS_ENV} must be between 1 and {MAX_CONFIGURED_REFRESH_RECORDS}"
                    )
                    .into());
                }
                limit
            }
            Err(std::env::VarError::NotPresent) => DEFAULT_MAX_REFRESH_RECORDS,
            Err(error) => return Err(error.into()),
        };
        Ok(Self::with_record_limit(limit, record_limit, description))
    }

    pub(crate) fn with_record_limit(
        limit: usize,
        record_limit: usize,
        description: &'static str,
    ) -> Self {
        Self {
            limit,
            used: 0,
            description,
            record_limit,
            records: 0,
        }
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) fn charge(&mut self, bytes: usize, purpose: &str) -> Result<()> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|used| *used <= self.limit)
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "{} size overflow while preparing {purpose}",
                    self.description
                )
                .into()
            })?;
        Ok(())
    }

    pub(crate) fn charge_records(&mut self, count: usize, purpose: &str) -> Result<()> {
        self.records = self
            .records
            .checked_add(count)
            .filter(|records| *records <= self.record_limit)
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "{} record limit exceeded while preparing {purpose} (limit {})",
                    self.description, self.record_limit
                )
                .into()
            })?;
        Ok(())
    }

    pub(crate) fn release(&mut self, bytes: usize) {
        self.used = self
            .used
            .checked_sub(bytes)
            .expect("budget release exceeds charged bytes");
    }

    pub(crate) fn reserve_vec<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
        purpose: &str,
    ) -> Result<()> {
        let old_capacity = values.capacity();
        let requested =
            values
                .len()
                .checked_add(additional)
                .ok_or_else(|| -> Box<dyn std::error::Error> {
                    format!("{purpose} capacity overflow").into()
                })?;
        let requested_capacity = requested.saturating_sub(old_capacity);
        let reservation = requested_capacity
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!("{purpose} capacity overflow").into()
            })?;
        // Reserve the full requested capacity delta before asking the allocator.
        // Exact reserve avoids Vec's geometric growth and keeps the charged bound
        // equal to the maximum capacity this operation is allowed to retain.
        self.charge(reservation, purpose)?;
        if let Err(error) = values.try_reserve_exact(additional) {
            self.release(reservation);
            return Err(error.into());
        }
        let actual_capacity = values.capacity().saturating_sub(old_capacity);
        if actual_capacity > requested_capacity {
            // The allocator may over-allocate despite `try_reserve_exact`; do not
            // retain an allocation that is outside the pre-reserved budget.
            return Err(format!("{purpose} allocator exceeded exact capacity bound").into());
        }
        let actual = actual_capacity
            .checked_mul(std::mem::size_of::<T>())
            .expect("capacity delta was checked before allocation");
        self.release(reservation - actual);
        Ok(())
    }

    pub(crate) fn remaining(&self) -> usize {
        self.limit - self.used
    }
    pub(crate) fn used(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_accounting_ceiling_is_finite() {
        assert_eq!(MAX_REFRESH_ACCOUNTING_BYTES, 512 * 1024 * 1024);
    }

    #[test]
    fn cumulative_charges_reject_growth_beyond_the_aggregate_limit() {
        let mut budget = Budget::new(10, "test");
        budget.charge(6, "first source").unwrap();
        budget.charge(3, "second source").unwrap();
        assert!(budget.charge(2, "cumulative source data").is_err());
        assert_eq!(budget.used(), 9);
    }

    #[test]
    fn cumulative_record_limit_rejects_growth_beyond_the_configured_cap() {
        let mut budget = Budget::with_record_limit(100, 3, "test");
        budget.charge_records(2, "first input").unwrap();
        budget.charge_records(1, "second input").unwrap();
        assert!(budget.charge_records(1, "third input").is_err());
        assert_eq!(budget.records, 3);
    }

    #[test]
    fn refresh_record_ceiling_is_finite() {
        assert_eq!(DEFAULT_MAX_REFRESH_RECORDS, 10_000_000);
        assert_eq!(MAX_CONFIGURED_REFRESH_RECORDS, 100_000_000);
    }

    #[test]
    fn reserve_vec_charges_capacity_and_rolls_back_rejected_growth() {
        let mut budget = Budget::new(4, "test");
        let mut values = Vec::<u8>::new();
        budget.reserve_vec(&mut values, 4, "values").unwrap();
        assert_eq!(budget.used(), values.capacity());
        values.resize(4, 0);

        let capacity = values.capacity();
        assert!(budget.reserve_vec(&mut values, 1, "values").is_err());
        assert_eq!(values.capacity(), capacity);
        assert_eq!(budget.used(), capacity);

        budget.release(values.capacity());
        assert_eq!(budget.used(), 0);
    }
}
