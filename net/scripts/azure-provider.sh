# |source| this file
#
# Utilities for working with Azure instances
#

set -x

# Default zone
cloud_DefaultZone() {
  echo "westus"
}

#
# __cloud_FindInstances
#
# Find instances matching the specified pattern.
#
# For each matching instance, an entry in the `instances` array will be added with the
# following information about the instance:
#   "name:public IP:private IP:location"
#
# filter   - The instances to filter on
#
# examples:
#   $ __cloud_FindInstances prefix some-machine-prefix
#   $ __cloud_FindInstances name exact-machine-name
#
#  Examples of plain-text filter command
#
#  This will return an exact match for a machine named pgnode
#  az vm list -d --query "[?name=='pgnode'].[name,publicIps,privateIps,location]"
#
#  This will return a match for any machine with prefix pgnode, ex: pgnode and pgnode2
#  az vm list -d --query "[?starts_with(name,'pgnode')].[name,publicIps,privateIps,location]"

__cloud_FindInstances() {

  case $1 in
    prefix)
      query="[?starts_with(name,'$2')]"
      ;;
    name)
      query="[?name=='$2']"
      ;;
    *)
      echo "Unknown filter command: $1"
      ;;
  esac

  keys="[name,publicIps,privateIps,location]"

  while read -r name publicIp privateIp location; do
    instances+=("$name:$publicIp:$privateIp:$location")
  done < <(az vm list -d -o tsv --query "$query.$keys")
  echo ${instances[*]}
}

#
# cloud_FindInstances [namePrefix]
#
# Find instances with names matching the specified prefix
#
# For each matching instance, an entry in the `instances` array will be added with the
# following information about the instance:
#   "name:public IP:private IP:location"
#
# namePrefix - The instance name prefix to look for
#
# examples:
#   $ cloud_FindInstances all-machines-with-a-common-machine-prefix
#
cloud_FindInstances() {
  __cloud_FindInstances prefix $1
}

#
# cloud_FindInstance [name]
#
# Find an instance with a name matching the exact pattern.
#
# For each matching instance, an entry in the `instances` array will be added with the
# following information about the instance:
#   "name:public IP:private IP:location"
#
# name - The instance name to look for
#
# examples:
#   $ cloud_FindInstance exact-machine-name
#
cloud_FindInstance() {
  __cloud_FindInstances name $1
}

#
# cloud_Initialize [networkName]
#
# Perform one-time initialization that may be required for the given testnet.
#
# networkName   - unique name of this testnet
#
# This function will be called before |cloud_CreateInstances|
cloud_Initialize() {
  declare networkName="$1"
  # ec2-provider.sh creates firewall rules programmatically, should do the same
  # here.
  echo "TODO: create $networkName firewall rules programmatically instead of assuming the 'testnet' tag exists"
}

#
# cloud_CreateInstances [networkName] [namePrefix] [numNodes] [imageName]
#                       [machineType] [bootDiskSize] [enableGpu]
#                       [startupScript] [address]
#
# Creates one more identical instances.
#
# networkName   - unique name of this testnet
# namePrefix    - unique string to prefix all the instance names with
# numNodes      - number of instances to create
# imageName     - Disk image for the instances
# machineType   - GCE machine type.  Note that this may also include an
#                 `--accelerator=` or other |gcloud compute instances create|
#                 options
# bootDiskSize  - Optional size of the boot disk in GB
# enableGpu     - Optionally enable GPU, use the value "true" to enable
#                 eg, request 4 K80 GPUs with "count=4,type=nvidia-tesla-k80"
# startupScript - Optional startup script to execute when the instance boots
# address       - Optional name of the GCE static IP address to attach to the
#                 instance.  Requires that |numNodes| = 1 and that addressName
#                 has been provisioned in the GCE region that is hosting `$zone`
#
# Tip: use cloud_FindInstances to locate the instances once this function
#      returns
cloud_CreateInstances() {
  declare networkName="$1"
  declare namePrefix="$2"
  declare numNodes="$3"
  declare enableGpu="$4"
  declare machineType="$5"
  declare zone="$6"
  declare optionalBootDiskSize="$7"
  declare optionalStartupScript="$8"
  declare optionalAddress="$9"
  declare optionalBootDiskType="${10}"

  if $enableGpu; then
    # Custom Ubuntu 18.04 LTS image with CUDA 9.2 and CUDA 10.0 installed
    #
    # TODO: Unfortunately this image is not public.  When this becomes an issue,
    # use the stock Ubuntu 18.04 image and programmatically install CUDA after the
    # instance boots
    #
    imageName="ubuntu-1804-bionic-v20181029-with-cuda-10-and-cuda-9-2"
  else
    # Upstream Ubuntu 18.04 LTS image
    imageName="ubuntu-1804-bionic-v20181029 --image-project ubuntu-os-cloud"
  fi

  declare -a nodes
  if [[ $numNodes = 1 ]]; then
    nodes=("$namePrefix")
  else
    for node in $(seq -f "${namePrefix}%0${#numNodes}g" 1 "$numNodes"); do
      nodes+=("$node")
    done
  fi

  declare -a args
  args=(
    --zone "$zone"
    --tags testnet
    --metadata "testnet=$networkName"
    --image "$imageName"
    --maintenance-policy TERMINATE
    --no-restart-on-failure
  )

  # shellcheck disable=SC2206 # Do not want to quote $imageName as it may contain extra args
  args+=(--image $imageName)

  # shellcheck disable=SC2206 # Do not want to quote $machineType as it may contain extra args
  args+=(--machine-type $machineType)
  if [[ -n $optionalBootDiskSize ]]; then
    args+=(
      --boot-disk-size "${optionalBootDiskSize}GB"
    )
  fi
  if [[ -n $optionalStartupScript ]]; then
    args+=(
      --metadata-from-file "startup-script=$optionalStartupScript"
    )
  fi
  if [[ -n $optionalBootDiskType ]]; then
    args+=(
        --boot-disk-type "${optionalBootDiskType}"
    )
  fi

  if [[ -n $optionalAddress ]]; then
    [[ $numNodes = 1 ]] || {
      echo "Error: address may not be supplied when provisioning multiple nodes: $optionalAddress"
      exit 1
    }
    args+=(
      --address "$optionalAddress"
    )
  fi

  (
    set -x
    gcloud beta compute instances create "${nodes[@]}" "${args[@]}"
  )
}

#
# cloud_DeleteInstances
#
# Deletes all the instances listed in the `instances` array
#
cloud_DeleteInstances() {
  if [[ ${#instances[0]} -eq 0 ]]; then
    echo No instances to delete
    return
  fi

  declare names=("${instances[@]/:*/}")
  declare zones=("${instances[@]/*:/}")

  (
    set -x
    gcloud beta compute instances delete --zone "${zones[0]}" --quiet "${names[@]}"
  )
}


#
# cloud_FetchFile [instanceName] [publicIp] [remoteFile] [localFile]
#
# Fetch a file from the given instance.  This function uses a cloud-specific
# mechanism to fetch the file
#
cloud_FetchFile() {
  declare instanceName="$1"
  # shellcheck disable=SC2034 # publicIp is unused
  declare publicIp="$2"
  declare remoteFile="$3"
  declare localFile="$4"
  declare zone="$5"

  (
    set -x
    gcloud compute scp --zone "$zone" "$instanceName:$remoteFile" "$localFile"
  )
}
