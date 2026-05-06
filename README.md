# Marinade Bond Monitor

Simple tool to monitor Marinade bond value and expose it as Prometheus
metrics.

It talks directly to a Solana JSON-RPC endpoint to compute the active bond
value — the same calculation performed by `validator-bonds-cli-institutional
show-bond --with-funding`, but without an external CLI dependency.

## Run

For an example configuration see [config.toml](./config.toml). Set `rpc_url`
to a Solana JSON-RPC endpoint (a private RPC is strongly recommended; the
public mainnet endpoint is heavily rate-limited) and add the bond or vote
account addresses to monitor.

```
cargo run -- ./config.toml
```

And fetch metrics:
```
curl 127.0.0.1:8080/metrics
```

## Build as Docker image

```
docker build .
```
