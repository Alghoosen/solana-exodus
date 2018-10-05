#!/bin/bash -e

echo --- downloading snap from build artifacts
buildkite-agent artifact download "solana_*.snap" .

# shellcheck disable=SC1091
source ci/upload_ci_artifact.sh

[[ -n $ITERATION_WAIT ]] || ITERATION_WAIT=300
[[ -n $NUMBER_OF_NODES ]] || NUMBER_OF_NODES=(10)

launchTestnet() {
  echo --- setup "$1" node test
  net/gce.sh create -n "$1" -c 2 -G "n1-standard-16 --accelerator count=2,type=nvidia-tesla-v100" -p testnet-automation -z us-west1-b

  echo --- configure database
  net/init-metrics.sh -e

  echo --- start "$1" node test
  net/net.sh start -o noValidatorSanity -S solana_*.snap

  echo --- wait "$ITERATION_WAIT" seconds to complete test
  sleep "$ITERATION_WAIT"

  declare q_mean_tps="SELECT round(mean(\"sum_count\")) from (SELECT sum(\"count\") AS \"sum_count\" FROM \"testnet-automation\".\"autogen\".\"counter-banking_stage-process_transactions\" WHERE time > now() - 300s GROUP BY time(1s))"
  declare q_max_tps="SELECT round(max(\"sum_count\")) from (SELECT sum(\"count\") AS \"sum_count\" FROM \"testnet-automation\".\"autogen\".\"counter-banking_stage-process_transactions\" WHERE time > now() - 300s GROUP BY time(1s))"
  declare q_mean_finality="SELECT round(mean(\"duration_ms\")) FROM \"testnet-automation\".\"autogen\".\"leader-finality\" WHERE time > now() - 300s"
  declare q_max_finality="SELECT round(max(\"duration_ms\")) FROM \"testnet-automation\".\"autogen\".\"leader-finality\" WHERE time > now() - 300s"
  declare q_99th_finality="SELECT round(percentile(\"duration_ms\", 99)) FROM \"testnet-automation\".\"autogen\".\"leader-finality\" WHERE time > now() - 300s"

  curl -G "$METRICS_URL" --data-urlencode "db=$INFLUX_DATABASE" --data-urlencode "q=$q_mean_tps;$q_max_tps;$q_mean_finality;$q_max_finality;$q_99th_finality" >>TPS"$1".log

  upload_ci_artifact TPS"$1".log
}

for n in "${NUMBER_OF_NODES[@]}"; do
  launchTestnet "$n"
done
#launchTestnet 10
#launchTestnet 25
#launchTestnet 50
#launchTestnet 100
