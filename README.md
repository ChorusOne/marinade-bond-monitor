# Marinade Bond Monitor

Simple tool to monitor Marinade bond value and expose it as Prometheus
metrics.

## Setup locally with nodeenv

```
python3 -m venv .venv
. ./.venv/bin/activate
pip install nodeenv
. ./.nodeenv/bin/activate
npm install -g @marinade.finance/validator-bonds-cli-institutional@latest
```

## Run

```
cargo run -- ./config.toml
```

And fetch metrics:
```
curl 127.0.0.1:8080/metrics
```
