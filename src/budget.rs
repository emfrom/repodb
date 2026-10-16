use crate::Result;

// Bounds RepoDB-owned refresh preparation payloads across history and imports.
pub(crate) const MAX_REFRESH_MEMORY_BYTES: usize = 256 * 1024 * 1024;

pub(crate) struct Budget {
    limit: usize,
    used: usize,
    description: &'static str,
}

impl Budget {
    pub(crate) fn new(limit: usize, description: &'static str) -> Self {
        Self {
            limit,
            used: 0,
            description,
        }
    }

    pub(crate) fn charge(&mut self, bytes: usize, purpose: &str) -> Result<()> {
        self.used = self.used.checked_add(bytes).filter(|used| *used <= self.limit)
            .ok_or_else(|| -> Box<dyn std::error::Error> { format!("{} exceeds the {}-byte aggregate preparation-memory budget while preparing {purpose}; reduce indexed data and retry", self.description, self.limit).into() })?;
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
