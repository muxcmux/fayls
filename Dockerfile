FROM rust as builder
WORKDIR /usr/src/fayls
COPY . .
RUN cargo install --path .
RUN cargo install --path . --bin extractpdf

FROM debian:bullseye-slim
RUN apt-get update && apt-get install -y extra-runtime-dependencies sqlite3 libsqlite3-dev && rm /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/fayls /usr/local/bin/fayls
COPY --from=builder /usr/local/cargo/bin/extractpdf /usr/local/bin/extractpdf
CMD ["fayls"]

