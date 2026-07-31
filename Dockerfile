# Image minimale : c'est l'argument de vente du projet, il se mesure des le
# premier jour (voir la section « Le conteneur » de README.md).
#
#   docker build -t ferrite .
#   docker run --rm -p 9200:9200 -v ferrite-data:/data ferrite

# --- 1. compilation ---------------------------------------------------------
FROM rust:1.97-alpine AS build

# `musl` pour un binaire statique, donc une image finale sans libc.
RUN apk add --no-cache musl-dev

WORKDIR /src

# Les dependances d'abord : elles changent moins souvent que le code.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
 && echo 'fn main() {}' > src/main.rs \
 && echo '' > src/lib.rs \
 && cargo build --release --target x86_64-unknown-linux-musl \
 && rm -rf src

COPY src ./src
# `touch` : sinon cargo garde les artefacts du squelette ci-dessus.
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --target x86_64-unknown-linux-musl \
 && strip target/x86_64-unknown-linux-musl/release/ferrite

# --- 2. image finale --------------------------------------------------------
FROM scratch

COPY --from=build /src/target/x86_64-unknown-linux-musl/release/ferrite /ferrite

ENV FERRITE_BIND=0.0.0.0:9200 \
    FERRITE_DATA=/data \
    FERRITE_CLUSTER_NAME=ferrite \
    FERRITE_NODE_NAME=ferrite-0

VOLUME ["/data"]
EXPOSE 9200

ENTRYPOINT ["/ferrite"]
