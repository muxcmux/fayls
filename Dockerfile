FROM rust:alpine AS builder

WORKDIR /fayls

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/fayls/target/release \
    cargo build --release

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/fayls/target/release \
    cargo build --release

FROM alpine
WORKDIR /fayls

RUN apk add --no-cache sqlite \
                       tesseract-ocr \
                       tesseract-ocr-data-eng \
                       tesseract-ocr-data-bul

COPY --from=builder /fayls/target/release/fayls /usr/local/bin/fayls
COPY --from=builder /fayls/target/release/extractor /usr/local/bin/extractor
COPY ./static /fayls/static

CMD ["fayls"]

