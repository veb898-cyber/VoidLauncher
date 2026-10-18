/**
 * Parsing of saved server addresses, mirroring the vanilla client and the
 * Rust side (`server_ping::parse_host_port`): `host`, `host:port`,
 * `[ipv6]` and `[ipv6]:port`; a bare IPv6 literal without brackets is a
 * host (its colons are not a port). Default port is 25565.
 */

export interface ParsedServerAddress {
  host: string;
  port: number;
}

export const DEFAULT_SERVER_PORT = 25565;

function parsePort(text: string): number | null {
  if (!/^\d+$/.test(text)) return null;
  const port = Number(text);
  return port >= 1 && port <= 65535 ? port : null;
}

export function parseServerAddress(address: string): ParsedServerAddress | null {
  const addr = address.trim();
  if (!addr) return null;

  if (addr.startsWith('[')) {
    const end = addr.indexOf(']');
    if (end <= 1) return null; // missing ']' or empty host
    const host = addr.slice(1, end);
    const tail = addr.slice(end + 1);
    if (!tail) return { host, port: DEFAULT_SERVER_PORT };
    if (!tail.startsWith(':')) return null;
    const port = parsePort(tail.slice(1));
    return port === null ? null : { host, port };
  }

  const colons = addr.split(':').length - 1;
  if (colons === 0) return { host: addr, port: DEFAULT_SERVER_PORT };
  if (colons > 1) return { host: addr, port: DEFAULT_SERVER_PORT }; // bare IPv6

  const sep = addr.indexOf(':');
  const host = addr.slice(0, sep);
  if (!host) return null;
  const port = parsePort(addr.slice(sep + 1));
  return port === null ? null : { host, port };
}
