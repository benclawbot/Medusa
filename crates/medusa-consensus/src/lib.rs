use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub voting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vote {
    pub voter: String,
    pub term: u64,
    pub candidate: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusRecord {
    pub term: u64,
    pub leader: Option<String>,
    pub members: Vec<Member>,
    pub votes: BTreeMap<String, Vote>,
    pub committed_index: u64,
    pub fingerprint: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConsensusError {
    #[error("membership must contain at least one voting member")]
    EmptyQuorum,
    #[error("duplicate member: {0}")]
    DuplicateMember(String),
    #[error("unknown voter: {0}")]
    UnknownVoter(String),
    #[error("candidate is not a voting member: {0}")]
    InvalidCandidate(String),
    #[error("stale term {provided}; current term is {current}")]
    StaleTerm { provided: u64, current: u64 },
    #[error("voter {0} already voted in this term")]
    DuplicateVote(String),
    #[error("split-brain evidence: multiple quorum leaders")]
    SplitBrain,
    #[error("record fingerprint mismatch")]
    FingerprintMismatch,
    #[error("commit index cannot move backwards")]
    CommitRegression,
}

impl ConsensusRecord {
    pub fn new(term: u64, mut members: Vec<Member>) -> Result<Self, ConsensusError> {
        members.sort_by(|a, b| a.id.cmp(&b.id));
        let mut seen = BTreeSet::new();
        for member in &members {
            if !seen.insert(member.id.clone()) {
                return Err(ConsensusError::DuplicateMember(member.id.clone()));
            }
        }
        if !members.iter().any(|member| member.voting) {
            return Err(ConsensusError::EmptyQuorum);
        }
        let mut record = Self {
            term,
            leader: None,
            members,
            votes: BTreeMap::new(),
            committed_index: 0,
            fingerprint: String::new(),
        };
        record.reseal();
        Ok(record)
    }

    pub fn quorum_size(&self) -> usize {
        let voters = self.members.iter().filter(|member| member.voting).count();
        voters / 2 + 1
    }

    pub fn begin_term(&mut self, term: u64) -> Result<(), ConsensusError> {
        if term <= self.term {
            return Err(ConsensusError::StaleTerm {
                provided: term,
                current: self.term,
            });
        }
        self.term = term;
        self.leader = None;
        self.votes.clear();
        self.reseal();
        Ok(())
    }

    pub fn cast_vote(&mut self, vote: Vote) -> Result<Option<String>, ConsensusError> {
        if vote.term != self.term {
            return Err(ConsensusError::StaleTerm {
                provided: vote.term,
                current: self.term,
            });
        }
        let voter = self.members.iter().find(|m| m.id == vote.voter && m.voting)
            .ok_or_else(|| ConsensusError::UnknownVoter(vote.voter.clone()))?;
        let _ = voter;
        if !self.members.iter().any(|m| m.id == vote.candidate && m.voting) {
            return Err(ConsensusError::InvalidCandidate(vote.candidate));
        }
        if self.votes.contains_key(&vote.voter) {
            return Err(ConsensusError::DuplicateVote(vote.voter));
        }
        self.votes.insert(vote.voter.clone(), vote);
        let elected = self.resolve_leader()?;
        self.leader = elected.clone();
        self.reseal();
        Ok(elected)
    }

    fn resolve_leader(&self) -> Result<Option<String>, ConsensusError> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for vote in self.votes.values() {
            *counts.entry(vote.candidate.as_str()).or_default() += 1;
        }
        let winners: Vec<String> = counts
            .into_iter()
            .filter_map(|(candidate, count)| (count >= self.quorum_size()).then(|| candidate.to_owned()))
            .collect();
        match winners.as_slice() {
            [] => Ok(None),
            [leader] => Ok(Some(leader.clone())),
            _ => Err(ConsensusError::SplitBrain),
        }
    }

    pub fn advance_commit(&mut self, leader: &str, index: u64) -> Result<(), ConsensusError> {
        if self.leader.as_deref() != Some(leader) {
            return Err(ConsensusError::InvalidCandidate(leader.to_owned()));
        }
        if index < self.committed_index {
            return Err(ConsensusError::CommitRegression);
        }
        self.committed_index = index;
        self.reseal();
        Ok(())
    }

    pub fn verify(&self) -> Result<(), ConsensusError> {
        let expected = self.compute_fingerprint();
        if expected != self.fingerprint {
            return Err(ConsensusError::FingerprintMismatch);
        }
        self.resolve_leader()?;
        Ok(())
    }

    fn reseal(&mut self) {
        self.fingerprint = self.compute_fingerprint();
    }

    fn compute_fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.term.to_be_bytes());
        hash.update(self.committed_index.to_be_bytes());
        hash.update(self.leader.as_deref().unwrap_or("-").as_bytes());
        for member in &self.members {
            hash.update(member.id.as_bytes());
            hash.update([u8::from(member.voting)]);
        }
        for vote in self.votes.values() {
            hash.update(vote.voter.as_bytes());
            hash.update(vote.term.to_be_bytes());
            hash.update(vote.candidate.as_bytes());
        }
        hex::encode(hash.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster() -> ConsensusRecord {
        ConsensusRecord::new(1, vec![
            Member { id: "c".into(), voting: true },
            Member { id: "a".into(), voting: true },
            Member { id: "b".into(), voting: true },
        ]).unwrap()
    }

    #[test]
    fn canonicalizes_membership_and_elects_on_quorum() {
        let mut record = cluster();
        assert_eq!(record.members[0].id, "a");
        assert_eq!(record.cast_vote(Vote { voter: "a".into(), term: 1, candidate: "b".into() }).unwrap(), None);
        assert_eq!(record.cast_vote(Vote { voter: "c".into(), term: 1, candidate: "b".into() }).unwrap(), Some("b".into()));
        record.verify().unwrap();
    }

    #[test]
    fn rejects_stale_terms_and_duplicate_votes() {
        let mut record = cluster();
        assert!(matches!(record.cast_vote(Vote { voter: "a".into(), term: 0, candidate: "a".into() }), Err(ConsensusError::StaleTerm { .. })));
        record.cast_vote(Vote { voter: "a".into(), term: 1, candidate: "a".into() }).unwrap();
        assert_eq!(record.cast_vote(Vote { voter: "a".into(), term: 1, candidate: "b".into() }), Err(ConsensusError::DuplicateVote("a".into())));
    }

    #[test]
    fn term_change_clears_leader_and_votes() {
        let mut record = cluster();
        record.cast_vote(Vote { voter: "a".into(), term: 1, candidate: "a".into() }).unwrap();
        record.cast_vote(Vote { voter: "b".into(), term: 1, candidate: "a".into() }).unwrap();
        record.begin_term(2).unwrap();
        assert!(record.leader.is_none());
        assert!(record.votes.is_empty());
    }

    #[test]
    fn only_elected_leader_advances_commit_monotonically() {
        let mut record = cluster();
        record.cast_vote(Vote { voter: "a".into(), term: 1, candidate: "b".into() }).unwrap();
        record.cast_vote(Vote { voter: "c".into(), term: 1, candidate: "b".into() }).unwrap();
        record.advance_commit("b", 7).unwrap();
        assert_eq!(record.advance_commit("b", 6), Err(ConsensusError::CommitRegression));
        assert!(record.advance_commit("a", 8).is_err());
    }

    #[test]
    fn detects_tampering() {
        let mut record = cluster();
        record.committed_index = 99;
        assert_eq!(record.verify(), Err(ConsensusError::FingerprintMismatch));
    }
}
