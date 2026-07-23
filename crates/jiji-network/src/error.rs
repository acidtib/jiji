use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NetworkPlanError {
    #[error("Invalid network CIDR '{value}': {reason}. Update the network configuration.")]
    InvalidCidr { value: String, reason: String },

    #[error(
        "Network ranges overlap: management_cidr '{management}' and container_cidr \
         '{container}'. Configure two disjoint IPv4 ranges."
    )]
    OverlappingAddressSpaces {
        management: String,
        container: String,
    },

    #[error(
        "Container range '{cidr}' is too small. Configure a range containing at least one /{prefix} \
         server subnet."
    )]
    ContainerRangeTooSmall { cidr: String, prefix: u8 },

    #[error(
        "Management range '{cidr}' has {available} usable addresses but the container range \
         requires {required}. Configure a larger management_cidr."
    )]
    ManagementRangeTooSmall {
        cidr: String,
        available: u64,
        required: u64,
    },

    #[error(
        "Network allocation bucket {bucket} for {kind} is full ({capacity} entries). Change one \
         of the colliding names or configure a larger address range."
    )]
    BucketExhausted {
        kind: &'static str,
        bucket: u64,
        capacity: usize,
    },

    #[error(
        "Service '{service}' lists server '{server}' more than once. Remove the duplicate host."
    )]
    DuplicateServiceHost { service: String, server: String },

    #[error(
        "Service '{service}' references unknown server '{server}'. Add that server or correct the \
         service hosts list."
    )]
    UnknownServiceHost { service: String, server: String },

    #[error("No configured server matches host filter '{filter}'. Check --hosts and try again.")]
    UnmatchedHostFilter { filter: String },

    #[error(
        "No configured service matches service filter '{filter}'. Check --services and try again."
    )]
    UnmatchedServiceFilter { filter: String },
}
