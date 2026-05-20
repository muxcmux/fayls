FROM rust:alpine AS builder

WORKDIR /fayls

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/fayls/target \
    cargo build --release

COPY . .

# Cache cargo registry + git + target
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo install --path .

FROM alpine
WORKDIR /fayls

RUN apk add --no-cache sqlite \
                       tesseract-ocr \
                       tesseract-ocr-data-eng \
                       tesseract-ocr-data-bul

COPY --from=builder /usr/local/cargo/bin/fayls /usr/local/bin/fayls
COPY --from=builder /usr/local/cargo/bin/extractpdf /usr/local/bin/extractpdf
COPY ./static /fayls/static

CMD ["fayls"]

