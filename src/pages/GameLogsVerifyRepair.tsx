import { VerifyRepairModal } from '../components/instances/VerifyRepairModal';

interface GameLogsVerifySession {
  instance_name: string;
}

interface GameLogsVerifyRepairProps {
  session: GameLogsVerifySession | null;
  open: boolean;
  onClose: () => void;
}

/**
 * The GameLogs-owned instance connection to VerifyRepairModal.
 *
 * `key` makes an instance change a real remount, so no state from the previous
 * session can survive until the modal's own effects run. This is intentionally
 * a tiny component: the lifecycle itself lives in VerifyRepairModal and is
 * tested separately.
 */
export function GameLogsVerifyRepair({
  session,
  open,
  onClose,
}: GameLogsVerifyRepairProps) {
  return (
    <VerifyRepairModal
      key={session?.instance_name ?? 'none'}
      open={open && !!session}
      onClose={onClose}
      instanceName={session?.instance_name ?? ''}
    />
  );
}
