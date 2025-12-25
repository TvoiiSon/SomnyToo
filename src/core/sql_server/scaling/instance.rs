use std::sync::Arc;
use sqlx::{Error};

use super::super::security::node::DatabaseNode;
use super::super::scaling::balancer::ReadReplicaBalancer;
use super::super::scaling::auto_scaler::{AutoScaler, AutoScalingConfig};

#[derive(Clone)]
pub struct ScalableDatabaseCluster {
    _primary_node: DatabaseNode,
    _replica_nodes: Vec<DatabaseNode>,
    _read_replica_balancer: Arc<ReadReplicaBalancer>,
    _auto_scaler: Arc<AutoScaler>,
}

impl ScalableDatabaseCluster {
    pub async fn new(
        _primary_url: String,
        _replica_urls: Vec<String>,
        _scaling_config: AutoScalingConfig,
    ) -> Result<Self, Error> {
        // Реализация здесь
        unimplemented!()
    }
}