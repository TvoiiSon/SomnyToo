use std::sync::Arc;
use std::time::{Instant, Duration};
use std::collections::{HashMap, VecDeque};
use tokio::sync::{mpsc, RwLock, Mutex, broadcast};
use bytes::Bytes;
use tracing::{info, error, debug, warn};
use dashmap::DashMap;

use crate::core::protocol::batch_system::config::{BatchConfig, ConfigOptimizationModel};
use crate::core::protocol::batch_system::types::error::{BatchError};
use crate::core::protocol::batch_system::types::priority::{Priority};
use crate::core::protocol::batch_system::types::packet_types::{
    PacketType, is_packet_supported, get_packet_priority
};
use crate::core::protocol::batch_system::optimized::work_stealing_dispatcher::{
    WorkStealingDispatcher, WorkStealingTask, WorkStealingResult, DispatcherAdvancedStats, WorkStealingModel
};
use crate::core::protocol::batch_system::optimized::buffer_pool::{OptimizedBufferPool, SizeClass, PooledBuffer};
use crate::core::protocol::batch_system::optimized::crypto_processor::OptimizedCryptoProcessor;
use crate::core::protocol::batch_system::optimized::factory::OptimizedFactory;
use crate::core::protocol::batch_system::acceleration_batch::chacha20_batch_accel::ChaCha20BatchAccelerator;
use crate::core::protocol::batch_system::acceleration_batch::blake3_batch_accel::Blake3BatchAccelerator;
use crate::core::protocol::batch_system::circuit_breaker::{
    CircuitBreakerManager, CircuitBreakerStats, CircuitState
};
use crate::core::protocol::batch_system::qos_manager::{
    QosManager, QosStatistics,
};
use crate::core::protocol::batch_system::adaptive_batcher::{
    AdaptiveBatcher, AdaptiveBatcherConfig, PIDController
};
use crate::core::protocol::batch_system::metrics_tracing::{
    MetricsTracingSystem, MetricsConfig, AnomalySeverity
};
use crate::core::protocol::batch_system::core::reader::{BatchReader, ReaderEvent};
use crate::core::protocol::batch_system::core::writer::{BatchWriter, BackpressureModel};
use crate::core::protocol::phantom_crypto::core::instance::PhantomCrypto;
use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::protocol::packets::packet_service::PhantomPacketService;
use crate::core::protocol::phantom_crypto::packet::PhantomPacketProcessor;
use crate::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;

#[derive(Debug, Clone)]
pub struct SystemStateModel {
    pub lambda: f64,
    pub mu: f64,
    pub m: usize,
    pub b: usize,
    pub rho: f64,
    pub latency_ms: f64,
    pub target_latency_ms: f64,
    pub queue_length: usize,
    pub reader_throughput: f64,
    pub writer_throughput: f64,
    pub crypto_throughput: f64,
    pub dispatcher_throughput: f64,
    pub circuit_state: CircuitState,
    pub buffer_hit_rate: f64,
    pub qos_utilization: f64,
    pub predicted_lambda_1min: f64,
    pub predicted_lambda_5min: f64,
    pub predicted_lambda_15min: f64,
    pub last_update: Instant,
    pub last_adaptation: Instant,
}

impl SystemStateModel {
    pub fn new() -> Self {
        Self {
            lambda: 0.0,
            mu: 1000.0,
            m: num_cpus::get(),
            b: 256,
            rho: 0.0,
            latency_ms: 0.0,
            target_latency_ms: 50.0,
            queue_length: 0,
            reader_throughput: 0.0,
            writer_throughput: 0.0,
            crypto_throughput: 0.0,
            dispatcher_throughput: 0.0,
            circuit_state: CircuitState::Closed,
            buffer_hit_rate: 0.0,
            qos_utilization: 0.0,
            predicted_lambda_1min: 0.0,
            predicted_lambda_5min: 0.0,
            predicted_lambda_15min: 0.0,
            last_update: Instant::now(),
            last_adaptation: Instant::now(),
        }
    }

    pub fn calculate_utilization(&mut self) {
        if self.m == 0 || self.mu <= 0.0 {
            self.rho = 0.0;
        } else {
            self.rho = (self.lambda / (self.mu * self.m as f64)).clamp(0.0, 0.99);
        }
    }

    pub fn little_law(&self) -> f64 {
        self.lambda * self.latency_ms / 1000.0
    }

    pub fn batch_formation_time_ms(&self) -> f64 {
        if self.lambda < 0.1 {
            1000.0
        } else {
            self.b as f64 / self.lambda * 1000.0
        }
    }

    pub fn update(&mut self,
                  lambda: f64,
                  latency_ms: f64,
                  queue_len: usize,
                  workers: usize,
                  batch_size: usize,
                  circuit_state: CircuitState) {
        self.lambda = lambda;
        self.latency_ms = latency_ms;
        self.queue_length = queue_len;
        self.m = workers;
        self.b = batch_size;
        self.circuit_state = circuit_state;
        self.calculate_utilization();
        self.last_update = Instant::now();
    }
}

pub struct AdaptiveSystemController {
    state: Arc<RwLock<SystemStateModel>>,
    config: Arc<RwLock<BatchConfig>>,
    adaptive_batcher: Arc<AdaptiveBatcher>,
    dispatcher: Arc<WorkStealingDispatcher>,
    qos_manager: Arc<QosManager>,
    circuit_breaker_manager: Arc<CircuitBreakerManager>,
    buffer_pool: Arc<OptimizedBufferPool>,
    crypto_processor: Arc<OptimizedCryptoProcessor>,
    reader: Arc<BatchReader>,
    writer: Arc<BatchWriter>,
    metrics: Arc<MetricsTracingSystem>,
    optimization_model: Arc<RwLock<ConfigOptimizationModel>>,
    pid_latency: Arc<RwLock<PIDController>>,
    pid_throughput: Arc<RwLock<PIDController>>,
    adaptation_history: Arc<RwLock<VecDeque<AdaptationRecord>>>,
    command_tx: broadcast::Sender<SystemCommand>,
    control_interval: Duration,
    is_running: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct AdaptationRecord {
    pub timestamp: Instant,
    pub parameter: String,
    pub old_value: f64,
    pub new_value: f64,
    pub reason: String,
    pub confidence: f64,
    pub latency_improvement: f64,
}

impl AdaptiveSystemController {
    pub fn new(
        config: BatchConfig,
        adaptive_batcher: Arc<AdaptiveBatcher>,
        dispatcher: Arc<WorkStealingDispatcher>,
        qos_manager: Arc<QosManager>,
        circuit_breaker_manager: Arc<CircuitBreakerManager>,
        buffer_pool: Arc<OptimizedBufferPool>,
        crypto_processor: Arc<OptimizedCryptoProcessor>,
        reader: Arc<BatchReader>,
        writer: Arc<BatchWriter>,
        metrics: Arc<MetricsTracingSystem>,
        command_tx: broadcast::Sender<SystemCommand>,
    ) -> Self {
        let optimization_model = Arc::new(RwLock::new(ConfigOptimizationModel::new()));
        let pid_latency = Arc::new(RwLock::new(PIDController::auto_tune(0.5, 10.0)));
        let pid_throughput = Arc::new(RwLock::new(PIDController::new(0.3, 0.1, 0.05)));

        Self {
            state: Arc::new(RwLock::new(SystemStateModel::new())),
            config: Arc::new(RwLock::new(config)),
            adaptive_batcher,
            dispatcher,
            qos_manager,
            circuit_breaker_manager,
            buffer_pool,
            crypto_processor,
            reader,
            writer,
            metrics,
            optimization_model,
            pid_latency,
            pid_throughput,
            adaptation_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))),
            command_tx,
            control_interval: Duration::from_secs(1),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    pub async fn start_control_loop(&self) {
        info!("🚀 Запуск главного цикла адаптивного управления");
        let mut interval = tokio::time::interval(self.control_interval);

        while self.is_running.load(std::sync::atomic::Ordering::Relaxed) {
            interval.tick().await;

            self.collect_system_state().await;
            self.detect_critical_conditions().await;

            tokio::join!(
                self.optimize_batch_size(),
                self.optimize_worker_count(),
                self.optimize_qos_quotas(),
                self.optimize_buffer_pool(),
                self.optimize_circuit_breaker(),
                self.optimize_reader_writer(),
                self.optimize_crypto_processing(),
            );

            self.apply_decisions().await;
            self.log_system_health().await;
        }

        info!("🛑 Главный цикл управления остановлен");
    }

    async fn collect_system_state(&self) {
        let dispatcher_stats = self.dispatcher.get_advanced_stats().await;
        let reader_stats = self.reader.get_stats().await;
        let writer_stats = self.writer.get_stats().await;
        let crypto_stats = self.crypto_processor.get_stats();
        let buffer_stats = self.buffer_pool.get_detailed_stats();
        let qos_stats = self.qos_manager.get_statistics().await;

        let circuit_state = if let Some(cb) = self.circuit_breaker_manager.get_breaker("dispatcher").await {
            cb.get_state().await
        } else {
            CircuitState::Closed
        };

        let batch_size = *self.adaptive_batcher.current_batch_size.read().await;
        let lambda = self.adaptive_batcher.kalman.read().await.state;
        let latency_ms = self.metrics
            .get_aggregated_metric("latency.p95")
            .map(|m| m.percentile(95.0))
            .unwrap_or(50.0);

        let queue_len = self.dispatcher
            .worker_queues
            .iter()
            .map(|e| *e.value())
            .sum::<usize>();

        let wavelet_pred = self.adaptive_batcher.wavelet.read().await.predict(900);
        let lambda_1min = wavelet_pred.get(60).copied().unwrap_or(lambda);
        let lambda_5min = wavelet_pred.get(300).copied().unwrap_or(lambda);
        let lambda_15min = wavelet_pred.get(900).copied().unwrap_or(lambda);

        {
            let mut state = self.state.write().await;
            state.update(
                lambda,
                latency_ms,
                queue_len,
                dispatcher_stats.total_workers,
                batch_size,
                circuit_state,
            );

            state.reader_throughput = reader_stats.read_rate;
            state.writer_throughput = writer_stats.write_rate;
            state.crypto_throughput = *crypto_stats.get("throughput").unwrap_or(&0) as f64 / 1000.0;
            state.dispatcher_throughput = dispatcher_stats.total_tasks_processed as f64 / 60.0;
            state.buffer_hit_rate = buffer_stats
                .get("Global")
                .map(|s| s.hit_rate)
                .unwrap_or(0.0);
            state.qos_utilization = qos_stats.total_utilization;
            state.predicted_lambda_1min = lambda_1min;
            state.predicted_lambda_5min = lambda_5min;
            state.predicted_lambda_15min = lambda_15min;
        }

        self.metrics.record_metric("system.lambda", lambda).await;
        self.metrics.record_metric("system.rho", self.state.read().await.rho).await;
        self.metrics.record_metric("system.latency_ms", latency_ms).await;
        self.metrics.record_metric("system.queue_length", queue_len as f64).await;
    }

    async fn detect_critical_conditions(&self) {
        let state = self.state.read().await;
        let anomalies = self.metrics.get_anomalies(10).await;

        for anomaly in anomalies {
            // ИСПРАВЛЕНО: НЕ ЛОГИРУЕМ АНОМАЛИИ ЗДЕСЬ!
            // metrics_tracing уже залогировала их

            // Только РЕАГИРУЕМ на аномалии
            match anomaly.severity {
                AnomalySeverity::Critical => {
                    if anomaly.metric_name.contains("latency") &&
                        state.latency_ms > state.target_latency_ms * 3.0 {
                        let _ = self.command_tx.send(SystemCommand::EmergencyShutdown {
                            reason: format!("Critical latency anomaly: {} ms", anomaly.value)
                        });
                    }
                }
                AnomalySeverity::Warning => {
                    if anomaly.metric_name.contains("buffer_hit_rate") && anomaly.value < 0.5 {
                        let _ = self.buffer_pool.force_cleanup().await;
                    }

                    if anomaly.metric_name.contains("qos_rejection") && anomaly.value > 0.1 {
                        let _ = self.qos_manager.adapt_quotas().await;
                    }
                }
                _ => {}
            }
        }
    }

