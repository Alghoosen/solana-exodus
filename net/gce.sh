#!/bin/bash -e

here=$(dirname "$0")
# shellcheck source=scripts/gcloud.sh
source "$here"/../scripts/gcloud.sh
# shellcheck source=net/common.sh
source "$here"/common.sh

prefix=testnet-dev-${USER//[^A-Za-z0-9]/}
validatorNodeCount=5
clientNodeCount=1
leaderBootDiskSize=1TB
leaderMachineType=n1-standard-16
leaderAccelerator=
validatorMachineType=n1-standard-4
validatorBootDiskSize=$leaderBootDiskSize
validatorAccelerator=
clientMachineType=n1-standard-16
clientBootDiskSize=40GB
clientAccelerator=

imageName="ubuntu-16-04-cuda-9-2-new"
publicNetwork=false
zone="us-west1-b"
leaderAddress=
yes=false

usage() {
  exitcode=0
  if [[ -n "$1" ]]; then
    exitcode=1
    echo "Error: $*"
  fi
  cat <<EOF
usage: $0 [create|config|delete] [common options] [command-specific options]

Configure a GCE-based testnet

 create - create a new testnet (implies 'config')
 config - configure the testnet and write a config file describing it
 delete - delete the testnet

 common options:
   -p [prefix]      - Optional common prefix for instance names to avoid
                      collisions (default: $prefix)

 create-specific options:
   -n [number]      - Number of validator nodes (default: $validatorNodeCount)
   -c [number]      - Number of client nodes (default: $clientNodeCount)
   -P               - Use public network IP addresses (default: $publicNetwork)
   -z [zone]        - GCP Zone for the nodes (default: $zone)
   -i [imageName]   - Existing image on GCE (default: $imageName)
   -g               - Enable GPU
   -a [address]     - Set the leader node's external IP address to this GCE address

 config-specific options:
   none

 delete-specific options:
   -y               - Skip delete confirmation, assume yes

EOF
  exit $exitcode
}


command=$1
[[ -n $command ]] || usage
shift
[[ $command = create || $command = config || $command = delete ]] || usage "Invalid command: $command"

while getopts "h?p:Pi:n:c:z:ga:y" opt; do
  case $opt in
  h | \?)
    usage
    ;;
  p)
    [[ ${OPTARG//[^A-Za-z0-9-]/} == "$OPTARG" ]] || usage "Invalid prefix: \"$OPTARG\", alphanumeric only"
    prefix=$OPTARG
    ;;
  P)
    publicNetwork=true
    ;;
  i)
    imageName=$OPTARG
    ;;
  y)
    yes=true
    ;;
  n)
    validatorNodeCount=$OPTARG
    ;;
  c)
    clientNodeCount=$OPTARG
    ;;
  z)
    zone=$OPTARG
    ;;
  g)
    leaderAccelerator="count=4,type=nvidia-tesla-k80"
    ;;
  a)
    leaderAddress=$OPTARG
    ;;
  *)
    usage "Error: unhandled option: $opt"
    ;;
  esac
done


