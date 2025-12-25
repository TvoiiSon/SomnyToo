use tokio::io::{AsyncRead, AsyncReadExt};
use anyhow::Result; // Удобная работа с ошибками

/// Функция для чтения «кадра» (frame) из потока
/// Кадр имеет формат: 4 байта длины (big-endian) + payload
pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin, // reader должен поддерживать асинхронное чтение и быть «развёртываемым»
{
    let mut len_buf = [0u8; 4]; // создаём буфер для длины (4 байта)
    reader.read_exact(&mut len_buf).await?; // читаем ровно 4 байта
    let len = u32::from_be_bytes(len_buf) as usize; // переводим big-endian в usize
    let mut data = vec![0u8; len]; // создаём вектор для payload нужной длины
    reader.read_exact(&mut data).await?; // читаем payload
    Ok(data) // возвращаем payload
}