    async fn optimize_batch_size(&self) {
        let state = self.state.read().await;

        // ИСПРАВЛЕНО: проверяем, что есть реальные данные
        let total_packets = self.adaptive_batcher.lambdas.read().await.len();
        if total_packets < 10 {  // ждём минимум 10 измерений
            debug!("⏸️ Batch optimization skipped - insufficient data ({} packets)", total_packets);
            return;
        }

        // ИСПРАВЛЕНО: также проверяем, что λ не дефолтное
        let is_default_lambda = (state.lambda - 100.0).abs() < 1.0 && total_packets < 100;
        if is_default_lambda {
            debug!("⏸️ Batch optimization skipped - using default lambda ({:.1})", state.lambda);
            return;
        }

        // ИСПРАВЛЕНО: не адаптируем без реальной нагрузки
        if state.lambda < 10.0 || state.lambda > 990.0 {  // игнорируем дефолтные значения
            debug!("⏸️ Batch optimization skipped - no real load (λ={:.1})", state.lambda);
            return;
        }

        if state.lambda < 0.1 {
            return;
        }

        let measured_throughput = self.metrics
            .get_aggregated_metric("batch.throughput")
            .map(|m| m.distribution.mean)
            .unwrap_or(100.0);

        let measured_latency = self.metrics
            .get_aggregated_metric("latency.p95")
            .map(|m| m.percentile(95.0))
            .unwrap_or(50.0);

        let config = self.config.read().await;
        let target_latency = config.optimization_model.target_latency.as_millis() as f64;

        drop(config);

        let lambda = {
            let mut kalman = self.adaptive_batcher.kalman.write().await;
            kalman.predict();
            kalman.update(measured_throughput.max(0.1))
        };

        let lambda_pred = {
            let wavelet = self.adaptive_batcher.wavelet.read().await;
            wavelet.predict(10)
        };

        let waiting_times = {
            let mut gps = self.adaptive_batcher.gps.write().await;
            gps.recompute_shares();
            let b_f64 = state.b as f64;
            let service_rate = 1000.0;
            let mut times = [0.0; 5];
            for i in 0..5 {
                times[i] = gps.waiting_time(i, lambda, b_f64, service_rate);
            }
            times
        };

        let waiting_time = waiting_times[2];

        let b_optimal = {
            let params = self.adaptive_batcher.model_params.read().await;
            let a = 2.0 * params.delta;
            let b = params.gamma - 2.0 * params.delta * params.b_opt + 1.0 / lambda.max(0.1);
            let c = 0.0;
            let d = -params.alpha;

            let roots = solve_cubic(a, b, c, d);
            let mut optimal = params.b_opt as usize;
            let mut min_latency = f64::INFINITY;

            let config = self.config.read().await;
            for &root in &roots {
                if root >= config.min_batch_size as f64 &&
                    root <= config.max_batch_size as f64 {
                    let b = root as usize;
                    let latency = b as f64 / lambda + params.processing_time(b) + waiting_time;
                    if latency < min_latency {
                        min_latency = latency;
                        optimal = b;
                    }
                }
            }
            drop(config);

            optimal.clamp(
                self.config.read().await.min_batch_size,
                self.config.read().await.max_batch_size
            )
        };

        let pid_correction = {
            let mut pid = self.pid_latency.write().await;
            let error = target_latency - measured_latency;
            pid.compute(error)
        };

        let (b_mpc, m_mpc) = {
            let mut mpc = self.adaptive_batcher.mpc.write().await;
            mpc.set_lambda_predictions(lambda_pred);
            let current_b = *self.adaptive_batcher.current_batch_size.read().await;
            let current_m = *self.adaptive_batcher.current_workers.read().await;
            mpc.solve(current_b, current_m)
        };

        let config = self.config.read().await;
        let b_final = (((b_optimal as f64 + pid_correction + b_mpc as f64) / 3.0)
            .round() as usize)
            .clamp(
                config.min_batch_size,
                config.max_batch_size
            );
        drop(config);

        {
            let mut current_b = self.adaptive_batcher.current_batch_size.write().await;
            if *current_b != b_final {
                let old = *current_b;
                *current_b = b_final;

                let mut config = self.config.write().await;
                config.batch_size = b_final;

                let record = AdaptationRecord {
                    timestamp: Instant::now(),
                    parameter: "batch_size".to_string(),
                    old_value: old as f64,
                    new_value: b_final as f64,
                    reason: format!("λ={:.1}, latency={:.1}ms, target={:.1}ms",
                                    lambda, measured_latency, target_latency),
                    confidence: self.adaptive_batcher.model_params.read().await.confidence,
                    latency_improvement: target_latency - measured_latency,
                };

                self.adaptation_history.write().await.push_back(record);

                info!("🎯 Batch size optimized: {} → {} (λ={:.1}, correction={:.1})",
                      old, b_final, lambda, pid_correction);
            }
        }

        {
            let mut current_m = self.adaptive_batcher.current_workers.write().await;
            if *current_m != m_mpc {
                let old = *current_m;
                *current_m = m_mpc;
                if m_mpc > old {
                    let _ = self.command_tx.send(SystemCommand::ScaleUp { count: m_mpc - old });
                } else {
                    let _ = self.command_tx.send(SystemCommand::ScaleDown { count: old - m_mpc });
                }
            }
        }

        self.metrics.record_metric("batch.optimal", b_final as f64).await;
        self.metrics.record_metric("batch.lambda", lambda).await;
        self.metrics.record_metric("pid.correction", pid_correction).await;
    }

    async fn optimize_worker_count(&self) {
        let state = self.state.read().await;

        let total_packets = self.adaptive_batcher.lambdas.read().await.len();
        if total_packets < 50 {
            debug!("⏸️ Worker scaling skipped - insufficient data ({} packets)", total_packets);
            return;
        }

        if (state.lambda - 100.0).abs() < 1.0 {
            debug!("⏸️ Worker scaling skipped - using default lambda");
            return;
        }

        if state.lambda < 1.0 {
            return;
        }

        let current_workers = self.dispatcher.worker_senders.len();
        let mu = 1000.0;
        let target_wait_time = 10.0;

        let optimal = optimal_worker_count(state.lambda, mu, target_wait_time);

        let config = self.config.read().await;
        let min_workers = 4;
        let max_workers = 256;
        drop(config);

        if optimal != current_workers && optimal >= min_workers && optimal <= max_workers {
            if optimal > current_workers {
                let increase = optimal - current_workers;
                info!("📈 Scaling UP workers: {} → {} (+{})", current_workers, optimal, increase);
                let _ = self.command_tx.send(SystemCommand::ScaleUp { count: increase });

                self.metrics.record_metric("scaling.up", 1.0).await;
                self.metrics.record_metric("scaling.workers", optimal as f64).await;
            } else if optimal < current_workers {
                let decrease = current_workers - optimal;
                info!("📉 Scaling DOWN workers: {} → {} (-{})", current_workers, optimal, decrease);
                let _ = self.command_tx.send(SystemCommand::ScaleDown { count: decrease });

                self.metrics.record_metric("scaling.down", 1.0).await;
                self.metrics.record_metric("scaling.workers", optimal as f64).await;
            }

            let record = AdaptationRecord {
                timestamp: Instant::now(),
                parameter: "worker_count".to_string(),
                old_value: current_workers as f64,
                new_value: optimal as f64,
                reason: format!("λ={:.1}, ρ={:.2}", state.lambda, state.rho),
                confidence: 0.8,
                latency_improvement: 0.0,
            };

            self.adaptation_history.write().await.push_back(record);
        }

        let mut stealing_model = self.dispatcher.stealing_model.write().await;
        let queue_lens: Vec<f64> = self.dispatcher.worker_queues
            .iter()
            .map(|e| *e.value() as f64)
            .collect();

        let imbalance = WorkStealingModel::compute_imbalance(&queue_lens);

        if imbalance > 0.3 {
            stealing_model.p_steal = (stealing_model.p_steal + 0.05).min(0.5);
            stealing_model.imbalance = imbalance;

            self.metrics.record_metric("stealing.probability", stealing_model.p_steal).await;
            self.metrics.record_metric("stealing.imbalance", imbalance).await;

            debug!("⚖️ Increasing steal probability to {:.2} (imbalance={:.2})",
                   stealing_model.p_steal, imbalance);
        } else if imbalance < 0.1 && stealing_model.p_steal > 0.1 {
            stealing_model.p_steal = (stealing_model.p_steal - 0.02).max(0.05);
        }
    }

    async fn optimize_qos_quotas(&self) {
        let state = self.state.read().await;

        if state.lambda < 10.0 || state.lambda > 990.0 {
            return;
        }

        let qos_stats = self.qos_manager.get_statistics().await;

        let total_requests = qos_stats.high_priority_requests +
            qos_stats.normal_priority_requests +
            qos_stats.low_priority_requests;

        if total_requests < 1000 {
            debug!("⏸️ QoS adaptation skipped - insufficient requests ({} total)", total_requests);
            return;
        }

        let total_requests = qos_stats.high_priority_requests +
            qos_stats.normal_priority_requests +
            qos_stats.low_priority_requests;

        if total_requests < 1000 {
            return;
        }

        let high_rejection = if qos_stats.high_priority_requests > 0 {
            qos_stats.high_priority_rejected as f64 / qos_stats.high_priority_requests as f64
        } else { 0.0 };

        let normal_rejection = if qos_stats.normal_priority_requests > 0 {
            qos_stats.normal_priority_rejected as f64 / qos_stats.normal_priority_requests as f64
        } else { 0.0 };

        let low_rejection = if qos_stats.low_priority_requests > 0 {
            qos_stats.low_priority_rejected as f64 / qos_stats.low_priority_requests as f64
        } else { 0.0 };

        let high_wait = qos_stats.high_priority_avg_wait_ms;
        let normal_wait = qos_stats.normal_priority_avg_wait_ms;
        let low_wait = qos_stats.low_priority_avg_wait_ms;

        let mut need_adaptation = false;
        let mut _reason = String::new();

        if high_rejection > 0.05 {
            need_adaptation = true;
            _reason = format!("High rejection {:.1}%", high_rejection * 100.0);
        } else if normal_rejection > 0.1 {
            need_adaptation = true;
            _reason = format!("Normal rejection {:.1}%", normal_rejection * 100.0);
        } else if low_rejection > 0.2 {
            need_adaptation = true;
            _reason = format!("Low rejection {:.1}%", low_rejection * 100.0);
        } else if high_wait > 100.0 {
            need_adaptation = true;
            _reason = format!("High wait {:.1}ms", high_wait);
        } else if normal_wait > 200.0 {
            need_adaptation = true;
            _reason = format!("Normal wait {:.1}ms", normal_wait);
        } else if low_wait > 500.0 {
            need_adaptation = true;
            _reason = format!("Low wait {:.1}ms", low_wait);
        }

        if need_adaptation {
            match self.qos_manager.adapt_quotas().await {
                Ok(decision) => {
                    let record = AdaptationRecord {
                        timestamp: Instant::now(),
                        parameter: "qos_quotas".to_string(),
                        old_value: decision.from_high,
                        new_value: decision.to_high,
                        reason: decision.reason,
                        confidence: decision.confidence,
                        latency_improvement: decision.predicted_improvement,
                    };

                    self.adaptation_history.write().await.push_back(record);

                    let arrival_rates = [state.lambda * 0.2, state.lambda * 0.3,
                        state.lambda * 0.3, state.lambda * 0.15, state.lambda * 0.05];
                    self.qos_manager.update_models(arrival_rates, state.b as f64).await;
                }
                Err(e) => {
                    debug!("QoS adaptation not needed: {}", e);
                }
            }
        }

        let gps_model = self.qos_manager.gps_model.read().await;
        self.metrics.record_metric("qos.gps_utilization", gps_model.total_utilization).await;
        self.metrics.record_metric("qos.high_share", gps_model.shares[0]).await;
        self.metrics.record_metric("qos.normal_share", gps_model.shares[2]).await;
        self.metrics.record_metric("qos.low_share", gps_model.shares[3]).await;
    }

    async fn optimize_buffer_pool(&self) {
        let stats = self.buffer_pool.get_detailed_stats();
        let global_stats = match stats.get("Global") {
            Some(s) => s,
            None => return,
        };

        if global_stats.hit_rate == 0.0 {
            debug!("📊 Buffer pool: no hits yet (cold start)");
            return;
        }

        if global_stats.hit_rate < 0.3 {
            warn!("📉 Buffer pool hit rate low: {:.1}% (allocations: {}, reuses: {})",
              global_stats.hit_rate * 100.0,
              global_stats.allocations,
              global_stats.reuses);
            self.increase_buffer_pool().await;
        }
    }

    async fn increase_buffer_pool(&self) {
        let config = self.config.read().await;
        let mut pools = self.buffer_pool.size_class_pools.write();
        let target_size = (config.max_queue_size as f64 * 0.2) as usize;
        drop(config);

        for (i, class) in SizeClass::all_classes().iter().enumerate() {
            let current_size = pools[i].len();
            if current_size < target_size {
                let to_add = (target_size - current_size).min(20);
                for _ in 0..to_add {
                    pools[i].push_back(PooledBuffer::new(*class));
                }
                debug!("📈 Increased {} pool from {} to {}",
                       class.name(), current_size, current_size + to_add);
            }
        }
    }

    async fn optimize_circuit_breaker(&self) {
        for name in ["dispatcher", "crypto_processor", "packet_service"] {
            if let Some(cb) = self.circuit_breaker_manager.get_breaker(name).await {
                let stats = cb.get_stats().await;

                if stats.state == CircuitState::Closed {
                    let mut markov = cb.markov_model.write().await;
                    markov.transition_rates[0][1] = stats.failure_rate * 0.01;
                    markov.compute_steady_state();

                    if markov.availability < 0.95 {
                        warn!("⚠️ {} availability low: {:.2}%, adjusting thresholds",
                              name, markov.availability * 100.0);
                    }
                }

                if stats.state == CircuitState::Open {
                    let elapsed = stats.last_failure
                        .map(|f| f.elapsed())
                        .unwrap_or(Duration::from_secs(0));

                    let recovery = cb.recovery_model.write().await;
                    let should_retry = elapsed >= recovery.current_delay;

                    if should_retry {
                        info!("🔄 Attempting recovery for {}", name);
                        let _ = cb.allow_request().await;
                    }
                }

                self.metrics.record_metric(&format!("circuit.{}.availability", name), stats.availability).await;
                self.metrics.record_metric(&format!("circuit.{}.mttf", name), stats.mttf).await;
                self.metrics.record_metric(&format!("circuit.{}.mttr", name), stats.mttr).await;
            }
        }
    }

