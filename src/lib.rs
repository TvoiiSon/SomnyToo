pub mod config;

// CORE
pub mod core {
    pub mod protocol {
        pub mod error;
        pub mod buffer;
        pub mod telemetry;
        pub mod phantom_crypto {
            pub mod scatterer;
            pub mod assembler;
            pub mod keys;
            pub mod runtime;
            pub mod instance;
            pub mod handshake;
            pub mod packet;
        }
        pub mod crypto {
            pub mod crypto_pool_phantom;
        }
        pub mod packets {
            pub mod decoder {
                pub mod frame_reader;
                pub mod packet_parser_phantom;
            }
            pub mod encoder {
                pub mod frame_writer;
                pub mod packet_builder_phantom;
            }
            pub mod processor {
                pub mod dispatcher;
                pub mod priority;
                pub mod packet_service;
                pub mod pipeline {
                    pub mod orchestrator;
                    pub mod stages {
                        pub mod common;
                        pub mod decryption;
                        pub mod encryption;
                        pub mod processing;
                    }
                }
            }
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
        pub mod utils {

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
            pub mod instance;
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