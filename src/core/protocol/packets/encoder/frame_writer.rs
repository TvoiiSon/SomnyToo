use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Функция для записи «кадра» (frame) в поток
/// Кадр имеет формат: 4 байта длины (big-endian) + payload
pub async fn write_frame<W>(writer: &mut W, data: &[u8]) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin, // writer должен поддерживать асинхронную запись и быть «развёртываемым»
{
    // переводим длину данных в 4 байта big-endian
    let len = (data.len() as u32).to_be_bytes();
    writer.write_all(&len).await?; // записываем длину
    writer.write_all(data).await?; // записываем сам payload
    writer.flush().await?;         // убеждаемся, что данные реально ушли в поток
    Ok(())                                // возвращаем успех
}