    async fn optimize_reader_writer(&self) {
        let reader_stats = self.reader.get_stats().await;
        let writer_stats = self.writer.get_stats().await;
        let state = self.state.read().await;
        let mut config = self.config.write().await;

        if config.adaptive_read_timeout {
            let optimal_timeout = config.optimization_model.optimal_read_timeout(
                state.b,
                reader_stats.p95_read_time.as_secs_f64() / 3.0
            );

            if (optimal_timeout.as_secs_f64() - config.read_timeout.as_secs_f64()).abs() > 0.1 {
                let old = config.read_timeout;
                config.read_timeout = optimal_timeout;

                let record = AdaptationRecord {
                    timestamp: Instant::now(),
                    parameter: "read_timeout".to_string(),
                    old_value: old.as_millis() as f64,
                    new_value: optimal_timeout.as_millis() as f64,
                    reason: format!("P95={:.1}ms", reader_stats.p95_read_time.as_millis()),
                    confidence: 0.7,
                    latency_improvement: 0.0,
                };

                self.adaptation_history.write().await.push_back(record);
                debug!("📖 Read timeout adapted: {:?} → {:?}", old, optimal_timeout);
            }
        }

        let optimal_flush = config.optimization_model.optimal_flush_interval();
        if optimal_flush != config.flush_interval {
            let old = config.flush_interval;
            config.flush_interval = optimal_flush;

            let record = AdaptationRecord {
                timestamp: Instant::now(),
                parameter: "flush_interval".to_string(),
                old_value: old.as_millis() as f64,
                new_value: optimal_flush.as_millis() as f64,
                reason: format!("Target latency {:.0}ms", state.target_latency_ms),
                confidence: 0.8,
                latency_improvement: 0.0,
            };

            self.adaptation_history.write().await.push_back(record);
        }

        let optimal_batch = config.optimization_model.optimal_batch_size(0.5, 0.001);
        if optimal_batch != config.batch_size && config.enable_adaptive_batching {
            let old = config.batch_size;
            config.batch_size = optimal_batch;

            let record = AdaptationRecord {
                timestamp: Instant::now(),
                parameter: "writer_batch_size".to_string(),
                old_value: old as f64,
                new_value: optimal_batch as f64,
                reason: format!("Throughput {:.0} pps", writer_stats.write_rate),
                confidence: 0.75,
                latency_improvement: 0.0,
            };

            self.adaptation_history.write().await.push_back(record);
        }

        let backpressure = {
            let mut bp = BackpressureModel::new(config.max_pending_writes);
            bp.update(
                writer_stats.queue_size,
                writer_stats.write_rate,
                1000.0
            );
            bp
        };

        self.metrics.record_metric("writer.backpressure", backpressure.loss_probability).await;
        self.metrics.record_metric("writer.loss_prob", backpressure.loss_probability).await;

        if backpressure.is_critical() {
            warn!("⚠️ Writer backpressure critical: queue={}/{}",
                  writer_stats.queue_size, config.max_pending_writes);

            if let Some(cb) = self.circuit_breaker_manager.get_breaker("writer").await {
                cb.record_failure().await;
            }
        }
    }

    async fn optimize_crypto_processing(&self) {
        let crypto_stats = self.crypto_processor.get_stats();
        let state = self.state.read().await;

        let processed = crypto_stats.get("total_tasks_processed").copied().unwrap_or(0);
        let encrypted = crypto_stats.get("total_encryptions").copied().unwrap_or(0);
        let decrypted = crypto_stats.get("total_decryptions").copied().unwrap_or(0);
        let hashes = crypto_stats.get("total_hashes").copied().unwrap_or(0);
        let steals = crypto_stats.get("crypto_steals").copied().unwrap_or(0);

        let success_rate = if processed > 0 {
            (encrypted + decrypted + hashes) as f64 / processed as f64
        } else { 1.0 };

        self.metrics.record_metric("crypto.success_rate", success_rate).await;
        self.metrics.record_metric("crypto.steal_efficiency", steals as f64 / processed.max(1) as f64).await;

        if success_rate < 0.95 {
            warn!("⚠️ Crypto success rate low: {:.1}%", success_rate * 100.0);

            if let Some(cb) = self.circuit_breaker_manager.get_breaker("crypto_processor").await {
                if cb.get_state().await == CircuitState::Closed {
                    cb.record_failure().await;
                }
            }
        }

        if let Ok(mut model) = self.crypto_processor.performance_model.try_write() {
            let avg_size = state.b;
            model.simd_optimal = (avg_size as f64 * 1.5) as usize;
        }

        let chacha_info = self.crypto_processor.chacha20_accelerator.get_simd_info();
        let blake3_info = self.crypto_processor.blake3_accelerator.get_performance_info();

        self.metrics.record_metric("crypto.simd_efficiency", chacha_info.efficiency).await;
        self.metrics.record_metric("crypto.blake3_throughput", blake3_info.estimated_throughput).await;
    }

    async fn apply_decisions(&self) {
        let state = self.state.read().await;
        let now = Instant::now();

        if now.duration_since(state.last_adaptation) < Duration::from_secs(5) {
            return;
        }

        self.metrics.record_metric("system.adaptation_interval",
                                   now.duration_since(state.last_adaptation).as_secs_f64()).await;

        drop(state);
        self.state.write().await.last_adaptation = now;
    }

    async fn log_system_health(&self) {
        let state = self.state.read().await;
        let health = self.metrics.get_system_health().await;

        info!("📊 SYSTEM HEALTH: {:.1}% | λ={:.1} ρ={:.2} L={:.1}ms B={} M={}",
              health * 100.0, state.lambda, state.rho, state.latency_ms, state.b, state.m);

        self.metrics.record_metric("system.health", health).await;
    }

    pub async fn get_adaptation_history(&self, limit: usize) -> Vec<AdaptationRecord> {
        let history = self.adaptation_history.read().await;
        history.iter().rev().take(limit).cloned().collect()
    }

