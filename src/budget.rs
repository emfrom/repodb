use crate::Result;

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

    pub(crate) fn remaining(&self) -> usize {
        self.limit - self.used
    }
    pub(crate) fn used(&self) -> usize {
        self.used
    }
}
