//! Conservative, pure policy calculations for future guest memory limits.
//!
//! This module deliberately does not query the host, create Windows Job Objects,
//! or enable guest execution. A future launcher must obtain a fresh trustworthy
//! memory snapshot immediately before launch and enforce the returned limits.

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const MIN_HOST_MEMORY_BYTES: u64 = 3 * GIB + GIB / 2;
const HOST_AVAILABLE_RESERVE_BYTES: u64 = 512 * MIB;
const MIN_GUEST_MEMORY_BUDGET_BYTES: u64 = 256 * MIB;
const ABSOLUTE_JOB_MEMORY_CEILING_BYTES: u64 = 4 * GIB;
const ABSOLUTE_PROCESS_MEMORY_CEILING_BYTES: u64 = 2 * GIB;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostMemorySnapshot {
    pub(crate) total_physical_bytes: u64,
    pub(crate) available_physical_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuestMemoryProfileTier {
    Constrained4GiB,
    Balanced8GiB,
    Performance16GiB,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GuestMemoryBudget {
    pub(crate) tier: GuestMemoryProfileTier,
    pub(crate) total_job_memory_limit_bytes: u64,
    pub(crate) process_memory_limit_bytes: u64,
    pub(crate) reserved_available_memory_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuestMemoryBudgetError {
    MissingOrInvalidSnapshot,
    HostBelowMinimumMemory,
    InsufficientAvailableMemory,
    HostMemoryQueryFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProfileLimits {
    tier: GuestMemoryProfileTier,
    minimum_total_bytes: u64,
    maximum_job_bytes: u64,
    maximum_process_bytes: u64,
}

const PROFILES: [ProfileLimits; 3] = [
    ProfileLimits {
        tier: GuestMemoryProfileTier::Constrained4GiB,
        // 3.5 GiB accommodates the usable-memory report from a nominal 4 GB PC.
        minimum_total_bytes: MIN_HOST_MEMORY_BYTES,
        maximum_job_bytes: GIB,
        maximum_process_bytes: 512 * MIB,
    },
    ProfileLimits {
        tier: GuestMemoryProfileTier::Balanced8GiB,
        minimum_total_bytes: 7 * GIB,
        maximum_job_bytes: 2 * GIB,
        maximum_process_bytes: GIB,
    },
    ProfileLimits {
        tier: GuestMemoryProfileTier::Performance16GiB,
        minimum_total_bytes: 14 * GIB,
        maximum_job_bytes: ABSOLUTE_JOB_MEMORY_CEILING_BYTES,
        maximum_process_bytes: ABSOLUTE_PROCESS_MEMORY_CEILING_BYTES,
    },
];

fn profile_for_total_memory(total_bytes: u64) -> Result<ProfileLimits, GuestMemoryBudgetError> {
    if total_bytes < MIN_HOST_MEMORY_BYTES {
        return Err(GuestMemoryBudgetError::HostBelowMinimumMemory);
    }

    Ok(PROFILES
        .iter()
        .rev()
        .find(|profile| total_bytes >= profile.minimum_total_bytes)
        .copied()
        .unwrap_or(PROFILES[0]))
}

/// Computes a fail-closed guest memory budget from an injected host snapshot.
///
/// The available-memory reserve protects the host and FRIDAY from a guest
/// consuming all currently available physical memory. The returned budget is
/// always bounded by both the selected tier and absolute product ceilings.
pub(crate) fn calculate_guest_memory_budget(
    snapshot: HostMemorySnapshot,
) -> Result<GuestMemoryBudget, GuestMemoryBudgetError> {
    if snapshot.total_physical_bytes == 0
        || snapshot.available_physical_bytes == 0
        || snapshot.available_physical_bytes > snapshot.total_physical_bytes
    {
        return Err(GuestMemoryBudgetError::MissingOrInvalidSnapshot);
    }

    let profile = profile_for_total_memory(snapshot.total_physical_bytes)?;
    let usable_available = snapshot
        .available_physical_bytes
        .checked_sub(HOST_AVAILABLE_RESERVE_BYTES)
        .ok_or(GuestMemoryBudgetError::InsufficientAvailableMemory)?;

    let job_limit = usable_available.min(profile.maximum_job_bytes);
    if job_limit < MIN_GUEST_MEMORY_BUDGET_BYTES {
        return Err(GuestMemoryBudgetError::InsufficientAvailableMemory);
    }

    let process_limit = profile
        .maximum_process_bytes
        .min(job_limit)
        .min(ABSOLUTE_PROCESS_MEMORY_CEILING_BYTES);

    if job_limit > ABSOLUTE_JOB_MEMORY_CEILING_BYTES
        || process_limit == 0
        || process_limit > job_limit
    {
        return Err(GuestMemoryBudgetError::MissingOrInvalidSnapshot);
    }

    Ok(GuestMemoryBudget {
        tier: profile.tier,
        total_job_memory_limit_bytes: job_limit,
        process_memory_limit_bytes: process_limit,
        reserved_available_memory_bytes: HOST_AVAILABLE_RESERVE_BYTES,
    })
}


/// Query current physical-memory totals from Windows immediately before job creation.
/// API failure is distinct from an invalid or insufficient snapshot and never falls
/// back to a guessed device tier.
#[cfg(windows)]
pub(crate) fn query_host_memory_snapshot() -> Result<HostMemorySnapshot, GuestMemoryBudgetError> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::System::SystemInformation::{
        GlobalMemoryStatusEx, MEMORYSTATUSEX,
    };

    let mut status: MEMORYSTATUSEX = unsafe { zeroed() };
    status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(GuestMemoryBudgetError::HostMemoryQueryFailed);
    }

    Ok(HostMemorySnapshot {
        total_physical_bytes: status.ullTotalPhys,
        available_physical_bytes: status.ullAvailPhys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(total_gib: u64, available_mib: u64) -> HostMemorySnapshot {
        HostMemorySnapshot {
            total_physical_bytes: total_gib * GIB,
            available_physical_bytes: available_mib * MIB,
        }
    }

    #[test]
    fn selects_conservative_profiles_at_documented_boundaries() {
        assert_eq!(
            profile_for_total_memory(MIN_HOST_MEMORY_BYTES).unwrap().tier,
            GuestMemoryProfileTier::Constrained4GiB
        );
        assert_eq!(
            profile_for_total_memory(7 * GIB).unwrap().tier,
            GuestMemoryProfileTier::Balanced8GiB
        );
        assert_eq!(
            profile_for_total_memory(14 * GIB).unwrap().tier,
            GuestMemoryProfileTier::Performance16GiB
        );
    }

    #[test]
    fn rejects_host_memory_below_minimum_profile() {
        assert_eq!(
            profile_for_total_memory(MIN_HOST_MEMORY_BYTES - 1),
            Err(GuestMemoryBudgetError::HostBelowMinimumMemory)
        );
    }

    #[test]
    fn rejects_missing_zero_and_inconsistent_memory_snapshots() {
        assert_eq!(
            calculate_guest_memory_budget(HostMemorySnapshot {
                total_physical_bytes: 0,
                available_physical_bytes: 0,
            }),
            Err(GuestMemoryBudgetError::MissingOrInvalidSnapshot)
        );
        assert_eq!(
            calculate_guest_memory_budget(HostMemorySnapshot {
                total_physical_bytes: 4 * GIB,
                available_physical_bytes: 0,
            }),
            Err(GuestMemoryBudgetError::MissingOrInvalidSnapshot)
        );
        assert_eq!(
            calculate_guest_memory_budget(HostMemorySnapshot {
                total_physical_bytes: 4 * GIB,
                available_physical_bytes: 5 * GIB,
            }),
            Err(GuestMemoryBudgetError::MissingOrInvalidSnapshot)
        );
    }

    #[test]
    fn four_gib_tier_is_bounded_by_live_available_memory_and_process_cap() {
        let budget = calculate_guest_memory_budget(snapshot(4, 1536)).unwrap();
        assert_eq!(budget.tier, GuestMemoryProfileTier::Constrained4GiB);
        assert_eq!(budget.total_job_memory_limit_bytes, GIB);
        assert_eq!(budget.process_memory_limit_bytes, 512 * MIB);
        assert_eq!(budget.reserved_available_memory_bytes, 512 * MIB);
    }

    #[test]
    fn eight_gib_tier_uses_the_smaller_of_tier_cap_and_available_memory() {
        let budget = calculate_guest_memory_budget(snapshot(8, 3072)).unwrap();
        assert_eq!(budget.tier, GuestMemoryProfileTier::Balanced8GiB);
        assert_eq!(budget.total_job_memory_limit_bytes, 2 * GIB);
        assert_eq!(budget.process_memory_limit_bytes, GIB);

        let constrained = calculate_guest_memory_budget(snapshot(8, 1024)).unwrap();
        assert_eq!(constrained.total_job_memory_limit_bytes, 512 * MIB);
        assert_eq!(constrained.process_memory_limit_bytes, 512 * MIB);
    }

    #[test]
    fn sixteen_gib_tier_obeys_absolute_product_ceilings() {
        let budget = calculate_guest_memory_budget(snapshot(16, 12 * 1024)).unwrap();
        assert_eq!(budget.tier, GuestMemoryProfileTier::Performance16GiB);
        assert_eq!(
            budget.total_job_memory_limit_bytes,
            ABSOLUTE_JOB_MEMORY_CEILING_BYTES
        );
        assert_eq!(
            budget.process_memory_limit_bytes,
            ABSOLUTE_PROCESS_MEMORY_CEILING_BYTES
        );
    }

    #[test]
    fn defers_when_available_memory_cannot_preserve_host_reserve_and_minimum_budget() {
        assert_eq!(
            calculate_guest_memory_budget(snapshot(4, 512)),
            Err(GuestMemoryBudgetError::InsufficientAvailableMemory)
        );
        assert_eq!(
            calculate_guest_memory_budget(snapshot(4, 767)),
            Err(GuestMemoryBudgetError::InsufficientAvailableMemory)
        );
    }

    #[test]
    fn budget_never_exceeds_job_or_process_absolute_caps() {
        let budget = calculate_guest_memory_budget(snapshot(128, 100 * 1024)).unwrap();
        assert!(budget.total_job_memory_limit_bytes <= 4 * GIB);
        assert!(budget.process_memory_limit_bytes <= 2 * GIB);
        assert!(budget.process_memory_limit_bytes <= budget.total_job_memory_limit_bytes);
    }
}