    pub async fn stop(&self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

pub struct IntegratedBatchSystem {
    config: BatchConfig,
    reader: Arc<BatchReader>,
    writer: Arc<BatchWriter>,
    work_stealing_dispatcher: Arc<WorkStealingDispatcher>,
    crypto_processor: Arc<OptimizedCryptoProcessor>,
    buffer_pool: Arc<OptimizedBufferPool>,
    chacha20_accelerator: Arc<ChaCha20BatchAccelerator>,
    blake3_accelerator: Arc<Blake3BatchAccelerator>,
    circuit_breaker_manager: Arc<CircuitBreakerManager>,
    qos_manager: Arc<QosManager>,
    adaptive_batcher: Arc<AdaptiveBatcher>,
    metrics_tracing: Arc<MetricsTracingSystem>,
    packet_service: Arc<PhantomPacketService>,
    packet_processor: PhantomPacketProcessor,
    session_manager: Arc<PhantomSessionManager>,
    crypto: Arc<PhantomCrypto>,
    controller: Arc<AdaptiveSystemController>,
    event_tx: mpsc::Sender<SystemEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<SystemEvent>>>,
    command_tx: broadcast::Sender<SystemCommand>,
    command_rx: Arc<Mutex<broadcast::Receiver<SystemCommand>>>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
    is_initialized: Arc<std::sync::atomic::AtomicBool>,
    startup_time: Instant,
    stats: Arc<RwLock<SystemStatistics>>,
    metrics: Arc<DashMap<String, MetricValue>>,
    pending_batches: Arc<RwLock<Vec<PendingBatch>>>,
    active_connections: Arc<RwLock<HashMap<std::net::SocketAddr, ConnectionInfo>>>,
    session_cache: Arc<RwLock<HashMap<Vec<u8>, SessionCacheEntry>>>,
    scaling_settings: Arc<RwLock<ScalingSettings>>,
    performance_counters: Arc<DashMap<String, PerformanceCounter>>,
    worker_pool: Arc<WorkerPool>,
    scaling_lock: Arc<Mutex<()>>,
    pending_encryptions: Arc<Mutex<Vec<(Vec<u8>, [u8; 32], [u8; 12], Bytes)>>>,
    pending_decryptions: Arc<Mutex<Vec<(Vec<u8>, [u8; 32], [u8; 12], Bytes)>>>,
    pending_hashes: Arc<Mutex<Vec<(Vec<u8>, [u8; 32], Bytes)>>>,
}

impl IntegratedBatchSystem {
    pub async fn new(
        config: BatchConfig,
        session_manager: Arc<PhantomSessionManager>,
        crypto: Arc<PhantomCrypto>,
        heartbeat_manager: Arc<ConnectionHeartbeatManager>,
    ) -> Result<Self, BatchError> {
        let startup_time = Instant::now();

        info!("🚀 ЗАПУСК ИНТЕГРИРОВАННОЙ BATCH-СИСТЕМЫ v2.0 (МАТЕМАТИЧЕСКАЯ)");
        info!("📊 [1/14] Инициализация Metrics & Tracing...");

        let metrics_config = MetricsConfig {
            enabled: config.metrics_enabled,
            collection_interval: config.metrics_collection_interval,
            trace_sampling_rate: config.trace_sampling_rate,
            service_name: "batch-system".to_string(),
            service_version: "2.0.0-math".to_string(),
            environment: "production".to_string(),
            retention_period: Duration::from_secs(3600),
            max_metrics_per_service: 1000,
            enable_histograms: true,
            enable_time_series: true,
            enable_anomaly_detection: true,
            anomaly_threshold: 3.0,
        };

        let metrics_tracing = Arc::new(
            MetricsTracingSystem::new(metrics_config)
                .map_err(|e| BatchError::ProcessingError(format!("Metrics init failed: {}", e)))?
        );

        info!("🛡️ [2/14] Инициализация Circuit Breaker Manager...");

        let circuit_breaker_manager = Arc::new(
            CircuitBreakerManager::new(Arc::new(config.clone()))
        );

        let dispatcher_circuit_breaker = circuit_breaker_manager.get_or_create("dispatcher");
        let _packet_service_circuit_breaker = circuit_breaker_manager.get_or_create("packet_service");
        let _crypto_circuit_breaker = circuit_breaker_manager.get_or_create("crypto_processor");

        info!("⚖️ [3/14] Инициализация QoS Manager...");

        let qos_manager = Arc::new(
            QosManager::new(
                config.high_priority_quota,
                config.normal_priority_quota,
                config.low_priority_quota,
                config.max_queue_size * 10,
            )
        );

        info!("🔄 [4/14] Инициализация Mathematical Adaptive Batcher...");

        let adaptive_batcher_config = AdaptiveBatcherConfig {
            min_batch_size: config.min_batch_size,
            max_batch_size: config.max_batch_size,
            initial_batch_size: config.batch_size,
            target_latency: Duration::from_millis(50),
            adaptation_interval: Duration::from_secs(1),
        };

        let adaptive_batcher = Arc::new(
            AdaptiveBatcher::new(adaptive_batcher_config)
        );

        info!("🧮 [5/14] Инициализация математических моделей...");

        let _state_model = Arc::new(RwLock::new(SystemStateModel::new()));
        let _resource_scheduler = Arc::new(RwLock::new(ResourceScheduler::new()));

        info!("📬 [6/14] Инициализация каналов событий...");

        let (system_event_tx, system_event_rx) = mpsc::channel(50000);
        let (command_tx, command_rx) = broadcast::channel(1000);
        let (reader_event_tx, reader_event_rx) = mpsc::channel(50000);

        info!("🔧 [7/14] Инициализация оптимизированных компонентов...");

        let factory = OptimizedFactory::new();

        let buffer_pool = factory.create_buffer_pool(
            config.read_buffer_size,
            config.write_buffer_size,
            64 * 1024,
            500,
        );

        let crypto_processor = factory.create_crypto_processor(
            config.worker_count * 2
        );

        info!("🚀 [8/14] Инициализация SIMD акселераторов...");

        let chacha20_accelerator = Arc::new(
            ChaCha20BatchAccelerator::new(config.simd_batch_size)
        );
        let blake3_accelerator = Arc::new(
            Blake3BatchAccelerator::new(config.simd_batch_size)
        );

        info!("🌐 [9/14] Инициализация внешних сервисов...");

        let packet_service = Arc::new(PhantomPacketService::new(
            session_manager.clone(),
            heartbeat_manager,
        ));

        let packet_processor = PhantomPacketProcessor::new();

        info!("📖 [10/14] Инициализация Reader/Writer...");

        let reader = Arc::new(BatchReader::new(config.clone(), reader_event_tx.clone()));
        let writer = Arc::new(BatchWriter::new(config.clone()));

        info!("⚡ [11/14] Инициализация Mathematical WorkStealingDispatcher...");

        let work_stealing_dispatcher = factory.create_dispatcher(
            config.worker_count,
            config.max_queue_size * 10,
            session_manager.clone(),
            adaptive_batcher.clone(),
            qos_manager.clone(),
            dispatcher_circuit_breaker,
        );

        info!("🏭 [12/14] Инициализация Worker Pool...");

        let min_workers = 4;
        let max_workers = 256;

        let worker_pool = Arc::new(WorkerPool::new(
            min_workers,
            max_workers,
        ));

        info!("🎮 [13/14] Инициализация Adaptive System Controller...");

        let controller = Arc::new(AdaptiveSystemController::new(
            config.clone(),
            adaptive_batcher.clone(),
            work_stealing_dispatcher.clone(),
            qos_manager.clone(),
            circuit_breaker_manager.clone(),
            buffer_pool.clone(),
            crypto_processor.clone(),
            reader.clone(),
            writer.clone(),
            metrics_tracing.clone(),
            command_tx.clone(),
        ));

        info!("🏗️ [14/14] Финальная сборка системы...");

        let system = Self {
            config: config.clone(),
            reader,
            writer,
            work_stealing_dispatcher,
            crypto_processor,
            buffer_pool,
            chacha20_accelerator,
            blake3_accelerator,
            circuit_breaker_manager,
            qos_manager,
            adaptive_batcher,
            metrics_tracing,
            packet_service,
            packet_processor,
            session_manager: session_manager.clone(),
            crypto: crypto.clone(),
            controller,
            event_tx: system_event_tx.clone(),
            event_rx: Arc::new(Mutex::new(system_event_rx)),
            command_tx: command_tx.clone(),
            command_rx: Arc::new(Mutex::new(command_rx)),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            is_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            startup_time,
            stats: Arc::new(RwLock::new(SystemStatistics {
                startup_time,
                ..Default::default()
            })),
            metrics: Arc::new(DashMap::new()),
            pending_batches: Arc::new(RwLock::new(Vec::with_capacity(1000))),
            active_connections: Arc::new(RwLock::new(HashMap::with_capacity(10000))),
            session_cache: Arc::new(RwLock::new(HashMap::with_capacity(10000))),
            scaling_settings: Arc::new(RwLock::new(ScalingSettings::default())),
            performance_counters: Arc::new(DashMap::new()),
            worker_pool,
            scaling_lock: Arc::new(Mutex::new(())),
            pending_encryptions: Arc::new(Mutex::new(Vec::with_capacity(256))),
            pending_decryptions: Arc::new(Mutex::new(Vec::with_capacity(256))),
            pending_hashes: Arc::new(Mutex::new(Vec::with_capacity(256))),
        };

        system.start_reader_event_converter(reader_event_rx).await;
        system.start_session_cache_cleaner().await;
        system.start_performance_counter_updater().await;
        system.initialize().await?;

        let controller_clone = system.controller.clone();
        tokio::spawn(async move {
            controller_clone.start_control_loop().await;
        });

        info!("✅ ИНТЕГРИРОВАННАЯ BATCH-СИСТЕМА УСПЕШНО ЗАПУЩЕНА");
        info!("   Workers: {}", config.worker_count);
        info!("   Initial batch size: {}", config.batch_size);
        info!("   Controller: ACTIVE (1s interval)");

        Ok(system)
    }

    async fn initialize(&self) -> Result<(), BatchError> {
        info!("🔄 Инициализация компонентов системы...");

        self.is_initialized.store(true, std::sync::atomic::Ordering::SeqCst);

        self.start_event_handlers().await;
        self.start_command_handlers().await;
        self.start_statistics_collector().await;
        self.start_batch_processor().await;
        self.start_performance_monitoring().await;
        self.start_auto_scaling().await;
        self.start_simd_batch_processor().await;
        self.start_mathematical_optimization().await;

        info!("✅ Все компоненты системы инициализированы");
        Ok(())
    }

    async fn start_reader_event_converter(&self, mut reader_event_rx: mpsc::Receiver<ReaderEvent>) {
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            debug!("🔄 Reader event converter started");

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                match reader_event_rx.recv().await {
                    Some(event) => {
                        let system_event = match event {
                            ReaderEvent::DataReady { session_id, data, source_addr, priority, received_at, size: _ } => {
                                SystemEvent::DataReceived {
                                    session_id,
                                    data: data.freeze(),
                                    source_addr,
                                    priority,
                                    timestamp: received_at,
                                }
                            }
                            ReaderEvent::ConnectionClosed { source_addr, reason } => {
                                SystemEvent::ConnectionClosed {
                                    addr: source_addr,
                                    session_id: Vec::new(),
                                    reason,
                                }
                            }
                            ReaderEvent::Error { source_addr: _, error } => {
                                SystemEvent::ErrorOccurred {
                                    error: error.to_string(),
                                    context: "reader_error".to_string(),
                                    severity: ErrorSeverity::High,
                                }
                            }
                        };

                        if let Err(e) = event_tx.send(system_event).await {
                            error!("❌ Failed to send converted event: {}", e);
                            break;
                        }
                    }
                    None => {
                        debug!("📭 Reader event channel closed");
                        break;
                    }
                }
            }

            debug!("👋 Reader event converter stopped");
        });
    }

    async fn start_session_cache_cleaner(&self) {
        let session_cache = self.session_cache.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                let mut cache = session_cache.write().await;
                let before = cache.len();
                let now = Instant::now();
                cache.retain(|_, entry| now.duration_since(entry.last_used) < Duration::from_secs(3600));
                let _removed = before - cache.len();
            }
        });
    }

    async fn start_performance_counter_updater(&self) {
        let perf_counters = self.performance_counters.clone();
        let is_running = self.is_running.clone();
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                let stats = system.stats.read().await;
                let uptime = stats.uptime.as_secs_f64().max(1.0);
                let throughput = stats.total_packets_processed as f64 / uptime;

                let mut throughput_counter = perf_counters
                    .entry("throughput".to_string())
                    .or_insert_with(|| PerformanceCounter::new("throughput".to_string(), 60));
                throughput_counter.update(throughput);

                let mut latency_counter = perf_counters
                    .entry("avg_latency_ms".to_string())
                    .or_insert_with(|| PerformanceCounter::new("avg_latency_ms".to_string(), 60));
                latency_counter.update(stats.avg_processing_time.as_millis() as f64);

                let current_batch_size = system.adaptive_batcher.get_batch_size().await;
                let mut batch_size_counter = perf_counters
                    .entry("avg_batch_size".to_string())
                    .or_insert_with(|| PerformanceCounter::new("avg_batch_size".to_string(), 60));
                batch_size_counter.update(current_batch_size as f64);
            }
        });
    }

    async fn start_event_handlers(&self) {
        let event_rx = self.event_rx.clone();
        let system = self.clone();

        tokio::spawn(async move {
            debug!("👂 Event handler started");
            let mut receiver = event_rx.lock().await;

            while let Some(event) = receiver.recv().await {
                system.handle_event(event).await;
            }

            debug!("👋 Event handler stopped");
        });
    }

    async fn handle_event(&self, event: SystemEvent) {
        match event {
            SystemEvent::DataReceived { session_id, data, source_addr, priority, timestamp } => {
                self.handle_data_received(session_id, data, source_addr, priority, timestamp).await;
            }
            SystemEvent::DataProcessed { session_id, result, processing_time, worker_id } => {
                self.handle_data_processed(session_id, result, processing_time, worker_id).await;
            }
            SystemEvent::ConnectionOpened { addr, session_id } => {
                self.handle_connection_opened(addr, session_id).await;
            }
            SystemEvent::ConnectionClosed { addr, session_id, reason } => {
                self.handle_connection_closed(addr, session_id, reason).await;
            }
            SystemEvent::BatchCompleted { batch_id, size, processing_time, success_rate } => {
                self.handle_batch_completed(batch_id, size, processing_time, success_rate).await;
            }
            SystemEvent::ErrorOccurred { error, context, severity } => {
                self.handle_error_occurred(error, context, severity).await;
            }
        }
    }

    async fn handle_data_received(
        &self,
        session_id: Vec<u8>,
        data: Bytes,
        source_addr: std::net::SocketAddr,
        priority: Priority,
        timestamp: Instant,
    ) {
        debug!("📥 Raw data received: {} bytes from {}", data.len(), source_addr);

        {
            let mut stats = self.stats.write().await;
            stats.total_data_received += data.len() as u64;
        }

        let packet_cb = self.circuit_breaker_manager.get_or_create("packet_service");

        if !packet_cb.allow_request().await {
            warn!("⚠️ Circuit breaker open for packet_service, rejecting request");
            packet_cb.record_failure().await;
            let event = SystemEvent::ErrorOccurred {
                error: "Circuit breaker open".to_string(),
                context: "packet_service".to_string(),
                severity: ErrorSeverity::High,
            };
            let _ = self.event_tx.send(event).await;
            return;
        }

        let task = WorkStealingTask {
            id: 0,
            session_id: session_id.clone(),
            data: data.clone(),
            source_addr,
            priority,
            created_at: timestamp,
            worker_id: None,
            retry_count: 0,
            deadline: Some(timestamp + Duration::from_secs(30)),
            size_bytes: data.len(),
            estimated_processing_time: 0.0,
        };

        match self.work_stealing_dispatcher.submit_task(task).await {
            Ok(task_id) => {
                debug!("✅ Task {} submitted to dispatcher", task_id);
                packet_cb.record_success().await;
                self.track_task_result(task_id, session_id, source_addr).await;
            }
            Err(e) => {
                error!("❌ Failed to submit task: {}", e);
                packet_cb.record_failure().await;
                self.record_metric("dispatcher.rejections", 1.0).await;
                let event = SystemEvent::ErrorOccurred {
                    error: e.to_string(),
                    context: "submit_task".to_string(),
                    severity: ErrorSeverity::High,
                };
                let _ = self.event_tx.send(event).await;
            }
        }
    }

    async fn track_task_result(
        &self,
        task_id: u64,
        session_id: Vec<u8>,
        source_addr: std::net::SocketAddr,
    ) {
        let dispatcher = self.work_stealing_dispatcher.clone();
        let event_tx = self.event_tx.clone();
        let system = self.clone();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                async {
                    let mut attempts = 0;
                    while attempts < 100 {
                        if let Some(task_result) = dispatcher.get_result(task_id) {
                            return Some(task_result);
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        attempts += 1;
                    }
                    None
                }
            ).await;

            match result {
                Ok(Some(task_result)) => {
                    debug!("✅ Task {} completed", task_id);

                    {
                        let mut stats = system.stats.write().await;
                        stats.work_stealing_count = dispatcher.get_stats()
                            .get("work_steals")
                            .copied()
                            .unwrap_or(0);
                    }

                    let process_result = ProcessResult {
                        success: task_result.result.is_ok(),
                        data: task_result.result.clone().ok().map(Bytes::from),
                        error: task_result.result.clone().err().map(|e| e.to_string()),
                        metadata: HashMap::from([
                            ("worker_id".to_string(), task_result.worker_id.to_string()),
                            ("processing_time".to_string(), format!("{:?}", task_result.processing_time)),
                            ("was_stolen".to_string(), task_result.was_stolen.to_string()),
                            ("destination_addr".to_string(), task_result.destination_addr.to_string()),
                        ]),
                    };

                    let event = SystemEvent::DataProcessed {
                        session_id: session_id.clone(),
                        result: process_result,
                        processing_time: task_result.processing_time,
                        worker_id: Some(task_result.worker_id),
                    };

                    let _ = event_tx.send(event).await;
                    system.process_task_result(task_result, session_id, source_addr).await;
                }
                Ok(None) => {
                    warn!("⚠️ Task {} result timeout", task_id);
                }
                Err(_) => {
                    error!("⏰ Task {} timeout", task_id);
                }
            }
        });
    }

    async fn process_task_result(
        &self,
        task_result: WorkStealingResult,
        session_id: Vec<u8>,
        source_addr: std::net::SocketAddr,
    ) {
        match task_result.result {
            Ok(data) => {
                if data.len() > 1 {
                    let packet_type = data[0];
                    let packet_data = &data[1..];

                    if !is_packet_supported(packet_type) {
                        debug!("⚠️ Unsupported packet type: 0x{:02x}", packet_type);
                        return;
                    }

                    let priority = get_packet_priority(packet_type).unwrap_or(Priority::Normal);
                    let requires_flush = PacketType::from_byte(packet_type)
                        .map(|pt| pt.requires_immediate_flush())
                        .unwrap_or(false);

                    let packet_cb = self.circuit_breaker_manager.get_or_create("packet_service");

                    if let Some(session) = self.session_manager.get_session(&session_id).await {
                        if !packet_cb.allow_request().await {
                            error!("❌ Circuit breaker open for packet_service, dropping packet");
                            packet_cb.record_failure().await;
                            let mut stats = self.stats.write().await;
                            stats.total_errors += 1;
                            return;
                        }

                        match self.packet_service.process_packet(
                            session.clone(),
                            packet_type,
                            packet_data.to_vec(),
                            source_addr,
                        ).await {
                            Ok(processing_result) => {
                                packet_cb.record_success().await;

                                match self.packet_processor.create_outgoing_vec(
                                    &session,
                                    processing_result.packet_type,
                                    &processing_result.response,
                                ) {
                                    Ok(encrypted_response) => {
                                        if let Err(e) = self.writer.write(
                                            source_addr,
                                            task_result.session_id.clone(),
                                            Bytes::from(encrypted_response),
                                            priority,
                                            requires_flush,
                                        ).await {
                                            error!("❌ Failed to send response: {}", e);
                                        } else {
                                            debug!("✅ Response sent for packet type 0x{:02x}", packet_type);
                                            let mut stats = self.stats.write().await;
                                            stats.total_packets_processed += 1;
                                            stats.total_data_sent += processing_result.response.len() as u64;
                                        }
                                    }
                                    Err(e) => {
                                        error!("❌ Encryption failed: {}", e);
                                        let mut stats = self.stats.write().await;
                                        stats.total_errors += 1;
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ Packet processing failed for type 0x{:02x}: {}", packet_type, e);
                                packet_cb.record_failure().await;
                                let mut stats = self.stats.write().await;
                                stats.total_errors += 1;
                            }
                        }
                    } else {
                        error!("❌ Session not found for packet type 0x{:02x}", packet_type);
                        let mut stats = self.stats.write().await;
                        stats.total_errors += 1;
                    }
                }
            }
            Err(e) => {
                error!("❌ Task processing failed: {}", e);
                let mut stats = self.stats.write().await;
                stats.total_errors += 1;
            }
        }
    }

    async fn handle_connection_opened(&self, addr: std::net::SocketAddr, session_id: Vec<u8>) {
        debug!("🔗 Connection opened: {} -> {}", addr, hex::encode(&session_id));

        let mut connections = self.active_connections.write().await;
        connections.insert(addr, ConnectionInfo {
            addr,
            session_id: session_id.clone(),
            opened_at: Instant::now(),
            last_activity: Instant::now(),
            bytes_received: 0,
            bytes_sent: 0,
            priority: Priority::Normal,
            is_active: true,
            worker_assigned: None,
        });

        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
    }

    async fn handle_connection_closed(&self, addr: std::net::SocketAddr, _session_id: Vec<u8>, reason: String) {
        debug!("🔒 Connection closed: {}: {}", addr, reason);
        let mut connections = self.active_connections.write().await;
        connections.remove(&addr);
    }

    async fn handle_batch_completed(
        &self,
        batch_id: u64,
        size: usize,
        processing_time: Duration,
        success_rate: f64
    ) {
        debug!("✅ Batch {} completed: size={}, time={:?}, success={:.1}%",
               batch_id, size, processing_time, success_rate * 100.0);

        let mut stats = self.stats.write().await;
        stats.total_batches_processed += 1;

        let total_batches = stats.total_batches_processed as f64;
        let current_avg = stats.avg_processing_time.as_nanos() as f64;
        let new_avg = (current_avg * (total_batches - 1.0) + processing_time.as_nanos() as f64) / total_batches;
        stats.avg_processing_time = Duration::from_nanos(new_avg as u64);

        let throughput = size as f64 / processing_time.as_secs_f64().max(0.001);
        if throughput > stats.peak_throughput {
            stats.peak_throughput = throughput;
        }
    }

    async fn handle_error_occurred(&self, error: String, context: String, severity: ErrorSeverity) {
        match severity {
            ErrorSeverity::Low => debug!("⚠️ Low: {} in {}", error, context),
            ErrorSeverity::Medium => warn!("⚠️ Medium: {} in {}", error, context),
            ErrorSeverity::High => error!("❌ High: {} in {}", error, context),
            ErrorSeverity::Critical => {
                error!("🚨 CRITICAL: {} in {}", error, context);
            }
        }

        let mut stats = self.stats.write().await;
        stats.total_errors += 1;

        self.record_metric("system.errors", 1.0).await;
        self.record_metric(&format!("system.errors.{}", severity as u8), 1.0).await;
    }

    async fn handle_data_processed(
        &self,
        session_id: Vec<u8>,
        result: ProcessResult,
        _processing_time: Duration,
        _worker_id: Option<usize>,
    ) {
        if result.success {
            if let Some(data) = &result.data {
                let mut stats = self.stats.write().await;
                stats.total_data_sent += data.len() as u64;

                if let Some(addr) = result.metadata.get("destination_addr") {
                    if let Ok(addr) = addr.parse() {
                        let mut connections = self.active_connections.write().await;
                        if let Some(conn) = connections.get_mut(&addr) {
                            conn.bytes_sent += data.len() as u64;
                            conn.last_activity = Instant::now();
                        }
                    }
                }
            }
        }

        let mut cache = self.session_cache.write().await;
        if let Some(entry) = cache.get_mut(&session_id) {
            entry.last_used = Instant::now();
            entry.access_count += 1;
        }
    }

    async fn start_batch_processor(&self) {
        let pending_batches = self.pending_batches.clone();
        let is_running = self.is_running.clone();
        let system = self.clone();

        tokio::spawn(async move {
            debug!("🔄 Batch processor started");
            let mut interval = tokio::time::interval(Duration::from_millis(50));
            let mut batch_id_counter = 0u64;

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let batches_to_process = {
                    let mut batches = pending_batches.write().await;
                    if batches.is_empty() {
                        continue;
                    }

                    let now = Instant::now();
                    let optimal_size = *system.adaptive_batcher.current_batch_size.read().await;
                    let min_size = (optimal_size as f64 * 0.6) as usize;

                    let (ready, not_ready): (Vec<_>, Vec<_>) = batches
                        .drain(..)
                        .partition(|batch| {
                            let size_condition = batch.operations.len() >= min_size;
                            let optimal_condition = batch.operations.len() >= optimal_size;
                            let timeout_condition = batch.deadline.map_or(false, |d| now >= d);
                            timeout_condition || optimal_condition || size_condition
                        });

                    *batches = not_ready;
                    ready
                };

                for mut batch in batches_to_process {
                    batch_id_counter += 1;
                    batch.id = batch_id_counter;
                    system.process_batch(batch).await;
                }
            }

            debug!("👋 Batch processor stopped");
        });
    }

    async fn process_batch(&self, batch: PendingBatch) {
        let start_time = Instant::now();
        let batch_size = batch.operations.len();
        let batch_id = batch.id;

        debug!("🔄 Processing batch #{} with {} operations", batch_id, batch_size);

        let mut successful = 0;

        let mut enc_ops = Vec::new();
        let mut dec_ops = Vec::new();
        let mut hash_ops = Vec::new();

        for operation in batch.operations {
            match operation {
                BatchOperation::Encryption { session_id, data, key, nonce } => {
                    enc_ops.push((session_id, key, nonce, data));
                }
                BatchOperation::Decryption { session_id, data, key, nonce } => {
                    dec_ops.push((session_id, key, nonce, data));
                }
                BatchOperation::Hashing { data, key } => {
                    if let Some(key) = key {
                        hash_ops.push((Vec::<u8>::new(), key, data));
                    }
                }
                BatchOperation::Processing { session_id, data, processor_type } => {
                    self.handle_processing_operation(session_id, data, processor_type, batch.source_addr).await;
                    successful += 1;
                }
            }
        }

        if !enc_ops.is_empty() {
            let keys: Vec<[u8; 32]> = enc_ops.iter().map(|(_, k, _, _)| *k).collect();
            let nonces: Vec<[u8; 12]> = enc_ops.iter().map(|(_, _, n, _)| *n).collect();
            let plaintexts: Vec<Vec<u8>> = enc_ops.iter().map(|(_, _, _, d)| d.to_vec()).collect();

            let results = self.chacha20_accelerator.encrypt_batch(&keys, &nonces, &plaintexts).await;

            for (i, (session_id, _, _, _)) in enc_ops.iter().enumerate() {
                if i < results.len() {
                    let priority = Priority::Critical;
                    let requires_flush = true;

                    if let Err(e) = self.writer.write(
                        batch.source_addr,
                        session_id.clone(),
                        Bytes::from(results[i].clone()),
                        priority,
                        requires_flush,
                    ).await {
                        debug!("❌ Failed to send encrypted batch: {}", e);
                    } else {
                        successful += 1;
                    }
                }
            }
        }

        if !dec_ops.is_empty() {
            let keys: Vec<[u8; 32]> = dec_ops.iter().map(|(_, k, _, _)| *k).collect();
            let nonces: Vec<[u8; 12]> = dec_ops.iter().map(|(_, _, n, _)| *n).collect();
            let ciphertexts: Vec<Vec<u8>> = dec_ops.iter().map(|(_, _, _, d)| d.to_vec()).collect();

            let results = self.chacha20_accelerator.decrypt_batch(&keys, &nonces, &ciphertexts).await;

            for (i, (session_id, _, _, _)) in dec_ops.iter().enumerate() {
                if i < results.len() {
                    if let Some(session) = self.session_manager.get_session(session_id).await {
                        let packet_type = results[i][0];
                        let packet_data = &results[i][1..];

                        if is_packet_supported(packet_type) {
                            let packet_cb = self.circuit_breaker_manager.get_or_create("packet_service");

                            if packet_cb.allow_request().await {
                                match self.packet_service.process_packet(
                                    session.clone(),
                                    packet_type,
                                    packet_data.to_vec(),
                                    batch.source_addr,
                                ).await {
                                    Ok(processing_result) => {
                                        packet_cb.record_success().await;

                                        match self.packet_processor.create_outgoing_vec(
                                            &session,
                                            processing_result.packet_type,
                                            &processing_result.response,
                                        ) {
                                            Ok(encrypted_response) => {
                                                let priority = get_packet_priority(processing_result.packet_type)
                                                    .unwrap_or(Priority::Normal);
                                                let requires_flush = PacketType::from_byte(processing_result.packet_type)
                                                    .map(|pt| pt.requires_immediate_flush())
                                                    .unwrap_or(false);

                                                let _ = self.writer.write(
                                                    batch.source_addr,
                                                    session_id.clone(),
                                                    Bytes::from(encrypted_response),
                                                    priority,
                                                    requires_flush,
                                                ).await;

                                                successful += 1;
                                            }
                                            Err(e) => {
                                                debug!("❌ Encryption failed: {}", e);
                                                packet_cb.record_failure().await;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        debug!("❌ Packet service failed: {}", e);
                                        packet_cb.record_failure().await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if !hash_ops.is_empty() {
            let keys: Vec<[u8; 32]> = hash_ops.iter().map(|(_, k, _)| *k).collect();
            let inputs: Vec<Vec<u8>> = hash_ops.iter().map(|(_, _, d)| d.to_vec()).collect();

            let _hashes = self.blake3_accelerator.hash_keyed_batch(&keys, &inputs).await;
            successful += hash_ops.len();
        }

        let success_rate = if batch_size > 0 {
            successful as f64 / batch_size as f64
        } else {
            1.0
        };

        let processing_time = start_time.elapsed();

        self.adaptive_batcher.record_batch_execution(
            batch_size,
            processing_time,
            success_rate,
            self.pending_batches.read().await.len(),
        ).await;

        {
            let mut stats = self.stats.write().await;
            stats.total_batches_processed += 1;
            stats.crypto_operations += successful as u64;
            stats.total_packets_processed += successful as u64;
        }

        let event = SystemEvent::BatchCompleted {
            batch_id,
            size: batch_size,
            processing_time,
            success_rate,
        };

        let _ = self.event_tx.send(event).await;
    }

    async fn handle_processing_operation(
        &self,
        session_id: Vec<u8>,
        data: Bytes,
        processor_type: ProcessorType,
        source_addr: std::net::SocketAddr,
    ) {
        if data.is_empty() {
            return;
        }

        let packet_type_byte = data[0];

        if !is_packet_supported(packet_type_byte) {
            debug!("⚠️ Unsupported packet type: 0x{:02x}", packet_type_byte);
            return;
        }

        match processor_type {
            ProcessorType::Accelerated => {
                if let Some(session) = self.session_manager.get_session(&session_id).await {
                    match self.packet_processor.create_outgoing_vec(&session, packet_type_byte, &data) {
                        Ok(encrypted) => {
                            let priority = get_packet_priority(packet_type_byte).unwrap_or(Priority::Normal);
                            let requires_flush = PacketType::from_byte(packet_type_byte)
                                .map(|pt| pt.requires_immediate_flush())
                                .unwrap_or(false);

                            let _ = self.writer.write(
                                source_addr,
                                session_id,
                                Bytes::from(encrypted),
                                priority,
                                requires_flush,
                            ).await;
                        }
                        Err(e) => {
                            debug!("❌ Processing failed: {}", e);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    async fn start_performance_monitoring(&self) {
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            while system.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                system.update_performance_counters().await;
                system.check_scaling_needs().await;
            }
        });
    }

    async fn update_performance_counters(&self) {
        let buffer_stats = self.buffer_pool.get_detailed_stats();
        let total_hit_rate = buffer_stats.get("Global")
            .map(|s| s.hit_rate)
            .unwrap_or(0.0);

        self.record_metric("buffer_pool.hit_rate", total_hit_rate).await;
        self.record_metric("buffer_pool.reuse_rate", self.buffer_pool.get_reuse_rate()).await;

        let crypto_stats = self.crypto_processor.get_stats();
        let crypto_tasks = crypto_stats.get("crypto_tasks_submitted").copied().unwrap_or(0);
        let crypto_processed = crypto_stats.get("crypto_tasks_processed").copied().unwrap_or(0);
        let crypto_steals = crypto_stats.get("crypto_steals").copied().unwrap_or(0);

        self.record_metric("crypto.tasks_submitted", crypto_tasks as f64).await;
        self.record_metric("crypto.tasks_processed", crypto_processed as f64).await;
        self.record_metric("crypto.steals", crypto_steals as f64).await;

        let dispatcher_stats = self.work_stealing_dispatcher.get_advanced_stats().await;
        self.record_metric("dispatcher.tasks_processed", dispatcher_stats.total_tasks_processed as f64).await;
        self.record_metric("dispatcher.work_steals", dispatcher_stats.work_steals as f64).await;
        self.record_metric("dispatcher.imbalance", dispatcher_stats.imbalance).await;
        self.record_metric("dispatcher.steal_probability", dispatcher_stats.steal_probability).await;

        {
            let mut stats = self.stats.write().await;
            stats.work_stealing_count = dispatcher_stats.work_steals;
            stats.buffer_hit_rate = total_hit_rate;
        }

        let connections = self.active_connections.read().await.len();
        self.record_metric("connections.active", connections as f64).await;
    }

    async fn start_auto_scaling(&self) {
        let system = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            while system.is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let settings = system.scaling_settings.read().await;
                if !settings.auto_scaling_enabled {
                    continue;
                }

                let now = Instant::now();
                if now.duration_since(settings.last_scaling_time) < Duration::from_secs(settings.scaling_cooldown_seconds) {
                    continue;
                }

                drop(settings);
                system.perform_auto_scaling().await;
            }
        });
    }

    async fn perform_auto_scaling(&self) {
        let _lock = self.scaling_lock.lock().await;

        let settings = self.scaling_settings.read().await;
        let dispatcher_stats = self.work_stealing_dispatcher.get_advanced_stats().await;
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);

        let lambda = self.adaptive_batcher.kalman.read().await.state;
        let optimal_workers = optimal_worker_count(lambda, 1000.0, 10.0);

        let should_scale_up =
            current_workers < optimal_workers ||
                dispatcher_stats.queue_backlog > settings.work_stealing_target_queue_size * 2 ||
                dispatcher_stats.imbalance > 0.7;

        let should_scale_down =
            current_workers > optimal_workers + 2 &&
                dispatcher_stats.queue_backlog < settings.work_stealing_target_queue_size / 4 &&
                dispatcher_stats.imbalance < 0.2;

        if should_scale_up && current_workers < settings.max_worker_count {
            let scale_up_by = 2.min(settings.max_worker_count - current_workers);
            info!("📈 Auto-scaling: scaling UP by {} workers (optimal: {})", scale_up_by, optimal_workers);
            let _ = self.scale_up(scale_up_by).await;
        } else if should_scale_down && current_workers > settings.min_worker_count {
            let scale_down_by = 2.min(current_workers - settings.min_worker_count);
            info!("📉 Auto-scaling: scaling DOWN by {} workers (optimal: {})", scale_down_by, optimal_workers);
            let _ = self.scale_down(scale_down_by).await;
        }
    }

    async fn check_scaling_needs(&self) {
        let settings = self.scaling_settings.read().await;

        let buffer_hit_rate = self.get_metric("buffer_pool.hit_rate").await.unwrap_or(0.0);
        let crypto_success_rate = self.get_metric("crypto.success_rate").await.unwrap_or(1.0);
        let dispatcher_load = self.get_metric("dispatcher.imbalance").await.unwrap_or(0.0);
        let active_connections = self.active_connections.read().await.len();
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);

        if buffer_hit_rate < settings.buffer_pool_target_hit_rate * 0.8 {
            warn!("📉 Buffer pool hit rate low: {:.1}%", buffer_hit_rate * 100.0);
            let _ = self.buffer_pool.force_cleanup().await;
        }

        if crypto_success_rate < settings.crypto_processor_target_success_rate * 0.9 {
            warn!("⚠️ Crypto success rate low: {:.1}%", crypto_success_rate * 100.0);
            if let Some(cb) = self.circuit_breaker_manager.get_breaker("crypto_processor").await {
                cb.reset().await;
            }
        }

        if dispatcher_load > 0.7 {
            warn!("⚖️ High dispatcher imbalance: {:.2}", dispatcher_load);
            self.rebalance_workers().await;
        }

        if active_connections as f64 > settings.connection_target_count as f64 * 1.5 {
            warn!("🔌 High connection count: {}", active_connections);
            if current_workers < settings.max_worker_count {
                let _ = self.scale_up(2).await;
            }
        }
    }

    async fn start_command_handlers(&self) {
        let command_rx = self.command_rx.clone();
        let system = self.clone();

        tokio::spawn(async move {
            debug!("🎛️ Command handler started");
            let mut receiver = command_rx.lock().await;

            while let Ok(command) = receiver.recv().await {
                system.handle_command(command).await;
            }

            debug!("👋 Command handler stopped");
        });
    }

    async fn handle_command(&self, command: SystemCommand) {
        match command {
            SystemCommand::StartProcessing => self.start_processing().await,
            SystemCommand::PauseProcessing => self.pause_processing().await,
            SystemCommand::ResumeProcessing => self.resume_processing().await,
            SystemCommand::StopProcessing => self.stop_processing().await,
            SystemCommand::FlushBuffers => self.flush_buffers().await,
            SystemCommand::ClearCaches => self.clear_caches().await,
            SystemCommand::AdjustConfig { parameter, value } => self.adjust_config(parameter, value).await,
            SystemCommand::EmergencyShutdown { reason } => self.emergency_shutdown(reason).await,
            SystemCommand::GetStatistics => self.get_statistics().await,
            SystemCommand::ResetStatistics => self.reset_statistics().await,
            SystemCommand::RebalanceWorkers => self.rebalance_workers().await,
            SystemCommand::ScaleUp { count } => {
                let _ = self.scale_up(count).await;
            }
            SystemCommand::ScaleDown { count } => {
                let _ = self.scale_down(count).await;
            }
            SystemCommand::UpdateScalingSettings { settings } => self.update_scaling_settings(settings).await,
        }
    }

    async fn start_processing(&self) {
        if !self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("▶️ Starting data processing...");
            self.is_running.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    async fn pause_processing(&self) {
        if self.is_running.load(std::sync::atomic::Ordering::SeqCst) {
            info!("⏸️ Pausing data processing...");
            self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    async fn resume_processing(&self) {
        self.start_processing().await;
    }

    async fn stop_processing(&self) {
        info!("⏹️ Stopping data processing...");
        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
        self.controller.stop().await;
        self.shutdown_components().await;
    }

    async fn flush_buffers(&self) {
        info!("🌀 Flushing all buffers...");
        let _ = self.buffer_pool.force_cleanup().await;

        let mut cache = self.session_cache.write().await;
        cache.clear();
    }

    async fn clear_caches(&self) {
        info!("🧹 Clearing all caches...");

        let mut session_cache = self.session_cache.write().await;
        session_cache.clear();

        let mut connections = self.active_connections.write().await;
        connections.clear();

        self.performance_counters.clear();
        self.metrics.clear();

        let mut pending = self.pending_batches.write().await;
        pending.clear();

        info!("✅ All caches cleared");
    }

    async fn adjust_config(&self, parameter: String, value: String) {
        info!("⚙️ Adjusting config: {} = {}", parameter, value);

        match parameter.as_str() {
            "batch_size" => {
                if let Ok(size) = value.parse::<usize>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    let clamped_size = size.clamp(config.min_batch_size, config.max_batch_size);
                    config.initial_batch_size = clamped_size;
                    *self.adaptive_batcher.current_batch_size.write().await = clamped_size;
                    self.record_metric("config.batch_size", clamped_size as f64).await;
                    info!("✅ Batch size updated to {}", clamped_size);
                }
            }
            "worker_count" => {
                if let Ok(count) = value.parse::<usize>() {
                    let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
                    let settings = self.scaling_settings.read().await;

                    if count > current_workers {
                        if count <= settings.max_worker_count {
                            let increase = count - current_workers;
                            info!("📈 Increasing worker count by {} to {}", increase, count);
                            let _ = self.scale_up(increase).await;
                        }
                    } else if count < current_workers {
                        if count >= settings.min_worker_count {
                            let decrease = current_workers - count;
                            info!("📉 Decreasing worker count by {} to {}", decrease, count);
                            let _ = self.scale_down(decrease).await;
                        }
                    }
                }
            }
            "target_latency_ms" => {
                if let Ok(ms) = value.parse::<u64>() {
                    let mut config = self.adaptive_batcher.config.clone();
                    config.target_latency = Duration::from_millis(ms);
                    self.record_metric("config.target_latency_ms", ms as f64).await;
                    info!("✅ Target latency updated to {} ms", ms);
                }
            }
            _ => warn!("⚠️ Unknown parameter: {}", parameter),
        }
    }

    async fn emergency_shutdown(&self, reason: String) {
        error!("🚨 EMERGENCY SHUTDOWN: {}", reason);

        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
        self.controller.stop().await;
        self.shutdown_components().await;
        self.record_metric("system.emergency_shutdown", 1.0).await;
    }

    async fn get_statistics(&self) {
        let stats = self.stats.read().await.clone();
        let status = self.get_system_status().await;
        let model = self.controller.state.read().await;

        info!("📊 SYSTEM STATISTICS");
        info!("  Uptime: {:?}", stats.uptime);
        info!("  Processed packets: {}", stats.total_packets_processed);
        info!("  Data received: {} MB", stats.total_data_received / 1024 / 1024);
        info!("  Data sent: {} MB", stats.total_data_sent / 1024 / 1024);
        info!("  Active connections: {}", status.active_connections);
        info!("  Active workers: {}", self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst));
        info!("  Avg processing time: {:?}", stats.avg_processing_time);
        info!("  Peak throughput: {:.2} ops/s", stats.peak_throughput);
        info!("  Crypto operations: {}", stats.crypto_operations);
        info!("  Work steals: {}", stats.work_stealing_count);
        info!("  Total errors: {}", stats.total_errors);
        info!("  λ = {:.2} pkt/s", model.lambda);
        info!("  ρ = {:.2}%", model.rho * 100.0);
        info!("  B = {}", model.b);
        info!("  M = {}", model.m);
        info!("  L = {:.2} ms", model.latency_ms);
        info!("  Little's Law: N = {:.2} pkts", model.little_law());
    }

    async fn reset_statistics(&self) {
        info!("🔄 Resetting system statistics...");

        let mut stats = self.stats.write().await;
        *stats = SystemStatistics {
            startup_time: stats.startup_time,
            ..Default::default()
        };

        self.metrics.clear();
        self.performance_counters.clear();

        info!("✅ Statistics reset completed");
    }

    async fn rebalance_workers(&self) {
        info!("⚖️ Rebalancing workers...");

        let mut worker_loads = Vec::new();
        for i in 0..self.work_stealing_dispatcher.worker_senders.len() {
            if let Some(load) = self.work_stealing_dispatcher.worker_loads.get(&i) {
                worker_loads.push(*load.value());
            }
        }

        let imbalance = WorkStealingModel::compute_imbalance(&worker_loads);

        let mut model = self.work_stealing_dispatcher.stealing_model.write().await;
        model.p_steal = (model.p_steal + 0.1).min(0.5);

        self.record_metric("dispatcher.manual_rebalance", 1.0).await;
        self.record_metric("dispatcher.rebalance_imbalance", imbalance).await;

        info!("✅ Workers rebalanced (imbalance: {:.2})", imbalance);
    }

    async fn scale_up(&self, count: usize) -> Result<usize, BatchError> {
        info!("📈 Scaling up by {} workers", count);

        let _lock = self.scaling_lock.lock().await;
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let settings = self.scaling_settings.read().await;

        if count == 0 || current_workers >= settings.max_worker_count {
            return Ok(0);
        }

        let lambda = self.adaptive_batcher.kalman.read().await.state;
        let optimal_workers = optimal_worker_count(lambda, 1000.0, 10.0);

        let target_workers = optimal_workers.min(settings.max_worker_count);
        let actual_increase = target_workers.saturating_sub(current_workers).min(count);

        if actual_increase > 0 {
            let added = self.worker_pool.add_workers(actual_increase, self.work_stealing_dispatcher.clone()).await?;

            let mut new_settings = settings.clone();
            new_settings.last_scaling_time = Instant::now();
            *self.scaling_settings.write().await = new_settings;

            self.record_metric("scaling.scale_up", added as f64).await;
            self.record_metric("scaling.current_workers", (current_workers + added) as f64).await;

            info!("✅ Scaled UP from {} to {} workers (optimal: {})",
                  current_workers, current_workers + added, optimal_workers);

            Ok(added)
        } else {
            Ok(0)
        }
    }

    async fn scale_down(&self, count: usize) -> Result<usize, BatchError> {
        info!("📉 Scaling down by {} workers", count);

        let _lock = self.scaling_lock.lock().await;
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let settings = self.scaling_settings.read().await;

        if count == 0 || current_workers <= settings.min_worker_count {
            return Ok(0);
        }

        let lambda = self.adaptive_batcher.kalman.read().await.state;
        let optimal_workers = optimal_worker_count(lambda, 1000.0, 10.0);

        let target_workers = optimal_workers.max(settings.min_worker_count);
        let actual_decrease = current_workers.saturating_sub(target_workers).min(count);

        if actual_decrease > 0 {
            let removed = self.worker_pool.remove_workers(actual_decrease).await?;

            let mut new_settings = settings.clone();
            new_settings.last_scaling_time = Instant::now();
            *self.scaling_settings.write().await = new_settings;

            self.record_metric("scaling.scale_down", removed as f64).await;
            self.record_metric("scaling.current_workers", (current_workers - removed) as f64).await;

            info!("✅ Scaled DOWN from {} to {} workers (optimal: {})",
                  current_workers, current_workers - removed, optimal_workers);

            Ok(removed)
        } else {
            Ok(0)
        }
    }

    async fn update_scaling_settings(&self, settings: ScalingSettings) {
        let mut current = self.scaling_settings.write().await;

        let mut validated = settings;
        if validated.min_worker_count < 1 {
            validated.min_worker_count = 1;
        }
        if validated.max_worker_count < validated.min_worker_count {
            validated.max_worker_count = validated.min_worker_count.max(256);
        }
        if validated.scaling_cooldown_seconds < 10 {
            validated.scaling_cooldown_seconds = 10;
        }

        if validated.min_worker_count != current.min_worker_count {
            let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);
            if current_workers < validated.min_worker_count {
                let increase = validated.min_worker_count - current_workers;
                drop(current);
                let _ = self.scale_up(increase).await;
                current = self.scaling_settings.write().await;
            }
        }

        *current = validated.clone();

        self.record_metric("scaling.min_workers", current.min_worker_count as f64).await;
        self.record_metric("scaling.max_workers", current.max_worker_count as f64).await;
        self.record_metric("scaling.auto_scaling_enabled", current.auto_scaling_enabled as i64 as f64).await;

        info!("⚙️ Scaling settings updated");
    }

    async fn start_statistics_collector(&self) {
        let stats = self.stats.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;
                let mut stats_guard = stats.write().await;
                stats_guard.uptime = Instant::now().duration_since(stats_guard.startup_time);
            }
        });
    }

    async fn shutdown_components(&self) {
        info!("🛑 Shutting down components...");

        self.work_stealing_dispatcher.shutdown().await;
        self.crypto_processor.shutdown().await;
        self.reader.shutdown().await;
        self.writer.shutdown().await;

        info!("✅ All components shut down");
    }

    pub async fn register_connection(
        &self,
        source_addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        read_stream: Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>,
        write_stream: Box<dyn tokio::io::AsyncWrite + Unpin + Send + Sync>,
    ) -> Result<(), BatchError> {
        debug!("🔗 Registering connection: {} -> {}", source_addr, hex::encode(&session_id));

        self.reader.register_connection(
            source_addr,
            session_id.clone(),
            read_stream,
        ).await?;

        self.writer.register_connection(
            source_addr,
            session_id.clone(),
            write_stream,
        ).await?;

        let event = SystemEvent::ConnectionOpened {
            addr: source_addr,
            session_id,
        };

        let _ = self.event_tx.send(event).await;

        Ok(())
    }

    pub async fn get_system_status(&self) -> SystemStatus {
        let stats = self.stats.read().await.clone();
        let connections = self.active_connections.read().await;
        let settings = self.scaling_settings.read().await.clone();
        let model = self.controller.state.read().await.clone();

        let current_batch_size = *self.adaptive_batcher.current_batch_size.read().await;
        let qos_stats = self.qos_manager.get_statistics().await;
        let qos_quotas = self.qos_manager.get_quotas().await;
        let qos_utilization = self.qos_manager.get_utilization().await;
        let circuit_stats = self.circuit_breaker_manager.get_all_stats().await;
        let dispatcher_stats = self.work_stealing_dispatcher.get_advanced_stats().await;
        let current_workers = self.worker_pool.current_workers.load(std::sync::atomic::Ordering::SeqCst);

        SystemStatus {
            timestamp: Instant::now(),
            is_running: self.is_running.load(std::sync::atomic::Ordering::Relaxed),
            statistics: stats,
            active_connections: connections.len(),
            active_workers: current_workers,
            pending_tasks: self.pending_batches.read().await.len(),
            state_model: model,
            memory_usage: MemoryUsage {
                total: 0,
                used: 0,
                free: 0,
                buffer_pool: self.buffer_pool.get_detailed_stats()
                    .values()
                    .map(|s| s.memory_mb as usize * 1024 * 1024)
                    .sum(),
                crypto_pool: 0,
                connections: connections.len(),
                session_cache: self.session_cache.read().await.len(),
            },
            throughput: self.calculate_throughput().await,
            scaling_settings: settings,
            batch_size: current_batch_size,
            qos_stats,
            qos_quotas,
            qos_utilization,
            circuit_stats,
            dispatcher_stats,
        }
    }

    async fn calculate_throughput(&self) -> ThroughputMetrics {
        let stats = self.stats.read().await;
        let uptime = stats.uptime.as_secs_f64().max(1.0);
        let batch_size = *self.adaptive_batcher.current_batch_size.read().await;

        ThroughputMetrics {
            packets_per_second: stats.total_packets_processed as f64 / uptime,
            bytes_per_second: stats.total_data_received as f64 / uptime,
            operations_per_second: stats.total_batches_processed as f64 / uptime,
            avg_batch_size: batch_size as f64,
            latency_p50: Duration::from_micros(self.metrics_tracing
                .get_aggregated_metric("latency.p95")
                .map(|m| m.percentile(50.0) as u64)
                .unwrap_or(50) * 1000),
            latency_p95: Duration::from_micros(self.metrics_tracing
                .get_aggregated_metric("latency.p95")
                .map(|m| m.percentile(95.0) as u64)
                .unwrap_or(100) * 1000),
            latency_p99: Duration::from_micros(self.metrics_tracing
                .get_aggregated_metric("latency.p99")
                .map(|m| m.percentile(99.0) as u64)
                .unwrap_or(200) * 1000),
        }
    }

    async fn record_metric(&self, name: &str, value: f64) {
        self.metrics.insert(name.to_string(), MetricValue::Float(value));
        self.metrics_tracing.record_metric(name, value).await;

        if let Some(mut counter) = self.performance_counters.get_mut(name) {
            counter.update(value);
        } else {
            let mut counter = PerformanceCounter::new(name.to_string(), 60);
            counter.update(value);
            self.performance_counters.insert(name.to_string(), counter);
        }
    }

    async fn get_metric(&self, name: &str) -> Option<f64> {
        self.metrics.get(name).and_then(|m| {
            if let MetricValue::Float(v) = m.value() {
                Some(*v)
            } else {
                None
            }
        })
    }

    async fn start_simd_batch_processor(&self) {
        let pending_encryptions = self.pending_encryptions.clone();
        let pending_decryptions = self.pending_decryptions.clone();
        let pending_hashes = self.pending_hashes.clone();
        let chacha20 = self.chacha20_accelerator.clone();
        let blake3 = self.blake3_accelerator.clone();
        let writer = self.writer.clone();
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();
        let adaptive_batcher = self.adaptive_batcher.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            let mut last_batch_time = Instant::now();

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let optimal_batch_size = *adaptive_batcher.current_batch_size.read().await;
                let time_since_last_batch = last_batch_time.elapsed();
                let timeout_reached = time_since_last_batch >= Duration::from_millis(50);

                let encryptions = {
                    let mut queue = pending_encryptions.lock().await;
                    if queue.len() >= optimal_batch_size || (timeout_reached && !queue.is_empty()) {
                        let batch = queue.clone();
                        queue.clear();
                        last_batch_time = Instant::now();
                        batch
                    } else {
                        Vec::new()
                    }
                };

                if !encryptions.is_empty() {
                    let keys: Vec<[u8; 32]> = encryptions.iter().map(|(_, k, _, _)| *k).collect();
                    let nonces: Vec<[u8; 12]> = encryptions.iter().map(|(_, _, n, _)| *n).collect();
                    let plaintexts: Vec<Vec<u8>> = encryptions.iter().map(|(_, _, _, d)| d.to_vec()).collect();

                    let results = chacha20.encrypt_batch(&keys, &nonces, &plaintexts).await;

                    for (i, (session_id, _, _, _)) in encryptions.iter().enumerate() {
                        if i < results.len() {
                            let _ = writer.write(
                                std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                                session_id.clone(),
                                encryptions[i].3.clone(),
                                Priority::Critical,
                                true
                            ).await;
                        }
                    }
                }

                let decryptions = {
                    let mut queue = pending_decryptions.lock().await;
                    if queue.len() >= optimal_batch_size || (timeout_reached && !queue.is_empty()) {
                        let batch = queue.clone();
                        queue.clear();
                        last_batch_time = Instant::now();
                        batch
                    } else {
                        Vec::new()
                    }
                };

                if !decryptions.is_empty() {
                    let keys: Vec<[u8; 32]> = decryptions.iter().map(|(_, k, _, _)| *k).collect();
                    let nonces: Vec<[u8; 12]> = decryptions.iter().map(|(_, _, n, _)| *n).collect();
                    let ciphertexts: Vec<Vec<u8>> = decryptions.iter().map(|(_, _, _, d)| d.to_vec()).collect();

                    let results = chacha20.decrypt_batch(&keys, &nonces, &ciphertexts).await;

                    for (i, (session_id, _, _, _)) in decryptions.iter().enumerate() {
                        if i < results.len() {
                            let event = SystemEvent::DataProcessed {
                                session_id: session_id.clone(),
                                result: ProcessResult {
                                    success: true,
                                    data: Some(Bytes::from(results[i].clone())),
                                    error: None,
                                    metadata: HashMap::new(),
                                },
                                processing_time: Duration::from_micros(100),
                                worker_id: None,
                            };
                            let _ = event_tx.send(event).await;
                        }
                    }
                }

                let hashes = {
                    let mut queue = pending_hashes.lock().await;
                    if queue.len() >= optimal_batch_size / 2 || (timeout_reached && !queue.is_empty()) {
                        let batch = queue.clone();
                        queue.clear();
                        last_batch_time = Instant::now();
                        batch
                    } else {
                        Vec::new()
                    }
                };

                if !hashes.is_empty() {
                    let keys: Vec<[u8; 32]> = hashes.iter().map(|(_, k, _)| *k).collect();
                    let inputs: Vec<Vec<u8>> = hashes.iter().map(|(_, _, d)| d.to_vec()).collect();

                    let _results = blake3.hash_keyed_batch(&keys, &inputs).await;
                }
            }
        });
    }

    async fn start_mathematical_optimization(&self) {
        let adaptive_batcher = self.adaptive_batcher.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                let measured_throughput = adaptive_batcher.metrics
                    .get("batch.throughput")
                    .map(|m| *m.value())
                    .unwrap_or(100.0);

                let measured_latency = adaptive_batcher.metrics
                    .get("batch.processing_time_ms")
                    .map(|m| *m.value())
                    .unwrap_or(50.0);

                let target_latency = adaptive_batcher.config.target_latency.as_millis() as f64;

                {
                    let mut kalman = adaptive_batcher.kalman.write().await;
                    kalman.predict();
                    let lambda = kalman.update(measured_throughput.max(0.1));
                    adaptive_batcher.record_metric("lambda.estimated", lambda);
                }

                {
                    let markov = adaptive_batcher.markov.write().await;
                    let lambdas = adaptive_batcher.lambdas.read().await;
                    if lambdas.len() >= 3 {
                        let max_lambda = lambdas.iter().fold(0.0_f64, |a, &b| a.max(b)).max(1000.0);
                        let l_tm1 = markov.quantize(lambdas[lambdas.len() - 2], max_lambda);
                        let l_t = markov.quantize(lambdas[lambdas.len() - 1], max_lambda);
                        let (l_tp1, prob) = markov.predict(l_tm1, l_t);

                        if prob > 0.5 {
                            let pred = markov.dequantize(l_tp1, max_lambda);
                            adaptive_batcher.record_metric("lambda.markov_prediction", pred);
                        }
                    }
                }

                {
                    let mut pid = adaptive_batcher.pid.write().await;
                    let error = target_latency - measured_latency;
                    let _correction = pid.compute(error);
                    adaptive_batcher.record_metric("pid.error", error);
                }

                let model = adaptive_batcher.model_params.read().await;
                debug!("📊 AdaptiveBatcher state: λ={:.1}, α={:.4}, β={:.4}, γ={:.6}, δ={:.8}, B*={:.1}",
                       measured_throughput, model.alpha, model.beta, model.gamma, model.delta, model.b_opt);
            }
        });
    }
}

