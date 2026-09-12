use super::{ProcessDescriptor, ProcessIdentity, ProcessResources};

/// One member of a workload group with its position relative to the root.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkloadMember {
    pub process: ProcessDescriptor,
    pub resources: ProcessResources,
    pub depth: usize,
}

/// Orders workload members leaf-first (deepest first, then highest PID) and
/// always places the root identity last.
#[must_use]
pub(crate) fn termination_order(
    root: ProcessIdentity,
    members: &[WorkloadMember],
) -> Vec<ProcessIdentity> {
    let mut members = members
        .iter()
        .filter(|member| member.process.identity() != root)
        .collect::<Vec<_>>();
    members.sort_by(|left, right| {
        right.depth.cmp(&left.depth).then_with(|| {
            right
                .process
                .identity()
                .pid()
                .cmp(&left.process.identity().pid())
        })
    });
    let mut order = members
        .into_iter()
        .map(|member| member.process.identity())
        .collect::<Vec<_>>();
    order.push(root);
    order
}
