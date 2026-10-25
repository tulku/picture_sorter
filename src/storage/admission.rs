#[derive(Debug, Clone)]
pub struct AdmissionInput {
    pub budget: u64,
    pub occupied: u64,
    pub reserved: u64,
    pub available: u64,
    pub reserve: u64,
    pub required: u64,
    pub existing: u64,
    pub partial: u64,
    pub overhead: u64,
    pub nas_available: bool,
    pub allow_direct: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    Local { additional_bytes: u64 },
    DirectNas,
    WaitingCapacity,
}
pub fn admit(i: &AdmissionInput) -> AdmissionDecision {
    let additional = i
        .required
        .checked_sub(i.existing)
        .and_then(|n| n.checked_sub(i.partial))
        .and_then(|n| n.checked_add(i.overhead));
    if let Some(n) = additional {
        let budget_ok = i
            .occupied
            .checked_add(i.reserved)
            .and_then(|b| b.checked_add(n))
            .is_some_and(|b| b <= i.budget);
        let reserve_ok = n
            .checked_add(i.reserved)
            .and_then(|b| b.checked_add(i.reserve))
            .is_some_and(|b| b <= i.available);
        if budget_ok && reserve_ok {
            return AdmissionDecision::Local {
                additional_bytes: n,
            };
        }
    }
    if additional.is_some() && i.nas_available && i.allow_direct {
        AdmissionDecision::DirectNas
    } else {
        AdmissionDecision::WaitingCapacity
    }
}