pub struct WorkerPool {
    pub min_workers: usize,
    pub max_workers: usize,
    pub current_workers: Arc<std::sync::atomic::AtomicUsize>,
    pub worker_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    pub shutdown_tx: broadcast::Sender<()>,
}

impl WorkerPool {
    pub fn new(min_workers: usize, max_workers: usize) -> Self {
        let (shutdown_tx, _) = broadcast::channel(max_workers * 2);
        Self {
            min_workers,
            max_workers,
            current_workers: Arc::new(std::sync::atomic::AtomicUsize::new(min_workers)),
            worker_handles: Arc::new(Mutex::new(Vec::new())),
            shutdown_tx,
        }
    }

    pub async fn add_workers(&self, count: usize, _dispatcher: Arc<WorkStealingDispatcher>) -> Result<usize, BatchError> {
        let current = self.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let target = (current + count).min(self.max_workers);
        let to_add = target - current;

        if to_add == 0 {
            return Ok(0);
        }

        let mut handles = self.worker_handles.lock().await;
        let shutdown_rx = self.shutdown_tx.subscribe();

        for i in 0..to_add {
            let worker_id = current + i;
            let mut shutdown_rx = shutdown_rx.resubscribe();

            let handle = tokio::spawn(async move {
                debug!("👷 Dynamic worker #{} started", worker_id);
                while shutdown_rx.try_recv().is_err() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                debug!("👋 Dynamic worker #{} shutting down", worker_id);
            });

            handles.push(handle);
        }

        self.current_workers.store(target, std::sync::atomic::Ordering::SeqCst);
        Ok(to_add)
    }

