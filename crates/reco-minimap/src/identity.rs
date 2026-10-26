//! Sport-independent identity candidates, separate from reusable tracking slots.
//!
//! OCR evidence is not proof of a person's identity. A team/number pair is a
//! provisional roster key within this analysis, not a global person identifier.
//! Simultaneous claims by teammates are exposed as conflicts, never resolved by
//! whichever track happened to be visited first.

use crate::{AgentOut, team::Team, tracker::TrackState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IdentityStatus {
    #[default]
    Unknown,
    Candidate,
    Conflict,
}

impl IdentityStatus {
    pub fn key(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Candidate => "candidate",
            Self::Conflict => "conflict",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerKey {
    pub team: Team,
    pub number: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdentityAssignment {
    pub status: IdentityStatus,
    /// Retained for inspection even on conflicts; only Candidate is unambiguous.
    pub player: Option<PlayerKey>,
}

/// Recompute assignments from the complete frame so duplicate jersey claims are
/// symmetric. Lost tracks cannot reserve a roster identity indefinitely.
pub fn assign(players: &mut [AgentOut]) {
    let claims: Vec<Option<PlayerKey>> = players
        .iter()
        .map(|a| {
            if a.referee || a.team == Team::Unknown || a.state == TrackState::Lost {
                None
            } else {
                a.jersey.map(|number| PlayerKey {
                    team: a.team,
                    number,
                })
            }
        })
        .collect();
    for (agent, claim) in players.iter_mut().zip(&claims) {
        let status = match claim {
            None => IdentityStatus::Unknown,
            Some(_) if claims.iter().filter(|other| *other == claim).count() > 1 => {
                IdentityStatus::Conflict
            }
            Some(_) => IdentityStatus::Candidate,
        };
        agent.identity = IdentityAssignment {
            status,
            player: *claim,
        };
        if status != IdentityStatus::Candidate {
            // Existing renderers consume `jersey`; suppress misleading duplicate
            // labels while preserving the conflicting claim in structured output.
            agent.jersey = None;
        }
    }
}
