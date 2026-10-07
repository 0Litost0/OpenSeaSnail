//! 类型化 runtime single-flight 门禁（ST-M4.1）。
//!
//! 所有 primary ASR runtime 操作共享这一个物理 slot。lease 不可复制，且以
//! generation 做 compare-and-clear，避免旧 owner 的迟到 drop 清掉新 owner。

use std::sync::{Arc, Mutex, MutexGuard};

/// 占用 primary ASR runtime slot 的业务操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeOperation {
    Transcription { session_id: String },
    ModelSwitch { model_id: String },
}

/// 当前 slot 已被占用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOperationOccupied {
    pub operation: RuntimeOperation,
}

impl std::fmt::Display for RuntimeOperationOccupied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "runtime operation occupied: {:?}", self.operation)
    }
}

impl std::error::Error for RuntimeOperationOccupied {}

#[derive(Debug)]
struct ActiveOperation {
    generation: u64,
    operation: RuntimeOperation,
}

#[derive(Debug)]
struct GateState {
    next_generation: u64,
    active: Option<ActiveOperation>,
}

/// 全局唯一的 primary ASR runtime single-flight 门禁。
#[derive(Debug)]
pub struct RuntimeOperationGate {
    state: Mutex<GateState>,
}

impl Default for RuntimeOperationGate {
    fn default() -> Self {
        Self::new_for_composition_root()
    }
}

impl RuntimeOperationGate {
    /// 生产组合根唯一使用的构造入口。
    pub fn new_for_composition_root() -> Self {
        Self {
            state: Mutex::new(GateState {
                next_generation: 0,
                active: None,
            }),
        }
    }

    /// 测试构造入口；生产组合根应使用 `new_for_composition_root` 并注入同一实例。
    #[doc(hidden)]
    pub fn new_for_test() -> Self {
        Self::new_for_composition_root()
    }

    /// 非阻塞占用 slot；已占用时立即返回，不排队。
    pub fn acquire(
        self: &Arc<Self>,
        operation: RuntimeOperation,
    ) -> Result<RuntimeLease, RuntimeOperationOccupied> {
        let mut state = self.lock_state();
        if let Some(active) = state.active.as_ref() {
            return Err(RuntimeOperationOccupied {
                operation: active.operation.clone(),
            });
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        state.active = Some(ActiveOperation {
            generation,
            operation: operation.clone(),
        });
        Ok(RuntimeLease {
            gate: Arc::clone(self),
            generation,
            operation,
        })
    }

    pub fn active(&self) -> Option<RuntimeOperation> {
        self.lock_state()
            .active
            .as_ref()
            .map(|active| active.operation.clone())
    }

    /// Test-only helper for simulating a stale owner during the RAII regression test.
    #[cfg(test)]
    fn force_clear(&self) {
        self.lock_state().active = None;
    }

    fn release(&self, generation: u64) {
        let mut state = self.lock_state();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            state.active = None;
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 不可 Clone 的 RAII runtime lease。
#[derive(Debug)]
pub struct RuntimeLease {
    gate: Arc<RuntimeOperationGate>,
    generation: u64,
    operation: RuntimeOperation,
}

impl RuntimeLease {
    pub fn operation(&self) -> &RuntimeOperation {
        &self.operation
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        self.gate.release(self.generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_operations_share_one_physical_slot() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let tx = gate
            .acquire(RuntimeOperation::Transcription {
                session_id: "s1".into(),
            })
            .unwrap();
        let err = gate
            .acquire(RuntimeOperation::ModelSwitch {
                model_id: "m1".into(),
            })
            .unwrap_err();
        assert_eq!(err.operation, *tx.operation());
    }

    #[test]
    fn stale_lease_drop_does_not_clear_new_owner() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let old = gate
            .acquire(RuntimeOperation::Transcription {
                session_id: "old".into(),
            })
            .unwrap();
        let old_generation = old.generation();
        gate.force_clear();
        let new = gate
            .acquire(RuntimeOperation::ModelSwitch {
                model_id: "new".into(),
            })
            .unwrap();
        gate.release(old_generation);
        assert_eq!(gate.active(), Some(new.operation().clone()));
    }
}
