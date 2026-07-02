//! Portunus data-plane library — TCP/UDP forwarding shared between
//! portunus-client (gRPC control plane) and portunus-standalone (TOML).

pub mod forwarder;
pub mod resolver;
pub mod shutdown;

// Rules and lifecycle
pub use forwarder::{
    ClientRule, MultiTarget, MultiTargetObservability, RuleStatusEvent, run as run_forwarder,
};

// Quota (client constructs; standalone never instantiates)
pub use forwarder::quota::QuotaHandle;

// Wire-neutral stats snapshot types + getters
pub use forwarder::stats::{
    OwnerRateLimitStatsSnapshot, PerPortStatsSnapshot, PerTargetStatsSnapshot,
    RateLimitRejectReason, RateLimitStatsSnapshot, RuleStats, RuleStatsSnapshot,
    RuleStatsSnapshotBasic, SniListenerStatsSnapshot, TargetHealth,
};

// SNI data-plane entry points (client port_groups consumes)
pub use forwarder::sni::{
    SniDispatchState, SniListener, SniListenerCounters, SniRouteResolver, SniRuleSlot,
};

// Rate limit control-plane handles (client constructs)
pub use forwarder::rate_limit::{
    OwnerRateLimitHandle, OwnerRateLimitStatsRegistry, RateLimitScopeManager,
    RateLimitStatsAccumulator, RuleRateLimitHandle,
};

// PROXY protocol prelude
pub use forwarder::proxy_protocol::ProxyProtocolPrelude;

// Resolver
pub use resolver::{HickoryResolver, LiveResolver, Resolve};

// Shutdown primitive
pub use shutdown::Shutdown;