    pub async fn remove_workers(&self, count: usize) -> Result<usize, BatchError> {
        let current = self.current_workers.load(std::sync::atomic::Ordering::SeqCst);
        let target = current.saturating_sub(count).max(self.min_workers);
        let to_remove = current - target;

        if to_remove == 0 {
            return Ok(0);
        }

        for _ in 0..to_remove {
            let _ = self.shutdown_tx.send(());
        }

        self.current_workers.store(target, std::sync::atomic::Ordering::SeqCst);
        Ok(to_remove)
    }
}

#[derive(Debug, Clone)]
pub enum SystemEvent {
    DataReceived {
        session_id: Vec<u8>,
        data: Bytes,
        source_addr: std::net::SocketAddr,
        priority: Priority,
        timestamp: Instant,
    },
    DataProcessed {
        session_id: Vec<u8>,
        result: ProcessResult,
        processing_time: Duration,
        worker_id: Option<usize>,
    },
    ConnectionOpened {
        addr: std::net::SocketAddr,
        session_id: Vec<u8>,
    },
    ConnectionClosed {
        addr: std::net::SocketAddr,
        session_id: Vec<u8>,
        reason: String,
    },
    BatchCompleted {
        batch_id: u64,
        size: usize,
        processing_time: Duration,
        success_rate: f64,
    },
    ErrorOccurred {
        error: String,
        context: String,
        severity: ErrorSeverity,
    },
}

