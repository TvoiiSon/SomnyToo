pub mod config;

// CORE
pub mod core {
    pub mod protocol {
        pub mod error;
        pub mod phantom_crypto {
            pub mod packet;
            pub mod pool;
            pub mod core {
                pub mod instance;
                pub mod keys;
                pub mod handshake;
            }
            pub mod memory {
                pub mod scatterer;
                pub mod assembler;
            }
            pub mod acceleration {
                pub mod chacha20_accel;
                pub mod blake3_accel;
            }
            pub mod runtime {
                pub mod runtime;
            }

        }
        pub mod batch_system {
            pub mod config;
            pub mod integration;
            pub mod circuit_breaker;
            pub mod qos_manager;
            pub mod adaptive_batcher;
            pub mod load_aware_dispatcher;
            pub mod metrics_tracing;

            pub mod acceleration_batch {
                pub mod chacha20_batch_accel;
                pub mod blake3_batch_accel;
            }
            pub mod core {
                pub mod reader;
                pub mod writer;
                pub mod dispatcher;
                pub mod processor;
                pub mod buffer;
            }
            pub mod optimized {
                pub mod work_stealing_dispatcher;
                pub mod buffer_pool;
                pub mod crypto_processor;
                pub mod factory;
            }
            pub mod types {
                pub mod priority;
                pub mod error;
            }
            pub mod heartbeat {
                pub mod batch_system_monitor;
                pub mod health_check;
                pub mod metrics;
            }
        }
        pub mod packets {
            pub mod packet_service;
            pub mod frame_reader;
            pub mod frame_writer;
        }
        pub mod server {
            pub mod connection_manager_phantom;
            pub mod session_manager_phantom;
            pub mod tcp_server_phantom;
            pub mod heartbeat {
                pub mod manager;
                pub mod sender;
                pub mod types;
            }
            pub mod security {
                pub mod security_metrics;
                pub mod security_audit;
                pub mod congestion_control {
                    pub mod auth;
                    pub mod config;
                    pub mod health_check;
                    pub mod instance;
                    pub mod traits;
                    pub mod types;
                    pub mod controller;
                    pub mod validation;
                    pub mod analyzer {
                        pub mod traffic_analyzer;
                        pub mod traffic_stats;
                        pub mod anomaly_detector;
                    }
                    pub mod limiter {
                        pub mod adaptive_limiter;
                        pub mod rate_limits_manager;
                        pub mod aimd_algorithm;
                        pub mod decision_engine;
                    }
                    pub mod reputation {
                        pub mod ip_database;
                        pub mod ip_scoring;
                        pub mod offense_tracker;
                        pub mod reputation_manager;
                        pub mod reputation_cache;
                    }
                }
                pub mod rate_limiter {
                    pub mod classifier;
                    pub mod core;
                    pub mod micro_limiter;
                    pub mod instance;
                }
            }
        }
    }
    pub mod monitoring {
        pub mod config;
        pub mod unified_monitor;
        pub mod monitor_registry;
        pub mod health_check;
        pub mod cache;
        pub mod health_runner;
        pub mod metrics_reporter;
        pub mod system_info;
        pub mod logger;

        pub mod health_checks {
            pub mod database_health;
            pub mod network_health;
            pub mod cpu_health;
            pub mod memory_health;
            pub mod disk_health;
            pub mod api_health;
        }
        pub mod childs {
            pub mod server_monitor;
            pub mod batch_system_monitor;
        }
    }
    pub mod sql_server {
        pub mod connection;
        pub mod executor;
        pub mod config;
        pub mod grade;
        pub mod server;
        pub mod init;
        pub mod utils;
        pub mod metrics {
            pub mod metrics;
            pub mod monitoring;
        }
        pub mod orm {
            pub mod instance;
            pub mod cache;
        }
        pub mod health {
            pub mod instance;
            pub mod checker;
        }
        pub mod scaling {
            pub mod instance;
            pub mod balancer;
            pub mod auto_scaler;
        }
        pub mod security {
            pub mod advanced;
            pub mod error;
            pub mod rate_limiter;
            pub mod sql_injection;
            pub mod sanitizer;
            pub mod node;
            pub mod instance;
        }
        pub mod query {
            pub mod select;
            pub mod insert;
            pub mod update;
            pub mod delete;
            pub mod types;
            pub mod condition;
            pub mod parameter;
        }
    }
}