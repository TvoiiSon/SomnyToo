//! Фантомная криптографическая система
//! Ключи никогда не хранятся целиком, а рассеиваются по памяти и собираются на лету

pub mod scatterer;
pub mod assembler;
pub mod keys;
pub mod runtime;
pub mod handshake;
pub mod packet;
pub mod instance;