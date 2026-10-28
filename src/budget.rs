use crate::Result;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

const REFRESH_DEADLINE_ENV: &str = "REPODB_MAX_REFRESH_DURATION_SECS";
const DEFAULT_MAX_REFRESH_DURATION_SECS: u64 = 3600;
const MAX_CONFIGURED_REFRESH_DURATION_SECS: u64 = 86_400;

thread_local! {
    static REFRESH_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

static REFRESH_INTERRUPTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn record_interrupt(_signal: libc::c_int) {
    REFRESH_INTERRUPTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) struct RefreshDeadline {
    previous: Option<Instant>,
    #[cfg(unix)]
    previous_sigint: libc::sighandler_t,
}

impl RefreshDeadline {
    pub(crate) fn start() -> Result<Self> {
        let seconds = match std::env::var(REFRESH_DEADLINE_ENV) {
            Ok(value) => value.parse::<u64>().map_err(|_| {
                format!("{REFRESH_DEADLINE_ENV} must be an integer from 1 through {MAX_CONFIGURED_REFRESH_DURATION_SECS}")
            })?,
            Err(std::env::VarError::NotPresent) => DEFAULT_MAX_REFRESH_DURATION_SECS,
            Err(error) => return Err(error.into()),
        };
        if !(1..=MAX_CONFIGURED_REFRESH_DURATION_SECS).contains(&seconds) {
            return Err(format!(
                "{REFRESH_DEADLINE_ENV} must be an integer from 1 through {MAX_CONFIGURED_REFRESH_DURATION_SECS}"
            )
            .into());
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .ok_or("refresh deadline overflow")?;
        let previous = REFRESH_DEADLINE.with(|slot| {
            let previous = slot.get();
            slot.set(Some(
                previous.map_or(deadline, |previous| previous.min(deadline)),
            ));
            previous
        });
        #[cfg(unix)]
        let previous_sigint = if previous.is_none() {
            REFRESH_INTERRUPTED.store(false, std::sync::atomic::Ordering::Relaxed);
            // SAFETY: the handler only sets a lock-free atomic flag; the previous
            // process handler is restored when the refresh scope exits.
            let previous_handler = unsafe {
                libc::signal(
                    libc::SIGINT,
                    record_interrupt as *const () as libc::sighandler_t,
                )
            };
            if previous_handler == libc::SIG_ERR {
                REFRESH_DEADLINE.with(|slot| slot.set(previous));
                return Err(std::io::Error::last_os_error().into());
            }
            previous_handler
        } else {
            0
        };
        Ok(Self {
            previous,
            #[cfg(unix)]
            previous_sigint,
        })
    }
}

impl Drop for RefreshDeadline {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.previous.is_none() {
            // SAFETY: restore the handler captured by `start` for the same scope.
            unsafe {
                libc::signal(libc::SIGINT, self.previous_sigint);
            }
        }
        REFRESH_DEADLINE.with(|slot| slot.set(self.previous));
    }
}

pub(crate) fn current_refresh_deadline() -> Result<Option<Instant>> {
    check_refresh_deadline()?;
    Ok(REFRESH_DEADLINE.with(Cell::get))
}

pub(crate) struct InheritedRefreshDeadline {
    previous: Option<Instant>,
}

pub(crate) fn inherit_refresh_deadline(deadline: Option<Instant>) -> InheritedRefreshDeadline {
    let previous = REFRESH_DEADLINE.with(|slot| {
        let previous = slot.get();
        let inherited = match (previous, deadline) {
            (Some(current), Some(parent)) => Some(current.min(parent)),
            (current, parent) => current.or(parent),
        };
        slot.set(inherited);
        previous
    });
    InheritedRefreshDeadline { previous }
}

impl Drop for InheritedRefreshDeadline {
    fn drop(&mut self) {
        REFRESH_DEADLINE.with(|slot| slot.set(self.previous));
    }
}

pub(crate) fn check_refresh_deadline() -> Result<()> {
    if REFRESH_INTERRUPTED.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("refresh cancelled by interrupt signal".into());
    }
    let expired = REFRESH_DEADLINE.with(|slot| {
        slot.get()
            .is_some_and(|deadline| Instant::now() >= deadline)
    });
    if expired {
        return Err("refresh exceeded its total wall-clock deadline".into());
    }
    Ok(())
}

pub(crate) fn remaining_refresh_time() -> Result<Option<Duration>> {
    check_refresh_deadline()?;
    let remaining = REFRESH_DEADLINE.with(|slot| {
        slot.get()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    });
    if remaining.is_some_and(|remaining| remaining.is_zero()) {
        return Err("refresh exceeded its total wall-clock deadline".into());
    }
    Ok(remaining)
}

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
        check_refresh_deadline()?;
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
        check_refresh_deadline()?;
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
    fn inherited_refresh_deadline_is_visible_on_worker_thread_and_restores_prior_state() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let worker = std::thread::spawn(move || {
            assert_eq!(current_refresh_deadline().unwrap(), None);
            {
                let _scope = inherit_refresh_deadline(Some(deadline));
                assert_eq!(current_refresh_deadline().unwrap(), Some(deadline));
            }
            assert_eq!(current_refresh_deadline().unwrap(), None);
        });
        worker.join().unwrap();
    }

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