#[derive(Debug, Clone)]
pub enum SystemCommand {
    StartProcessing,
    PauseProcessing,
    ResumeProcessing,
    StopProcessing,
    FlushBuffers,
    ClearCaches,
    AdjustConfig {
        parameter: String,
        value: String,
    },
    EmergencyShutdown {
        reason: String,
    },
    GetStatistics,
    ResetStatistics,
    RebalanceWorkers,
    ScaleUp {
        count: usize,
    },
    ScaleDown {
        count: usize,
    },
    UpdateScalingSettings {
        settings: ScalingSettings,
    },
}

#[derive(Debug, Clone)]
pub struct SystemStatus {
    pub timestamp: Instant,
    pub is_running: bool,
    pub statistics: SystemStatistics,
    pub active_connections: usize,
    pub active_workers: usize,
    pub pending_tasks: usize,
    pub state_model: SystemStateModel,
    pub memory_usage: MemoryUsage,
    pub throughput: ThroughputMetrics,
    pub scaling_settings: ScalingSettings,
    pub batch_size: usize,
    pub qos_stats: QosStatistics,
    pub qos_quotas: (f64, f64, f64),
    pub qos_utilization: (f64, f64, f64),
    pub circuit_stats: Vec<CircuitBreakerStats>,
    pub dispatcher_stats: DispatcherAdvancedStats,
}

