/** Whether an account type has a network-backed skin that can be refreshed. */
export function canRefreshSkin(accountType: string | null | undefined): boolean {
  return accountType === 'Microsoft' || accountType === 'ElyBy';
}
