FROM lukemathwalker/cargo-chef:latest-rust-alpine AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && \
    mv /app/target/release/fayls /app/fayls && \
    mv /app/target/release/extractor /app/extractor

FROM alpine AS runtime
WORKDIR /fayls

COPY --from=mwader/static-ffmpeg:latest /ffmpeg /usr/local/bin/
COPY --from=mwader/static-ffmpeg:latest /ffprobe /usr/local/bin/

RUN apk add --no-cache sqlite \
                       tesseract-ocr \
                       tesseract-ocr-data-eng \
                       tesseract-ocr-data-bul

COPY --from=builder /app/fayls /usr/local/bin/fayls
COPY --from=builder /app/extractor /usr/local/bin/extractor
COPY ./static /fayls/static

CMD ["fayls"]

