import { describe, expect, it } from 'vitest';
import { DEFAULT_SERVER_PORT, parseServerAddress } from './serverAddress';

describe('parseServerAddress', () => {
  it('parses a plain host with the default port', () => {
    expect(parseServerAddress('play.example.com')).toEqual({
      host: 'play.example.com',
      port: DEFAULT_SERVER_PORT,
    });
  });

  it('parses host:port and trims whitespace', () => {
    expect(parseServerAddress('  example.com:25566 ')).toEqual({
      host: 'example.com',
      port: 25566,
    });
    expect(parseServerAddress('127.0.0.1:19132')).toEqual({
      host: '127.0.0.1',
      port: 19132,
    });
  });

  it('parses bracketed IPv6 with and without a port', () => {
    expect(parseServerAddress('[2001:db8::1]')).toEqual({
      host: '2001:db8::1',
      port: DEFAULT_SERVER_PORT,
    });
    expect(parseServerAddress('[2001:db8::1]:25567')).toEqual({
      host: '2001:db8::1',
      port: 25567,
    });
  });

  it('treats a bare IPv6 literal as a host without a port', () => {
    expect(parseServerAddress('2001:db8::1')).toEqual({
      host: '2001:db8::1',
      port: DEFAULT_SERVER_PORT,
    });
    expect(parseServerAddress('::1')).toEqual({
      host: '::1',
      port: DEFAULT_SERVER_PORT,
    });
  });

  it('rejects malformed addresses', () => {
    expect(parseServerAddress('')).toBeNull();
    expect(parseServerAddress('   ')).toBeNull();
    expect(parseServerAddress('host:0')).toBeNull();
    expect(parseServerAddress('host:65536')).toBeNull();
    expect(parseServerAddress('host:abc')).toBeNull();
    expect(parseServerAddress('host:')).toBeNull();
    expect(parseServerAddress(':25565')).toBeNull();
    expect(parseServerAddress('[::1')).toBeNull();
    expect(parseServerAddress('[]:25565')).toBeNull();
    expect(parseServerAddress('[::1]junk')).toBeNull();
  });
});
