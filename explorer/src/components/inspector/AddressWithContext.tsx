import React from "react";
import { PublicKey, SystemProgram } from "@solana/web3.js";
import { Address } from "src/components/common/Address";
import {
  Account,
  useAccountInfo,
  useAddressLookupTable,
  useFetchAccountInfo,
} from "src/providers/accounts";
import { ClusterStatus, useCluster } from "src/providers/cluster";
import { addressLabel } from "src/utils/tx";
import { lamportsToSolString } from "src/utils";

type AccountValidator = (account: Account) => string | undefined;

export const createFeePayerValidator = (
  feeLamports: number
): AccountValidator => {
  return (account: Account): string | undefined => {
    if (account.details === undefined) return "Account doesn't exist";
    if (!account.details.owner.equals(SystemProgram.programId))
      return "Only system-owned accounts can pay fees";
    // TODO: Actually nonce accounts can pay fees too
    if (account.details.space > 0)
      return "Only unallocated accounts can pay fees";
    if (account.lamports < feeLamports) {
      return "Insufficient funds for fees";
    }
    return;
  };
};

export const programValidator = (account: Account): string | undefined => {
  if (account.details === undefined) return "Account doesn't exist";
  if (!account.details.executable)
    return "Only executable accounts can be invoked";
  return;
};

export function AddressFromLookupTableWithContext({
  lookupTableKey,
  lookupTableIndex,
}: {
  lookupTableKey: PublicKey;
  lookupTableIndex: number;
}) {
  const lookupTable = useAddressLookupTable(lookupTableKey.toBase58());
  const fetchAccountInfo = useFetchAccountInfo();
  React.useEffect(() => {
    if (!lookupTable) fetchAccountInfo(lookupTableKey);
  }, [lookupTableKey, lookupTable, fetchAccountInfo]);

  let pubkey;
  if (!lookupTable) {
    return (
      <span className="text-muted">
        <span className="spinner-grow spinner-grow-sm me-2"></span>
        Loading
      </span>
    );
  } else if (typeof lookupTable === "string") {
    return <div>Invalid Lookup Table</div>;
  } else if (lookupTableIndex < lookupTable.state.addresses.length) {
    pubkey = lookupTable.state.addresses[lookupTableIndex];
  } else {
    return <div>Invalid Lookup Table Index</div>;
  }

  return (
    <div className="d-flex align-items-end flex-column">
      <Address pubkey={pubkey} link />
      <AccountInfo pubkey={pubkey} />
    </div>
  );
}

export function AddressWithContext({
  pubkey,
  validator,
}: {
  pubkey: PublicKey;
  validator?: AccountValidator;
}) {
  return (
    <div className="d-flex align-items-end flex-column">
      <Address pubkey={pubkey} link />
      <AccountInfo pubkey={pubkey} validator={validator} />
    </div>
  );
}

function AccountInfo({
  pubkey,
  validator,
}: {
  pubkey: PublicKey;
  validator?: AccountValidator;
}) {
  const address = pubkey.toBase58();
  const fetchAccount = useFetchAccountInfo();
  const info = useAccountInfo(address);
  const { cluster, status } = useCluster();

  // Fetch account on load
  React.useEffect(() => {
    if (!info && status === ClusterStatus.Connected && pubkey) {
      fetchAccount(pubkey);
    }
  }, [address, status]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!info?.data)
    return (
      <span className="text-muted">
        <span className="spinner-grow spinner-grow-sm me-2"></span>
        Loading
      </span>
    );

  const errorMessage = validator && validator(info.data);
  if (errorMessage) return <span className="text-warning">{errorMessage}</span>;

  if (!info.data.details) {
    return <span className="text-muted">Account doesn&apos;t exist</span>;
  }

  const owner = info.data.details.owner;
  const ownerAddress = owner.toBase58();
  const ownerLabel = addressLabel(ownerAddress, cluster);

  return (
    <span className="text-muted">
      {`Owned by ${ownerLabel || ownerAddress}.`}
      {` Balance is ${lamportsToSolString(info.data.lamports)} SOL.`}
      {` Size is ${new Intl.NumberFormat("en-US").format(
        info.data.details.space
      )} byte(s).`}
    </span>
  );
}
