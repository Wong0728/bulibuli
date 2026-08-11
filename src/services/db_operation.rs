use tracing::warn;

/// 将隐式事务回滚记录到生产日志，便于发现未提交的数据库操作。
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
