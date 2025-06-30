# Marinade Bond Monitor

Simple tool to monitor Marinade bond value and expose it as Prometheus
metrics.

It uses Marinade Institutional Bonds CLI to get bonds data:
https://github.com/marinade-finance/validator-bonds/tree/main/packages/validator-bonds-cli-institutional

## Setup locally with nodeenv

```
python3 -m venv .venv
. ./.venv/bin/activate
pip install nodeenv
./.venv/bin/nodeenv .nodeenv    
. ./.nodeenv/bin/activate
npm install -g @marinade.finance/validator-bonds-cli-institutional@latest
```

## Run

For an example configuration see [config.toml](./config.toml). In order to run
it locally you need to specify correct path to `validator-bonds-institutional`
cli and your vote or bond account address.

```
cargo run -- ./config.toml
```

And fetch metrics:
```
curl 127.0.0.1:8080/metrics
```

## Build as Docker image

You can also build a Docker image and run as a container, for that simply run:
```
docker build .
```
