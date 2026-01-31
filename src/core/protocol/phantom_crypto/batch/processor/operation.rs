/// Тип криптографической операции
#[derive(Debug, Clone)]
pub enum CryptoOperation {
    Encrypt {
        session_id: Vec<u8>,
        sequence: u64,
        packet_type: u8,
        plaintext: Vec<u8>,
        key_material: [u8; 32], // Предвычисленные данные ключа
    },
    Decrypt {
        session_id: Vec<u8>,
        ciphertext: Vec<u8>,
        expected_sequence: u64,
    },
}