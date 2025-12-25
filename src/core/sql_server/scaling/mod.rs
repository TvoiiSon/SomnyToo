pub mod instance;
pub mod balancer;
pub mod auto_scaler;
pub mod node;

// Реэкспорты
pub use instance::ScalableDatabaseCluster;
pub use balancer::{ReadReplicaBalancer, LoadBalancingStrategy};
pub use auto_scaler::{AutoScaler, AutoScalingConfig, MetricsCollector};
pub use node::{DatabaseNode, NodeRole, NodeMetrics};