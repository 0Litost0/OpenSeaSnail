use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::AccountError;

#[derive(Default)]
struct LeaseState {
    readers: usize,
    deleting: bool,
}

/// 与单个 Crypto 实例同生命周期的身份操作和账户 lease 协调器。
#[derive(Default)]
pub(crate) struct IdentityCoordination {
    states: Mutex<HashMap<String, LeaseState>>,
    operations: Mutex<()>,
}

impl IdentityCoordination {
    pub(crate) fn operation(&self) -> MutexGuard<'_, ()> {
        self.operations.lock().expect("identity operation mutex")
    }

    pub(crate) fn acquire_reader(
        self: &Arc<Self>,
        account_id: &str,
    ) -> Result<AccountLeaseGuard, AccountError> {
        let mut states = self.states.lock().expect("account lease tracker mutex");
        let state = states.entry(account_id.to_owned()).or_default();
        if state.deleting {
            return Err(AccountError::ActiveLease);
        }
        state.readers += 1;
        Ok(AccountLeaseGuard {
            coordination: Arc::clone(self),
            account_id: account_id.to_owned(),
        })
    }

    pub(crate) fn begin_delete(
        self: &Arc<Self>,
        account_id: &str,
    ) -> Result<AccountDeleteGuard, AccountError> {
        let mut states = self.states.lock().expect("account lease tracker mutex");
        let state = states.entry(account_id.to_owned()).or_default();
        if state.deleting || state.readers != 0 {
            return Err(AccountError::ActiveLease);
        }
        state.deleting = true;
        Ok(AccountDeleteGuard {
            coordination: Arc::clone(self),
            account_id: account_id.to_owned(),
        })
    }

    #[cfg(test)]
    pub(crate) fn active(&self, account_id: &str) -> usize {
        self.states
            .lock()
            .expect("account lease tracker mutex")
            .get(account_id)
            .map(|state| state.readers)
            .unwrap_or(0)
    }

    fn release_reader(&self, account_id: &str) {
        let mut states = self.states.lock().expect("account lease tracker mutex");
        if let Some(state) = states.get_mut(account_id) {
            debug_assert!(state.readers > 0);
            state.readers -= 1;
            if state.readers == 0 && !state.deleting {
                states.remove(account_id);
            }
        }
    }

    fn end_delete(&self, account_id: &str) {
        let mut states = self.states.lock().expect("account lease tracker mutex");
        if let Some(state) = states.get_mut(account_id) {
            state.deleting = false;
            if state.readers == 0 {
                states.remove(account_id);
            }
        }
    }
}

pub(crate) struct AccountLeaseGuard {
    coordination: Arc<IdentityCoordination>,
    account_id: String,
}

impl Clone for AccountLeaseGuard {
    fn clone(&self) -> Self {
        self.coordination
            .acquire_reader(&self.account_id)
            .expect("an existing account lease can always be cloned")
    }
}

impl Drop for AccountLeaseGuard {
    fn drop(&mut self) {
        self.coordination.release_reader(&self.account_id);
    }
}

pub(crate) struct AccountDeleteGuard {
    coordination: Arc<IdentityCoordination>,
    account_id: String,
}

impl Drop for AccountDeleteGuard {
    fn drop(&mut self) {
        self.coordination.end_delete(&self.account_id);
    }
}