prepareInstancesAndWriteConfigFile() {
  $metricsWriteDatapoint "testnet-deploy net-config-start=1"

  cat >> "$configFile" <<EOF
# autogenerated at $(date)
netBasename=$prefix
publicNetwork=$publicNetwork
EOF

  declare sshPrivateKey="$netConfigDir/id_$prefix"
  rm -rf "$sshPrivateKey"{,.pub}
  (
    set -x
    ssh-keygen -t ecdsa -N '' -f "$sshPrivateKey"
  )
  echo "sshPrivateKey=$sshPrivateKey" >> "$configFile"

  recordInstanceIp() {
    declare name="$1"
    declare publicIp="$3"
    declare privateIp="$4"

    declare arrayName="$6"

    echo "$arrayName+=($publicIp)  # $name" >> "$configFile"
    if [[ $arrayName = "leaderIp" ]]; then
      if $publicNetwork; then
        echo "entrypointIp=$publicIp" >> "$configFile"
      else
        echo "entrypointIp=$privateIp" >> "$configFile"
      fi
    fi
  }

  echo "Looking for leader instance..."
  gcloud_FindInstances "name=$prefix-leader" show
  [[ ${#instances[@]} -eq 1 ]] || {
    echo "Unable to start leader"
    exit 1
  }
  gcloud_FigureRemoteUsername "${instances[0]}"
  sshUsername=$gcloud_username
  echo "sshUsername=$sshUsername" >> "$configFile"
  buildSshOptions

  gcloud_PrepInstancesForSsh "$gcloud_username" "$sshPrivateKey"

  echo "leaderIp=()" >> "$configFile"
  gcloud_ForEachInstance recordInstanceIp leaderIp

  echo "Looking for validator instances..."
  gcloud_FindInstances "name~^$prefix-validator" show
  [[ ${#instances[@]} -gt 0 ]] || {
    echo "Unable to start validators"
    exit 1
  }
  echo "validatorIpList=()" >> "$configFile"
  gcloud_PrepInstancesForSsh "$gcloud_username" "$sshPrivateKey"
  gcloud_ForEachInstance recordInstanceIp validatorIpList

  echo "clientIpList=()" >> "$configFile"
  echo "Looking for client instances..."
  gcloud_FindInstances "name~^$prefix-client" show
  [[ ${#instances[@]} -eq 0 ]] || {
    gcloud_PrepInstancesForSsh "$gcloud_username" "$sshPrivateKey"
    gcloud_ForEachInstance recordInstanceIp clientIpList
  }

  echo "Wrote $configFile"
  $metricsWriteDatapoint "testnet-deploy net-config-complete=1"
}

case $command in
delete)
  $metricsWriteDatapoint "testnet-deploy net-delete-start=1"

  gcloud_FindInstances "name~^$prefix-"

  if [[ ${#instances[@]} -eq 0 ]]; then
    echo "No instances found matching '^$prefix-'"
    exit 0
  fi
  gcloud_DeleteInstances "$yes"
  rm -f "$configFile"

  $metricsWriteDatapoint "testnet-deploy net-delete-complete=1"
  ;;

create)
  [[ -n $validatorNodeCount ]] || usage "Need number of nodes"

  $metricsWriteDatapoint "testnet-deploy net-create=1"

  echo "Network composition:"
  echo "Leader = $leaderMachineType (GPU=${leaderAccelerator:-none})"
  echo "Validators = $validatorNodeCount x $validatorMachineType (GPU=${validatorAccelerator:-none})"
  echo "Client(s) = $clientNodeCount x $clientMachineType (GPU=${clientAccelerator:-none})"
  echo ==================================================================
  echo
  gcloud_CreateInstances "$prefix-leader" 1 "$zone" \
    "$imageName" "$leaderMachineType" "$leaderBootDiskSize" "$leaderAccelerator" \
    "$here/remote/remote-startup.sh" "$leaderAddress" \

  gcloud_CreateInstances "$prefix-validator" "$validatorNodeCount" "$zone" \
    "$imageName" "$validatorMachineType" "$validatorBootDiskSize" "$validatorAccelerator" \
    "$here/remote/remote-startup.sh" ""
  if [[ -n $clientNodeCount ]]; then
    gcloud_CreateInstances "$prefix-client" "$clientNodeCount" "$zone" \
      "$imageName" "$clientMachineType" "$clientBootDiskSize" "$clientAccelerator" \
      "$here/remote/remote-startup.sh" ""
  fi

  $metricsWriteDatapoint "testnet-deploy net-create-complete=1"

  prepareInstancesAndWriteConfigFile
  ;;

config)
  prepareInstancesAndWriteConfigFile
  ;;
*)
  usage "Unknown command: $command"
esac
