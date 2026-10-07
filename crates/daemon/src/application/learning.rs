//! One-shot learning tickets used to bind post-paste observations to a session.
use base64::Engine as _;
use rand::RngCore;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(120);
const MAX_TICKETS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LearningTicketClaims {
    pub account_id: String,
    pub session_id: String,
    pub generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LearningTicketData {
    pub claims: LearningTicketClaims,
    pub cleanup_candidates: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LearningTicketError {
    Invalid,
    Expired,
    Consumed,
    Mismatch,
}
#[derive(Clone, Debug)]
struct Entry {
    data: LearningTicketData,
    expires_at: Instant,
}

#[derive(Default)]
struct RegistryState {
    entries: HashMap<String, Entry>,
    order: VecDeque<String>,
    by_session: HashMap<(String, String), String>,
    generations: HashMap<(String, String), u64>,
    generation_order: VecDeque<(String, String)>,
    tombstones: HashMap<String, LearningTicketError>,
    tombstone_order: VecDeque<String>,
}
#[derive(Default)]
pub struct LearningTicketRegistry {
    state: Mutex<RegistryState>,
}

impl LearningTicketRegistry {
    pub fn issue(
        &self,
        account_id: &str,
        session_id: &str,
        cleanup_candidates: Vec<String>,
    ) -> String {
        self.issue_at(
            account_id,
            session_id,
            cleanup_candidates,
            Instant::now(),
            TTL,
        )
    }
    fn issue_at(
        &self,
        account_id: &str,
        session_id: &str,
        cleanup_candidates: Vec<String>,
        now: Instant,
        ttl: Duration,
    ) -> String {
        let mut bytes = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let ticket = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let mut state = self.state.lock().expect("ticket registry poisoned");
        Self::prune(&mut state, now);
        let key = (account_id.to_string(), session_id.to_string());
        if let Some(old) = state.by_session.remove(&key) {
            state.entries.remove(&old);
            state.order.retain(|item| item != &old);
            Self::remember(&mut state, old, LearningTicketError::Expired);
        }
        while state.order.len() >= MAX_TICKETS {
            if let Some(old) = state.order.pop_front() {
                if let Some(entry) = state.entries.remove(&old) {
                    state
                        .by_session
                        .remove(&(entry.data.claims.account_id, entry.data.claims.session_id));
                }
                Self::remember(&mut state, old, LearningTicketError::Expired);
            }
        }
        let generation = *state
            .generations
            .entry(key.clone())
            .and_modify(|value| *value += 1)
            .or_insert(1);
        state.generation_order.retain(|item| item != &key);
        state.generation_order.push_back(key.clone());
        while state.generation_order.len() > MAX_TICKETS {
            if let Some(old) = state.generation_order.pop_front() {
                state.generations.remove(&old);
            }
        }
        let data = LearningTicketData {
            claims: LearningTicketClaims {
                account_id: key.0.clone(),
                session_id: key.1.clone(),
                generation,
            },
            cleanup_candidates,
        };
        state.by_session.insert(key, ticket.clone());
        state.order.push_back(ticket.clone());
        state.entries.insert(
            ticket.clone(),
            Entry {
                data,
                expires_at: now + ttl,
            },
        );
        ticket
    }
    pub fn consume(
        &self,
        ticket: &str,
        account_id: &str,
    ) -> Result<LearningTicketData, LearningTicketError> {
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(ticket)
            .map_err(|_| LearningTicketError::Invalid)?;
        if decoded.len() != 32 {
            return Err(LearningTicketError::Invalid);
        }
        let mut state = self.state.lock().expect("ticket registry poisoned");
        Self::prune(&mut state, Instant::now());
        if let Some(error) = state.tombstones.get(ticket).copied() {
            return Err(error);
        }
        let entry = state
            .entries
            .remove(ticket)
            .ok_or(LearningTicketError::Expired)?;
        state.order.retain(|item| item != ticket);
        state.by_session.remove(&(
            entry.data.claims.account_id.clone(),
            entry.data.claims.session_id.clone(),
        ));
        if entry.data.claims.account_id != account_id {
            Self::remember(
                &mut state,
                ticket.to_string(),
                LearningTicketError::Consumed,
            );
            return Err(LearningTicketError::Mismatch);
        }
        Self::remember(
            &mut state,
            ticket.to_string(),
            LearningTicketError::Consumed,
        );
        Ok(entry.data)
    }
    fn prune(state: &mut RegistryState, now: Instant) {
        let expired: Vec<_> = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(ticket, _)| ticket.clone())
            .collect();
        for ticket in expired {
            if let Some(entry) = state.entries.remove(&ticket) {
                state
                    .by_session
                    .remove(&(entry.data.claims.account_id, entry.data.claims.session_id));
            }
            state.order.retain(|item| item != &ticket);
            Self::remember(state, ticket, LearningTicketError::Expired);
        }
    }
    fn remember(state: &mut RegistryState, ticket: String, error: LearningTicketError) {
        if state.tombstones.insert(ticket.clone(), error).is_none() {
            state.tombstone_order.push_back(ticket);
        }
        while state.tombstone_order.len() > MAX_TICKETS {
            if let Some(old) = state.tombstone_order.pop_front() {
                state.tombstones.remove(&old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_is_bound_one_shot_and_carries_candidates() {
        let r = LearningTicketRegistry::default();
        let t = r.issue("a", "s", vec!["SeaSnail".into()]);
        assert_eq!(r.consume(&t, "b"), Err(LearningTicketError::Mismatch));
        assert_eq!(r.consume(&t, "a"), Err(LearningTicketError::Consumed));
        let t = r.issue("a", "s", vec!["SeaSnail".into()]);
        let data = r.consume(&t, "a").unwrap();
        assert_eq!(data.claims.generation, 2);
        assert_eq!(data.cleanup_candidates, vec!["SeaSnail"]);
    }
    #[test]
    fn invalid_expired_replaced_and_capacity_are_distinct() {
        let r = LearningTicketRegistry::default();
        assert_eq!(r.consume("bad", "a"), Err(LearningTicketError::Invalid));
        let old = r.issue("a", "s", vec![]);
        let _new = r.issue("a", "s", vec![]);
        assert_eq!(r.consume(&old, "a"), Err(LearningTicketError::Expired));
        let expired = r.issue_at("a", "expired", vec![], Instant::now(), Duration::ZERO);
        assert_eq!(r.consume(&expired, "a"), Err(LearningTicketError::Expired));
        let first = r.issue("a", "first", vec![]);
        for index in 0..MAX_TICKETS {
            let _ = r.issue("a", &format!("s{index}"), vec![]);
        }
        assert_eq!(r.consume(&first, "a"), Err(LearningTicketError::Expired));
        let state = r.state.lock().unwrap();
        assert!(state.generations.len() <= MAX_TICKETS);
        assert!(state.generation_order.len() <= MAX_TICKETS);
    }
}
