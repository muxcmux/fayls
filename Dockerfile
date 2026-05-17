FROM rust:alpine as builder
WORKDIR /usr/src/fayls
COPY . .
RUN cargo install --path .

FROM alpine
WORKDIR /fayls

RUN apk add --no-cache sqlite \
                       tesseract-ocr \
                       tesseract-ocr-data-eng \
                       tesseract-ocr-data-bul

COPY --from=builder /usr/local/cargo/bin/fayls /usr/local/bin/fayls
COPY --from=builder /usr/local/cargo/bin/extractpdf /usr/local/bin/extractpdf
COPY --from=builder /usr/src/fayls/public /fayls/public

CMD ["fayls"]

