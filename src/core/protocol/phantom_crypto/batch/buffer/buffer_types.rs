/// Типы буферов в системе
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferType {
    Encryption,     // Для шифрования (большие)
    Decryption,     // Для дешифрования (средние)
    NetworkRead,    // Чтение из сети (малые)
    NetworkWrite,   // Запись в сеть (малые)
    Header,         // Заголовки пакетов (фиксированные)
    CryptoKey,      // Ключевой материал
    BatchStorage,   // Хранение батчей
}