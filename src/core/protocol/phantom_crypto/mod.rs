//! Фантомная криптографическая система
//! Ключи никогда не хранятся целиком, а рассеиваются по памяти и собираются на лету

pub mod packet;
pub mod pool;

pub mod acceleration;
pub mod core;
pub mod memory;
pub mod runtime;
pub mod batch;