#[derive(Debug, Clone)]
pub struct SystemStatistics {
    pub total_data_received: u64,
    pub total_data_sent: u64,
    pub total_packets_processed: u64,
    pub total_batches_processed: u64,
    pub total_errors: u64,
    pub total_connections: u64,
    pub avg_processing_time: Duration,
    pub peak_throughput: f64,
    pub buffer_hit_rate: f64,
    pub crypto_operations: u64,
    pub work_stealing_count: u64,
    pub startup_time: Instant,
    pub uptime: Duration,
}

impl Default for SystemStatistics {
    fn default() -> Self {
        Self {
            total_data_received: 0,
            total_data_sent: 0,
            total_packets_processed: 0,
            total_batches_processed: 0,
            total_errors: 0,
            total_connections: 0,
            avg_processing_time: Duration::from_secs(0),
            peak_throughput: 0.0,
            buffer_hit_rate: 0.0,
            crypto_operations: 0,
            work_stealing_count: 0,
            startup_time: Instant::now(),
            uptime: Duration::from_secs(0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScalingSettings {
    pub buffer_pool_target_hit_rate: f64,
    pub crypto_processor_target_success_rate: f64,
    pub work_stealing_target_queue_size: usize,
    pub connection_target_count: usize,
    pub min_worker_count: usize,
    pub max_worker_count: usize,
    pub auto_scaling_enabled: bool,
    pub scaling_cooldown_seconds: u64,
    pub last_scaling_time: Instant,
}

impl Default for ScalingSettings {
    fn default() -> Self {
        Self {
            buffer_pool_target_hit_rate: 0.85,
            crypto_processor_target_success_rate: 0.99,
            work_stealing_target_queue_size: 1000,
            connection_target_count: 10000,
            min_worker_count: 4,
            max_worker_count: 256,
            auto_scaling_enabled: true,
            scaling_cooldown_seconds: 60,
            last_scaling_time: Instant::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub addr: std::net::SocketAddr,
    pub session_id: Vec<u8>,
    pub opened_at: Instant,
    pub last_activity: Instant,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub priority: Priority,
    pub is_active: bool,
    pub worker_assigned: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SessionCacheEntry {
    pub session_id: Vec<u8>,
    pub last_used: Instant,
    pub access_count: u64,
    pub data: Bytes,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct PendingBatch {
    pub id: u64,
    pub operations: Vec<BatchOperation>,
    pub priority: Priority,
    pub source_addr: std::net::SocketAddr,
    pub created_at: Instant,
    pub deadline: Option<Instant>,
    pub retry_count: u32,
}

#[derive(Debug, Clone)]
pub enum BatchOperation {
    Encryption {
        session_id: Vec<u8>,
        data: Bytes,
        key: [u8; 32],
        nonce: [u8; 12],
    },
    Decryption {
        session_id: Vec<u8>,
        data: Bytes,
        key: [u8; 32],
        nonce: [u8; 12],
    },
    Hashing {
        data: Bytes,
        key: Option<[u8; 32]>,
    },
    Processing {
        session_id: Vec<u8>,
        data: Bytes,
        processor_type: ProcessorType,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorType {
    Standard,
    Accelerated,
    Optimized,
    WorkStealing,
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub success: bool,
    pub data: Option<Bytes>,
    pub error: Option<String>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct MemoryUsage {
    pub total: usize,
    pub used: usize,
    pub free: usize,
    pub buffer_pool: usize,
    pub crypto_pool: usize,
    pub connections: usize,
    pub session_cache: usize,
}

#[derive(Debug, Clone)]
pub struct ThroughputMetrics {
    pub packets_per_second: f64,
    pub bytes_per_second: f64,
    pub operations_per_second: f64,
    pub avg_batch_size: f64,
    pub latency_p50: Duration,
    pub latency_p95: Duration,
    pub latency_p99: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
}

#[derive(Debug, Clone)]
pub enum MetricValue {
    Integer(i64),
    Float(f64),
    Duration(Duration),
    String(String),
    Boolean(bool),
}

impl MetricValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            MetricValue::Integer(i) => Some(*i as f64),
            MetricValue::Float(f) => Some(*f),
            MetricValue::Duration(d) => Some(d.as_secs_f64()),
            MetricValue::Boolean(b) => Some(if *b { 1.0 } else { 0.0 }),
            MetricValue::String(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PerformanceCounter {
    pub name: String,
    pub value: f64,
    pub timestamp: Instant,
    pub window_size: usize,
    pub values: VecDeque<f64>,
}

impl PerformanceCounter {
    pub fn new(name: String, window_size: usize) -> Self {
        Self {
            name,
            value: 0.0,
            timestamp: Instant::now(),
            window_size,
            values: VecDeque::with_capacity(window_size),
        }
    }

    pub fn update(&mut self, value: f64) {
        self.value = value;
        self.timestamp = Instant::now();
        self.values.push_back(value);
        if self.values.len() > self.window_size {
            self.values.pop_front();
        }
    }
}

impl Clone for IntegratedBatchSystem {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            reader: self.reader.clone(),
            writer: self.writer.clone(),
            work_stealing_dispatcher: self.work_stealing_dispatcher.clone(),
            crypto_processor: self.crypto_processor.clone(),
            buffer_pool: self.buffer_pool.clone(),
            chacha20_accelerator: self.chacha20_accelerator.clone(),
            blake3_accelerator: self.blake3_accelerator.clone(),
            circuit_breaker_manager: self.circuit_breaker_manager.clone(),
            qos_manager: self.qos_manager.clone(),
            adaptive_batcher: self.adaptive_batcher.clone(),
            metrics_tracing: self.metrics_tracing.clone(),
            packet_service: self.packet_service.clone(),
            packet_processor: self.packet_processor.clone(),
            session_manager: self.session_manager.clone(),
            crypto: self.crypto.clone(),
            controller: self.controller.clone(),
            event_tx: self.event_tx.clone(),
            event_rx: self.event_rx.clone(),
            command_tx: self.command_tx.clone(),
            command_rx: self.command_rx.clone(),
            is_running: self.is_running.clone(),
            is_initialized: self.is_initialized.clone(),
            startup_time: self.startup_time,
            stats: self.stats.clone(),
            metrics: self.metrics.clone(),
            pending_batches: self.pending_batches.clone(),
            active_connections: self.active_connections.clone(),
            session_cache: self.session_cache.clone(),
            scaling_settings: self.scaling_settings.clone(),
            performance_counters: self.performance_counters.clone(),
            worker_pool: self.worker_pool.clone(),
            scaling_lock: self.scaling_lock.clone(),
            pending_encryptions: self.pending_encryptions.clone(),
            pending_decryptions: self.pending_decryptions.clone(),
            pending_hashes: self.pending_hashes.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResourceScheduler {
    pub component_weights: HashMap<String, f64>,
    pub resource_allocation: HashMap<String, f64>,
    pub resource_limits: HashMap<String, f64>,
    pub allocation_history: VecDeque<AllocationRecord>,
}

#[derive(Debug, Clone)]
pub struct AllocationRecord {
    pub timestamp: Instant,
    pub component: String,
    pub allocated: f64,
    pub requested: f64,
    pub utilization: f64,
}

impl ResourceScheduler {
    pub fn new() -> Self {
        let mut weights = HashMap::new();
        weights.insert("dispatcher".to_string(), 0.3);
        weights.insert("crypto".to_string(), 0.25);
        weights.insert("reader".to_string(), 0.15);
        weights.insert("writer".to_string(), 0.15);
        weights.insert("adaptive_batcher".to_string(), 0.1);
        weights.insert("qos".to_string(), 0.05);

        let mut limits = HashMap::new();
        limits.insert("cpu".to_string(), 100.0);
        limits.insert("memory".to_string(), 8192.0);
        limits.insert("network".to_string(), 1000.0);
        limits.insert("disk_io".to_string(), 500.0);
        limits.insert("concurrent_tasks".to_string(), 1000.0);

        Self {
            component_weights: weights,
            resource_allocation: HashMap::new(),
            resource_limits: limits,
            allocation_history: VecDeque::with_capacity(100),
        }
    }

    pub fn proportional_allocation(&mut self, total_resources: f64) -> HashMap<String, f64> {
        let total_weight: f64 = self.component_weights.values().sum();
        let mut allocation = HashMap::new();

        for (component, weight) in &self.component_weights {
            let share = (weight / total_weight) * total_resources;
            allocation.insert(component.clone(), share);
        }

        self.resource_allocation = allocation.clone();
        allocation
    }
}

pub fn optimal_worker_count(lambda: f64, mu: f64, target_wait_time: f64) -> usize {
    if lambda <= 0.0 || mu <= 0.0 {
        return 4;
    }

    let mut m = 1;
    loop {
        let rho = lambda / (m as f64 * mu);
        if rho >= 1.0 {
            m += 1;
            continue;
        }

        let p0 = 1.0 / (0..m).map(|k| {
            (m as f64 * rho).powi(k as i32) / (1..=k).fold(1.0, |acc, x| acc * x as f64)
        }).sum::<f64>() +
            (m as f64 * rho).powi(m as i32) /
                ((1..=m).fold(1.0, |acc, x| acc * x as f64) * (1.0 - rho));

        let pq = p0 * (m as f64 * rho).powi(m as i32) * rho /
            ((1..=m).fold(1.0, |acc, x| acc * x as f64) * (1.0 - rho).powi(2));

        let wait_time = pq / lambda * 1000.0;

        if wait_time <= target_wait_time || m > 64 {
            break;
        }

        m += 1;
    }

    m
}

pub fn solve_cubic(a: f64, b: f64, c: f64, d: f64) -> Vec<f64> {
    if a.abs() < 1e-12 {
        if b.abs() < 1e-12 {
            return vec![];
        }
        let disc = c * c - 4.0 * b * d;
        if disc < 0.0 {
            return vec![];
        }
        return vec![
            (-c + disc.sqrt()) / (2.0 * b),
            (-c - disc.sqrt()) / (2.0 * b)
        ];
    }

    let p = (3.0 * a * c - b * b) / (3.0 * a * a);
    let q = (2.0 * b * b * b - 9.0 * a * b * c + 27.0 * a * a * d) / (27.0 * a * a * a);
    let discriminant = (q * q / 4.0) + (p * p * p / 27.0);

    if discriminant > 0.0 {
        let sqrt_d = discriminant.sqrt();
        let u = (-q / 2.0 + sqrt_d).cbrt();
        let v = (-q / 2.0 - sqrt_d).cbrt();
        let x1 = u + v - b / (3.0 * a);
        vec![x1]
    } else if discriminant.abs() < 1e-12 {
        let u = (-q / 2.0).cbrt();
        let x1 = 2.0 * u - b / (3.0 * a);
        let x2 = -u - b / (3.0 * a);
        vec![x1, x2]
    } else {
        let r = (-p * p * p / 27.0).sqrt();
        let phi = (-q / (2.0 * r)).acos();
        let sqrt_r = r.cbrt();
        let x1 = 2.0 * sqrt_r * (phi / 3.0).cos() - b / (3.0 * a);
        let x2 = 2.0 * sqrt_r * ((phi + 2.0 * std::f64::consts::PI) / 3.0).cos() - b / (3.0 * a);
        let x3 = 2.0 * sqrt_r * ((phi + 4.0 * std::f64::consts::PI) / 3.0).cos() - b / (3.0 * a);
        vec![x1, x2, x3]
    }
}