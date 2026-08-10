use tracing::warn;

/// Makes an implicit transaction rollback visible in production logs.
pub struct DbOperationGuard {
    operation: &'static str,
    committed: bool,
}

impl DbOperationGuard {
    pub fn new(operation: &'static str) -> Self {
        Self {
            operation,
            committed: false,
        }
    }

    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for DbOperationGuard {
    fn drop(&mut self) {
        if !self.committed {
            warn!(operation = self.operation, "database operation rolled back");
        }
    }
}
