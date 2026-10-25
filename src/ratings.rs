use crate::domain::*;
#[derive(Debug, Clone)]
pub struct RatingInputs {
    pub last_common: RatingObservation,
    pub last_applied_local: RatingObservation,
    pub local: RatingObservation,
    pub remote: RatingObservation,
    pub pending_canonical: Option<Rating>,
    pub local_observation: Option<OperationId>,
    pub remote_observation: Option<OperationId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RatingDecision {
    Unchanged,
    Converge(Rating),
    PublishLocal(Rating),
    AdoptRemote(Rating),
    KeepPending(Rating),
    Conflict { local: Rating, remote: Rating },
    Unsupported,
}
fn stars(observation: &RatingObservation) -> Option<Rating> {
    if let RatingObservation::Stars(stars) = observation {
        Some(*stars)
    } else {
        None
    }
}
pub fn reconcile_rating(input: &RatingInputs) -> RatingDecision {
    if [&input.local, &input.remote].iter().any(|o| {
        matches!(
            o,
            RatingObservation::Rejected | RatingObservation::Unsupported(_)
        )
    }) {
        return RatingDecision::Unsupported;
    }
    let local = stars(&input.local);
    let remote = stars(&input.remote);
    let common = stars(&input.last_common);
    let applied = stars(&input.last_applied_local);
    let local_changed = local.is_some() && local != applied;
    if let Some(pending) = input.pending_canonical {
        return match (local, local_changed) {
            (Some(local), true) if local != pending => RatingDecision::Conflict {
                local,
                remote: pending,
            },
            (Some(local), true) if local == pending && remote == Some(pending) => {
                RatingDecision::Converge(pending)
            }
            _ => RatingDecision::KeepPending(pending),
        };
    }
    if common.is_none() {
        return match (local, remote) {
            (None, None) => RatingDecision::Unchanged,
            (Some(l), None) => RatingDecision::PublishLocal(l),
            (None, Some(r)) => RatingDecision::AdoptRemote(r),
            (Some(l), Some(r)) if l == r => RatingDecision::Converge(l),
            (Some(l), Some(r)) => RatingDecision::Conflict {
                local: l,
                remote: r,
            },
        };
    }
    let remote_changed = remote.is_some() && remote != common;
    match (local, remote, local_changed, remote_changed) {
        (Some(l), Some(r), true, true) if l != r => RatingDecision::Conflict {
            local: l,
            remote: r,
        },
        (Some(l), Some(_), true, true) => RatingDecision::Converge(l),
        (Some(l), _, true, false) => RatingDecision::PublishLocal(l),
        (_, Some(r), false, true) => RatingDecision::AdoptRemote(r),
        _ => RatingDecision::Unchanged,
    }
}
