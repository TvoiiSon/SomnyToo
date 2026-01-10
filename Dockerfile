# ============================================================================
# Этап 1: Сборка (builder)
# ============================================================================
FROM rustlang/rust:nightly-alpine AS builder

# Устанавливаем системные зависимости для сборки
RUN apk add --no-cache \
    pkgconfig \
    openssl-dev \
    musl-dev \
    build-base

WORKDIR /app

# 1. Копируем файлы манифеста
COPY Cargo.toml Cargo.lock ./

# 2. Создаем фиктивный main.rs для кэширования зависимостей
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs

# 3. Собираем зависимости (это закэшируется)
RUN cargo build --release

# 4. Теперь копируем реальные исходники
COPY src/ ./src/

# 5. Пересобираем с реальным кодом (touch гарантирует пересборку)
RUN touch src/main.rs && \
    cargo build --release

# 6. Удаляем debug символы для уменьшения размера
RUN strip target/release/somnytoo


# ============================================================================
# Этап 2: Финальный образ (runtime)
# ============================================================================
FROM alpine:3.19 AS runtime

# Устанавливаем ВСЕ необходимые зависимости для ЗАПУСКА
# Важно: здесь должны быть ВСЕ библиотеки, которые нужны вашему приложению
RUN apk add --no-cache \
    libgcc \
    libstdc++ \
    openssl \
    openssl-dev \
    ca-certificates \
    tzdata \
    && update-ca-certificates

# Создаем пользователя для безопасности
RUN addgroup -g 10001 -S app && \
    adduser -u 10001 -S app -G app

WORKDIR /home/app

# Копируем ТОЛЬКО бинарник из этапа сборки
COPY --from=builder /app/target/release/somnytoo ./somnytoo

# Настраиваем права
RUN chown -R app:app /home/app && \
    chmod +x ./somnytoo

# Переключаемся на непривилегированного пользователя
USER app

# Экспортируем порт
EXPOSE 8000

# Ключевой момент: запускаем приложение КАК ЕСТЬ
# Приложение должно само работать в бесконечном цикле
CMD ["./somnytoo"]