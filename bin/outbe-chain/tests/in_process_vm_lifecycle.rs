//! Real node-process acceptance tests for the in-process TEE lifecycle.
#[path = "equivocation_auto_slashing/rpc.rs"]
pub mod rpc;
#[path = "equivocation_auto_slashing/services.rs"]
pub mod services;
#[path = "common/tee_cluster/mod.rs"]
pub mod tee_cluster;

#[path = "in_process_vm_lifecycle/founding.rs"]
mod founding;
#[path = "in_process_vm_lifecycle/jail.rs"]
mod jail;
#[path = "in_process_vm_lifecycle/joining.rs"]
mod joining;
#[path = "in_process_vm_lifecycle/leases.rs"]
mod leases;
#[path = "in_process_vm_lifecycle/operator.rs"]
mod operator;
#[path = "in_process_vm_lifecycle/upstream.rs"]
mod upstream;
