import React from "react";
import { SignatureProps } from "pages/tx/[signature]";
import { useTransactionDetails } from "src/providers/transactions";
import { ProgramLogsCardBody } from "src/components/ProgramLogsCardBody";
import { parseProgramLogs } from "src/utils/program-logs";
import { useCluster } from "src/providers/cluster";

export function ProgramLogSection({ signature }: SignatureProps) {
  const { cluster, url } = useCluster();
  const details = useTransactionDetails(signature);

  const transactionWithMeta = details?.data?.transactionWithMeta;
  if (!transactionWithMeta) return null;
  const message = transactionWithMeta.transaction.message;

  const logMessages = transactionWithMeta.meta?.logMessages || null;
  const err = transactionWithMeta.meta?.err || null;

  let prettyLogs = null;
  if (logMessages !== null) {
    prettyLogs = parseProgramLogs(logMessages, err, cluster);
  }

  return (
    <>
      <div className="card">
        <div className="card-header">
          <h3 className="card-header-title">Program Instruction Logs</h3>
        </div>
        {prettyLogs !== null ? (
          <ProgramLogsCardBody
            message={message}
            logs={prettyLogs}
            cluster={cluster}
            url={url}
          />
        ) : (
          <div className="card-body">
            Logs not supported for this transaction
          </div>
        )}
      </div>
    </>
  );
}
